//! Syntax highlighting in the viewer (`crate::syntax`): the viewer keeps
//! byte offsets, so the parse states are kept at the starts of lines (by
//! offset, every `STEP` lines from a known state) and the colors come out
//! as byte ranges for the cells to look up.

use ratatui::style::Color;
use syntect::parsing::SyntaxReference;

use super::codepage::Codec;
use super::layout::char_at;
use super::source::Source;
use crate::syntax::{LINE_LIMIT, LineState};

/// The parse state is kept before every this many lines.
const STEP: usize = 64;
/// A place this many bytes past the nearest kept state is parsed afresh
/// from a few lines above it (a jump to the end of a huge file stays
/// quick; colors there may be off at first).
const REACH: u64 = 1 << 20;
/// Lines (and bytes at most) a fresh parse starts above the screen.
const LOOKBACK_LINES: usize = 200;
const LOOKBACK_BYTES: u64 = 64 * 1024;

/// A colored piece of the text: bytes `from..to`.
pub type Range = (u64, u64, Color);

pub struct Highlight {
    /// On (Alt+F3).
    pub on: bool,
    syntax: Option<&'static SyntaxReference>,
    /// Parse states at line starts, sorted by offset.
    marks: Vec<(u64, LineState)>,
}

impl Highlight {
    /// The syntax of `path`, by its name or its first line (read from
    /// `head` in the file's code page).
    pub fn new(on: bool, path: &std::path::Path, head: &[u8], codec: &Codec) -> Self {
        let mut first = String::new();
        let mut i = 0;
        while i < head.len() && first.len() < 256 {
            let (c, n) = codec.decode(&head[i..]);
            if c == '\n' || c == '\r' {
                break;
            }
            if !(i == 0 && c == '\u{FEFF}') {
                first.push(c);
            }
            i += n.max(1);
        }
        Self {
            on,
            syntax: crate::syntax::syntax_for(path, &first),
            marks: Vec::new(),
        }
    }

    /// The name of the file's syntax, if it has one.
    pub fn name(&self) -> Option<&'static str> {
        self.syntax.map(|s| s.name.as_str())
    }

    /// The file or its code page changed: the states go.
    pub fn reset(&mut self) {
        self.marks.clear();
    }

    /// The colored ranges of the lines from the one holding `from` up to
    /// `to` (empty when off or without a syntax).
    pub fn ranges(&mut self, src: &mut Source, codec: &Codec, from: u64, to: u64) -> Vec<Range> {
        let Some(syn) = self.syntax.filter(|_| self.on) else {
            return Vec::new();
        };
        if self.marks.is_empty() {
            self.marks.push((0, LineState::start(syn)));
        }
        let first = line_begin(src, codec, from, LINE_LIMIT as u64 * 4);
        let k = self.marks.partition_point(|(o, _)| *o <= first) - 1;
        let (mut pos, mut st, exact) = if first - self.marks[k].0 <= REACH {
            (self.marks[k].0, self.marks[k].1.clone(), true)
        } else {
            (lines_back(src, codec, first), LineState::start(syn), false)
        };
        let mut out = Vec::new();
        let mut n = 0usize;
        while pos < to && pos < src.size() {
            if exact && n > 0 && n.is_multiple_of(STEP) {
                let at = self.marks.partition_point(|(o, _)| *o < pos);
                if self.marks.get(at).is_none_or(|(o, _)| *o != pos) {
                    self.marks.insert(at, (pos, st.clone()));
                }
            }
            let line = read_line(src, codec, pos, to);
            match &line.text {
                Some((text, offs)) => {
                    let pieces = st.line(text);
                    if line.next > from {
                        for (s, len, color) in pieces {
                            let a = offs[s];
                            let b = offs.get(s + len).copied().unwrap_or(line.end);
                            out.push((a, b, color));
                        }
                    }
                }
                // Too long: a line for the state, not colored.
                None => {
                    st.line("");
                }
            }
            if line.next <= pos {
                break;
            }
            pos = line.next;
            n += 1;
        }
        out
    }
}

/// The color of the byte at `pos` from sorted `ranges`.
pub fn color_at(ranges: &[Range], pos: u64) -> Option<Color> {
    let k = ranges.partition_point(|(a, _, _)| *a <= pos);
    let (_, b, c) = ranges.get(k.checked_sub(1)?)?;
    (pos < *b).then_some(*c)
}

/// The start of the line holding `pos`, looking back at most `limit`
/// bytes (then a unit boundary there).
fn line_begin(src: &mut Source, codec: &Codec, pos: u64, limit: u64) -> u64 {
    let u = codec.unit() as u64;
    let stop = pos.saturating_sub(limit);
    let mut q = pos - pos % u;
    while q > stop {
        if q >= u
            && let Some((c, n)) = char_at(src, codec, q - u)
            && n as u64 == u
            && (c == '\n' || c == '\r')
        {
            return q;
        }
        q -= u;
    }
    q
}

/// Where a fresh parse for the line at `pos` starts: some lines above.
fn lines_back(src: &mut Source, codec: &Codec, pos: u64) -> u64 {
    let u = codec.unit() as u64;
    let stop = pos.saturating_sub(LOOKBACK_BYTES);
    let is_break = |src: &mut Source, q: u64| {
        q >= u && matches!(char_at(src, codec, q - u), Some(('\n' | '\r', n)) if n as u64 == u)
    };
    let mut q = pos;
    for _ in 0..LOOKBACK_LINES {
        // Over the line break ending the line above, to its start.
        let mut b = q;
        while b > stop && is_break(src, b) {
            b -= u;
        }
        if b <= stop {
            break;
        }
        q = line_begin(src, codec, b, b - stop);
    }
    q
}

struct Line {
    /// The text and the offset of each character; `None` for a line too
    /// long to color.
    text: Option<(String, Vec<u64>)>,
    /// Where the text ends (the line break) and the next line starts.
    end: u64,
    next: u64,
}

/// The line at `pos`. A long line is read on only up to `stop` (what is
/// beyond is not needed).
fn read_line(src: &mut Source, codec: &Codec, pos: u64, stop: u64) -> Line {
    let mut text = String::new();
    let mut offs = Vec::new();
    let mut long = false;
    let mut p = pos;
    while let Some((c, n)) = char_at(src, codec, p) {
        let n = n.max(1) as u64;
        if c == '\n' || c == '\r' {
            let end = p;
            p += n;
            if c == '\r' {
                match char_at(src, codec, p) {
                    Some(('\n', m)) => p += m as u64,
                    Some(('\r', m)) => {
                        if let Some(('\n', k)) = char_at(src, codec, p + m as u64) {
                            p += (m + k) as u64;
                        }
                    }
                    _ => {}
                }
            }
            return Line {
                text: (!long).then_some((text, offs)),
                end,
                next: p,
            };
        }
        if !long {
            if p == 0 && c == '\u{FEFF}' {
                p += n;
                continue;
            }
            offs.push(p);
            text.push(c);
            if text.len() > LINE_LIMIT {
                long = true;
                text.clear();
                offs.clear();
            }
        } else if p >= stop {
            break;
        }
        p += n;
    }
    Line {
        text: (!long).then_some((text, offs)),
        end: p,
        next: p,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viewer::codepage::UTF8;

    fn source(name: &str, bytes: &[u8]) -> (Source, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("afar-vhl-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        (Source::open(&path).unwrap(), path)
    }

    #[test]
    fn colors_come_as_byte_ranges() {
        let text = "/* one\ntwo */ fn x() {}\r\nlet s = \"ü\";\n";
        let (mut src, path) = source("a.rs", text.as_bytes());
        let codec = Codec::new(UTF8).unwrap();
        let mut h = Highlight::new(true, &path, text.as_bytes(), &codec);
        assert_eq!(h.name(), Some("Rust"));
        let size = src.size();
        let r = h.ranges(&mut src, &codec, 0, size);
        let at = |s: &str| text.find(s).unwrap() as u64;
        let comment = color_at(&r, at("two"));
        assert_eq!(comment, Some(crate::theme::SYNTAX[0]));
        assert_eq!(color_at(&r, at("fn")), Some(crate::theme::SYNTAX[3]));
        // After a two-byte character the offsets still match.
        assert_eq!(color_at(&r, at("\";")), Some(crate::theme::SYNTAX[1]));
        assert_eq!(color_at(&r, at(";\n")), None);
        // From the middle of the file: the comment is known from the top.
        let r = h.ranges(&mut src, &codec, at("two") + 2, size);
        assert_eq!(color_at(&r, at("two")), comment);
        h.on = false;
        assert!(h.ranges(&mut src, &codec, 0, size).is_empty());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn states_are_kept_every_step_lines() {
        let mut text = String::from("/*\n");
        for i in 0..300 {
            text.push_str(&format!("line {i}\n"));
        }
        text.push_str("*/ let x = 1;\n");
        let (mut src, path) = source("b.rs", text.as_bytes());
        let codec = Codec::new(UTF8).unwrap();
        let mut h = Highlight::new(true, &path, text.as_bytes(), &codec);
        let size = src.size();
        let last = text.find("let x").unwrap() as u64;
        let r = h.ranges(&mut src, &codec, last, size);
        assert_eq!(color_at(&r, last), Some(crate::theme::SYNTAX[3]));
        assert!(h.marks.len() >= 300 / STEP);
        assert!(h.marks.windows(2).all(|w| w[0].0 < w[1].0));
        std::fs::remove_file(&path).unwrap();
    }
}
