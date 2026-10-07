//! The built-in editor (F4), as Far's (`editor.cpp`, `edit.cpp`,
//! `fileedit.cpp`; docs/17): a list of lines with their own line endings,
//! a cursor that may stand past the end of a line, stream selection,
//! undo in steps (typing in a line merges, `begin…end` groups), and the
//! screen: a top line and a horizontal offset shared by all lines.
//! The window around it (status line, key bar, dialogs) is in
//! `app/editors.rs`.

pub mod text;
mod undo;

use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use unicode_width::UnicodeWidthChar as _;

use crate::command::EditorCmd;
use crate::theme;
pub use text::{Eol, Line};
use undo::{Change, History};

/// A place in the text: the line and the character index in it (may be
/// past the line's end).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

impl Pos {
    pub fn new(line: usize, col: usize) -> Self {
        Self { line, col }
    }
}

/// The editor's settings (Far's `EditorOptions`, docs/17 §9).
#[derive(Clone, Debug)]
pub struct Settings {
    pub tab_size: usize,
    /// 0: keep tabs; 1: new tabs as spaces; 2: all tabs as spaces.
    pub expand_tabs: u8,
    pub cursor_beyond_eol: bool,
    pub persistent_blocks: bool,
    pub del_removes_blocks: bool,
    pub word_div: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            tab_size: 8,
            expand_tabs: 0,
            cursor_beyond_eol: true,
            persistent_blocks: false,
            del_removes_blocks: true,
            word_div: "~!%^&*()+|{}:\"<>?`-=\\[];',./".to_string(),
        }
    }
}

/// What a command leaves for the window to do.
pub enum Outcome {
    Done,
    /// A window command (save, quit, F6, …) or one needing the clipboard.
    App(EditorCmd),
}

/// What one agent's portion did: the lines it changed or added (from 0)
/// and how many it removed.
#[derive(Debug, Default)]
pub struct AgentEdit {
    pub changed: Vec<usize>,
    pub removed: usize,
}

pub struct Editor {
    pub id: u32,
    path: PathBuf,
    lines: Vec<Line>,
    /// The file's code page and byte order mark.
    pub cp: u32,
    pub bom: bool,
    /// Far's `GlobalEOL`: what a line without an ending gets when a line
    /// follows it.
    pub default_eol: Eol,
    pub cursor: Pos,
    /// The selection's other end (stream selection from it to the cursor).
    anchor: Option<Pos>,
    /// The column to keep on up / down (screen columns).
    want_vcol: Option<usize>,
    pub top: usize,
    pub left: usize,
    /// The text area of the last frame.
    pub area: Rect,
    pub overtype: bool,
    /// Ctrl+L: editing locked.
    pub locked: bool,
    /// Ctrl+Q: the next key goes in as it is.
    pub quote_next: bool,
    pub line_numbers: bool,
    pub settings: Settings,
    history: History,
    /// Grows with every change (for the agent: what it has seen).
    pub version: u64,
    /// Not on disk yet (Shift+F4 on a new name).
    pub new_file: bool,
    /// The file's time and size when read or written (changes outside).
    pub stamp: Option<(SystemTime, u64)>,
    /// A change on the disk the user chose to keep their text over.
    pub ignored_stamp: Option<(SystemTime, u64)>,
    /// Changes now are the agent's (its lines are marked).
    agent_writing: bool,
    /// The texts of the versions the agent read (the last few; it gets
    /// the changes since one of them).
    agent_seen: Vec<(u64, Vec<String>)>,
    /// The file changed on the disk while this buffer had unsaved changes
    /// and the user kept the buffer.
    pub disk_changed: bool,
    pub opened: Instant,
}

impl Editor {
    pub fn new(id: u32, path: &Path, lines: Vec<Line>, cp: u32, bom: bool, eol: Eol) -> Self {
        let lines = if lines.is_empty() {
            vec![Line::default()]
        } else {
            lines
        };
        Self {
            id,
            path: path.to_path_buf(),
            lines,
            cp,
            bom,
            default_eol: eol,
            cursor: Pos::default(),
            anchor: None,
            want_vcol: None,
            top: 0,
            left: 0,
            area: Rect::default(),
            overtype: false,
            locked: false,
            quote_next: false,
            line_numbers: false,
            settings: Settings::default(),
            history: History::default(),
            version: 0,
            new_file: false,
            stamp: None,
            ignored_stamp: None,
            agent_writing: false,
            agent_seen: Vec::new(),
            disk_changed: false,
            opened: Instant::now(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn set_path(&mut self, path: &Path) {
        self.path = path.to_path_buf();
    }

    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn modified(&self) -> bool {
        self.history.modified()
    }

    /// The text was written: this is the state the file has now.
    pub fn saved(&mut self) {
        self.history.mark_saved();
        self.new_file = false;
        // Saving accepts the agent's text.
        for l in &mut self.lines {
            l.by_agent = false;
        }
        self.disk_changed = false;
    }

    /// The whole text replaced (the file read again): no undo across it.
    pub fn reload(&mut self, lines: Vec<Line>, cp: u32, bom: bool, eol: Eol) {
        self.lines = if lines.is_empty() {
            vec![Line::default()]
        } else {
            lines
        };
        self.cp = cp;
        self.bom = bom;
        self.default_eol = eol;
        self.history = History::default();
        self.anchor = None;
        self.version += 1;
        self.disk_changed = false;
        let last = self.lines.len() - 1;
        self.cursor.line = self.cursor.line.min(last);
        self.top = self.top.min(last);
    }

    // ------------------------------------------------------------ geometry

    fn line_len(&self, line: usize) -> usize {
        self.lines.get(line).map_or(0, Line::len)
    }

    fn char_width(&self, c: char, vcol: usize) -> usize {
        if c == '\t' {
            let t = self.settings.tab_size.max(1);
            t - vcol % t
        } else {
            c.width().unwrap_or(0)
        }
    }

    /// The screen column of `col` in `line` (tabs and wide characters;
    /// past the end, a column per position).
    pub fn vcol(&self, line: usize, col: usize) -> usize {
        let Some(l) = self.lines.get(line) else {
            return col;
        };
        let mut v = 0;
        let mut n = 0;
        for c in l.text.chars() {
            if n == col {
                return v;
            }
            v += self.char_width(c, v);
            n += 1;
        }
        v + (col - n)
    }

    /// The character index at screen column `vcol` (the character under
    /// it; past the end when allowed).
    pub fn col_at(&self, line: usize, vcol: usize) -> usize {
        let Some(l) = self.lines.get(line) else {
            return 0;
        };
        let mut v = 0;
        for (n, c) in l.text.chars().enumerate() {
            let w = self.char_width(c, v);
            if vcol < v + w.max(1) {
                return n;
            }
            v += w;
        }
        let len = l.len();
        if self.settings.cursor_beyond_eol {
            len + (vcol - v)
        } else {
            len
        }
    }

    fn page(&self) -> usize {
        usize::from(self.area.height.max(2)) - 1
    }

    fn number_width(&self) -> u16 {
        if self.line_numbers {
            let digits = self.lines.len().to_string().len().max(3);
            digits as u16 + 1
        } else {
            0
        }
    }

    /// Keeps the cursor on the screen with the least scrolling (Far).
    pub fn scroll_to_cursor(&mut self) {
        let h = usize::from(self.area.height.max(1));
        if self.cursor.line < self.top {
            self.top = self.cursor.line;
        } else if self.cursor.line >= self.top + h {
            self.top = self.cursor.line + 1 - h;
        }
        // Far (AllowEmptySpaceAfterEof off): no empty rows under the last
        // line while the file is longer than the screen.
        let max_top = self.lines.len().saturating_sub(h);
        if self.top > max_top {
            self.top = max_top;
        }
        let w = usize::from(self.area.width.saturating_sub(self.number_width()).max(1));
        let v = self.vcol(self.cursor.line, self.cursor.col);
        if v < self.left {
            self.left = v;
        } else if v >= self.left + w {
            self.left = v + 1 - w;
        }
    }

    // ----------------------------------------------------------- selection

    /// The stream selection, start ≤ end; `None` when empty.
    pub fn selection(&self) -> Option<(Pos, Pos)> {
        let a = self.anchor?;
        let (s, e) = if a <= self.cursor {
            (a, self.cursor)
        } else {
            (self.cursor, a)
        };
        (s != e).then_some((s, e))
    }

    /// A selection end within the text (past a line's end: its end).
    fn clamp(&self, p: Pos) -> Pos {
        let line = p.line.min(self.lines.len() - 1);
        Pos::new(line, p.col.min(self.line_len(line)))
    }

    /// The selected text, line endings included.
    pub fn selected_text(&self) -> Option<String> {
        let (s, e) = self.selection()?;
        let text = self.text_between(self.clamp(s), self.clamp(e));
        (!text.is_empty()).then_some(text)
    }

    fn text_between(&self, s: Pos, e: Pos) -> String {
        let mut out = String::new();
        for line in s.line..=e.line {
            let l = &self.lines[line];
            let from = if line == s.line { l.byte(s.col) } else { 0 };
            let to = if line == e.line {
                l.byte(e.col)
            } else {
                l.text.len()
            };
            out.push_str(&l.text[from..to]);
            if line < e.line {
                out.push_str(l.eol.as_str());
            }
        }
        out
    }

    /// The whole text as on disk (endings included).
    pub fn text(&self) -> String {
        let mut out = String::new();
        for l in &self.lines {
            out.push_str(&l.text);
            out.push_str(l.eol.as_str());
        }
        out
    }

    fn select_to(&mut self, to: Pos) {
        if self.anchor.is_none() {
            self.anchor = Some(self.cursor);
        }
        self.cursor = to;
    }

    /// Far's Ctrl+C without a block: the current line with its ending.
    pub fn select_line(&mut self) {
        let line = self.cursor.line;
        self.anchor = Some(Pos::new(line, 0));
        self.cursor = if line + 1 < self.lines.len() {
            Pos::new(line + 1, 0)
        } else {
            Pos::new(line, self.line_len(line))
        };
    }

    /// "Save as" with other line endings: every ending changes (Far does
    /// it in memory, not as an undo step).
    pub fn set_all_eols(&mut self, eol: Eol) {
        for l in &mut self.lines {
            if l.eol != Eol::None {
                l.eol = eol;
            }
        }
        self.default_eol = eol;
        self.version += 1;
    }

    pub fn unselect(&mut self) {
        self.anchor = None;
    }

    /// A move without Shift: the selection goes unless blocks persist.
    fn move_to(&mut self, to: Pos) {
        if !self.settings.persistent_blocks {
            self.anchor = None;
        }
        self.cursor = to;
        self.history.break_merge();
    }

    // -------------------------------------------------------------- moving

    fn left_of(&self, p: Pos, wrap: bool) -> Pos {
        if p.col > 0 {
            Pos::new(p.line, p.col - 1)
        } else if wrap && p.line > 0 {
            Pos::new(p.line - 1, self.line_len(p.line - 1))
        } else {
            p
        }
    }

    fn right_of(&self, p: Pos) -> Pos {
        let len = self.line_len(p.line);
        if p.col < len || self.settings.cursor_beyond_eol {
            Pos::new(p.line, p.col + 1)
        } else if p.line + 1 < self.lines.len() {
            Pos::new(p.line + 1, 0)
        } else {
            p
        }
    }

    /// The same screen column `delta` lines away.
    fn vertical(&mut self, delta: isize) -> Pos {
        let v = *self
            .want_vcol
            .get_or_insert(self.vcol(self.cursor.line, self.cursor.col));
        let line =
            (self.cursor.line as isize + delta).clamp(0, self.lines.len() as isize - 1) as usize;
        Pos::new(line, self.col_at(line, v))
    }

    fn is_word(&self, c: char) -> bool {
        !c.is_whitespace() && !self.settings.word_div.contains(c)
    }

    fn word_left(&self, p: Pos) -> Pos {
        let chars: Vec<char> = self.lines[p.line].text.chars().collect();
        let mut i = p.col.min(chars.len());
        if i == 0 {
            return self.left_of(p, true);
        }
        while i > 0 && !self.is_word(chars[i - 1]) {
            i -= 1;
        }
        while i > 0 && self.is_word(chars[i - 1]) {
            i -= 1;
        }
        Pos::new(p.line, i)
    }

    fn word_right(&self, p: Pos) -> Pos {
        let chars: Vec<char> = self.lines[p.line].text.chars().collect();
        let mut i = p.col;
        if i >= chars.len() {
            return if p.line + 1 < self.lines.len() {
                Pos::new(p.line + 1, 0)
            } else {
                p
            };
        }
        while i < chars.len() && self.is_word(chars[i]) {
            i += 1;
        }
        while i < chars.len() && !self.is_word(chars[i]) {
            i += 1;
        }
        Pos::new(p.line, i)
    }

    fn file_end(&self) -> Pos {
        let last = self.lines.len() - 1;
        Pos::new(last, self.line_len(last))
    }

    // ------------------------------------------------------------- editing

    /// Replaces the text from `s` to `e` with `ins` (line breaks in it get
    /// `eol`); `pad`: a start past the line's end is filled with spaces.
    /// Returns the end of the inserted text. One change of the undo step.
    fn replace(&mut self, s: Pos, e: Pos, ins: &str, eol: Eol, pad: bool) -> Pos {
        let e = Pos::new(e.line, e.col);
        let first = &self.lines[s.line];
        let mut prefix = first.text[..first.byte(s.col)].to_string();
        let len = first.len();
        if pad && s.col > len {
            prefix.extend(std::iter::repeat_n(' ', s.col - len));
        }
        let last = &self.lines[e.line];
        let suffix = last.text[last.byte(e.col)..].to_string();
        let last_eol = last.eol;
        // The new lines.
        let segments: Vec<&str> = split_breaks(ins);
        let break_eol = match eol {
            Eol::None => self.default_eol,
            e => e,
        };
        let mut new_lines = Vec::with_capacity(segments.len());
        let n = segments.len();
        for (i, seg) in segments.iter().enumerate() {
            let mut text = String::new();
            if i == 0 {
                text.push_str(&prefix);
            }
            text.push_str(seg);
            if i + 1 == n {
                text.push_str(&suffix);
            }
            let mut line = Line::new(text, if i + 1 == n { last_eol } else { break_eol });
            line.by_agent = self.agent_writing;
            new_lines.push(line);
        }
        let end_col = if n == 1 {
            prefix.chars().count() + segments[0].chars().count()
        } else {
            segments[n - 1].chars().count()
        };
        let end = Pos::new(s.line + n - 1, end_col);
        let old: Vec<Line> = self
            .lines
            .splice(s.line..=e.line, new_lines.clone())
            .collect();
        self.history.record(Change {
            at: s.line,
            old,
            new: new_lines,
        });
        self.version += 1;
        end
    }

    /// One undo step around `f` (Far's `begin…end` group).
    fn step(&mut self, f: impl FnOnce(&mut Self)) {
        let before = self.cursor;
        self.history.begin(before);
        f(self);
        self.history.end(self.cursor);
    }

    fn editable(&self) -> bool {
        !self.locked
    }

    fn delete_selection_inner(&mut self) -> bool {
        let Some((s, e)) = self.selection() else {
            return false;
        };
        let (s, e) = (self.clamp(s), self.clamp(e));
        self.anchor = None;
        self.cursor = self.replace(s, e, "", Eol::None, false);
        true
    }

    /// Deletes the selection (Ctrl+D, Del with a block).
    pub fn delete_selection(&mut self) -> bool {
        if !self.editable() || self.selection().is_none() {
            return false;
        }
        self.step(|ed| {
            ed.delete_selection_inner();
        });
        true
    }

    /// A typed character (Far: a stream block is replaced unless blocks
    /// persist; past the end the line is filled with spaces).
    pub fn type_char(&mut self, c: char) {
        if !self.editable() {
            return;
        }
        if self.selection().is_some() && !self.settings.persistent_blocks {
            self.step(|ed| {
                ed.delete_selection_inner();
                ed.insert_char_inner(c);
            });
        } else {
            if !self.settings.persistent_blocks {
                self.anchor = None;
            }
            let at = self.cursor;
            let merge = self.history.can_merge(at);
            if merge {
                self.insert_char_inner(c);
                self.history.merged(self.cursor);
            } else {
                self.step(|ed| ed.insert_char_inner(c));
                self.history.typing(self.cursor);
            }
        }
        self.want_vcol = None;
    }

    fn insert_char_inner(&mut self, c: char) {
        let p = self.cursor;
        let len = self.line_len(p.line);
        let mut buf = [0u8; 4];
        let s = c.encode_utf8(&mut buf);
        if self.overtype && p.col < len {
            self.cursor = self.replace(p, Pos::new(p.line, p.col + 1), s, Eol::None, false);
        } else {
            self.cursor = self.replace(p, p, s, Eol::None, true);
        }
    }

    /// Text inserted at the cursor (paste, Ctrl+F); line breaks get the
    /// ending of the line before the cursor (Far's `KeepEditorEOL`).
    pub fn insert_text(&mut self, text: &str) {
        if !self.editable() || text.is_empty() {
            return;
        }
        let persistent = self.settings.persistent_blocks;
        self.step(|ed| {
            if !persistent {
                ed.delete_selection_inner();
            }
            let at = ed.cursor;
            let eol = ed.lines[at.line].eol;
            let end = ed.replace(at, at, text, eol, true);
            ed.cursor = end;
            // The pasted text stays selected only with persistent blocks.
            ed.anchor = persistent.then_some(at);
        });
        self.want_vcol = None;
    }

    fn enter(&mut self) {
        self.step(|ed| {
            if !ed.settings.persistent_blocks {
                ed.delete_selection_inner();
            }
            let p = ed.cursor;
            let len = ed.line_len(p.line);
            let at = Pos::new(p.line, p.col.min(len));
            let eol = ed.lines[p.line].eol;
            ed.cursor = ed.replace(at, at, "\n", eol, false);
        });
        self.history.break_merge();
    }

    fn delete(&mut self) {
        if self.settings.del_removes_blocks && self.selection().is_some() {
            self.delete_selection();
            return;
        }
        let p = self.cursor;
        let len = self.line_len(p.line);
        if p.col < len {
            self.step(|ed| {
                ed.replace(p, Pos::new(p.line, p.col + 1), "", Eol::None, false);
            });
        } else if p.line + 1 < self.lines.len() {
            // Joins the next line (past the end: at the cursor).
            self.step(|ed| {
                ed.replace(p, Pos::new(p.line + 1, 0), "", Eol::None, true);
            });
        }
    }

    fn backspace(&mut self) {
        if self.settings.del_removes_blocks && self.selection().is_some() {
            self.delete_selection();
            return;
        }
        let p = self.cursor;
        let len = self.line_len(p.line);
        if p.col > len {
            // Past the end: only the cursor moves.
            self.cursor.col -= 1;
        } else if p.col > 0 {
            self.step(|ed| {
                ed.cursor = ed.replace(Pos::new(p.line, p.col - 1), p, "", Eol::None, false);
            });
        } else if p.line > 0 {
            let prev = Pos::new(p.line - 1, self.line_len(p.line - 1));
            self.step(|ed| {
                ed.cursor = ed.replace(prev, p, "", Eol::None, false);
            });
        }
    }

    fn delete_range(&mut self, s: Pos, e: Pos) {
        if s == e {
            return;
        }
        self.step(|ed| {
            ed.cursor = ed.replace(s, e, "", Eol::None, false);
        });
    }

    fn delete_line(&mut self) {
        let p = self.cursor;
        if p.line + 1 < self.lines.len() {
            self.step(|ed| {
                ed.replace(
                    Pos::new(p.line, 0),
                    Pos::new(p.line + 1, 0),
                    "",
                    Eol::None,
                    false,
                );
                ed.cursor = p;
            });
        } else {
            let len = self.line_len(p.line);
            self.step(|ed| {
                ed.replace(
                    Pos::new(p.line, 0),
                    Pos::new(p.line, len),
                    "",
                    Eol::None,
                    false,
                );
                ed.cursor = p;
            });
        }
    }

    fn tab(&mut self) {
        let t = self.settings.tab_size.max(1);
        let v = self.vcol(self.cursor.line, self.cursor.col);
        let next = (v / t + 1) * t;
        if self.overtype {
            let line = self.cursor.line;
            self.cursor = Pos::new(line, self.col_at(line, next));
            return;
        }
        if self.settings.expand_tabs > 0 {
            for _ in v..next {
                self.type_char(' ');
            }
        } else {
            self.type_char('\t');
        }
    }

    pub fn undo(&mut self) {
        if let Some(pos) = self.history.undo(&mut self.lines) {
            self.cursor = pos;
            self.anchor = None;
            self.version += 1;
        }
    }

    pub fn redo(&mut self) {
        if let Some(pos) = self.history.redo(&mut self.lines) {
            self.cursor = pos;
            self.anchor = None;
            self.version += 1;
        }
    }

    // --------------------------------------------------------------- agent

    /// The text's lines without their endings (what the agent sees).
    pub fn plain_lines(&self) -> Vec<String> {
        self.lines.iter().map(|l| l.text.clone()).collect()
    }

    /// The position of byte `offset` of the text joined with `\n`.
    fn pos_of_offset(&self, offset: usize) -> Pos {
        let mut start = 0;
        for (n, l) in self.lines.iter().enumerate() {
            let end = start + l.text.len();
            if offset <= end {
                return Pos::new(n, l.text[..offset - start].chars().count());
            }
            start = end + 1;
        }
        self.file_end()
    }

    /// An agent's change from `s` to `e` — one undo step; the user's
    /// cursor, screen and selection stay where they were in the text.
    fn agent_change(&mut self, s: Pos, e: Pos, new: &str) -> (usize, usize) {
        let before = self.lines.len();
        let (cursor, top, anchor) = (self.cursor, self.top, self.anchor);
        let eol = self.lines[s.line].eol;
        self.agent_writing = true;
        let end = self.replace(s, e, new, eol, false);
        self.agent_writing = false;
        let delta = self.lines.len() as isize - before as isize;
        // Below the change: moved by the lines it added or removed; inside
        // it: kept within it.
        fn shift(p: Pos, s: Pos, e: Pos, end: Pos, delta: isize) -> Pos {
            if p.line > e.line {
                Pos::new((p.line as isize + delta).max(0) as usize, p.col)
            } else if p.line >= s.line {
                Pos::new(p.line.min(end.line), p.col)
            } else {
                p
            }
        }
        self.cursor = shift(cursor, s, e, end, delta);
        self.anchor = anchor.map(|a| shift(a, s, e, end, delta));
        if top > e.line {
            self.top = (top as isize + delta).max(0) as usize;
        }
        (s.line, end.line)
    }

    /// One portion of the agent's (one tool call): one undo step, one
    /// version. Afterwards only the lines whose text it really changed or
    /// added stay marked as its own (a line rebuilt around an insertion
    /// keeps its mark); the changed lines and the count of removed ones
    /// are returned.
    fn agent_portion(&mut self, f: impl FnOnce(&mut Self)) -> AgentEdit {
        use similar::DiffOp;
        let old = self.lines.clone();
        let version = self.version;
        let before = self.cursor;
        self.history.begin(before);
        f(self);
        self.history.end(self.cursor);
        self.version = version + 1;
        let a: Vec<&str> = old.iter().map(|l| l.text.as_str()).collect();
        let b: Vec<String> = self.lines.iter().map(|l| l.text.clone()).collect();
        let b: Vec<&str> = b.iter().map(String::as_str).collect();
        let mut edit = AgentEdit::default();
        for op in similar::capture_diff_slices(similar::Algorithm::Myers, &a, &b) {
            match op {
                DiffOp::Equal {
                    old_index,
                    new_index,
                    len,
                } => {
                    for k in 0..len {
                        self.lines[new_index + k].by_agent = old[old_index + k].by_agent;
                    }
                }
                DiffOp::Delete { old_len, .. } => edit.removed += old_len,
                DiffOp::Insert {
                    new_index, new_len, ..
                } => edit.changed.extend(new_index..new_index + new_len),
                DiffOp::Replace {
                    old_len,
                    new_index,
                    new_len,
                    ..
                } => {
                    edit.changed.extend(new_index..new_index + new_len);
                    edit.removed += old_len.saturating_sub(new_len);
                }
            }
        }
        // Undo and redo go to the change, as for the user's own; redo
        // marks just these lines again.
        let first = edit.changed.first().copied().unwrap_or_else(|| {
            similar::capture_diff_slices(similar::Algorithm::Myers, &a, &b)
                .iter()
                .find(|op| !matches!(op, DiffOp::Equal { .. }))
                .map_or(0, |op| op.new_range().start)
        });
        let at = Pos::new(first.min(self.lines.len() - 1), 0);
        // (A portion that changed nothing left no step of its own.)
        if !edit.changed.is_empty() || edit.removed > 0 {
            let marks = self.lines.iter().map(|l| l.by_agent).collect();
            self.history.agent_step(at, marks);
        }
        edit
    }

    /// Line `n` with its ending (Ctrl+C without a block).
    pub fn line_with_eol(&self, n: usize) -> String {
        self.lines
            .get(n)
            .map(|l| format!("{}{}", l.text, l.eol.as_str()))
            .unwrap_or_default()
    }

    /// The line endings in use: one name, or `mixed: …`.
    pub fn eol_summary(&self) -> String {
        let name = |e: Eol| match e {
            Eol::Lf => "LF",
            Eol::Cr => "CR",
            Eol::CrCrLf => "CR CR LF",
            _ => "CR LF",
        };
        let mut seen: Vec<Eol> = Vec::new();
        for l in &self.lines {
            if l.eol != Eol::None && !seen.contains(&l.eol) {
                seen.push(l.eol);
            }
        }
        match seen.as_slice() {
            [] => name(self.default_eol).to_string(),
            [one] => name(*one).to_string(),
            many => {
                let names: Vec<&str> = many.iter().map(|e| name(*e)).collect();
                format!("mixed: {}", names.join(", "))
            }
        }
    }

    /// `afar_buffer_edit`: `old` (lines joined with `\n`) replaced by
    /// `new`; `old` must be found once unless `all`.
    pub fn agent_replace(&mut self, old: &str, new: &str, all: bool) -> Result<AgentEdit, String> {
        if old.is_empty() {
            return Err("old_string is empty".into());
        }
        let text = self.plain_lines().join("\n");
        let found: Vec<usize> = text.match_indices(old).map(|(i, _)| i).collect();
        match found.len() {
            0 => return Err("old_string not found in the buffer (it may differ from the file on the disk; read it with afar_buffer_read)".into()),
            n if n > 1 && !all => {
                return Err(format!(
                    "old_string found {n} times; give more context to make it unique, or set replace_all"
                ));
            }
            _ => {}
        }
        let new = new.replace("\r\n", "\n");
        Ok(self.agent_portion(|ed| {
            // From the end, so the earlier offsets stay right.
            for &at in found.iter().rev() {
                let s = ed.pos_of_offset(at);
                let e = ed.pos_of_offset(at + old.len());
                ed.agent_change(s, e, &new);
            }
        }))
    }

    /// `afar_buffer_insert`: `text` as new lines after line `after` (from
    /// 1; 0: before the first).
    pub fn agent_insert(&mut self, after: usize, text: &str) -> Result<AgentEdit, String> {
        let n = self.lines.len();
        if after > n {
            return Err(format!(
                "line {after} is past the end: the buffer has {n} lines"
            ));
        }
        let text = text.replace("\r\n", "\n");
        let text = text.strip_suffix('\n').unwrap_or(&text).to_string();
        Ok(self.agent_portion(|ed| {
            if after == 0 {
                ed.agent_change(Pos::default(), Pos::default(), &format!("{text}\n"));
            } else {
                let line = after - 1;
                let end = Pos::new(line, ed.line_len(line));
                ed.agent_change(end, end, &format!("\n{text}"));
            }
        }))
    }

    /// The agent read the text at its current version (a few of the
    /// latest are kept for `since_version`).
    pub fn agent_read(&mut self) {
        let lines = self.plain_lines();
        self.agent_seen.retain(|(v, _)| *v != self.version);
        self.agent_seen.push((self.version, lines));
        if self.agent_seen.len() > 8 {
            self.agent_seen.remove(0);
        }
    }

    /// The text of a version the agent has read.
    pub fn agent_version(&self, version: u64) -> Option<&[String]> {
        self.agent_seen
            .iter()
            .find(|(v, _)| *v == version)
            .map(|(_, l)| l.as_slice())
    }

    /// The last version the agent read.
    pub fn agent_last_read(&self) -> Option<u64> {
        self.agent_seen.last().map(|(v, _)| *v)
    }

    // ------------------------------------------------------------ commands

    pub fn command(&mut self, cmd: EditorCmd) -> Outcome {
        use EditorCmd::*;
        let vertical = matches!(
            cmd,
            Up | Down
                | PageUp
                | PageDown
                | SelUp
                | SelDown
                | SelPageUp
                | SelPageDown
                | FirstLine
                | LastLine
                | SelFirstLine
                | SelLastLine
                | ScrollUp
                | ScrollDown
        );
        if !vertical {
            self.want_vcol = None;
        }
        let page = self.page() as isize;
        let p = self.cursor;
        match cmd {
            Left => self.move_to(self.left_of(p, true)),
            CharLeft => self.move_to(self.left_of(p, false)),
            Right => self.move_to(self.right_of(p)),
            Up => {
                let to = self.vertical(-1);
                self.move_to(to);
            }
            Down => {
                let to = self.vertical(1);
                self.move_to(to);
            }
            Home => self.move_to(Pos::new(p.line, 0)),
            End => self.move_to(Pos::new(p.line, self.line_len(p.line))),
            PageUp | PageDown => {
                let d = if cmd == PageUp { -page } else { page };
                let to = self.vertical(d);
                self.top = (self.top as isize + d).max(0) as usize;
                self.move_to(to);
            }
            FileStart => self.move_to(Pos::default()),
            FileEnd => self.move_to(self.file_end()),
            FirstLine | LastLine => {
                let d = if cmd == FirstLine {
                    isize::MIN / 2
                } else {
                    isize::MAX / 2
                };
                let to = self.vertical(d);
                self.move_to(to);
            }
            WordLeft => self.move_to(self.word_left(p)),
            WordRight => self.move_to(self.word_right(p)),
            ScrollUp | ScrollDown => {
                let d: isize = if cmd == ScrollUp { -1 } else { 1 };
                let top = self.top as isize + d;
                if top >= 0 && (top as usize) < self.lines.len() {
                    self.top = top as usize;
                    let to = self.vertical(d);
                    self.move_to(to);
                }
            }
            ScreenTop => {
                let line = self.top.min(self.lines.len() - 1);
                let v = self.vcol(p.line, p.col);
                self.move_to(Pos::new(line, self.col_at(line, v)));
            }
            ScreenBottom => {
                let line =
                    (self.top + usize::from(self.area.height.max(1)) - 1).min(self.lines.len() - 1);
                let v = self.vcol(p.line, p.col);
                self.move_to(Pos::new(line, self.col_at(line, v)));
            }
            SelLeft => self.select_to(self.left_of(p, true)),
            SelRight => self.select_to(self.right_of(p)),
            SelUp => {
                let to = self.vertical(-1);
                self.select_to(to);
            }
            SelDown => {
                let to = self.vertical(1);
                self.select_to(to);
            }
            SelHome => self.select_to(Pos::new(p.line, 0)),
            SelEnd => self.select_to(Pos::new(p.line, self.line_len(p.line))),
            SelPageUp | SelPageDown => {
                let d = if cmd == SelPageUp { -page } else { page };
                let to = self.vertical(d);
                self.select_to(to);
            }
            SelWordLeft => self.select_to(self.word_left(p)),
            SelWordRight => self.select_to(self.word_right(p)),
            SelFileStart => self.select_to(Pos::default()),
            SelFileEnd => self.select_to(self.file_end()),
            SelFirstLine | SelLastLine => {
                let d = if cmd == SelFirstLine {
                    isize::MIN / 2
                } else {
                    isize::MAX / 2
                };
                let to = self.vertical(d);
                self.select_to(to);
            }
            SelectAll => {
                self.anchor = Some(Pos::default());
                self.cursor = self.file_end();
            }
            Unselect => self.anchor = None,
            DeleteBlock => {
                self.delete_selection();
            }
            Delete if self.editable() => self.delete(),
            Backspace if self.editable() => self.backspace(),
            DeleteWordLeft if self.editable() => {
                let s = self.word_left(p);
                let e = self.clamp(p);
                if s.line == p.line {
                    self.delete_range(s, e);
                } else {
                    self.backspace();
                }
            }
            DeleteWordRight if self.editable() => {
                if p.col >= self.line_len(p.line) {
                    self.delete();
                } else {
                    let e = self.word_right(p);
                    let e = if e.line == p.line {
                        e
                    } else {
                        Pos::new(p.line, self.line_len(p.line))
                    };
                    self.delete_range(p, e);
                    self.cursor = p;
                }
            }
            DeleteToLineStart if self.editable() => {
                let e = self.clamp(p);
                self.delete_range(Pos::new(p.line, 0), e);
            }
            DeleteToLineEnd if self.editable() => {
                let len = self.line_len(p.line);
                if p.col < len {
                    self.delete_range(p, Pos::new(p.line, len));
                    self.cursor = p;
                }
            }
            DeleteLine if self.editable() => self.delete_line(),
            Enter if self.editable() => self.enter(),
            Tab if self.editable() => self.tab(),
            BackTab => {
                let t = self.settings.tab_size.max(1);
                let v = self.vcol(p.line, p.col);
                let prev = if v == 0 { 0 } else { (v - 1) / t * t };
                self.cursor = Pos::new(p.line, self.col_at(p.line, prev));
            }
            Overtype => self.overtype = !self.overtype,
            QuoteChar if self.editable() => self.quote_next = true,
            Undo if self.editable() => self.undo(),
            Redo if self.editable() => self.redo(),
            Lock => self.locked = !self.locked,
            LineNumbers => self.line_numbers = !self.line_numbers,
            Delete | Backspace | DeleteWordLeft | DeleteWordRight | DeleteToLineStart
            | DeleteToLineEnd | DeleteLine | Enter | Tab | QuoteChar | Undo | Redo => {}
            other => return Outcome::App(other),
        }
        self.scroll_to_cursor();
        Outcome::Done
    }

    // ---------------------------------------------------------------- mouse

    /// The text position at a screen cell of the last frame.
    pub fn pos_at(&self, x: u16, y: u16) -> Option<Pos> {
        let a = self.area;
        if !a.contains(Position::new(x, y)) {
            return None;
        }
        let line = (self.top + usize::from(y - a.y)).min(self.lines.len() - 1);
        let nw = self.number_width();
        let vx = usize::from(x.saturating_sub(a.x + nw)) + self.left;
        Some(Pos::new(line, self.col_at(line, vx)))
    }

    pub fn click(&mut self, x: u16, y: u16, shift: bool) {
        let Some(p) = self.pos_at(x, y) else { return };
        if shift {
            self.select_to(p);
        } else {
            self.anchor = None;
            self.cursor = p;
        }
        self.history.break_merge();
        self.want_vcol = None;
        self.scroll_to_cursor();
    }

    /// Dragging with the left button: the selection to the mouse.
    pub fn drag(&mut self, x: u16, y: u16) {
        if let Some(p) = self.pos_at(x, y) {
            self.select_to(p);
            self.scroll_to_cursor();
        }
    }

    /// A double click: the word under the mouse.
    pub fn select_word(&mut self, x: u16, y: u16) {
        let Some(p) = self.pos_at(x, y) else { return };
        let chars: Vec<char> = self.lines[p.line].text.chars().collect();
        if p.col >= chars.len() || !self.is_word(chars[p.col]) {
            return;
        }
        let mut s = p.col;
        while s > 0 && self.is_word(chars[s - 1]) {
            s -= 1;
        }
        let mut e = p.col;
        while e < chars.len() && self.is_word(chars[e]) {
            e += 1;
        }
        self.anchor = Some(Pos::new(p.line, s));
        self.cursor = Pos::new(p.line, e);
        self.scroll_to_cursor();
    }

    /// The wheel: the screen and the cursor by `lines` (Far: as Ctrl+Up /
    /// Ctrl+Down).
    pub fn wheel(&mut self, lines: isize) {
        let max_top = self.lines.len().saturating_sub(1) as isize;
        let top = (self.top as isize + lines).clamp(0, max_top);
        let d = top - self.top as isize;
        self.top = top as usize;
        let line = (self.cursor.line as isize + d).clamp(0, max_top) as usize;
        self.cursor.line = line;
        self.scroll_to_cursor();
    }

    // --------------------------------------------------------------- drawing

    /// Draws the text into `area`; returns the cursor's cell.
    pub fn draw(&mut self, area: Rect, buf: &mut Buffer) -> Option<Position> {
        self.area = area;
        self.scroll_to_cursor();
        buf.set_style(area, theme::EDITOR_TEXT);
        let nw = self.number_width();
        let text_x = area.x + nw;
        let text_w = area.width.saturating_sub(nw);
        let sel = self.selection();
        for row in 0..area.height {
            let y = area.y + row;
            for x in area.left()..area.right() {
                buf[(x, y)].set_symbol(" ").set_style(theme::EDITOR_TEXT);
            }
            let line = self.top + usize::from(row);
            if nw > 0 {
                let label = if line < self.lines.len() {
                    format!("{:>w$} ", line + 1, w = usize::from(nw - 1))
                } else {
                    " ".repeat(usize::from(nw))
                };
                buf.set_stringn(
                    area.x,
                    y,
                    &label,
                    usize::from(nw),
                    theme::EDITOR_LINE_NUMBERS,
                );
            }
            let Some(l) = self.lines.get(line) else {
                continue;
            };
            // The selected screen columns of this line.
            let sel_v = sel.and_then(|(s, e)| {
                if line < s.line || line > e.line {
                    return None;
                }
                let from = if line == s.line {
                    self.vcol(line, s.col)
                } else {
                    0
                };
                let to = if line == e.line {
                    self.vcol(line, e.col)
                } else {
                    usize::MAX
                };
                Some((from, to))
            });
            let mut v = 0usize;
            for c in l.text.chars() {
                let w = self.char_width(c, v);
                if v >= self.left + usize::from(text_w) {
                    break;
                }
                if v + w > self.left {
                    let selected = sel_v.is_some_and(|(a, b)| v >= a && v < b);
                    let style = if selected {
                        theme::EDITOR_SELECTED
                    } else if l.by_agent {
                        theme::EDITOR_AGENT
                    } else {
                        theme::EDITOR_TEXT
                    };
                    let x0 = text_x + (v.saturating_sub(self.left)) as u16;
                    if c == '\t' || v < self.left || (c as u32) < 0x20 {
                        // Tabs, a wide character cut at the left edge, and
                        // control characters show as spaces (Far: `?` for
                        // some; kept simple).
                        let shown = if (c as u32) < 0x20 && c != '\t' { 1 } else { w };
                        for k in 0..shown {
                            let x = x0 + k as u16;
                            if x < text_x + text_w && v + k >= self.left {
                                buf[(x, y)].set_symbol(" ").set_style(style);
                            }
                        }
                    } else if x0 + w as u16 <= text_x + text_w {
                        let mut b = [0u8; 4];
                        buf[(x0, y)]
                            .set_symbol(c.encode_utf8(&mut b))
                            .set_style(style);
                        if w == 2 {
                            buf[(x0 + 1, y)].set_symbol("").set_style(style);
                        }
                    }
                }
                v += w;
            }
            // Selection past the text (the line's end selected).
            if let Some((a, b)) = sel_v {
                let from = a.max(v).max(self.left);
                let to = b.min(self.left + usize::from(text_w));
                for vx in from..to {
                    let x = text_x + (vx - self.left) as u16;
                    buf[(x, y)].set_style(theme::EDITOR_SELECTED);
                }
            }
        }
        let v = self.vcol(self.cursor.line, self.cursor.col);
        let row = self.cursor.line.checked_sub(self.top)?;
        if row >= usize::from(area.height) || v < self.left || v - self.left >= usize::from(text_w)
        {
            return None;
        }
        Some(Position::new(
            text_x + (v - self.left) as u16,
            area.y + row as u16,
        ))
    }
}

/// Text split at its line breaks (`\r\n`, `\r\r\n`, `\n`, `\r`).
fn split_breaks(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                out.push(&text[start..i]);
                i += 1;
                start = i;
            }
            b'\r' => {
                out.push(&text[start..i]);
                i += if bytes[i + 1..].starts_with(b"\r\n") {
                    3
                } else if bytes[i + 1..].starts_with(b"\n") {
                    2
                } else {
                    1
                };
                start = i;
            }
            _ => i += 1,
        }
    }
    out.push(&text[start..]);
    out
}

#[cfg(test)]
mod tests;
