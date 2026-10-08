//! F8 / Shift+F8 in an open file (Far's `TryCodePage`, `SetCodePage`):
//! the same bytes read in another code page — not a conversion of the
//! text (that is "Save as" with another page).

use super::{Editor, text};
use crate::viewer::codepage::{self, Codec};

/// Why the text cannot be read in another code page: a character the
/// current page cannot hold, or bytes the new page cannot read.
#[derive(Debug, PartialEq, Eq)]
pub enum CpProblem {
    Char {
        line: usize,
        col: usize,
        ch: char,
    },
    Bytes {
        line: usize,
        col: usize,
        bytes: Vec<u8>,
    },
}

impl Editor {
    /// The first line that does not survive the change to `new_cp`.
    pub fn try_codepage(&self, new_cp: u32) -> Option<CpProblem> {
        let (old, new) = (Codec::new(self.cp)?, Codec::new(new_cp)?);
        for (n, l) in self.lines.iter().enumerate() {
            if l.text.is_empty() {
                continue;
            }
            let bytes = old.encode(&l.text);
            let back = text::decode(&old, &bytes);
            if back != l.text {
                let (col, ch) = l
                    .text
                    .chars()
                    .zip(back.chars().map(Some).chain(std::iter::repeat(None)))
                    .enumerate()
                    .find(|(_, (a, b))| Some(*a) != *b)
                    .map_or((0, '\u{FFFD}'), |(i, (a, _))| (i, a));
                return Some(CpProblem::Char { line: n, col, ch });
            }
            let mut i = 0;
            while i < bytes.len() {
                let (c, k) = new.decode(&bytes[i..]);
                let k = k.max(1);
                if c == '\u{FFFD}' {
                    let col = text::decode(&old, &bytes[..i]).chars().count();
                    let bytes = bytes[i..(i + k).min(bytes.len())].to_vec();
                    return Some(CpProblem::Bytes {
                        line: n,
                        col,
                        bytes,
                    });
                }
                i += k;
            }
        }
        None
    }

    /// The text read again in `new_cp` from the bytes it has in the
    /// current page — the undo history too, so undo goes on in the new
    /// page; the file's bytes stay, so does "modified".
    pub fn reinterpret(&mut self, new_cp: u32) {
        let (Some(old), Some(new)) = (Codec::new(self.cp), Codec::new(new_cp)) else {
            return;
        };
        let f = |s: &str| -> String {
            if s.is_empty() {
                String::new()
            } else {
                // Far: what the page cannot hold becomes `?`.
                text::decode(&new, &old.encode_lossy(s))
            }
        };
        for l in &mut self.lines {
            l.text = f(&l.text);
        }
        self.history.map_text(&f);
        self.syntax_reset();
        self.cp = new_cp;
        // Far: a byte order mark read as text in UTF-8 becomes the mark.
        self.bom = false;
        if new_cp == codepage::UTF8
            && let Some(rest) = self.lines[0].text.strip_prefix('\u{FEFF}')
        {
            self.lines[0].text = rest.to_string();
            self.bom = true;
        }
        self.unselect();
        self.version += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{Eol, Line};
    use std::path::Path;

    #[test]
    fn reads_the_same_bytes_in_another_page_and_undo_follows() {
        if Codec::new(1251).is_none() || Codec::new(866).is_none() {
            return;
        }
        let lines = vec![Line::new("Привет", Eol::CrLf)];
        let mut e = Editor::new(1, Path::new("t.txt"), lines, 1251, false, Eol::CrLf);
        e.cursor = crate::editor::Pos::new(0, 6);
        e.type_char('!');
        assert_eq!(e.try_codepage(866), None);
        e.reinterpret(866);
        let as_866 = e.plain_lines()[0].clone();
        assert_ne!(as_866, "Привет!");
        assert!(e.modified(), "the typed '!' is still unsaved");
        e.undo();
        assert_eq!(e.plain_lines()[0].chars().count(), 6);
        e.reinterpret(1251);
        assert_eq!(e.plain_lines(), vec!["Привет"]);
    }

    #[test]
    fn a_character_the_page_cannot_hold_is_reported() {
        if Codec::new(1251).is_none() {
            return;
        }
        let lines = vec![Line::new("a\u{4E2D}b", Eol::CrLf)];
        let e = Editor::new(1, Path::new("t.txt"), lines, 1251, false, Eol::CrLf);
        assert_eq!(
            e.try_codepage(866),
            Some(CpProblem::Char {
                line: 0,
                col: 1,
                ch: '\u{4E2D}'
            })
        );
    }
}
