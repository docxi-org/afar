//! The built-in editor (F4), as Far's (`editor.cpp`, `edit.cpp`,
//! `fileedit.cpp`; docs/17): a list of lines with their own line endings,
//! a cursor that may stand past the end of a line, stream selection,
//! undo in steps (typing in a line merges, `begin…end` groups), and the
//! screen: a top line and a horizontal offset shared by all lines.
//! The window around it (status line, key bar, dialogs) is in
//! `app/editors.rs`.

mod block;
mod indent;
pub mod marker;
mod propose;
mod recode;
mod search;
pub mod text;
mod undo;

use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use unicode_width::UnicodeWidthChar as _;

use crate::command::EditorCmd;
use crate::theme;
pub use block::{BlockText, VBlock};
pub use propose::{Proposal, ScreenRow};
pub use recode::CpProblem;
pub use search::{Finder, Found};
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

/// A bookmark (Far's `m_SavePos`): the cursor's line and position, the
/// screen's left column and the cursor's row on the screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Bookmark {
    pub line: usize,
    pub col: usize,
    pub left: usize,
    pub screen_line: usize,
}

/// A block that no longer follows the cursor (persistent blocks).
#[derive(Clone, Copy, Debug)]
enum Frozen {
    Stream(Pos, Pos),
    Vertical(VBlock),
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
    /// Far's `SearchCursorAtEnd`: the cursor after a match, not on it.
    pub search_cursor_at_end: bool,
    /// Far's `SearchSelFound`: a match is selected.
    pub search_select_found: bool,
    /// Enter puts the cursor at the indent of the line above.
    pub auto_indent: bool,
    /// 0: off; 1: spaces, tabs and line endings; 2: without line endings.
    pub show_whitespace: u8,
    pub scrollbar: bool,
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
            search_cursor_at_end: false,
            search_select_found: false,
            auto_indent: false,
            show_whitespace: 0,
            scrollbar: false,
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
    /// The live block is vertical: `anchor`'s line and this screen column
    /// are its other corner (Alt+arrows).
    vblock_vcol: Option<usize>,
    /// A block that no longer follows the cursor (persistent blocks).
    frozen: Option<Frozen>,
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
    /// Where the last search found its match (Shift+F7 goes on past it
    /// when the cursor is still there).
    pub last_found: Option<Found>,
    /// A match shown in the selection's colour while the replace question
    /// is open.
    pub highlight: Option<Found>,
    /// The Hex box of Alt+F8 (Far's `m_GotoHex`, per window).
    pub goto_hex: bool,
    /// The scroll bar's thumb is being dragged.
    pub dragging_bar: bool,
    /// Bytes the code page could not read when the file was read (Far's
    /// `BadConversion`): saving would lose them.
    pub bad_conversion: Option<Vec<u8>>,
    /// The user passed the turn to the agent (until the agent's turn ends).
    pub agent_turn: Option<Instant>,
    /// Lines marked by the agent (`afar_highlight`; lines from 1, as in
    /// the viewer): coloured, the label as a note in the margin.
    pub marks: Vec<crate::viewer::Mark>,
    /// The screen was moved to show something (the agent's marks): it
    /// stays there, the cursor off it, until the user acts.
    pub hold_view: bool,
    /// The agent's proposals waiting for the user's answer (in order).
    pub proposals: Vec<Proposal>,
    proposal_seq: u64,
    /// The rows of the last frame (lines and proposals' new lines).
    shown_rows: Vec<ScreenRow>,
    /// Proposals dropped by a change of their lines, for the journal.
    dropped_proposals: Vec<u64>,
    /// Syntax highlighting on (Alt+F3) and the file's syntax.
    pub syntax_on: bool,
    syntax: Option<&'static syntect::parsing::SyntaxReference>,
    /// The parse state before every `SYN_STEP`-th line (the ones known so
    /// far, from the top).
    syn_marks: Vec<crate::syntax::LineState>,
    /// Ctrl+Shift+0…9 / Ctrl+0…9.
    pub bookmarks: [Option<Bookmark>; 10],
}

impl Editor {
    pub fn new(id: u32, path: &Path, lines: Vec<Line>, cp: u32, bom: bool, eol: Eol) -> Self {
        let lines = if lines.is_empty() {
            vec![Line::default()]
        } else {
            lines
        };
        let mut e = Self {
            id,
            path: path.to_path_buf(),
            lines,
            cp,
            bom,
            default_eol: eol,
            cursor: Pos::default(),
            anchor: None,
            vblock_vcol: None,
            frozen: None,
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
            last_found: None,
            highlight: None,
            goto_hex: false,
            dragging_bar: false,
            bad_conversion: None,
            agent_turn: None,
            marks: Vec::new(),
            hold_view: false,
            proposals: Vec::new(),
            proposal_seq: 0,
            shown_rows: Vec::new(),
            dropped_proposals: Vec::new(),
            bookmarks: [None; 10],
            syntax_on: true,
            syntax: None,
            syn_marks: Vec::new(),
        };
        e.detect_syntax();
        e
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn set_path(&mut self, path: &Path) {
        self.path = path.to_path_buf();
        self.detect_syntax();
    }

    /// The syntax by the file's name, else its first line.
    pub fn detect_syntax(&mut self) {
        let first = self.lines.first().map_or("", |l| l.text.as_str());
        self.syntax = crate::syntax::syntax_for(&self.path, first);
        self.syn_marks.clear();
    }

    /// The name of the file's syntax, if it has one.
    pub fn syntax_name(&self) -> Option<&'static str> {
        self.syntax.map(|s| s.name.as_str())
    }

    /// Whole lines changed past `replace` (undo, a reload, another code
    /// page): the parse states go.
    fn syntax_reset(&mut self) {
        self.syn_marks.clear();
    }

    /// The colored pieces of lines `from..to` (syntax highlighting; empty
    /// when off). Parses from the nearest known state; a place much
    /// further down than parsed so far starts afresh a little above it.
    fn syntax_pieces(&mut self, from: usize, to: usize) -> Vec<crate::syntax::Pieces> {
        use crate::syntax::LineState;
        let Some(syn) = self.syntax.filter(|_| self.syntax_on) else {
            return Vec::new();
        };
        if self.syn_marks.is_empty() {
            self.syn_marks.push(LineState::start(syn));
        }
        let want = from / SYN_STEP;
        let have = self.syn_marks.len() - 1;
        let (mut st, mut line, exact) = if want <= have {
            (self.syn_marks[want].clone(), want * SYN_STEP, true)
        } else if (want - have) * SYN_STEP <= SYN_REACH {
            (self.syn_marks[have].clone(), have * SYN_STEP, true)
        } else {
            (
                LineState::start(syn),
                from.saturating_sub(SYN_LOOKBACK),
                false,
            )
        };
        let to = to.min(self.lines.len());
        let mut out = Vec::with_capacity(to.saturating_sub(from));
        while line < to {
            if exact && line % SYN_STEP == 0 && line / SYN_STEP == self.syn_marks.len() {
                self.syn_marks.push(st.clone());
            }
            let pieces = st.line(&self.lines[line].text);
            if line >= from {
                out.push(pieces);
            }
            line += 1;
        }
        out
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
        self.syn_marks.clear();
        self.lines = if lines.is_empty() {
            vec![Line::default()]
        } else {
            lines
        };
        self.cp = cp;
        self.bom = bom;
        self.default_eol = eol;
        self.history = History::default();
        self.unselect();
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

    /// The scroll bar is shown: asked for, and the text is taller than the
    /// window (Far's `ScrollBarRequired`).
    fn scrollbar_shown(&self) -> bool {
        self.settings.scrollbar
            && self.area.height >= 2
            && self.area.width > 1
            && self.lines.len() > usize::from(self.area.height)
    }

    /// The text's columns: the window less the line numbers and the bar.
    fn text_width(&self) -> u16 {
        self.area
            .width
            .saturating_sub(self.number_width() + u16::from(self.scrollbar_shown()))
    }

    /// Keeps the cursor's line on the screen when proposals' rows take
    /// room between the top and the cursor.
    fn scroll_with_proposals(&mut self, h: usize) {
        if self.cursor.line < self.top {
            self.top = self.cursor.line;
        }
        while self.top < self.cursor.line
            && !self
                .screen_rows(h)
                .contains(&ScreenRow::Line(self.cursor.line))
        {
            self.top += 1;
        }
        let w = usize::from(self.text_width().max(1));
        let v = self.vcol(self.cursor.line, self.cursor.col);
        if v < self.left {
            self.left = v;
        } else if v >= self.left + w {
            self.left = v + 1 - w;
        }
    }

    /// A press on the scroll bar (Far's `Editor::ProcessMouse`): the arrows
    /// scroll a line (as Ctrl+Up / Ctrl+Down), elsewhere the thumb goes
    /// there and follows a drag. Returns whether it was taken.
    pub fn scrollbar_press(&mut self, x: u16, y: u16) -> bool {
        let a = self.area;
        if !self.scrollbar_shown() || x + 1 != a.right() || !(a.top()..a.bottom()).contains(&y) {
            return false;
        }
        if y == a.y {
            self.command(EditorCmd::ScrollUp);
        } else if y + 1 == a.bottom() {
            self.command(EditorCmd::ScrollDown);
        } else {
            self.dragging_bar = true;
            self.thumb_to(y);
        }
        true
    }

    /// The thumb dragged to row `y`: that part of the text on the screen.
    pub fn thumb_to(&mut self, y: u16) {
        let a = self.area;
        let field = usize::from(a.height.saturating_sub(2)).max(1);
        let row = usize::from(y.clamp(a.y + 1, a.bottom().saturating_sub(2)) - (a.y + 1));
        let last = self.lines.len() - 1;
        let line = if field <= 1 {
            0
        } else {
            row * last / (field - 1)
        };
        let h = usize::from(a.height);
        self.top = line.min(self.lines.len().saturating_sub(h));
        let v = self.vcol(self.cursor.line, self.cursor.col);
        let line = line.min(last);
        self.cursor = Pos::new(line, self.col_at(line, v));
        if !self.settings.persistent_blocks {
            self.unselect();
        }
    }

    /// A proposal's new line: the text from the left column, tabs as
    /// blanks, the whole row in the proposal's colour.
    fn draw_proposed(&self, text: &str, text_x: u16, y: u16, text_w: u16, buf: &mut Buffer) {
        let st = theme::PROPOSAL_NEW;
        for x in text_x..text_x + text_w {
            buf[(x, y)].set_symbol(" ").set_style(st);
        }
        let mut v = 0usize;
        for c in text.chars() {
            let w = self.char_width(c, v);
            if v >= self.left + usize::from(text_w) {
                break;
            }
            if v >= self.left
                && c != '\t'
                && (c as u32) >= 0x20
                && v + w <= self.left + usize::from(text_w)
            {
                let x = text_x + (v - self.left) as u16;
                let mut b = [0u8; 4];
                buf[(x, y)].set_symbol(c.encode_utf8(&mut b)).set_style(st);
                if w == 2 {
                    buf[(x + 1, y)].set_symbol("").set_style(st);
                }
            }
            v += w;
        }
    }

    fn draw_scrollbar(&self, bar: Rect, buf: &mut Buffer) {
        let style = theme::EDITOR_SCROLLBAR;
        let h = bar.height;
        buf[(bar.x, bar.y)].set_symbol("▲").set_style(style);
        buf[(bar.x, bar.bottom() - 1)]
            .set_symbol("▼")
            .set_style(style);
        let field = h.saturating_sub(2);
        if field == 0 {
            return;
        }
        let size = self.lines.len().max(1) as u64;
        let shown = u64::from(h).min(size);
        let thumb = ((u64::from(field) * shown).div_ceil(size)).clamp(1, u64::from(field)) as u16;
        let start = if self.top as u64 + u64::from(h) >= size {
            field - thumb
        } else {
            ((u64::from(field) * self.top as u64 / size) as u16).min(field - thumb)
        };
        for i in 0..field {
            let s = if (start..start + thumb).contains(&i) {
                "█"
            } else {
                "░"
            };
            buf[(bar.x, bar.y + 1 + i)].set_symbol(s).set_style(style);
        }
    }

    /// Keeps the cursor on the screen with the least scrolling (Far).
    pub fn scroll_to_cursor(&mut self) {
        let h = usize::from(self.area.height.max(1));
        if self.hold_view {
            self.top = self.top.min(self.lines.len().saturating_sub(h));
            return;
        }
        if !self.proposals.is_empty() {
            self.scroll_with_proposals(h);
            return;
        }
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
        let w = usize::from(self.text_width().max(1));
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
        if self.vblock_vcol.is_some() {
            return None;
        }
        let Some(a) = self.anchor else {
            return match self.frozen {
                Some(Frozen::Stream(s, e)) if s != e => Some((s, e)),
                _ => None,
            };
        };
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

    /// Shift+movement: a stream block from where marking began (a new
    /// one replaces a vertical or finished block, as in Far).
    fn select_to(&mut self, to: Pos) {
        if self.anchor.is_none() || self.vblock_vcol.is_some() {
            self.mark_stream(self.cursor);
        }
        self.cursor = to;
    }

    /// Far's Ctrl+C without a block: the current line with its ending.
    pub fn select_line(&mut self) {
        let line = self.cursor.line;
        self.mark_stream(Pos::new(line, 0));
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
        self.vblock_vcol = None;
        self.frozen = None;
    }

    /// The live block stops following the cursor: it stays where it is
    /// with persistent blocks, and goes otherwise (Far).
    fn stop_marking(&mut self) {
        if !self.settings.persistent_blocks {
            self.unselect();
            return;
        }
        if let Some(b) = self.vblock() {
            self.frozen = Some(Frozen::Vertical(b));
        } else if let Some((s, e)) = self.selection() {
            self.frozen = Some(Frozen::Stream(s, e));
        }
        self.anchor = None;
        self.vblock_vcol = None;
    }

    /// A new stream block from `from` to the cursor (it follows the
    /// cursor while marking).
    fn mark_stream(&mut self, from: Pos) {
        self.anchor = Some(from);
        self.vblock_vcol = None;
        self.frozen = None;
    }

    /// A move without Shift: the selection goes unless blocks persist.
    fn move_to(&mut self, to: Pos) {
        self.stop_marking();
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
        let first_typed = first.typed;
        let last = &self.lines[e.line];
        let suffix = last.text[last.byte(e.col)..].to_string();
        let last_eol = last.eol;
        let last_typed = last.typed;
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
            // "Typed here": the user put text into the line, or it keeps a
            // typed line's text; a join or a break alone does not make a
            // line of the file typed.
            line.typed = (!seg.is_empty() && !self.agent_writing)
                || (i == 0 && !prefix.is_empty() && first_typed)
                || (i + 1 == n && !suffix.is_empty() && last_typed);
            new_lines.push(line);
        }
        let end_col = if n == 1 {
            prefix.chars().count() + segments[0].chars().count()
        } else {
            segments[n - 1].chars().count()
        };
        let end = Pos::new(s.line + n - 1, end_col);
        let removed = e.line - s.line + 1;
        let delta = n as isize - removed as isize;
        let old: Vec<Line> = self
            .lines
            .splice(s.line..=e.line, new_lines.clone())
            .collect();
        // Bookmarks keep to their lines (Far shifts their numbers).
        for b in self.bookmarks.iter_mut().flatten() {
            if b.line > e.line {
                b.line = (b.line as isize + delta).max(0) as usize;
            } else if b.line > s.line {
                b.line = b.line.min(end.line);
            }
        }
        self.proposals_after_change(s.line, e.line, delta);
        self.syn_marks.truncate(s.line / SYN_STEP + 1);
        // So do the agent's marks (lines from 1).
        let shift = |l: u64| -> u64 {
            let z = l.saturating_sub(1) as usize;
            let z = if z > e.line {
                (z as isize + delta).max(0) as usize
            } else if z > s.line {
                z.min(end.line)
            } else {
                z
            };
            z as u64 + 1
        };
        for m in &mut self.marks {
            m.from = shift(m.from);
            m.to = shift(m.to).max(m.from);
        }
        // A finished block moves with the text after the change.
        let after = |p: Pos| -> Pos {
            if p < e {
                if p > s { s } else { p }
            } else if p.line == e.line {
                Pos::new(end.line, end.col + (p.col - e.col))
            } else {
                Pos::new((p.line as isize + delta) as usize, p.col)
            }
        };
        self.frozen = match self.frozen {
            Some(Frozen::Stream(a, b)) => Some(Frozen::Stream(after(a), after(b))),
            Some(Frozen::Vertical(mut b)) => {
                let line = |l: usize| {
                    if l > e.line {
                        (l as isize + delta) as usize
                    } else {
                        l
                    }
                };
                b.top = line(b.top);
                b.bottom = line(b.bottom);
                Some(Frozen::Vertical(b))
            }
            None => None,
        };
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
        self.unselect();
        self.cursor = self.replace(s, e, "", Eol::None, false);
        true
    }

    /// Deletes the selection (Ctrl+D, Del with a block).
    pub fn delete_selection(&mut self) -> bool {
        if !self.editable() || !self.has_block() {
            return false;
        }
        if let Some(b) = self.vblock() {
            self.step(|ed| ed.delete_vblock_inner(b));
            return true;
        }
        self.step(|ed| {
            ed.delete_selection_inner();
        });
        true
    }

    /// A typed character (Far: a stream block is replaced unless blocks
    /// persist; past the end the line is filled with spaces).
    pub fn type_char(&mut self, c: char) {
        self.hold_view = false;
        if !self.editable() {
            return;
        }
        if self.selection().is_some() && !self.settings.persistent_blocks {
            self.step(|ed| {
                ed.delete_selection_inner();
                ed.insert_char_inner(c);
            });
        } else {
            self.stop_marking();
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
        } else if let Some(indent) = self.empty_line_indent(p) {
            let start = Pos::new(p.line, 0);
            self.cursor = self.replace(start, start, &format!("{indent}{s}"), Eol::None, false);
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
            // Far: any block (a vertical one too) goes; the pasted text is
            // the block only with persistent blocks.
            ed.unselect();
            let at = ed.cursor;
            let eol = ed.lines[at.line].eol;
            let end = ed.replace(at, at, text, eol, true);
            ed.cursor = end;
            // All tabs as spaces: the pasted ones too.
            if ed.settings.expand_tabs == 2 && text.contains('\t') {
                let v = ed.vcol(end.line, end.col);
                let tab = ed.settings.tab_size.max(1);
                for l in at.line..=end.line {
                    if ed.lines[l].text.contains('\t') {
                        let chars: Vec<char> = ed.lines[l].text.chars().collect();
                        let new: String = block::expand_tabs(&chars, tab).into_iter().collect();
                        let len = ed.line_len(l);
                        ed.replace(Pos::new(l, 0), Pos::new(l, len), &new, Eol::None, false);
                    }
                }
                ed.cursor = Pos::new(end.line, ed.real_col(end.line, v));
            }
            // The pasted text stays selected only with persistent blocks.
            if persistent {
                ed.mark_stream(at);
            }
        });
        self.want_vcol = None;
    }

    fn enter(&mut self) {
        self.step(|ed| {
            if !ed.settings.persistent_blocks {
                ed.delete_selection_inner();
            }
            ed.split_line();
        });
        self.history.break_merge();
    }

    fn delete(&mut self) {
        if self.settings.del_removes_blocks && self.has_block() {
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
        if self.settings.del_removes_blocks && self.has_block() {
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

    /// The line no longer counts as typed here (its marker went to the
    /// agent and the line stays).
    pub fn untype_line(&mut self, n: usize) {
        if let Some(l) = self.lines.get_mut(n) {
            l.typed = false;
        }
    }

    /// Ctrl+Enter: the line under the cursor, when the user typed it here
    /// and it is not blank, is the agent's instruction — taken out of the
    /// text (an undo step of its own). Returns its line (from 0) and text.
    pub fn take_instruction(&mut self) -> Option<(usize, String)> {
        let n = self.cursor.line;
        let l = &self.lines[n];
        let text = l.text.trim().to_string();
        if !self.editable() || !l.typed || text.is_empty() {
            return None;
        }
        self.unselect();
        self.delete_line();
        self.cursor.col = 0;
        Some((n, text))
    }

    /// Marks whose time ran out go.
    pub fn marks_tick(&mut self) {
        let now = Instant::now();
        self.marks.retain(|m| m.expires.is_none_or(|t| t > now));
    }

    /// F5 / Shift+F5: to the next / previous marked line.
    fn goto_mark(&mut self, forward: bool) {
        let at = self.cursor.line as u64 + 1;
        // Marks and proposals (lines from 1).
        let places: Vec<u64> = self
            .marks
            .iter()
            .map(|m| m.from)
            .chain(self.proposals.iter().map(|p| {
                (p.start.saturating_sub(usize::from(p.old.is_empty())) as u64 + 1)
                    .min(self.lines.len() as u64)
            }))
            .collect();
        let target = if forward {
            places.iter().copied().filter(|f| *f > at).min()
        } else {
            places.iter().copied().filter(|f| *f < at).max()
        };
        if let Some(line) = target {
            let line = (line as usize - 1).min(self.lines.len() - 1);
            let h = usize::from(self.area.height.max(1));
            if line < self.top || line >= self.top + h {
                self.top = line.saturating_sub(h / 4);
            }
            self.move_to(Pos::new(line, 0));
            // A proposal there: its new lines on the screen too.
            if let Some(k) = self
                .proposals
                .iter()
                .position(|p| p.start == line || (p.old.is_empty() && p.start == line + 1))
            {
                let last = self.proposals[k].new.len().saturating_sub(1);
                while self.top < line
                    && !self.screen_rows(h).contains(&ScreenRow::Proposed(k, last))
                {
                    self.top += 1;
                }
                self.hold_view = true;
            }
        }
    }

    /// The mark on `line` (from 0) drawn on top, if any.
    pub fn mark_at(&self, line: usize) -> Option<&crate::viewer::Mark> {
        let l = line as u64 + 1;
        self.marks.iter().rev().find(|m| l >= m.from && l <= m.to)
    }

    pub fn undo(&mut self) {
        self.syntax_reset();
        let before: Vec<String> = self.plain_lines();
        if let Some(pos) = self.history.undo(&mut self.lines) {
            self.cursor = pos;
            self.unselect();
            self.version += 1;
            self.remap_places(&before);
        }
    }

    pub fn redo(&mut self) {
        self.syntax_reset();
        let before: Vec<String> = self.plain_lines();
        if let Some(pos) = self.history.redo(&mut self.lines) {
            self.cursor = pos;
            self.unselect();
            self.version += 1;
            self.remap_places(&before);
        }
    }

    /// After undo / redo (which put whole lines back, past `replace`): the
    /// marks and bookmarks follow their lines, found by comparing the
    /// text before and after; a place on a line that went moves to the
    /// next one.
    fn remap_places(&mut self, before: &[String]) {
        let after = self.plain_lines();
        let mut map = vec![after.len().saturating_sub(1); before.len() + 1];
        let ops = similar::capture_diff_slices(similar::Algorithm::Myers, before, &after);
        for op in &ops {
            let (old, new) = (op.old_range(), op.new_range());
            for (k, o) in old.clone().enumerate() {
                map[o] = match op.tag() {
                    similar::DiffTag::Equal | similar::DiffTag::Replace => {
                        (new.start + k).min(new.end.saturating_sub(1).max(new.start))
                    }
                    _ => new.start,
                };
            }
        }
        let last = after.len().saturating_sub(1);
        let to_new = |l: usize| map.get(l).copied().unwrap_or(last).min(last);
        for b in self.bookmarks.iter_mut().flatten() {
            b.line = to_new(b.line);
        }
        for m in &mut self.marks {
            m.from = to_new(m.from as usize - 1) as u64 + 1;
            m.to = (to_new(m.to as usize - 1) as u64 + 1).max(m.from);
        }
        // Proposals follow their lines; one whose lines are not as it saw
        // them goes.
        let count = before.len();
        for p in &mut self.proposals {
            p.start = if p.start >= count {
                after.len()
            } else {
                to_new(p.start)
            };
        }
        let before_ids: Vec<u64> = self.proposals.iter().map(|p| p.id).collect();
        let lines = &self.lines;
        self.proposals.retain(|p| {
            p.old_end() <= lines.len()
                && p.old
                    .iter()
                    .zip(&lines[p.start..p.old_end()])
                    .all(|(o, l)| *o == l.text)
        });
        self.note_dropped(&before_ids);
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
        self.hold_view = false;
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
                | VSelUp
                | VSelDown
                | VSelPageUp
                | VSelPageDown
                | VSelFirstLine
                | VSelLastLine
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
                self.mark_stream(Pos::default());
                self.cursor = self.file_end();
            }
            Unselect => self.unselect(),
            VSelLeft => {
                if p.col > 0 {
                    self.vmark(Pos::new(p.line, p.col - 1));
                }
            }
            VSelRight => {
                if self.settings.cursor_beyond_eol || p.col < self.line_len(p.line) {
                    self.vmark(Pos::new(p.line, p.col + 1));
                }
            }
            VSelUp | VSelDown | VSelPageUp | VSelPageDown | VSelFirstLine | VSelLastLine => {
                let d = match cmd {
                    VSelUp => -1,
                    VSelDown => 1,
                    VSelPageUp => -page,
                    VSelPageDown => page,
                    VSelFirstLine => isize::MIN / 2,
                    _ => isize::MAX / 2,
                };
                let to = self.vertical(d);
                if to.line != p.line {
                    self.vmark(to);
                }
            }
            VSelHome => self.vmark(Pos::new(p.line, 0)),
            VSelEnd => self.vmark(Pos::new(p.line, self.line_len(p.line))),
            VSelWordLeft => {
                let c = p.col.min(self.line_len(p.line));
                let to = if c == 0 {
                    Pos::new(p.line, 0)
                } else {
                    self.word_left(Pos::new(p.line, c))
                };
                if to != p {
                    self.vmark(to);
                }
            }
            VSelWordRight => {
                if p.col < self.line_len(p.line) {
                    self.vmark(self.vword_right(p));
                }
            }
            SetBookmark(n) => {
                self.bookmarks[usize::from(n) % 10] = Some(Bookmark {
                    line: p.line,
                    col: p.col,
                    left: self.left,
                    screen_line: p.line.saturating_sub(self.top),
                });
            }
            GotoBookmark(n) => {
                if let Some(b) = self.bookmarks[usize::from(n) % 10] {
                    let line = b.line.min(self.lines.len() - 1);
                    self.move_to(Pos::new(line, b.col));
                    self.left = b.left;
                    self.top = line.saturating_sub(b.screen_line);
                }
            }
            // The window says the mark's label after the move.
            // The window says what it is now.
            Syntax => {
                self.syntax_on = !self.syntax_on;
                return Outcome::App(cmd);
            }
            NextMark | PrevMark => {
                self.goto_mark(cmd == NextMark);
                return Outcome::App(cmd);
            }
            BlockLeft => self.shift_block(false),
            BlockRight => self.shift_block(true),
            BlockCopyHere => self.copy_block_here(false),
            BlockMoveHere => self.copy_block_here(true),
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
        let row = usize::from(y - a.y);
        let line = match self.shown_rows.get(row) {
            Some(ScreenRow::Line(n)) => *n,
            // A proposal's new line: its first old line (or the line it
            // follows).
            Some(ScreenRow::Proposed(k, _)) => self.proposals.get(*k).map_or(self.top, |p| {
                p.start.saturating_sub(usize::from(p.old.is_empty()))
            }),
            _ => self.top + row,
        }
        .min(self.lines.len() - 1);
        let nw = self.number_width();
        let vx = usize::from(x.saturating_sub(a.x + nw)) + self.left;
        Some(Pos::new(line, self.col_at(line, vx)))
    }

    pub fn click(&mut self, x: u16, y: u16, shift: bool) {
        self.hold_view = false;
        let Some(p) = self.pos_at(x, y) else { return };
        if shift {
            self.select_to(p);
        } else {
            // Far: a click takes a block away unless blocks persist.
            self.stop_marking();
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
        self.mark_stream(Pos::new(p.line, s));
        self.cursor = Pos::new(p.line, e);
        self.scroll_to_cursor();
    }

    /// The wheel: the screen and the cursor by `lines` (Far: as Ctrl+Up /
    /// Ctrl+Down).
    pub fn wheel(&mut self, lines: isize) {
        self.hold_view = false;
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
        let text_w = self.text_width();
        let ws = self.settings.show_whitespace;
        let sel = self
            .highlight
            .map(|h| (h.start, h.end))
            .or_else(|| self.selection());
        let vb = self.highlight.is_none().then(|| self.vblock()).flatten();
        let rows = self.screen_rows(usize::from(area.height));
        let shown: Vec<usize> = rows
            .iter()
            .filter_map(|r| match r {
                ScreenRow::Line(n) => Some(*n),
                _ => None,
            })
            .collect();
        let syn_from = shown.first().copied().unwrap_or(self.top);
        let syn_to = shown.last().map_or(syn_from, |l| l + 1);
        let syn = self.syntax_pieces(syn_from, syn_to);
        for (row, kind) in rows.iter().enumerate() {
            let row = row as u16;
            let y = area.y + row;
            for x in area.left()..area.right() {
                buf[(x, y)].set_symbol(" ").set_style(theme::EDITOR_TEXT);
            }
            let line = match *kind {
                ScreenRow::Line(n) => n,
                ScreenRow::End => self.lines.len(),
                ScreenRow::Proposed(k, j) => {
                    // A proposal's new line: in its colour, no number.
                    let text = self
                        .proposals
                        .get(k)
                        .and_then(|p| p.new.get(j))
                        .cloned()
                        .unwrap_or_default();
                    self.draw_proposed(&text, text_x, y, text_w, buf);
                    continue;
                }
            };
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
            // The selected screen columns of this line (a vertical block:
            // its columns, past the text too).
            let vsel = vb
                .filter(|b| line >= b.top && line <= b.bottom && b.right > b.left)
                .map(|b| (b.left, b.right));
            let sel_v = vsel.or_else(|| {
                sel.and_then(|(s, e)| {
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
                })
            });
            // The agent's mark: the whole row in its colour (blinking when
            // asked), the label after the text.
            let mark = self.mark_at(line).map(|m| {
                let mut st = match m.kind {
                    crate::viewer::MarkKind::Info => theme::VIEWER_MARK_INFO,
                    crate::viewer::MarkKind::Warning => theme::VIEWER_MARK_WARNING,
                    crate::viewer::MarkKind::Error => theme::VIEWER_MARK_ERROR,
                    crate::viewer::MarkKind::Changed => theme::VIEWER_MARK_CHANGED,
                };
                let phase = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis() / 250);
                if m.flash_until.is_some_and(|t| t > Instant::now()) && phase.is_multiple_of(2) {
                    st = st.add_modifier(ratatui::style::Modifier::REVERSED);
                }
                // The label once, at the mark's first line.
                let label = if line as u64 + 1 == m.from {
                    m.label.clone()
                } else {
                    String::new()
                };
                (st, label)
            });
            if let Some((st, _)) = &mark {
                for x in text_x..text_x + text_w {
                    buf[(x, y)].set_style(*st);
                }
            }
            let pieces = line.checked_sub(syn_from).and_then(|k| syn.get(k));
            let mut v = 0usize;
            for (ci, c) in l.text.chars().enumerate() {
                let w = self.char_width(c, v);
                if v >= self.left + usize::from(text_w) {
                    break;
                }
                if v + w > self.left {
                    let selected = sel_v.is_some_and(|(a, b)| v >= a && v < b);
                    let style = if selected {
                        theme::EDITOR_SELECTED
                    } else if let Some((st, _)) = &mark {
                        *st
                    } else if self.proposal_over(line).is_some() {
                        theme::PROPOSAL_OLD
                    } else if l.by_agent {
                        theme::EDITOR_AGENT
                    } else {
                        // The token's color (syntax highlighting).
                        match pieces.and_then(|p| crate::syntax::color_at(p, ci)) {
                            Some(c) => theme::EDITOR_TEXT.fg(c),
                            None => theme::EDITOR_TEXT,
                        }
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
                                // White space shown: a tab as `→` (Far).
                                let sym = if c == '\t' && ws > 0 && k == 0 && v >= self.left {
                                    "→"
                                } else {
                                    " "
                                };
                                buf[(x, y)].set_symbol(sym).set_style(style);
                            }
                        }
                    } else if c == ' ' && ws > 0 {
                        buf[(x0, y)].set_symbol("·").set_style(style);
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
            // White space shown: the line's ending (Far's ♪ for CR, ◙ for
            // LF), and `□` after the text's last line.
            if ws > 0 {
                let mark = if line + 1 == self.lines.len() && l.eol == Eol::None {
                    "□"
                } else if ws == 1 {
                    match l.eol {
                        Eol::Cr => "♪",
                        Eol::Lf => "◙",
                        Eol::CrLf => "♪◙",
                        Eol::CrCrLf => "♪♪◙",
                        Eol::None => "",
                    }
                } else {
                    ""
                };
                for (k, ch) in mark.chars().enumerate() {
                    let vx = v + k;
                    if vx >= self.left && vx < self.left + usize::from(text_w) {
                        let x = text_x + (vx - self.left) as u16;
                        let mut b = [0u8; 4];
                        let style = if l.by_agent {
                            theme::EDITOR_AGENT
                        } else {
                            theme::EDITOR_TEXT
                        };
                        buf[(x, y)]
                            .set_symbol(ch.encode_utf8(&mut b))
                            .set_style(style);
                    }
                }
            }
            // The mark's label: a note in the margin, two columns after
            // the text (as far as the window goes).
            if let Some((st, label)) = &mark
                && !label.is_empty()
            {
                let start = (v + 2).max(self.left);
                let room = (self.left + usize::from(text_w)).saturating_sub(start);
                if room > 2 {
                    let x = text_x + (start - self.left) as u16;
                    let full = format!("◆ {label}");
                    let note: String = if full.chars().count() > room {
                        // Cut: the end shows it (F5 says it whole).
                        let mut s: String = full.chars().take(room - 1).collect();
                        s.push('…');
                        s
                    } else {
                        full
                    };
                    buf.set_stringn(x, y, &note, room, *st);
                }
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
        if self.scrollbar_shown() {
            self.draw_scrollbar(Rect::new(area.right() - 1, area.y, 1, area.height), buf);
        }
        let v = self.vcol(self.cursor.line, self.cursor.col);
        let row = rows
            .iter()
            .position(|r| *r == ScreenRow::Line(self.cursor.line));
        self.shown_rows = rows;
        let row = row?;
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

/// The parse state is kept before every this many lines.
const SYN_STEP: usize = 64;
/// A place this many lines past the parsed ones is parsed afresh from a
/// little above it rather than from the last known state (a jump to the
/// end of a huge file stays quick; colors there may be off at first).
const SYN_REACH: usize = 20_000;
const SYN_LOOKBACK: usize = 200;

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
