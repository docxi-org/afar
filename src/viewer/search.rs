//! Search in the viewer (F7, Shift+F7, Alt+F7), run in a background
//! thread over the file (Far's `DoSearchReplace`): text in the file's code
//! page — plain or a regular expression, case, whole words — or bytes.
//! Matches do not cross line breaks (Far has no multi-line search).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use regex::Regex;
use serde::{Deserialize, Serialize};

use super::codepage::{self, Codec};
use super::source::Source;

/// What to look for (shared by all viewers, as in Far).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Query {
    pub text: String,
    pub hex: bool,
    pub case: bool,
    pub regex: bool,
    pub words: bool,
}

/// Bytes searched at a time; consecutive pieces overlap so a match on a
/// boundary is found whole.
const PIECE: u64 = 1 << 20;
const OVERLAP: u64 = 64 * 1024;

enum Matcher {
    Bytes(Vec<u8>),
    Text(Regex),
}

impl Query {
    /// The bytes of a hex query ("0D 0A", "0d0a").
    pub fn hex_bytes(&self) -> Option<Vec<u8>> {
        let digits: String = self.text.chars().filter(|c| !c.is_whitespace()).collect();
        if digits.is_empty() || !digits.len().is_multiple_of(2) {
            return None;
        }
        (0..digits.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&digits[i..i + 2], 16).ok())
            .collect()
    }

    fn matcher(&self) -> Result<Matcher, String> {
        if self.hex {
            return self
                .hex_bytes()
                .map(Matcher::Bytes)
                .ok_or_else(|| "bad hex".to_string());
        }
        self.regex().map(Matcher::Text)
    }

    /// The text query compiled. A regular expression in Perl's form
    /// `/pattern/flags` (Far: a leading `/`) takes the flags `i`, `m`,
    /// `s`, `x`; "case" off adds `i`.
    pub fn regex(&self) -> Result<Regex, String> {
        let (pattern, flags) = if self.regex {
            match self
                .text
                .strip_prefix('/')
                .and_then(|rest| rest.rfind('/').map(|i| (&rest[..i], &rest[i + 1..])))
            {
                Some((p, f)) if f.chars().all(|c| "imsx".contains(c)) => (p.to_string(), f),
                _ => (self.text.clone(), ""),
            }
        } else {
            (regex::escape(&self.text), "")
        };
        regex::RegexBuilder::new(&pattern)
            .case_insensitive(!self.case || flags.contains('i'))
            .multi_line(true)
            .dot_matches_new_line(flags.contains('s'))
            .ignore_whitespace(flags.contains('x'))
            .build()
            .map_err(|e| e.to_string())
    }
}

/// A word character for "whole words" (Far: not in `WordDiv`, not a
/// blank).
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// A piece of the file decoded: the text and the file offset of each
/// character (by its byte index in the text).
struct Decoded {
    text: String,
    /// (byte index in `text`, file offset), ascending.
    offsets: Vec<(usize, u64)>,
    /// File offset after the last character.
    end: u64,
}

impl Decoded {
    fn file_pos(&self, text_index: usize) -> u64 {
        match self.offsets.binary_search_by_key(&text_index, |(i, _)| *i) {
            Ok(k) => self.offsets[k].1,
            Err(k) => self.offsets.get(k).map_or(self.end, |(_, p)| *p),
        }
    }
}

fn decode(src: &mut Source, codec: &Codec, from: u64, to: u64) -> Decoded {
    let bytes = src.read_vec(from, (to - from) as usize);
    let mut text = String::with_capacity(bytes.len());
    let mut offsets = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let (ch, n) = codec.decode(&bytes[i..]);
        offsets.push((text.len(), from + i as u64));
        text.push(ch);
        i += n.max(1);
    }
    Decoded {
        text,
        offsets,
        end: from + i as u64,
    }
}

/// Matches (start, end) in `from..to`, in file offsets.
fn matches_in(
    src: &mut Source,
    codec: &Codec,
    m: &Matcher,
    words: bool,
    from: u64,
    to: u64,
) -> Vec<(u64, u64)> {
    match m {
        Matcher::Bytes(pat) => {
            let bytes = src.read_vec(from, (to - from) as usize);
            if pat.is_empty() || bytes.len() < pat.len() {
                return Vec::new();
            }
            bytes
                .windows(pat.len())
                .enumerate()
                .filter(|(_, w)| w == pat)
                .map(|(i, _)| (from + i as u64, from + (i + pat.len()) as u64))
                .collect()
        }
        Matcher::Text(re) => {
            let d = decode(src, codec, from, to);
            re.find_iter(&d.text)
                .filter(|f| !f.as_str().is_empty())
                .filter(|f| {
                    !words
                        || (!d.text[..f.start()].chars().next_back().is_some_and(is_word)
                            && !d.text[f.end()..].chars().next().is_some_and(is_word))
                })
                .map(|f| (d.file_pos(f.start()), d.file_pos(f.end())))
                .collect()
        }
    }
}

pub enum Found {
    At(u64, u64),
    /// Reached the end (or the start, backward) without a match.
    Edge,
    Cancelled,
}

/// Looks for the first match starting at or after `from` and before
/// `limit` (forward), or the last one starting before `from` and at or
/// after `limit` (backward).
pub fn find(
    path: &Path,
    cp: u32,
    query: &Query,
    from: u64,
    limit: u64,
    backward: bool,
    cancel: &AtomicBool,
) -> Result<Found, String> {
    let mut src = Source::open(path).map_err(|e| e.to_string())?;
    let codec = Codec::new(cp)
        .or_else(|| Codec::new(codepage::UTF8))
        .expect("UTF-8 is always supported");
    let m = query.matcher()?;
    let unit = codec.unit() as u64;
    let align = |p: u64| p - p % unit;
    if !backward {
        let limit = limit.min(src.size());
        let mut start = align(from);
        while start < limit {
            if cancel.load(Ordering::Relaxed) {
                return Ok(Found::Cancelled);
            }
            let end = (start + PIECE + OVERLAP).min(src.size());
            let main_end = (start + PIECE).min(limit);
            let hit = matches_in(&mut src, &codec, &m, query.words, start, end)
                .into_iter()
                .find(|(s, _)| *s >= from && *s < main_end);
            if let Some((s, e)) = hit {
                return Ok(Found::At(s, e));
            }
            start = main_end;
        }
    } else {
        let mut end = from.min(src.size());
        while end > limit {
            if cancel.load(Ordering::Relaxed) {
                return Ok(Found::Cancelled);
            }
            let main_start = align(end.saturating_sub(PIECE).max(limit));
            let read_to = (end + OVERLAP).min(src.size());
            let hit = matches_in(&mut src, &codec, &m, query.words, main_start, read_to)
                .into_iter()
                .rfind(|(s, _)| *s < end && *s >= main_start);
            if let Some((s, e)) = hit {
                return Ok(Found::At(s, e));
            }
            end = main_start;
        }
    }
    Ok(Found::Edge)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "afar-search-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.txt");
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn at(r: Result<Found, String>) -> Option<(u64, u64)> {
        match r.unwrap() {
            Found::At(s, e) => Some((s, e)),
            _ => None,
        }
    }

    #[test]
    fn finds_text_both_ways() {
        let path = file("один Два два\nдва".as_bytes());
        let no = AtomicBool::new(false);
        let q = Query {
            text: "два".into(),
            ..Default::default()
        };
        let cp = codepage::UTF8;
        // Case-insensitive by default: "Два" first.
        assert_eq!(
            at(find(&path, cp, &q, 0, u64::MAX, false, &no)),
            Some((9, 15))
        );
        assert_eq!(
            at(find(&path, cp, &q, 10, u64::MAX, false, &no)),
            Some((16, 22))
        );
        assert_eq!(at(find(&path, cp, &q, 23, 0, true, &no)), Some((16, 22)));
        let q = Query { case: true, ..q };
        assert_eq!(
            at(find(&path, cp, &q, 0, u64::MAX, false, &no)),
            Some((16, 22))
        );
        assert!(matches!(
            find(&path, cp, &q, 30, u64::MAX, false, &no),
            Ok(Found::Edge)
        ));
    }

    #[test]
    fn whole_words_regex_hex_and_codepages() {
        let no = AtomicBool::new(false);
        let path = file(b"cat concat cat.");
        let q = Query {
            text: "cat".into(),
            words: true,
            ..Default::default()
        };
        assert_eq!(
            at(find(&path, codepage::UTF8, &q, 1, u64::MAX, false, &no)),
            Some((11, 14))
        );
        let q = Query {
            text: r"c\w+t".into(),
            regex: true,
            ..Default::default()
        };
        assert_eq!(
            at(find(&path, codepage::UTF8, &q, 1, u64::MAX, false, &no)),
            Some((4, 10))
        );
        let q = Query {
            text: "2e".into(),
            hex: true,
            ..Default::default()
        };
        assert_eq!(
            at(find(&path, codepage::UTF8, &q, 0, u64::MAX, false, &no)),
            Some((14, 15))
        );
        let bytes = Codec::new(1251).unwrap().encode("строка ПОИСК тут");
        let path = file(&bytes);
        let q = Query {
            text: "поиск".into(),
            ..Default::default()
        };
        assert_eq!(
            at(find(&path, 1251, &q, 0, u64::MAX, false, &no)),
            Some((7, 12))
        );
    }
}
