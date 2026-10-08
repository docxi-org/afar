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
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
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

/// A query compiled, for the matches on the screen.
pub struct Pattern {
    m: Matcher,
    words: bool,
}

impl Query {
    pub fn pattern(&self) -> Result<Pattern, String> {
        Ok(Pattern {
            m: self.matcher()?,
            words: self.words,
        })
    }
}

impl Pattern {
    /// Matches (start, end) in `from..to`.
    pub fn matches(&self, src: &mut Source, codec: &Codec, from: u64, to: u64) -> Vec<(u64, u64)> {
        matches_in(src, codec, &self.m, self.words, from, to)
    }
}

/// A match for the list of all of them.
#[derive(Clone, Debug)]
pub struct Hit {
    pub start: u64,
    pub end: u64,
    /// From 1, by line feeds (as the agent's tools count).
    pub line: u64,
    /// From 1, in characters.
    pub col: u64,
    /// The line's text (its start, at most `HIT_TEXT` characters).
    pub text: String,
}

/// Every match in the file: how many, where they start (the first
/// `STARTS_LIMIT`), and the first ones with their lines.
#[derive(Debug, Default)]
pub struct All {
    pub total: usize,
    pub starts: Vec<u64>,
    pub hits: Vec<Hit>,
    pub cancelled: bool,
}

const STARTS_LIMIT: usize = 1_000_000;
const HIT_TEXT: usize = 300;
/// Characters of the line shown before a match far from its start.
const HIT_BEFORE: usize = 30;
/// How far back the start of a match's line is looked for (its column).
const LINE_BACK: u64 = 64 * 1024;

/// Goes through the whole file for every match (the counter, the list):
/// `hits` of them get their line and text.
pub fn find_all(
    path: &Path,
    cp: u32,
    query: &Query,
    hits: usize,
    cancel: &AtomicBool,
) -> Result<All, String> {
    let mut src = Source::open(path).map_err(|e| e.to_string())?;
    let codec = Codec::new(cp)
        .or_else(|| Codec::new(codepage::UTF8))
        .expect("UTF-8 is always supported");
    let m = query.matcher()?;
    let size = src.size();
    let mut all = All::default();
    // Lines counted so far: the line of `counted`.
    let (mut counted, mut line) = (0u64, 1u64);
    let mut start = 0u64;
    while start < size {
        if cancel.load(Ordering::Relaxed) {
            all.cancelled = true;
            return Ok(all);
        }
        let end = (start + PIECE + OVERLAP).min(size);
        let main_end = (start + PIECE).min(size);
        for (s, e) in matches_in(&mut src, &codec, &m, query.words, start, end) {
            if s < start || s >= main_end || all.starts.last().is_some_and(|l| *l >= s) {
                continue;
            }
            all.total += 1;
            if all.starts.len() < STARTS_LIMIT {
                all.starts.push(s);
            }
            if all.hits.len() < hits {
                line += super::lines::count_feeds(&mut src, &codec, counted, s);
                counted = s;
                let (text, col) = line_around(&mut src, &codec, s);
                all.hits.push(Hit {
                    start: s,
                    end: e,
                    line,
                    col,
                    text,
                });
            }
        }
        start = main_end;
    }
    Ok(all)
}

/// The text of the line holding `pos` and the column of `pos` in it
/// (from 1): from a little before the match (`…` when the line starts
/// earlier), at most `HIT_TEXT` characters.
fn line_around(src: &mut Source, codec: &Codec, pos: u64) -> (String, u64) {
    let u = codec.unit() as u64;
    let back = LINE_BACK.min(pos);
    let mut begin = pos;
    while begin > pos - back {
        match super::layout::unit_before(src, codec, begin) {
            Some('\n' | '\r') => break,
            _ => begin -= u,
        }
    }
    // Before the match: counted, the last `HIT_BEFORE` characters kept.
    let before = src.read_vec(begin, (pos - begin) as usize);
    let mut kept: Vec<char> = Vec::new();
    let (mut col, mut i) = (1u64, 0usize);
    while i < before.len() {
        let (c, n) = codec.decode(&before[i..]);
        i += n.max(1);
        // The byte order mark is no character of the line.
        if begin == 0 && i == n.max(1) && c == '\u{FEFF}' {
            continue;
        }
        col += 1;
        kept.push(if c == '\t' { ' ' } else { c });
    }
    let mut text = String::new();
    if kept.len() > HIT_BEFORE {
        text.push('…');
        text.extend(&kept[kept.len() - HIT_BEFORE..]);
    } else {
        text.extend(&kept);
    }
    let bytes = src.read_vec(pos, HIT_TEXT * 4);
    let (mut i, mut n_chars) = (0usize, text.chars().count());
    while i < bytes.len() && n_chars < HIT_TEXT {
        let (c, n) = codec.decode(&bytes[i..]);
        if c == '\n' || c == '\r' {
            break;
        }
        text.push(if c == '\t' { ' ' } else { c });
        n_chars += 1;
        i += n.max(1);
    }
    (text, col)
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
    fn finds_all_with_lines() {
        let path = file("a cat\nno\ncat cat\n".as_bytes());
        let no = AtomicBool::new(false);
        let q = Query {
            text: "cat".into(),
            ..Default::default()
        };
        let all = find_all(&path, codepage::UTF8, &q, 2, &no).unwrap();
        assert_eq!(all.total, 3);
        assert_eq!(all.starts, vec![2, 9, 13]);
        assert_eq!(all.hits.len(), 2);
        assert_eq!((all.hits[0].line, all.hits[0].col), (1, 3));
        assert_eq!(all.hits[0].text, "a cat");
        assert_eq!((all.hits[1].line, all.hits[1].col), (3, 1));
        assert_eq!(all.hits[1].text, "cat cat");
        // A long line: from a little before the match; a byte order mark
        // is no character.
        let long = format!("\u{FEFF}{}cat\n", "x".repeat(100));
        let path = file(long.as_bytes());
        let all = find_all(&path, codepage::UTF8, &q, 1, &no).unwrap();
        assert_eq!(all.hits[0].col, 101);
        assert_eq!(all.hits[0].text, format!("…{}cat", "x".repeat(30)));
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
