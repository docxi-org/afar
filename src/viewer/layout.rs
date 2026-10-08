//! Rows of the text mode, Far's way (viewer.cpp `ReadString`, `Up`): a row
//! starts at a byte offset and ends at a line break (CR, LF, CRLF, CRCRLF),
//! at the width of the window with wrapping, or at the maximum line size.
//! The previous row is found by scanning back for a line break and laying
//! the line out again, so any file opens at once at any offset.

use unicode_width::UnicodeWidthChar;

use super::codepage::Codec;
use super::source::Source;

/// How rows are cut.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Wrap {
    /// One row per line (a line longer than the maximum size is cut).
    None,
    /// At the window width.
    Chars(usize),
    /// At the window width, after the last space or word separator.
    Words(usize),
}

#[derive(Clone, Copy, Debug)]
pub struct TextOpts {
    pub tab: usize,
    pub max_line: usize,
    pub wrap: Wrap,
}

#[derive(Clone, Copy, Debug)]
pub struct Cell {
    pub ch: char,
    pub pos: u64,
    /// Bytes of the character, with the zero-width characters after it.
    pub len: u32,
    /// Column in the line (tabs expanded).
    pub col: usize,
    pub width: u8,
    /// Zero-width characters follow (combining marks): the cell's text is
    /// its bytes, not `ch` alone.
    pub combined: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Row {
    pub start: u64,
    /// Where the next row starts.
    pub end: u64,
    pub cells: Vec<Cell>,
    /// Width in columns.
    pub cols: usize,
}

/// Far's default word separators (editor `WordDiv`) and blanks.
const WORD_DIV: &str = "~!%^&*()+|{}:\"<>?`-=\\[];',./ \t";

/// The character at `pos` and its length.
pub fn char_at(src: &mut Source, codec: &Codec, pos: u64) -> Option<(char, usize)> {
    let mut buf = [0u8; 4];
    let n = src.read(pos, &mut buf);
    (n > 0).then(|| codec.decode(&buf[..n]))
}

/// The code unit ending at `pos` (`\n` and `\r` are single units in every
/// supported code page and never part of a longer character).
pub fn unit_before(src: &mut Source, codec: &Codec, pos: u64) -> Option<char> {
    let u = codec.unit() as u64;
    if pos < u {
        return None;
    }
    let (c, n) = char_at(src, codec, pos - u)?;
    (n as u64 == u).then_some(c)
}

/// Cells a character takes; the soft hyphen is shown (as `-`).
pub fn display_width(ch: char) -> usize {
    if ch == '\u{AD}' {
        return 1;
    }
    ch.width().unwrap_or(1)
}

pub fn read_row(src: &mut Source, codec: &Codec, start: u64, opts: &TextOpts) -> Row {
    let mut row = Row {
        start,
        end: start,
        cells: Vec::new(),
        cols: 0,
    };
    let width = match opts.wrap {
        Wrap::None => None,
        Wrap::Chars(w) | Wrap::Words(w) => Some(w.max(1)),
    };
    let words = matches!(opts.wrap, Wrap::Words(_));
    let mut p = start;
    let mut col = 0usize;
    // Cells up to the last word separator.
    let mut last_break: Option<usize> = None;
    while let Some((ch, n)) = char_at(src, codec, p) {
        if ch == '\n' {
            p += n as u64;
            break;
        }
        if ch == '\r' {
            p += n as u64;
            match char_at(src, codec, p) {
                Some(('\n', m)) => p += m as u64,
                Some(('\r', m)) => {
                    if let Some(('\n', k)) = char_at(src, codec, p + m as u64) {
                        p += (m + k) as u64;
                    }
                }
                _ => {}
            }
            break;
        }
        if p == 0 && ch == '\u{FEFF}' {
            p += n as u64;
            continue;
        }
        let w = display_width(ch);
        if w == 0 && !row.cells.is_empty() {
            let last = row.cells.last_mut().unwrap();
            last.len += n as u32;
            last.combined = true;
            p += n as u64;
            continue;
        }
        if row.cells.len() >= opts.max_line {
            break;
        }
        let mut w = if ch == '\t' {
            opts.tab - col % opts.tab
        } else {
            w.max(1)
        };
        if let Some(width) = width {
            if ch == '\t' && col < width {
                w = w.min(width - col);
            }
            if col + w > width && !row.cells.is_empty() {
                if words && let Some(b) = last_break.filter(|b| *b < row.cells.len()) {
                    p = row.cells[b].pos;
                    row.cells.truncate(b);
                    col = row.cells.last().map_or(0, |c| c.col + usize::from(c.width));
                    // The next row starts after the blanks at the break.
                    while let Some((' ', n)) = char_at(src, codec, p) {
                        p += n as u64;
                    }
                }
                break;
            }
        }
        row.cells.push(Cell {
            ch,
            pos: p,
            len: n as u32,
            col,
            width: w as u8,
            combined: false,
        });
        col += w;
        p += n as u64;
        if words && WORD_DIV.contains(ch) {
            last_break = Some(row.cells.len());
        }
    }
    row.end = p;
    row.cols = col;
    row
}

/// How far back a line start is looked for (Far's `max_backward_size`).
fn max_back(opts: &TextOpts) -> u64 {
    ((opts.max_line * 2).max(1024) * 32).min(300_000) as u64
}

/// Start of the line holding the byte before `pos`.
fn line_start(src: &mut Source, codec: &Codec, pos: u64, opts: &TextOpts) -> u64 {
    let u = codec.unit() as u64;
    let limit = pos.saturating_sub(max_back(opts));
    let mut q = pos;
    while q > limit {
        match unit_before(src, codec, q) {
            Some('\n' | '\r') => return q,
            None if q < u => return 0,
            _ => q -= u,
        }
    }
    // No line break close enough: start at a unit boundary.
    q - q % u
}

/// Start of the row before the one at `pos`.
pub fn prev_row(src: &mut Source, codec: &Codec, pos: u64, opts: &TextOpts) -> Option<u64> {
    if pos == 0 {
        return None;
    }
    let u = codec.unit() as u64;
    // Step over the line break ending the previous line.
    let mut q = pos;
    match unit_before(src, codec, q) {
        Some('\n') => {
            q -= u;
            if unit_before(src, codec, q) == Some('\r') {
                q -= u;
                if unit_before(src, codec, q) == Some('\r') {
                    q -= u;
                }
            }
        }
        Some('\r') => q -= u,
        _ => {}
    }
    let mut p = line_start(src, codec, q, opts);
    loop {
        let row = read_row(src, codec, p, opts);
        if row.end >= pos || row.end == p {
            return Some(p);
        }
        p = row.end;
    }
}

/// Start of the row holding `pos` (Far's `AdjustFilePos`).
pub fn row_start(src: &mut Source, codec: &Codec, pos: u64, opts: &TextOpts) -> u64 {
    if pos == 0 {
        return 0;
    }
    let mut p = line_start(src, codec, pos, opts);
    loop {
        let row = read_row(src, codec, p, opts);
        if row.end > pos || row.end == p {
            return p;
        }
        p = row.end;
    }
}

/// Top of the last page: `height` rows back from the end.
pub fn end_top(src: &mut Source, codec: &Codec, height: usize, opts: &TextOpts) -> u64 {
    let mut top = src.size();
    for _ in 0..height.max(1) {
        match prev_row(src, codec, top, opts) {
            Some(p) => top = p,
            None => break,
        }
    }
    top
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viewer::codepage::{UTF8, UTF16LE};

    fn source(bytes: &[u8]) -> (Source, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "afar-layout-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.txt");
        std::fs::write(&path, bytes).unwrap();
        (Source::open(&path).unwrap(), path)
    }

    fn opts(wrap: Wrap) -> TextOpts {
        TextOpts {
            tab: 8,
            max_line: 10000,
            wrap,
        }
    }

    fn text(row: &Row) -> String {
        row.cells.iter().map(|c| c.ch).collect()
    }

    fn rows(src: &mut Source, codec: &Codec, o: &TextOpts) -> Vec<(u64, String)> {
        let mut out = Vec::new();
        let mut p = 0;
        while p < src.size() {
            let r = read_row(src, codec, p, o);
            out.push((p, text(&r)));
            p = r.end;
        }
        out
    }

    #[test]
    fn line_breaks_like_far() {
        let (mut src, _) = source(b"a\nb\r\nc\rd\r\r\ne");
        let codec = Codec::new(UTF8).unwrap();
        let o = opts(Wrap::None);
        let all = rows(&mut src, &codec, &o);
        let texts: Vec<&str> = all.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(texts, ["a", "b", "c", "d", "e"]);
        // Walking back gives the same rows.
        let mut p = src.size();
        let mut back = Vec::new();
        while let Some(q) = prev_row(&mut src, &codec, p, &o) {
            back.push(q);
            p = q;
        }
        back.reverse();
        let starts: Vec<u64> = all.iter().map(|(s, _)| *s).collect();
        assert_eq!(back, starts);
    }

    #[test]
    fn wraps_and_expands_tabs() {
        let (mut src, _) = source(b"abcdefghij\tx\nshort");
        let codec = Codec::new(UTF8).unwrap();
        let o = opts(Wrap::Chars(4));
        let texts: Vec<String> = rows(&mut src, &codec, &o)
            .into_iter()
            .map(|r| r.1)
            .collect();
        assert_eq!(texts, ["abcd", "efgh", "ij\t", "x", "shor", "t"]);
        let r = read_row(&mut src, &codec, 0, &opts(Wrap::None));
        assert_eq!(r.cells[10].col, 10);
        assert_eq!(r.cells[10].width, 6);
        assert_eq!(r.cols, 17);
        // Back from the end over wrapped rows.
        let o = opts(Wrap::Chars(4));
        assert_eq!(end_top(&mut src, &codec, 2, &o), 13);
        assert_eq!(row_start(&mut src, &codec, 6, &o), 4);
    }

    #[test]
    fn wraps_by_words() {
        let (mut src, _) = source(b"one two three four");
        let codec = Codec::new(UTF8).unwrap();
        let o = opts(Wrap::Words(9));
        let texts: Vec<String> = rows(&mut src, &codec, &o)
            .into_iter()
            .map(|r| r.1)
            .collect();
        assert_eq!(texts, ["one two ", "three ", "four"]);
    }

    #[test]
    fn utf16_rows() {
        let bytes: Vec<u8> = "\u{FEFF}ab\r\nвд"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        let (mut src, _) = source(&bytes);
        let codec = Codec::new(UTF16LE).unwrap();
        let o = opts(Wrap::None);
        let texts: Vec<String> = rows(&mut src, &codec, &o)
            .into_iter()
            .map(|r| r.1)
            .collect();
        assert_eq!(texts, ["ab", "вд"]);
        assert_eq!(prev_row(&mut src, &codec, 10, &o), Some(0));
        assert_eq!(end_top(&mut src, &codec, 1, &o), 10);
    }

    #[test]
    fn wide_and_combining() {
        let (mut src, _) = source("日本e\u{301}x".as_bytes());
        let codec = Codec::new(UTF8).unwrap();
        let r = read_row(&mut src, &codec, 0, &opts(Wrap::None));
        let cols: Vec<(char, usize, u8, bool)> = r
            .cells
            .iter()
            .map(|c| (c.ch, c.col, c.width, c.combined))
            .collect();
        assert_eq!(
            cols,
            [
                ('日', 0, 2, false),
                ('本', 2, 2, false),
                ('e', 4, 1, true),
                ('x', 5, 1, false)
            ]
        );
    }
}
