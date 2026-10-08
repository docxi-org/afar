//! Blocks beyond the stream selection (Far, docs/17 §4): the vertical block
//! (Alt+arrows) — lines and screen columns — with its copy, delete, paste
//! as a column and shift; Alt+U / Alt+I for any block; Ctrl+P / Ctrl+M.

use super::{Editor, Eol, Frozen, Pos};

/// A vertical block: lines `top..=bottom`, screen columns `left..right`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VBlock {
    pub top: usize,
    pub bottom: usize,
    pub left: usize,
    pub right: usize,
}

impl VBlock {
    pub fn width(&self) -> usize {
        self.right - self.left
    }
}

/// What Ctrl+C takes: the text, and whether it is a column.
pub struct BlockText {
    pub text: String,
    pub vertical: bool,
}

/// The character at screen column `v` of `chars` (the tab or wide
/// character covering it); past the end, a position per column (Far's
/// `VisualPosToReal`).
fn real_index(chars: &[char], v: usize, tab: usize) -> usize {
    let mut x = 0;
    for (n, c) in chars.iter().enumerate() {
        let w = char_width(*c, x, tab);
        if v < x + w.max(1) {
            return n;
        }
        x += w;
    }
    chars.len() + (v - x)
}

fn char_width(c: char, x: usize, tab: usize) -> usize {
    use unicode_width::UnicodeWidthChar as _;
    if c == '\t' {
        tab - x % tab
    } else {
        c.width().unwrap_or(0)
    }
}

/// Tabs replaced by the spaces they show as (Far's `ReplaceTabs`).
fn expand_tabs(chars: &[char], tab: usize) -> Vec<char> {
    let mut out = Vec::with_capacity(chars.len());
    let mut x = 0;
    for &c in chars {
        let w = char_width(c, x, tab);
        if c == '\t' {
            out.extend(std::iter::repeat_n(' ', w));
        } else {
            out.push(c);
        }
        x += w;
    }
    out
}

impl Editor {
    /// The vertical block, if the selection is one (it may be 0 wide).
    pub fn vblock(&self) -> Option<VBlock> {
        let Some(av) = self.vblock_vcol else {
            return match self.frozen {
                Some(Frozen::Vertical(b)) => Some(b),
                _ => None,
            };
        };
        let a = self.anchor?;
        let cv = self.vcol(self.cursor.line, self.cursor.col);
        Some(VBlock {
            top: a.line.min(self.cursor.line),
            bottom: a.line.max(self.cursor.line),
            left: av.min(cv),
            right: av.max(cv),
        })
    }

    /// A block with something in it: a stream selection, or a vertical
    /// block at least a column wide.
    pub fn has_block(&self) -> bool {
        self.selection().is_some() || self.vblock().is_some_and(|b| b.width() > 0)
    }

    /// The character at screen column `v` of `line` (Far's
    /// `VisualPosToReal`, past the end whatever the settings).
    pub(super) fn real_col(&self, line: usize, v: usize) -> usize {
        let Some(l) = self.lines.get(line) else {
            return v;
        };
        let chars: Vec<char> = l.text.chars().collect();
        real_index(&chars, v, self.settings.tab_size.max(1))
    }

    /// Starts the vertical block at the cursor (or goes on with it) and
    /// moves its corner to `to` (Alt+arrows).
    pub(super) fn vmark(&mut self, to: Pos) {
        if self.anchor.is_none() || self.vblock_vcol.is_none() {
            self.frozen = None;
            self.anchor = Some(self.cursor);
            self.vblock_vcol = Some(self.vcol(self.cursor.line, self.cursor.col));
        }
        self.cursor = to;
        self.history.break_merge();
    }

    /// Ctrl+Alt+Right: over blanks and dividers, then a word (stays in the
    /// line).
    pub(super) fn vword_right(&self, p: Pos) -> Pos {
        let chars: Vec<char> = self.lines[p.line].text.chars().collect();
        let mut i = p.col;
        while i < chars.len() && !self.is_word(chars[i]) {
            i += 1;
        }
        while i < chars.len() && self.is_word(chars[i]) {
            i += 1;
        }
        Pos::new(p.line, i.max(p.col))
    }

    /// The block's text: a column is padded with spaces to its width and
    /// every line ends with a break (Far's `VBlock2Text`).
    pub fn block_text(&self) -> Option<BlockText> {
        if let Some(b) = self.vblock() {
            return (b.width() > 0).then(|| BlockText {
                text: self.vblock_text(b),
                vertical: true,
            });
        }
        self.selected_text().map(|text| BlockText {
            text,
            vertical: false,
        })
    }

    fn vblock_text(&self, b: VBlock) -> String {
        let eol = match self.default_eol {
            Eol::None => Eol::CrLf,
            e => e,
        };
        let mut out = String::new();
        for line in b.top..=b.bottom.min(self.lines.len() - 1) {
            let (s, e) = (self.real_col(line, b.left), self.real_col(line, b.right));
            let size = e - s;
            let have: String = self.lines[line].text.chars().skip(s).take(size).collect();
            let n = have.chars().count();
            out.push_str(&have);
            out.extend(std::iter::repeat_n(' ', size - n));
            out.push_str(eol.as_str());
        }
        out
    }

    /// Deletes the vertical block's columns (part of an undo step); the
    /// cursor goes to its corner unless blocks persist.
    pub(super) fn delete_vblock_inner(&mut self, b: VBlock) {
        for line in b.top..=b.bottom.min(self.lines.len() - 1) {
            let (s, e) = (self.real_col(line, b.left), self.real_col(line, b.right));
            let len = self.line_len(line);
            if s >= len {
                continue;
            }
            let e = e.min(len);
            self.replace(Pos::new(line, s), Pos::new(line, e), "", Eol::None, false);
            if self.cursor.line == line && self.cursor.col > s {
                self.cursor.col = self.cursor.col.saturating_sub(e - s).max(s);
            }
        }
        if !self.settings.persistent_blocks {
            self.cursor = Pos::new(b.top, self.col_at(b.top, b.left));
        }
        self.unselect();
    }

    /// Ctrl+V of a column (Far's `VPaste`): each line of `text` goes in at
    /// the cursor's screen column, one line under the other (new lines at
    /// the end of the text as needed); the column becomes the block.
    pub fn paste_vertical(&mut self, text: &str) {
        if !self.editable() || text.is_empty() {
            return;
        }
        let persistent = self.settings.persistent_blocks;
        self.step(|ed| {
            if !persistent {
                ed.delete_selection_inner();
            }
            ed.unselect();
            let first = ed.cursor.line;
            let start = ed.vcol(first, ed.cursor.col);
            let mut segs = super::split_breaks(text);
            if segs.len() > 1 && segs.last() == Some(&"") {
                segs.pop();
            }
            let mut width = 0;
            for (i, seg) in segs.iter().enumerate() {
                let line = first + i;
                if line >= ed.lines.len() {
                    let last = ed.lines.len() - 1;
                    let end = Pos::new(last, ed.line_len(last));
                    ed.replace(end, end, "\n", Eol::None, false);
                }
                let col = ed.real_col(line, start);
                let end = ed.replace(
                    Pos::new(line, col),
                    Pos::new(line, col),
                    seg,
                    Eol::None,
                    true,
                );
                width = width.max(ed.vcol(line, end.col).saturating_sub(start));
            }
            ed.cursor = Pos::new(first, ed.real_col(first, start));
            // The column becomes the block with persistent blocks (Far
            // unmarks it after Ctrl+V otherwise).
            if persistent {
                ed.anchor = Some(Pos::new(first + segs.len() - 1, 0));
                ed.vblock_vcol = Some(start + width);
            }
        });
        self.want_vcol = None;
    }

    /// Alt+U / Alt+I (Far's `BlockLeft` / `BlockRight`): the block's lines
    /// (or the cursor's line) by a character; a vertical block moves
    /// inside its lines.
    pub fn shift_block(&mut self, right: bool) {
        if !self.editable() {
            return;
        }
        if let Some(b) = self.vblock() {
            self.vshift(b, right);
            return;
        }
        let tab = self.settings.tab_size.max(1);
        // The lines and, per line, whether the block covers some of it.
        let lines: Vec<usize> = match self.selection() {
            Some((s, e)) => {
                let (s, e) = (self.clamp(s), self.clamp(e));
                (s.line..=e.line)
                    .filter(|&l| l < e.line || e.col > if l == s.line { s.col } else { 0 })
                    .collect()
            }
            None => vec![self.cursor.line],
        };
        let block = self.selection().is_some();
        self.step(|ed| {
            for l in lines {
                let text = ed.lines[l].text.clone();
                let Some(first) = text.chars().next() else {
                    continue;
                };
                let new = if right {
                    format!(" {text}")
                } else if first == ' ' {
                    text[1..].to_string()
                } else if first == '\t' {
                    format!("{}{}", " ".repeat(tab - 1), &text[1..])
                } else {
                    continue;
                };
                let len = ed.line_len(l);
                ed.replace(Pos::new(l, 0), Pos::new(l, len), &new, Eol::None, false);
                let shift = |c: usize| {
                    if right {
                        if c > 0 { c + 1 } else { c }
                    } else {
                        c.saturating_sub(1)
                    }
                };
                if block && let Some(a) = ed.anchor.as_mut().filter(|a| a.line == l) {
                    a.col = shift(a.col);
                }
                if ed.cursor.line == l {
                    ed.cursor.col = if right {
                        ed.cursor.col + 1
                    } else {
                        ed.cursor.col.saturating_sub(1)
                    };
                }
            }
        });
    }

    /// Far's `VBlockShift`: the column moves inside its lines, the
    /// character it passes goes to its other side; trailing blanks go.
    fn vshift(&mut self, b: VBlock, right: bool) {
        if (!right && b.left == 0) || b.width() == 0 {
            return;
        }
        let tab = self.settings.tab_size.max(1);
        self.step(|ed| {
            for line in b.top..=b.bottom.min(ed.lines.len() - 1) {
                let mut chars: Vec<char> = ed.lines[line].text.chars().collect();
                let mut s = real_index(&chars, b.left, tab);
                let mut e = real_index(&chars, b.right, tab);
                if s > chars.len() || (!right && s == 0) {
                    continue;
                }
                let tab_near = if right {
                    chars.get(e) == Some(&'\t')
                } else {
                    chars.get(s - 1) == Some(&'\t')
                };
                if tab_near {
                    chars = expand_tabs(&chars, tab);
                    s = real_index(&chars, b.left, tab);
                    e = real_index(&chars, b.right, tab);
                }
                let need = e + usize::from(right);
                if chars.len() < need {
                    chars.resize(need, ' ');
                }
                if right {
                    chars[s..=e].rotate_right(1);
                } else {
                    chars[s - 1..e].rotate_left(1);
                }
                while chars.last().is_some_and(|c| *c == ' ' || *c == '\t') {
                    chars.pop();
                }
                let new: String = chars.into_iter().collect();
                let len = ed.line_len(line);
                ed.replace(
                    Pos::new(line, 0),
                    Pos::new(line, len),
                    &new,
                    Eol::None,
                    false,
                );
            }
            // The block moves with its text; the cursor goes to its left
            // edge (left) or right edge (right), as in Far.
            let (left, right_edge) = if right {
                (b.left + 1, b.right + 1)
            } else {
                (b.left - 1, b.right - 1)
            };
            let line = ed.cursor.line;
            let other = if line == b.top { b.bottom } else { b.top };
            let (at, corner) = if right {
                (right_edge, left)
            } else {
                (left, right_edge)
            };
            ed.cursor = Pos::new(line, ed.real_col(line, at));
            ed.frozen = None;
            ed.anchor = Some(Pos::new(other, 0));
            ed.vblock_vcol = Some(corner);
        });
    }

    /// Ctrl+P / Ctrl+M: the block copied (moved) to the cursor; the
    /// clipboard is not touched (Far uses an internal one).
    pub fn copy_block_here(&mut self, moving: bool) {
        if !self.editable() {
            return;
        }
        let Some(bt) = self.block_text() else {
            return;
        };
        self.step(|ed| {
            if moving {
                let c = ed.cursor;
                if let Some(b) = ed.vblock() {
                    let mut to = c;
                    if (b.top..=b.bottom).contains(&c.line) {
                        let (s, e) = (ed.real_col(c.line, b.left), ed.real_col(c.line, b.right));
                        if c.col >= e {
                            to.col -= e - s;
                        }
                    }
                    ed.delete_vblock_inner(b);
                    ed.cursor = to;
                } else if let Some((s, e)) = ed.selection() {
                    let (s, e) = (ed.clamp(s), ed.clamp(e));
                    let to = if c <= s {
                        c
                    } else if c >= e {
                        if c.line == e.line {
                            Pos::new(s.line, s.col + (c.col - e.col))
                        } else {
                            Pos::new(c.line - (e.line - s.line), c.col)
                        }
                    } else {
                        s
                    };
                    ed.delete_selection_inner();
                    ed.cursor = to;
                }
            }
            if bt.vertical {
                ed.paste_vertical(&bt.text);
            } else {
                ed.insert_text(&bt.text);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use crate::command::EditorCmd;
    use crate::editor::{Editor, Eol, Line, Pos};
    use std::path::Path;

    fn ed(lines: &[&str]) -> Editor {
        let lines = lines
            .iter()
            .map(|t| Line::new(t.to_string(), Eol::Lf))
            .collect();
        Editor::new(1, Path::new("t.txt"), lines, 65001, false, Eol::Lf)
    }

    fn run(e: &mut Editor, cmds: &[EditorCmd]) {
        for c in cmds {
            e.command(*c);
        }
    }

    #[test]
    fn alt_arrows_mark_a_column_and_copy_pads_it() {
        use EditorCmd::*;
        let mut e = ed(&["abcdef", "ab", "abcdef"]);
        e.cursor = Pos::new(0, 1);
        run(
            &mut e,
            &[VSelRight, VSelRight, VSelRight, VSelDown, VSelDown],
        );
        let b = e.vblock().unwrap();
        assert_eq!((b.top, b.bottom, b.left, b.right), (0, 2, 1, 4));
        assert!(e.selection().is_none(), "not a stream");
        let t = e.block_text().unwrap();
        assert!(t.vertical);
        assert_eq!(t.text, "bcd\nb  \nbcd\n");
    }

    #[test]
    fn deletes_and_pastes_a_column() {
        use EditorCmd::*;
        let mut e = ed(&["abcdef", "ab", "abcdef"]);
        e.cursor = Pos::new(0, 1);
        run(&mut e, &[VSelRight, VSelRight, VSelDown, VSelDown]);
        let text = e.block_text().unwrap().text;
        e.delete_selection();
        assert_eq!(e.plain_lines(), vec!["adef", "a", "adef"]);
        assert_eq!(e.cursor, Pos::new(0, 1));
        e.cursor = Pos::new(1, 4);
        e.paste_vertical(&text);
        assert_eq!(e.plain_lines(), vec!["adef", "a   bc", "adefb ", "    bc"]);
        assert!(
            e.vblock().is_none(),
            "Far unmarks it without persistent blocks"
        );
        e.undo();
        e.settings.persistent_blocks = true;
        e.paste_vertical(&text);
        let b = e.vblock().unwrap();
        assert_eq!((b.top, b.bottom, b.left, b.right), (1, 3, 4, 6));
        e.settings.persistent_blocks = false;
        e.undo();
        assert_eq!(e.plain_lines(), vec!["adef", "a", "adef"]);
    }

    #[test]
    fn shifts_a_column_inside_its_lines() {
        use EditorCmd::*;
        let mut e = ed(&["abcdef", "abcdef"]);
        e.cursor = Pos::new(0, 2);
        run(&mut e, &[VSelRight, VSelRight, VSelDown]);
        e.shift_block(false);
        assert_eq!(e.plain_lines(), vec!["acdbef", "acdbef"]);
        let b = e.vblock().unwrap();
        assert_eq!((b.left, b.right), (1, 3));
        e.shift_block(true);
        e.shift_block(true);
        assert_eq!(e.plain_lines(), vec!["abecdf", "abecdf"]);
    }

    #[test]
    fn shifts_stream_lines_and_the_line_without_a_block() {
        use EditorCmd::*;
        let mut e = ed(&["a", "\tb", "c"]);
        e.shift_block(true);
        assert_eq!(e.plain_lines(), vec![" a", "\tb", "c"]);
        e.cursor = Pos::new(0, 0);
        run(&mut e, &[SelDown, SelDown]);
        e.shift_block(false);
        // The last line is not in the block (it starts at its column 0).
        assert_eq!(e.plain_lines(), vec!["a", "       b", "c"]);
        e.undo();
        assert_eq!(e.plain_lines(), vec![" a", "\tb", "c"]);
    }

    #[test]
    fn ctrl_m_moves_a_block_to_the_cursor() {
        let mut e = ed(&["one two three"]);
        e.settings.persistent_blocks = true;
        e.cursor = Pos::new(0, 4);
        e.command(crate::command::EditorCmd::SelWordRight);
        assert_eq!(e.selected_text().as_deref(), Some("two "));
        // A move leaves a persistent block where it is.
        e.command(crate::command::EditorCmd::End);
        assert_eq!(e.selected_text().as_deref(), Some("two "));
        e.copy_block_here(true);
        assert_eq!(e.plain_lines(), vec!["one threetwo "]);
        e.command(crate::command::EditorCmd::Home);
        e.copy_block_here(false);
        assert_eq!(e.plain_lines(), vec!["two one threetwo "]);
    }
}
