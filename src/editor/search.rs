//! Search and replace in the editor (Far's `DoSearchReplace`, docs/17 §5):
//! line by line from the cursor, never across a line break, and not round
//! the file's edges.

use regex::Regex;

use super::{Editor, Eol, Pos};
use crate::viewer::search::Query;

/// A compiled query of the editor.
pub struct Finder {
    re: Regex,
    words: bool,
    /// The replacement is a template (`$1`, `\n`, `\t`).
    regex: bool,
}

impl Finder {
    pub fn new(q: &Query) -> Result<Self, String> {
        Ok(Self {
            re: q.regex()?,
            words: q.words,
            regex: q.regex,
        })
    }
}

/// One match: where it starts and ends (character positions).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Found {
    pub start: Pos,
    pub end: Pos,
}

/// The byte index of character `col` in `s` (its length past the end).
fn byte_of(s: &str, col: usize) -> usize {
    s.char_indices().nth(col).map_or(s.len(), |(i, _)| i)
}

fn col_of(s: &str, byte: usize) -> usize {
    s[..byte].chars().count()
}

/// A regular-expression replacement's escapes: `\n`, `\r` (a line
/// break), `\t`, `\\`; `$` stays for the groups.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') | Some('r') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

impl Editor {
    /// The matches in line `line` starting in `from..to` (characters), in
    /// order; a match may start inside the one before it.
    fn line_matches(&self, f: &Finder, line: usize, from: usize, to: usize) -> Vec<(usize, usize)> {
        let text = &self.lines[line].text;
        let mut out = Vec::new();
        let mut at = byte_of(text, from);
        let limit = if to == usize::MAX {
            usize::MAX
        } else {
            byte_of(text, to)
        };
        while at <= text.len() {
            let Some(m) = f.re.find_at(text, at) else {
                break;
            };
            if m.start() >= limit {
                break;
            }
            // Far: a regular expression may match nothing (`^`, `$`).
            let ok = (f.regex || !m.as_str().is_empty())
                && (!f.words
                    || (!text[..m.start()]
                        .chars()
                        .next_back()
                        .is_some_and(|c| self.is_word(c))
                        && !text[m.end()..]
                            .chars()
                            .next()
                            .is_some_and(|c| self.is_word(c))));
            if ok {
                out.push((col_of(text, m.start()), col_of(text, m.end())));
            }
            // On from the next character after the match's start.
            match text[m.start()..].chars().next() {
                Some(c) => at = m.start() + c.len_utf8(),
                None => break,
            }
        }
        out
    }

    /// The first match at or after `from` (forward), or the last one
    /// starting before `from` (backward); not round the file's edges.
    pub fn find(&self, f: &Finder, from: Pos, backward: bool) -> Option<Found> {
        let n = self.lines.len();
        let line = from.line.min(n - 1);
        let found = |line: usize, (s, e): (usize, usize)| Found {
            start: Pos::new(line, s),
            end: Pos::new(line, e),
        };
        if backward {
            let col = from.col.min(self.line_len(line) + 1);
            if let Some(m) = self.line_matches(f, line, 0, col).last() {
                return Some(found(line, *m));
            }
            (0..line).rev().find_map(|l| {
                self.line_matches(f, l, 0, usize::MAX)
                    .last()
                    .map(|m| found(l, *m))
            })
        } else {
            let col = from.col.min(self.line_len(line) + 1);
            if col <= self.line_len(line)
                && let Some(m) = self.line_matches(f, line, col, usize::MAX).first()
            {
                return Some(found(line, *m));
            }
            (line + 1..n).find_map(|l| {
                self.line_matches(f, l, 0, usize::MAX)
                    .first()
                    .map(|m| found(l, *m))
            })
        }
    }

    /// Every match in the text (Far's "Find all").
    pub fn find_all(&self, f: &Finder) -> Vec<Found> {
        let mut out = Vec::new();
        for l in 0..self.lines.len() {
            // Far: the next one from a character after the last start.
            out.extend(
                self.line_matches(f, l, 0, usize::MAX)
                    .into_iter()
                    .map(|(s, e)| Found {
                        start: Pos::new(l, s),
                        end: Pos::new(l, e),
                    }),
            );
        }
        out
    }

    /// The text found.
    pub fn found_text(&self, m: Found) -> String {
        let text = &self.lines[m.start.line].text;
        text[byte_of(text, m.start.col)..byte_of(text, m.end.col)].to_string()
    }

    /// What the match is replaced with: the groups and escapes of a
    /// regular expression filled in, a plain text as it is.
    pub fn replacement(&self, f: &Finder, m: Found, with: &str) -> String {
        if !f.regex {
            return with.to_string();
        }
        let text = &self.lines[m.start.line].text;
        let at = byte_of(text, m.start.col);
        let template = unescape(with);
        match f.re.captures_at(text, at) {
            Some(caps) if caps.get(0).is_some_and(|g| g.start() == at) => {
                let mut out = String::new();
                caps.expand(&template, &mut out);
                out
            }
            _ => template,
        }
    }

    /// Replaces one match as an undo step of its own; returns the end
    /// of the new text. Line breaks in it get the line's ending.
    pub fn replace_found(&mut self, m: Found, new: &str) -> Pos {
        if !self.editable() {
            return m.start;
        }
        let mut end = m.start;
        self.step(|ed| {
            let eol = match ed.lines[m.start.line].eol {
                Eol::None => ed.default_eol,
                e => e,
            };
            end = ed.replace(m.start, m.end, new, eol, false);
            ed.cursor = end;
        });
        self.anchor = None;
        end
    }

    /// "All" in the replace question: this match and the rest of the pass
    /// replaced as one undo step. Returns how many, and where the cursor
    /// ends.
    pub fn replace_rest(&mut self, f: &Finder, first: Found, with: &str, backward: bool) -> usize {
        if !self.editable() {
            return 0;
        }
        let mut count = 0;
        self.step(|ed| {
            let mut m = Some(first);
            while let Some(found) = m {
                let new = ed.replacement(f, found, with);
                let eol = match ed.lines[found.start.line].eol {
                    Eol::None => ed.default_eol,
                    e => e,
                };
                let end = ed.replace(found.start, found.end, &new, eol, false);
                count += 1;
                // Forward goes on after the new text, and a character
                // further after an empty match (Far), so this ends.
                let next = match (backward, found.start == found.end) {
                    (true, _) => found.start,
                    (false, false) => end,
                    (false, true) => Pos::new(end.line, end.col + 1),
                };
                ed.cursor = next;
                m = ed.find(f, next, backward);
            }
        });
        self.anchor = None;
        count
    }

    /// Shows a match as Far does: the cursor at its start (or end), the
    /// line a quarter of the screen from the top, 8 columns after it
    /// visible; selected with `select`.
    pub fn show_found(&mut self, m: Found, cursor_at_end: bool, select: bool, screen_rows: usize) {
        if !self.settings.persistent_blocks || select {
            self.anchor = None;
        }
        self.cursor = if cursor_at_end { m.end } else { m.start };
        if select {
            self.anchor = Some(if cursor_at_end { m.start } else { m.end });
        }
        self.want_vcol = None;
        self.history.break_merge();
        // Far: (ScrY-2)/4 lines above it, unless that reaches the middle.
        let scr_y = screen_rows.saturating_sub(1) as isize;
        let mut from_top = (scr_y - 2) / 4;
        if from_top < 0 || from_top >= (scr_y - 5) / 2 - 2 {
            from_top = 0;
        }
        self.top = m.start.line.saturating_sub(from_top as usize);
        let w = usize::from(self.area.width.saturating_sub(self.number_width()).max(1));
        let v_end = self.vcol(m.end.line, m.end.col);
        if v_end + 8 > self.left + w {
            self.left = (v_end + 8).saturating_sub(w);
        }
    }

    /// Alt+F8: to line `line`, character `col` (from 0); a line off the
    /// screen becomes the top one.
    pub fn go_to(&mut self, line: usize, col: Option<usize>) {
        let line = line.min(self.lines.len() - 1);
        let h = usize::from(self.area.height.max(1));
        if line < self.top || line >= self.top + h {
            self.top = line;
        }
        let col = col.unwrap_or(self.cursor.col);
        self.move_to(Pos::new(line, col));
        self.want_vcol = None;
    }

    /// The word under (or just before) the cursor, for the search dialog.
    pub fn word_at_cursor(&self) -> Option<String> {
        let chars: Vec<char> = self.lines[self.cursor.line].text.chars().collect();
        let mut i = self.cursor.col.min(chars.len());
        if i == chars.len() || !self.is_word(chars[i]) {
            if i > 0 && self.is_word(chars[i - 1]) {
                i -= 1;
            } else {
                return None;
            }
        }
        let mut s = i;
        while s > 0 && self.is_word(chars[s - 1]) {
            s -= 1;
        }
        let mut e = i;
        while e < chars.len() && self.is_word(chars[e]) {
            e += 1;
        }
        Some(chars[s..e].iter().collect())
    }

    /// The selection's first line (or the cursor's line without one), for
    /// the search dialog.
    pub fn selection_first_line(&self) -> String {
        match self.selection() {
            Some((s, e)) => {
                let (s, e) = (self.clamp(s), self.clamp(e));
                let text = &self.lines[s.line].text;
                let end = if e.line == s.line { e.col } else { usize::MAX };
                let from = byte_of(text, s.col);
                let to = if end == usize::MAX {
                    text.len()
                } else {
                    byte_of(text, end)
                };
                text[from..to].to_string()
            }
            None => self.lines[self.cursor.line].text.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{Eol, Line};
    use std::path::Path;

    fn ed(lines: &[&str]) -> Editor {
        let lines = lines
            .iter()
            .map(|t| Line::new(t.to_string(), Eol::CrLf))
            .collect();
        Editor::new(1, Path::new("t.txt"), lines, 65001, false, Eol::CrLf)
    }

    fn finder(text: &str, regex: bool, case: bool, words: bool) -> Finder {
        Finder::new(&Query {
            text: text.into(),
            regex,
            case,
            words,
            ..Default::default()
        })
        .unwrap()
    }

    #[test]
    fn finds_forward_and_backward_from_the_cursor() {
        let e = ed(&["abc abc", "x", "ABC"]);
        let f = finder("abc", false, false, false);
        let m = e.find(&f, Pos::new(0, 0), false).unwrap();
        assert_eq!((m.start, m.end), (Pos::new(0, 0), Pos::new(0, 3)));
        let m = e.find(&f, Pos::new(0, 1), false).unwrap();
        assert_eq!(m.start, Pos::new(0, 4));
        let m = e.find(&f, Pos::new(0, 5), false).unwrap();
        assert_eq!(m.start, Pos::new(2, 0));
        assert!(
            e.find(&f, Pos::new(2, 1), false).is_none(),
            "not round the end"
        );
        let m = e.find(&f, Pos::new(2, 0), true).unwrap();
        assert_eq!(m.start, Pos::new(0, 4));
        let m = e.find(&f, Pos::new(0, 4), true).unwrap();
        assert_eq!(m.start, Pos::new(0, 0));
        assert!(e.find(&f, Pos::new(0, 0), true).is_none());
        let case = finder("abc", false, true, false);
        assert_eq!(e.find(&case, Pos::new(1, 0), false), None);
    }

    #[test]
    fn whole_words_use_the_word_dividers() {
        let e = ed(&["foobar foo.bar foo"]);
        let f = finder("foo", false, false, true);
        let all: Vec<usize> = e.find_all(&f).iter().map(|m| m.start.col).collect();
        assert_eq!(all, vec![7, 15]);
    }

    #[test]
    fn replaces_with_groups_and_line_breaks() {
        let mut e = ed(&["key=value", "a=b"]);
        let f = finder(r"(\w+)=(\w+)", true, false, false);
        let m = e.find(&f, Pos::new(0, 0), false).unwrap();
        let new = e.replacement(&f, m, r"$2:$1\n");
        assert_eq!(new, "value:key\n");
        e.replace_found(m, &new);
        assert_eq!(e.plain_lines(), vec!["value:key", "", "a=b"]);
        e.undo();
        assert_eq!(e.plain_lines(), vec!["key=value", "a=b"]);
    }

    #[test]
    fn replace_all_is_one_undo_step() {
        let mut e = ed(&["aXa", "a"]);
        let f = finder("a", false, false, false);
        let first = e.find(&f, Pos::new(0, 0), false).unwrap();
        let n = e.replace_rest(&f, first, "aa", false);
        assert_eq!(n, 3);
        assert_eq!(e.plain_lines(), vec!["aaXaa", "aa"]);
        e.undo();
        assert_eq!(e.plain_lines(), vec!["aXa", "a"]);
    }

    #[test]
    fn empty_matches_of_a_regex_count() {
        let mut e = ed(&["ab", "", "c"]);
        let f = finder("^", true, false, false);
        assert_eq!(e.find_all(&f).len(), 3);
        let first = e.find(&f, Pos::new(0, 0), false).unwrap();
        assert_eq!(e.replace_rest(&f, first, "// ", false), 3);
        assert_eq!(e.plain_lines(), vec!["// ab", "// ", "// c"]);
        let mut e = ed(&["ab", "c"]);
        let f = finder("$", true, false, false);
        let first = e.find(&f, Pos::new(0, 0), false).unwrap();
        assert_eq!(e.replace_rest(&f, first, ";", false), 2);
        assert_eq!(e.plain_lines(), vec!["ab;", "c;"]);
    }

    #[test]
    fn picks_the_word_at_the_cursor() {
        let mut e = ed(&["foo.bar baz"]);
        e.cursor = Pos::new(0, 5);
        assert_eq!(e.word_at_cursor().as_deref(), Some("bar"));
        e.cursor = Pos::new(0, 7);
        assert_eq!(e.word_at_cursor().as_deref(), Some("bar"));
        e.cursor = Pos::new(0, 11);
        assert_eq!(e.word_at_cursor().as_deref(), Some("baz"));
    }
}
