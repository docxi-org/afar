//! Finding files (Far's Alt+F7, `findfile.cpp`): a walk through folders
//! in a background thread — names by Far's masks, contents by a text in
//! one or several code pages (case, whole words, "not containing") — that
//! reports what it finds in batches and stops when asked.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use crate::masks::FileMasks;
use crate::viewer::codepage::{self, Codec};

/// Where to look.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    /// These folders and everything inside them.
    Trees(Vec<PathBuf>),
    /// This folder only.
    Folder(PathBuf),
}

/// Which code pages the text is looked for in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pages {
    /// UTF-8, UTF-16, ANSI and OEM (Far's "all standard code pages").
    Standard,
    One(u32),
}

#[derive(Clone, Debug)]
pub struct Query {
    pub masks: String,
    pub text: String,
    pub pages: Pages,
    pub case: bool,
    pub whole_words: bool,
    pub not_containing: bool,
    /// Folders matching the masks are found too (without a text).
    pub folders: bool,
    /// Go into folders that are symbolic links or junctions.
    pub links: bool,
    pub scope: Scope,
    /// The text is hexadecimal bytes (`48 65 6C`), no code page.
    pub hex: bool,
    /// Letters with and without diacritics alike (`ё` = `е`, `é` = `e`),
    /// any case (Far's "Fuzzy search").
    pub fuzzy: bool,
    /// Look in the files' alternate NTFS streams too (`file:stream`).
    pub streams: bool,
    /// Look for the text only in the first bytes of files.
    pub first_bytes: Option<u64>,
}

/// Hexadecimal bytes as typed (`48 65 6C`, `48656C`).
pub fn parse_hex(text: &str) -> Option<Vec<u8>> {
    let digits: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if digits.is_empty() || !digits.len().is_multiple_of(2) {
        return None;
    }
    (0..digits.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&digits[i..i + 2], 16).ok())
        .collect()
}

/// A letter without its diacritic (for the fuzzy search).
fn base_letter(c: char) -> char {
    const TABLE: &[(&str, char)] = &[
        ("ÀÁÂÃÄÅĀĂĄàáâãäåāăą", 'a'),
        ("ÇĆĈĊČçćĉċč", 'c'),
        ("ĎĐďđ", 'd'),
        ("ÈÉÊËĒĔĖĘĚèéêëēĕėęě", 'e'),
        ("ĜĞĠĢĝğġģ", 'g'),
        ("ĤĦĥħ", 'h'),
        ("ÌÍÎÏĨĪĬĮİìíîïĩīĭįı", 'i'),
        ("Ĵĵ", 'j'),
        ("Ķķ", 'k'),
        ("ĹĻĽĿŁĺļľŀł", 'l'),
        ("ÑŃŅŇñńņň", 'n'),
        ("ÒÓÔÕÖØŌŎŐòóôõöøōŏő", 'o'),
        ("ŔŖŘŕŗř", 'r'),
        ("ŚŜŞŠśŝşšß", 's'),
        ("ŢŤŦţťŧ", 't'),
        ("ÙÚÛÜŨŪŬŮŰŲùúûüũūŭůűų", 'u'),
        ("Ŵŵ", 'w'),
        ("ÝŸŶýÿŷ", 'y'),
        ("ŹŻŽźżž", 'z'),
        ("Ёё", 'е'),
        ("Йй", 'и'),
    ];
    let lower = c.to_lowercase().next().unwrap_or(c);
    TABLE
        .iter()
        .find(|(set, _)| set.contains(c))
        .map_or(lower, |(_, b)| *b)
}

/// Every letter with the same base as `c` (both cases).
fn fuzzy_forms(c: char) -> Vec<char> {
    let base = base_letter(c);
    let mut out: Vec<char> = Vec::new();
    let mut push = |x: char| {
        if !out.contains(&x) {
            out.push(x);
        }
    };
    push(c);
    for x in base.to_lowercase().chain(base.to_uppercase()) {
        push(x);
    }
    // The table's letters for this base.
    for x in ('\u{00C0}'..='\u{017F}').chain(['Ё', 'ё', 'Й', 'й']) {
        if base_letter(x) == base {
            push(x);
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

/// What the search tells its owner.
#[derive(Debug)]
pub enum Event {
    Found(Vec<Found>),
    /// The folder being searched.
    In(PathBuf),
    Done,
}

/// The text to look for, encoded in a code page: per character the
/// byte sequences it may be (its two cases when not case-sensitive).
struct Needle {
    codec: Codec,
    chars: Vec<Vec<Vec<u8>>>,
}

impl Needle {
    fn new(text: &str, cp: u32, case: bool, fuzzy: bool) -> Option<Self> {
        let codec = Codec::new(cp)?;
        let chars = text
            .chars()
            .map(|c| {
                let mut forms: Vec<Vec<u8>> = vec![codec.encode(&c.to_string())];
                if fuzzy {
                    for f in fuzzy_forms(c) {
                        let b = codec.encode(&f.to_string());
                        if !b.is_empty() && !forms.contains(&b) {
                            forms.push(b);
                        }
                    }
                } else if !case {
                    for f in c.to_lowercase().chain(c.to_uppercase()) {
                        let b = codec.encode(&f.to_string());
                        if !forms.contains(&b) {
                            forms.push(b);
                        }
                    }
                }
                forms
            })
            .collect::<Vec<_>>();
        (!chars.is_empty() && chars.iter().all(|f| f.iter().all(|b| !b.is_empty())))
            .then_some(Self { codec, chars })
    }

    /// Exact bytes (a hexadecimal search): one form per byte.
    fn bytes(bytes: &[u8]) -> Option<Self> {
        (!bytes.is_empty()).then(|| Self {
            codec: Codec::new(1252)
                .or_else(|| Codec::new(codepage::UTF8))
                .expect("a code page"),
            chars: bytes.iter().map(|b| vec![vec![*b]]).collect(),
        })
    }

    /// The length of a match at `at`, if there is one.
    fn match_at(&self, data: &[u8], at: usize) -> Option<usize> {
        let mut p = at;
        for forms in &self.chars {
            let f = forms.iter().find(|f| data[p..].starts_with(f))?;
            p += f.len();
        }
        Some(p - at)
    }

    /// The character before `at` (whole words): a few bytes back.
    fn char_before(&self, data: &[u8], at: usize) -> Option<char> {
        let unit = self.codec.unit();
        (1..=4)
            .map(|n| n * unit)
            .filter(|n| *n <= at)
            .find_map(|n| {
                let (c, len) = self.codec.decode(&data[at - n..at]);
                (len == n).then_some(c)
            })
    }

    fn found_in(&self, data: &[u8], whole_words: bool) -> bool {
        let unit = self.codec.unit();
        let first = &self.chars[0];
        let mut at = 0;
        while at < data.len() {
            if first.iter().any(|f| data[at..].starts_with(f))
                && let Some(len) = self.match_at(data, at)
            {
                if !whole_words {
                    return true;
                }
                let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
                let after = data
                    .get(at + len..)
                    .filter(|r| !r.is_empty())
                    .map(|r| self.codec.decode(r).0);
                if !word(self.char_before(data, at)) && !word(after) {
                    return true;
                }
            }
            at += unit;
        }
        false
    }
}

/// The code pages of `Pages::Standard`.
fn standard_pages() -> Vec<u32> {
    let mut pages = vec![codepage::UTF8, 1200, codepage::ansi(), codepage::oem()];
    pages.dedup();
    pages
}

/// Whether the file holds the text in one of the needles (read in
/// blocks with an overlap, stopping when cancelled).
fn file_contains(
    path: &Path,
    needles: &[Needle],
    whole_words: bool,
    first: Option<u64>,
    cancel: &AtomicBool,
) -> bool {
    use std::io::Read as _;
    const BLOCK: usize = 1 << 20;
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let longest = needles
        .iter()
        .map(|n| {
            n.chars
                .iter()
                .map(|f| f.iter().map(Vec::len).max().unwrap_or(0))
                .sum::<usize>()
        })
        .max()
        .unwrap_or(0);
    // Room for the needle and the characters around it (whole words).
    let keep = longest + 16;
    let mut buf: Vec<u8> = Vec::with_capacity(BLOCK + keep);
    let mut chunk = vec![0u8; BLOCK];
    let mut read_so_far = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        let n = match f.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        // Only the first bytes: the rest of this block is cut.
        let n = match first {
            Some(limit) => {
                let left = limit.saturating_sub(read_so_far) as usize;
                if left == 0 {
                    break;
                }
                n.min(left)
            }
            None => n,
        };
        read_so_far += n as u64;
        buf.extend_from_slice(&chunk[..n]);
        if needles.iter().any(|nd| nd.found_in(&buf, whole_words)) {
            return true;
        }
        // Keep the tail, even-aligned for UTF-16.
        let cut = buf.len().saturating_sub(keep) & !1;
        buf.drain(..cut);
    }
    false
}

/// Starts the search; `send` gets the events (the last is `Done`).
pub fn start(
    query: Query,
    cancel: Arc<AtomicBool>,
    send: impl Fn(Event) + Send + 'static,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        search(&query, &cancel, &send);
        send(Event::Done);
    })
}

fn search(q: &Query, cancel: &AtomicBool, send: &dyn Fn(Event)) {
    let masks_text = if q.masks.trim().is_empty() {
        "*"
    } else {
        q.masks.as_str()
    };
    let Some(masks) = FileMasks::parse(masks_text) else {
        return;
    };
    let needles: Vec<Needle> = if q.text.is_empty() {
        Vec::new()
    } else {
        let pages = match q.pages {
            Pages::Standard => standard_pages(),
            Pages::One(cp) => vec![cp],
        };
        if q.hex {
            parse_hex(&q.text)
                .and_then(|b| Needle::bytes(&b))
                .into_iter()
                .collect()
        } else {
            pages
                .into_iter()
                .filter_map(|cp| Needle::new(&q.text, cp, q.case, q.fuzzy))
                .collect()
        }
    };
    let (roots, deep) = match &q.scope {
        Scope::Trees(t) => (t.clone(), true),
        Scope::Folder(f) => (vec![f.clone()], false),
    };
    let mut batch = Vec::new();
    let mut last = Instant::now();
    let mut stack: Vec<PathBuf> = roots.into_iter().rev().collect();
    while let Some(dir) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        send(Event::In(dir.clone()));
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut subdirs = Vec::new();
        for entry in read.flatten() {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            let Ok(ft) = entry.file_type() else { continue };
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = ft.is_dir() || ft.is_symlink() && path.is_dir();
            if is_dir {
                if deep && (q.links || !ft.is_symlink()) {
                    subdirs.push(path.clone());
                }
                if q.folders && needles.is_empty() && masks.matches(&name) {
                    batch.push(found(path, true));
                }
                continue;
            }
            if !masks.matches(&name) {
                continue;
            }
            let contains =
                |p: &Path| file_contains(p, &needles, q.whole_words, q.first_bytes, cancel);
            let hit = needles.is_empty() || contains(&path) != q.not_containing;
            if hit {
                batch.push(found(path.clone(), false));
            }
            // The text in the file's other streams: `file:stream`.
            if q.streams && !needles.is_empty() {
                for stream in streams_of(&path) {
                    let sp = PathBuf::from(format!("{}:{stream}", path.display()));
                    if contains(&sp) != q.not_containing {
                        batch.push(found(sp, false));
                    }
                }
            }
            if !batch.is_empty() && last.elapsed() > Duration::from_millis(100) {
                send(Event::Found(std::mem::take(&mut batch)));
                last = Instant::now();
            }
        }
        subdirs.sort_by_key(|p| p.to_string_lossy().to_lowercase());
        stack.extend(subdirs.into_iter().rev());
        if !batch.is_empty() {
            send(Event::Found(std::mem::take(&mut batch)));
            last = Instant::now();
        }
    }
}

/// The names of a file's alternate data streams (not the main one).
fn streams_of(path: &Path) -> Vec<String> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        FindClose, FindFirstStreamW, FindNextStreamW, FindStreamInfoStandard,
        WIN32_FIND_STREAM_DATA,
    };
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: a zeroed out-structure; the handle is closed below.
    let mut data: WIN32_FIND_STREAM_DATA = unsafe { std::mem::zeroed() };
    let h = unsafe {
        FindFirstStreamW(
            wide.as_ptr(),
            FindStreamInfoStandard,
            (&mut data as *mut WIN32_FIND_STREAM_DATA).cast(),
            0,
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Vec::new();
    }
    let mut out = Vec::new();
    loop {
        let len = data.cStreamName.iter().position(|c| *c == 0).unwrap_or(0);
        let name = String::from_utf16_lossy(&data.cStreamName[..len]);
        // ":name:$DATA"; "::$DATA" is the file itself.
        if let Some(n) = name
            .strip_prefix(':')
            .and_then(|r| r.strip_suffix(":$DATA"))
            .filter(|n| !n.is_empty())
        {
            out.push(n.to_string());
        }
        // SAFETY: the handle from FindFirstStreamW.
        if unsafe { FindNextStreamW(h, (&mut data as *mut WIN32_FIND_STREAM_DATA).cast()) } == 0 {
            break;
        }
    }
    unsafe { FindClose(h) };
    out
}

fn found(path: PathBuf, is_dir: bool) -> Found {
    let meta = std::fs::metadata(&path).ok();
    Found {
        size: meta
            .as_ref()
            .map_or(0, |m| if is_dir { 0 } else { m.len() }),
        modified: meta.and_then(|m| m.modified().ok()),
        path,
        is_dir,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(q: Query) -> Vec<String> {
        let (tx, rx) = std::sync::mpsc::channel();
        start(q, Arc::new(AtomicBool::new(false)), move |e| {
            let _ = tx.send(e);
        })
        .join()
        .unwrap();
        let mut names: Vec<String> = rx
            .try_iter()
            .filter_map(|e| match e {
                Event::Found(f) => Some(f),
                _ => None,
            })
            .flatten()
            .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn finds_by_masks_and_text() {
        let dir = std::env::temp_dir().join(format!("afar-find-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), "Hello World").unwrap();
        std::fs::write(dir.join("b.txt"), "helloworld").unwrap();
        // UTF-16LE with a BOM, Cyrillic.
        let mut utf16 = vec![0xFF, 0xFE];
        utf16.extend("Привет мир".encode_utf16().flat_map(u16::to_le_bytes));
        std::fs::write(dir.join("sub").join("c.log"), utf16).unwrap();
        let q = |masks: &str, text: &str| Query {
            masks: masks.into(),
            text: text.into(),
            pages: Pages::Standard,
            case: false,
            whole_words: false,
            not_containing: false,
            folders: false,
            links: false,
            scope: Scope::Trees(vec![dir.clone()]),
            hex: false,
            fuzzy: false,
            streams: false,
            first_bytes: None,
        };
        assert_eq!(run(q("*.txt", "")), ["a.txt", "b.txt"]);
        assert_eq!(run(q("*", "hello")), ["a.txt", "b.txt"]);
        assert_eq!(run(q("*", "мир")), ["c.log"]);
        assert_eq!(
            run(Query {
                case: true,
                ..q("*", "hello")
            }),
            ["b.txt"]
        );
        assert_eq!(
            run(Query {
                whole_words: true,
                ..q("*", "world")
            }),
            ["a.txt"]
        );
        assert_eq!(
            run(Query {
                not_containing: true,
                ..q("*.txt", "World")
            }),
            Vec::<String>::new()
        );
        assert_eq!(
            run(Query {
                scope: Scope::Folder(dir.clone()),
                ..q("*", "")
            }),
            ["a.txt", "b.txt"]
        );
        assert_eq!(
            run(Query {
                folders: true,
                ..q("su*", "")
            }),
            ["sub"]
        );
        // Hexadecimal: "Hel" exactly.
        assert_eq!(
            run(Query {
                hex: true,
                ..q("*.txt", "48 65 6C")
            }),
            ["a.txt"]
        );
        // Fuzzy: "ё" = "е", "é" = "e".
        std::fs::write(dir.join("e.txt"), "ёлка café").unwrap();
        assert_eq!(
            run(Query {
                fuzzy: true,
                ..q("e.txt", "елка cafe")
            }),
            ["e.txt"]
        );
        assert_eq!(run(q("e.txt", "елка")), Vec::<String>::new());
        // Only the first bytes.
        assert_eq!(
            run(Query {
                first_bytes: Some(3),
                ..q("a.txt", "World")
            }),
            Vec::<String>::new()
        );
        // An alternate stream.
        std::fs::write(dir.join("a.txt:notes"), "secret").unwrap();
        assert_eq!(
            run(Query {
                streams: true,
                ..q("a.txt", "secret")
            }),
            ["a.txt:notes"]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
