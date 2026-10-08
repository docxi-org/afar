//! Indents (Far, docs/17 §3): auto indent on Enter, the indent of the line
//! above for typing in an empty line past its end, and "all tabs as
//! spaces".

use super::{Editor, Eol, Pos};

fn blank(c: char) -> bool {
    c == ' ' || c == '\t'
}

impl Editor {
    /// The nearest line at or above `line` with a non-blank character and
    /// that character's screen column (the source of the auto indent).
    fn indent_source(&self, line: usize) -> Option<(usize, usize)> {
        (0..=line).rev().find_map(|l| {
            let col = self.lines[l].text.chars().position(|c| !blank(c))?;
            Some((l, self.vcol(l, col)))
        })
    }

    /// The blanks at the start of line `src` up to screen column `width`
    /// (spaces where they run out or a tab would go past it; tabs as
    /// spaces when tabs are expanded).
    fn indent_from(&self, src: usize, width: usize) -> String {
        let tab = self.settings.tab_size.max(1);
        let from: Vec<char> = self.lines[src]
            .text
            .chars()
            .take_while(|c| blank(*c))
            .collect();
        let mut out = String::new();
        let mut v = 0;
        let mut i = 0;
        while v < width {
            let c = from.get(i).copied().unwrap_or(' ');
            let w = if c == '\t' { tab - v % tab } else { 1 };
            if c == '\t' && (v + w > width || self.settings.expand_tabs > 0) {
                let n = w.min(width - v);
                out.extend(std::iter::repeat_n(' ', n));
                v += n;
            } else {
                out.push(c);
                v += w;
            }
            i += 1;
        }
        out
    }

    /// Enter (Far's `InsertString`): the line split at the cursor; with
    /// auto indent the new line starts at the indent of the nearest
    /// non-blank line above, its blanks copied, and a blank tail left
    /// behind is cut.
    pub(super) fn split_line(&mut self) {
        let p = self.cursor;
        let len = self.line_len(p.line);
        let at = Pos::new(p.line, p.col.min(len));
        let eol = self.lines[p.line].eol;
        let source = if self.settings.auto_indent {
            self.indent_source(p.line)
        } else {
            None
        };
        let chars: Vec<char> = self.lines[p.line].text.chars().collect();
        let space_only = chars[..at.col].iter().all(|c| blank(*c));
        let rest_blank = chars[at.col..].iter().all(|c| blank(*c));
        self.cursor = self.replace(at, at, "\n", eol, false);
        let Some((src, indent)) = source.filter(|(_, v)| *v > 0) else {
            return;
        };
        if rest_blank {
            let t = &self.lines[p.line].text;
            let keep = t.trim_end_matches(blank).chars().count();
            let now = t.chars().count();
            if keep < now {
                self.replace(
                    Pos::new(p.line, keep),
                    Pos::new(p.line, now),
                    "",
                    Eol::None,
                    false,
                );
            }
        }
        let n = p.line + 1;
        let new_len = self.line_len(n);
        let mut need = indent;
        if space_only {
            // The new line's own blanks count towards the indent.
            let lead = self.lines[n].text.chars().take_while(|c| blank(*c)).count();
            need = need.saturating_sub(self.vcol(n, lead));
        }
        if need > 0 {
            if new_len > 0 || !self.settings.cursor_beyond_eol {
                let ins = self.indent_from(src, need);
                self.replace(Pos::new(n, 0), Pos::new(n, 0), &ins, Eol::None, false);
                self.cursor = Pos::new(n, ins.chars().count());
            } else {
                // An empty line: the cursor goes to the indent, nothing is
                // typed yet.
                self.cursor = Pos::new(n, need);
            }
        }
        if space_only {
            let t: Vec<char> = self.lines[n].text.chars().collect();
            let first = t.iter().position(|c| !blank(*c)).unwrap_or(t.len());
            let target = first.min(self.real_col(n, indent));
            if target > self.cursor.col {
                self.cursor.col = target;
            }
        }
    }

    /// Typing in an empty line past its end (Far): the blanks of the
    /// nearest non-empty line above come first instead of spaces only.
    pub(super) fn empty_line_indent(&self, p: Pos) -> Option<String> {
        if p.col == 0 || self.line_len(p.line) > 0 {
            return None;
        }
        let src = (0..p.line)
            .rev()
            .find(|&l| !self.lines[l].text.is_empty())?;
        Some(self.indent_from(src, p.col))
    }

    /// Far's "all tabs as spaces": the text's tabs become the spaces they
    /// show as; `record`: as an undo step (a change of the setting), else
    /// silently (on opening, as Far converts while reading).
    pub fn expand_all_tabs(&mut self, record: bool) {
        let tab = self.settings.tab_size.max(1);
        let changed: Vec<(usize, String)> = self
            .lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.text.contains('\t'))
            .map(|(n, l)| {
                let chars: Vec<char> = l.text.chars().collect();
                (
                    n,
                    super::block::expand_tabs(&chars, tab).into_iter().collect(),
                )
            })
            .collect();
        if changed.is_empty() {
            return;
        }
        if record {
            let cursor_v = self.vcol(self.cursor.line, self.cursor.col);
            self.step(|ed| {
                for (n, text) in changed {
                    let len = ed.line_len(n);
                    ed.replace(Pos::new(n, 0), Pos::new(n, len), &text, Eol::None, false);
                }
            });
            self.cursor.col = self.real_col(self.cursor.line, cursor_v);
        } else {
            for (n, text) in changed {
                self.lines[n].text = text;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::editor::{Editor, Eol, Line, Pos};
    use std::path::Path;

    fn ed(lines: &[&str]) -> Editor {
        let lines = lines
            .iter()
            .map(|t| Line::new(t.to_string(), Eol::Lf))
            .collect();
        let mut e = Editor::new(1, Path::new("t.txt"), lines, 65001, false, Eol::Lf);
        e.settings.auto_indent = true;
        e
    }

    #[test]
    fn enter_indents_like_the_line_above() {
        let mut e = ed(&["\t  foo bar"]);
        e.cursor = Pos::new(0, 6);
        e.command(crate::command::EditorCmd::Enter);
        // Far keeps the rest's own leading blank after the indent.
        assert_eq!(e.plain_lines(), vec!["\t  foo", "\t   bar"]);
        assert_eq!(e.cursor, Pos::new(1, 3));
        // At the end: the new line stays empty, the cursor at the indent;
        // the blank tail of the split line goes.
        let mut e = ed(&["    x   "]);
        e.cursor = Pos::new(0, 8);
        e.command(crate::command::EditorCmd::Enter);
        assert_eq!(e.plain_lines(), vec!["    x", ""]);
        assert_eq!(e.cursor, Pos::new(1, 4));
        e.type_char('y');
        assert_eq!(e.plain_lines(), vec!["    x", "    y"]);
    }

    #[test]
    fn typing_in_an_empty_line_takes_the_indent_above() {
        let mut e = ed(&["\tfoo", "", ""]);
        e.settings.auto_indent = false;
        e.cursor = Pos::new(2, 10);
        e.type_char('z');
        assert_eq!(e.plain_lines()[2], "\t  z");
    }

    #[test]
    fn all_tabs_to_spaces() {
        let mut e = ed(&["a\tb", "\t\tc"]);
        e.settings.tab_size = 4;
        e.cursor = Pos::new(1, 2);
        e.expand_all_tabs(true);
        assert_eq!(e.plain_lines(), vec!["a   b", "        c"]);
        assert_eq!(e.cursor, Pos::new(1, 8));
        e.undo();
        assert_eq!(e.plain_lines(), vec!["a\tb", "\t\tc"]);
    }
}
