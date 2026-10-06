//! The viewer (F3), Far's way (viewer.cpp, fileview.cpp; docs/12): the top
//! of the screen is a byte offset in the file, rows are found by scanning
//! from it, so a file of any size opens at once. Text, hex and dump modes,
//! code pages with detection, wrapping, Far's status line.

pub mod codepage;
pub mod layout;
pub mod positions;
pub mod search;
pub mod source;

use std::path::{Path, PathBuf};
use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde::{Deserialize, Serialize};

use crate::command::ViewerCmd;
use crate::theme;
use codepage::Codec;
use layout::{Row, TextOpts, Wrap};
use source::Source;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Mode {
    #[default]
    Text,
    Hex,
    Dump,
}

impl Mode {
    fn letter(self) -> char {
        match self {
            Mode::Text => 't',
            Mode::Hex => 'h',
            Mode::Dump => 'd',
        }
    }
}

/// Far's viewer settings (config.cpp defaults).
pub const TAB_SIZE: usize = 8;
pub const MAX_LINE: usize = 10_000;
const SHOW_ARROWS: bool = true;
/// Far's `DetectDumpMode`: a zero byte in the first 2 KB opens the dump.
const BINARY_PROBE: usize = 2048;
/// Bytes read for code page detection (Far: 32 KB).
const DETECT_PROBE: usize = 32 * 1024;
const MIN_BYTES_PER_LINE: usize = 8;

/// Glyphs of the C0 controls in the OEM font (console.cpp).
const C0_GLYPHS: [char; 32] = [
    ' ', '☺', '☻', '♥', '♦', '♣', '♠', '•', '◘', '○', '◙', '♂', '♀', '♪', '♫', '☼', '►', '◄', '↕',
    '‼', '¶', '§', '▬', '↨', '↑', '↓', '→', '←', '∟', '↔', '▲', '▼',
];

/// How a character is shown.
fn glyph(ch: char) -> char {
    match ch {
        '\0'..='\u{1F}' => C0_GLYPHS[ch as usize],
        '\u{7F}' => '⌂',
        '\u{80}'..='\u{9F}' => '\u{FFFD}',
        '\u{AD}' => '-',
        // A zero-width character with nothing to join (start of a row).
        _ if layout::display_width(ch) == 0 => ' ',
        _ => ch,
    }
}

/// What the viewer remembers about a file between openings (Far's
/// position cache): position, code page, mode, bookmarks.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Remembered {
    pub top: u64,
    pub left: usize,
    pub cp: u32,
    /// Saved only once the user changed it (Far's `touched`).
    #[serde(default)]
    pub mode: Option<Mode>,
    #[serde(default)]
    pub bookmarks: Vec<Option<(u64, usize)>>,
}

/// State kept from one viewer to the next (Far's `KeepInitParameters`).
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Defaults {
    pub wrap: bool,
    pub word_wrap: bool,
    pub scrollbar: bool,
    pub status_line: bool,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            wrap: true,
            word_wrap: false,
            scrollbar: false,
            status_line: true,
        }
    }
}

/// What a command needs from the application.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Done,
    /// Not handled here: the application does it (dialogs, menus, files).
    App(ViewerCmd),
}

pub struct Viewer {
    pub id: u32,
    src: Source,
    codec: Codec,
    pub mode: Mode,
    /// Where F4 returns from hex (Far's `m_DumpTextMode`).
    base: Mode,
    mode_touched: bool,
    pub wrap: bool,
    pub word_wrap: bool,
    pub scrollbar: bool,
    pub status_line: bool,
    /// Byte offset of the first row.
    pub top: u64,
    /// First column shown (text without wrapping, hex).
    pub left: usize,
    bytes_per_line: usize,
    undo: Vec<(u64, usize)>,
    pub bookmarks: [Option<(u64, usize)>; 10],
    /// Files of the panel for Gray+ / Gray- (Far's `ViewList`).
    pub list: Vec<PathBuf>,
    /// Found text: a byte range.
    pub selection: Option<(u64, u64)>,
    pub opened: Instant,
    /// The text area of the last frame.
    area: Rect,
    /// The last frame showed the end of the file.
    last_page: bool,
    /// Row starts of the last frame (text mode).
    rows: Vec<Row>,
}

impl Viewer {
    /// Opens `path`; `remembered` restores the last position.
    pub fn open(
        id: u32,
        path: &Path,
        defaults: &Defaults,
        remembered: Option<&Remembered>,
    ) -> std::io::Result<Self> {
        let mut src = Source::open(path)?;
        let head = src.read_vec(0, DETECT_PROBE);
        let whole = head.len() as u64 == src.size();
        let cp = remembered
            .map(|r| r.cp)
            .filter(|cp| Codec::new(*cp).is_some())
            .or_else(|| codepage::detect(&head, whole))
            .unwrap_or_else(codepage::ansi);
        let codec = Codec::new(cp)
            .or_else(|| Codec::new(codepage::ansi()))
            .or_else(|| Codec::new(codepage::UTF8))
            .expect("UTF-8 is always supported");
        let binary = is_binary(&head[..head.len().min(BINARY_PROBE)], codec.unit());
        let restored_mode = remembered.and_then(|r| r.mode);
        let mode = restored_mode.unwrap_or(if binary { Mode::Dump } else { Mode::Text });
        let mut v = Self {
            id,
            src,
            codec,
            mode,
            base: if mode == Mode::Dump {
                Mode::Dump
            } else {
                Mode::Text
            },
            mode_touched: restored_mode.is_some(),
            wrap: defaults.wrap,
            word_wrap: defaults.word_wrap,
            scrollbar: defaults.scrollbar,
            status_line: defaults.status_line,
            top: 0,
            left: 0,
            bytes_per_line: 16,
            undo: Vec::new(),
            bookmarks: [None; 10],
            list: Vec::new(),
            selection: None,
            opened: Instant::now(),
            area: Rect::default(),
            last_page: false,
            rows: Vec::new(),
        };
        if let Some(r) = remembered {
            v.top = r.top.min(v.src.size());
            v.left = r.left;
            for (i, b) in r.bookmarks.iter().take(10).enumerate() {
                v.bookmarks[i] = *b;
            }
            v.adjust_top();
        }
        Ok(v)
    }

    pub fn path(&self) -> &Path {
        self.src.path()
    }

    pub fn size(&self) -> u64 {
        self.src.size()
    }

    pub fn codepage(&self) -> u32 {
        self.codec.cp()
    }

    pub fn remembered(&self) -> Remembered {
        Remembered {
            top: self.top,
            left: self.left,
            cp: self.codec.cp(),
            mode: self.mode_touched.then_some(self.mode),
            bookmarks: self.bookmarks.to_vec(),
        }
    }

    pub fn defaults(&self) -> Defaults {
        Defaults {
            wrap: self.wrap,
            word_wrap: self.word_wrap,
            scrollbar: self.scrollbar,
            status_line: self.status_line,
        }
    }

    // ------------------------------------------------------------ geometry

    fn height(&self) -> usize {
        usize::from(self.area.height.max(1))
    }

    fn width(&self) -> usize {
        usize::from(self.area.width.max(1))
    }

    /// The text area inside `area`: below the status line, left of the
    /// scroll bar.
    fn text_area(&self, area: Rect) -> Rect {
        let status = u16::from(self.status_line && area.height > 1);
        let bar = u16::from(self.scrollbar && area.width > 1);
        Rect::new(
            area.x,
            area.y + status,
            area.width - bar,
            area.height - status,
        )
    }

    fn opts(&self) -> TextOpts {
        let w = self.width();
        TextOpts {
            tab: TAB_SIZE,
            max_line: MAX_LINE,
            wrap: match (self.wrap, self.word_wrap) {
                (false, _) => Wrap::None,
                (true, false) => Wrap::Chars(w),
                (true, true) => Wrap::Words(w),
            },
        }
    }

    /// Bytes per row in hex and dump.
    fn row_bytes(&self) -> u64 {
        match self.mode {
            Mode::Hex => self.bytes_per_line as u64,
            _ => (self.width() * self.codec.unit()) as u64,
        }
    }

    // ---------------------------------------------------------- movement

    /// Far's `AdjustFilePos`: the top at the start of its row (character
    /// in hex and dump).
    fn adjust_top(&mut self) {
        let u = self.codec.unit() as u64;
        self.top -= self.top % u;
        if self.mode == Mode::Text {
            let opts = self.opts();
            self.top = layout::row_start(&mut self.src, &self.codec, self.top, &opts);
        }
    }

    fn end_top(&mut self) -> u64 {
        let h = self.height();
        match self.mode {
            Mode::Text => {
                let opts = self.opts();
                layout::end_top(&mut self.src, &self.codec, h, &opts)
            }
            _ => {
                let rb = self.row_bytes();
                let size = self.src.size();
                // Rows keep the alignment of the top.
                let phase = self.top % rb;
                if size <= phase {
                    return 0;
                }
                let last = phase + (size - 1 - phase) / rb * rb;
                last.saturating_sub((h as u64 - 1) * rb)
            }
        }
    }

    fn next_row(&mut self, pos: u64) -> u64 {
        match self.mode {
            Mode::Text => {
                let opts = self.opts();
                layout::read_row(&mut self.src, &self.codec, pos, &opts).end
            }
            _ => pos + self.row_bytes(),
        }
    }

    fn prev_row(&mut self, pos: u64) -> Option<u64> {
        if pos == 0 {
            return None;
        }
        match self.mode {
            Mode::Text => {
                let opts = self.opts();
                layout::prev_row(&mut self.src, &self.codec, pos, &opts)
            }
            _ => Some(pos.saturating_sub(self.row_bytes())),
        }
    }

    /// Whether the screen shows the end of the file.
    fn at_last_page(&mut self) -> bool {
        let mut p = self.top;
        for _ in 0..self.height() {
            if p >= self.src.size() {
                return true;
            }
            p = self.next_row(p);
        }
        p >= self.src.size()
    }

    fn down(&mut self, rows: usize) {
        for _ in 0..rows {
            if self.at_last_page() {
                break;
            }
            self.top = self.next_row(self.top);
        }
    }

    fn up(&mut self, rows: usize) {
        for _ in 0..rows {
            match self.prev_row(self.top) {
                Some(p) => self.top = p,
                None => break,
            }
        }
    }

    fn page_down(&mut self) {
        if self.at_last_page() {
            return;
        }
        // The last row becomes the first.
        let mut p = self.top;
        for _ in 0..self.height().saturating_sub(1).max(1) {
            p = self.next_row(p);
        }
        let end = self.end_top();
        self.top = p.min(end.max(self.top));
    }

    fn go_end(&mut self) {
        self.top = self.end_top();
    }

    /// Remembers the position for Alt+BS (Far's undo stack).
    fn push_undo(&mut self) {
        let here = (self.top, self.left);
        if self.undo.last() != Some(&here) {
            self.undo.push(here);
            if self.undo.len() > 65_536 {
                self.undo.remove(0);
            }
        }
    }

    /// Shows `pos` (a byte offset): the row holding it a quarter of the
    /// screen from the top, unless it is already visible (Far's search).
    pub fn show_pos(&mut self, pos: u64, len: u64) {
        self.push_undo();
        let visible = pos >= self.top && pos < self.visible_end();
        if !visible {
            self.top = pos.min(self.src.size());
            self.adjust_top();
            self.up(self.height() / 4);
        }
        if self.mode == Mode::Text && !self.wrap {
            self.make_column_visible(pos, len);
        }
    }

    /// Line and column (both from 0) of a byte offset; lines end with LF.
    /// Scans from the start of the file (the line index comes in stage 2).
    pub fn line_col(&mut self, pos: u64) -> (u64, u64) {
        let lf = self.codec.encode("\n");
        let u = lf.len().max(1) as u64;
        let pos = pos.min(self.src.size());
        let (mut line, mut line_start, mut p) = (0u64, 0u64, 0u64);
        while p < pos {
            let len = (pos - p).min(1 << 20) as usize;
            let chunk = self.src.read_vec(p, len);
            if chunk.is_empty() {
                break;
            }
            let mut i = 0;
            while i + lf.len() <= chunk.len() {
                if chunk[i..i + lf.len()] == lf[..] {
                    line += 1;
                    line_start = p + i as u64 + u;
                }
                i += u as usize;
            }
            p += chunk.len() as u64;
        }
        let bytes = self
            .src
            .read_vec(line_start, (pos - line_start).min(1 << 20) as usize);
        let (mut col, mut i) = (0u64, 0usize);
        while i < bytes.len() {
            let (_, n) = self.codec.decode(&bytes[i..]);
            col += 1;
            i += n.max(1);
        }
        (line, col)
    }

    /// The text from `from` to `to` (bytes), decoded.
    pub fn text_between(&mut self, from: u64, to: u64) -> String {
        let bytes = self
            .src
            .read_vec(from, to.saturating_sub(from).min(1 << 20) as usize);
        let mut out = String::new();
        let mut i = 0;
        while i < bytes.len() {
            let (ch, n) = self.codec.decode(&bytes[i..]);
            out.push(ch);
            i += n.max(1);
        }
        out
    }

    /// Size of a code unit (2 in UTF-16).
    pub fn unit(&self) -> u64 {
        self.codec.unit() as u64
    }

    /// Byte offset after the last shown row.
    pub fn visible_end(&mut self) -> u64 {
        let mut p = self.top;
        for _ in 0..self.height() {
            p = self.next_row(p);
        }
        p
    }

    /// Scrolls horizontally so the bytes `pos..pos+len` are seen with a
    /// margin of 10 columns.
    fn make_column_visible(&mut self, pos: u64, len: u64) {
        let opts = self.opts();
        let start = layout::row_start(&mut self.src, &self.codec, pos, &opts);
        let row = layout::read_row(&mut self.src, &self.codec, start, &opts);
        let col_of = |p: u64| {
            row.cells
                .iter()
                .find(|c| c.pos >= p)
                .map_or(row.cols, |c| c.col)
        };
        let (from, to) = (col_of(pos), col_of(pos + len.max(1)));
        let w = self.width();
        if from < self.left || to > self.left + w {
            self.left = from.saturating_sub(10.min(w / 2));
        }
    }

    /// Goes to a byte offset (Alt+F8): to the start of its row.
    pub fn goto(&mut self, pos: u64, left: Option<usize>) {
        self.push_undo();
        self.top = pos.min(self.src.size());
        self.adjust_top();
        if let Some(l) = left {
            self.left = l;
        }
    }

    pub fn set_codepage(&mut self, cp: u32) -> bool {
        let Some(codec) = Codec::new(cp) else {
            return false;
        };
        self.codec = codec;
        self.adjust_top();
        true
    }

    /// Detects the code page again (Shift+F8, "Automatic detection").
    pub fn detect_codepage(&mut self) -> u32 {
        let head = self.src.read_vec(0, DETECT_PROBE);
        let whole = head.len() as u64 == self.src.size();
        codepage::detect(&head, whole).unwrap_or_else(codepage::ansi)
    }

    pub fn set_mode(&mut self, mode: Mode) {
        if mode != Mode::Hex {
            self.base = mode;
        }
        self.mode = mode;
        self.mode_touched = true;
        self.left = 0;
        self.adjust_top();
    }

    /// The text of the selection in the current code page (Ctrl+C).
    pub fn selected_text(&mut self) -> Option<String> {
        let (from, to) = self.selection?;
        let bytes = self.src.read_vec(from, (to - from).min(16 << 20) as usize);
        let mut out = String::new();
        let mut i = 0;
        while i < bytes.len() {
            let (ch, n) = self.codec.decode(&bytes[i..]);
            out.push(ch);
            i += n.max(1);
        }
        Some(out)
    }

    /// Checks the file for growth (Far's reload timer). A file that grew
    /// while its end was shown keeps the end in view (`tail -f`).
    pub fn check_changed(&mut self) -> bool {
        let was_last = self.last_page;
        if !self.src.refresh() {
            return false;
        }
        let size = self.src.size();
        if self.top > size || was_last {
            self.go_end();
        }
        true
    }

    /// Runs a viewer command; those needing dialogs or the panels go back
    /// to the application.
    pub fn command(&mut self, cmd: ViewerCmd) -> Outcome {
        use ViewerCmd::*;
        let text = self.mode == Mode::Text;
        match cmd {
            Up => self.up(1),
            Down => self.down(1),
            PageUp => self.up(self.height().saturating_sub(1).max(1)),
            PageDown => self.page_down(),
            Left if text && !self.wrap || self.mode == Mode::Hex => {
                self.left = self.left.saturating_sub(1)
            }
            Right if text && !self.wrap => self.left = (self.left + 1).min(MAX_LINE),
            Right if self.mode == Mode::Hex => {
                let max = hex_line_width(self.bytes_per_line, self.codec.unit())
                    .saturating_sub(self.width());
                self.left = (self.left + 1).min(max);
            }
            Left | Right => {}
            LeftMore if text => {
                if !self.wrap {
                    self.left = self.left.saturating_sub(20);
                }
            }
            RightMore if text => {
                if !self.wrap {
                    self.left = (self.left + 20).min(MAX_LINE);
                }
            }
            // Hex and dump: the content rolls by one character.
            LeftMore => self.top = self.top.saturating_sub(self.codec.unit() as u64),
            RightMore => {
                let next = self.top + self.codec.unit() as u64;
                if next < self.src.size() {
                    self.top = next;
                }
            }
            LeftStart => self.left = 0,
            RightEnd if text && !self.wrap => {
                let longest = self.rows.iter().map(|r| r.cols).max().unwrap_or(0);
                self.left = longest.saturating_sub(self.width());
            }
            RightEnd => {}
            Home => {
                self.push_undo();
                self.top = 0;
                self.left = 0;
            }
            StartKeepLeft => {
                self.push_undo();
                self.top = 0;
            }
            End => {
                self.push_undo();
                self.go_end();
                self.left = 0;
            }
            EndKeepLeft => {
                self.push_undo();
                self.go_end();
            }
            BytesLess | BytesMore | BytesLess16 | BytesMore16 if self.mode == Mode::Hex => {
                let b = self.bytes_per_line;
                self.bytes_per_line = match cmd {
                    BytesLess => b - 1,
                    BytesMore => b + 1,
                    BytesLess16 => (b - 1) / 16 * 16,
                    _ => (b / 16 + 1) * 16,
                }
                .clamp(MIN_BYTES_PER_LINE, 1024);
            }
            BytesLess | BytesMore | BytesLess16 | BytesMore16 => {}
            Wrap => match self.mode {
                Mode::Text => {
                    self.wrap = !self.wrap;
                    self.left = 0;
                    self.adjust_top();
                }
                Mode::Dump => self.set_mode(Mode::Text),
                // Hex: to the other of text and dump.
                Mode::Hex => self.set_mode(if self.base == Mode::Text {
                    Mode::Dump
                } else {
                    Mode::Text
                }),
            },
            WordWrap if text => {
                if self.wrap {
                    self.word_wrap = !self.word_wrap;
                } else {
                    self.wrap = true;
                    self.word_wrap = true;
                }
                self.left = 0;
                self.adjust_top();
            }
            WordWrap => {}
            Hex => {
                let to = if self.mode == Mode::Hex {
                    self.base
                } else {
                    Mode::Hex
                };
                self.set_mode(to);
            }
            Undo => {
                if let Some((top, left)) = self.undo.pop() {
                    self.top = top.min(self.src.size());
                    self.left = left;
                }
            }
            GotoBookmark(n) => {
                if let Some((top, left)) = self.bookmarks[usize::from(n)] {
                    self.push_undo();
                    self.top = top.min(self.src.size());
                    self.left = left;
                    self.adjust_top();
                }
            }
            SetBookmark(n) => self.bookmarks[usize::from(n)] = Some((self.top, self.left)),
            Unselect => self.selection = None,
            Scrollbar => self.scrollbar = !self.scrollbar,
            StatusLine => self.status_line = !self.status_line,
            other => return Outcome::App(other),
        }
        Outcome::Done
    }

    /// Mouse wheel: rows down (positive) or up.
    pub fn scroll(&mut self, rows: i32) {
        if rows > 0 {
            self.down(rows as usize);
        } else {
            self.up(rows.unsigned_abs() as usize);
        }
    }

    // -------------------------------------------------------------- draw

    /// Draws the viewer into `area`; `clock` reserves its place at the
    /// right of the status line.
    pub fn draw(&mut self, area: Rect, buf: &mut Buffer, clock: u16) {
        let text_area = self.text_area(area);
        let resized = text_area.width != self.area.width;
        self.area = text_area;
        if resized && self.mode == Mode::Text && self.wrap {
            self.adjust_top();
        }
        buf.set_style(area, theme::VIEWER_TEXT);
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                buf[(x, y)].set_symbol(" ");
            }
        }
        if self.top > self.src.size() {
            self.go_end();
        }
        match self.mode {
            Mode::Text => self.draw_text(buf),
            Mode::Hex => self.draw_hex(buf),
            Mode::Dump => self.draw_dump(buf),
        }
        self.last_page = self.at_last_page();
        if self.scrollbar && area.width > 1 {
            let bar = Rect::new(area.right() - 1, text_area.y, 1, text_area.height);
            self.draw_scrollbar(bar, buf);
        }
        if self.status_line && area.height > 1 {
            self.draw_status(Rect::new(area.x, area.y, area.width, 1), buf, clock);
        }
    }

    fn selected(&self, pos: u64) -> bool {
        self.selection
            .is_some_and(|(from, to)| pos >= from && pos < to)
    }

    fn draw_text(&mut self, buf: &mut Buffer) {
        let a = self.area;
        let opts = self.opts();
        let (left, width) = if self.wrap {
            (0, self.width())
        } else {
            (self.left, self.width())
        };
        self.rows.clear();
        let mut p = self.top;
        for y in 0..a.height {
            if p >= self.src.size() {
                break;
            }
            let row = layout::read_row(&mut self.src, &self.codec, p, &opts);
            p = row.end;
            let ry = a.y + y;
            for cell in &row.cells {
                let cw = usize::from(cell.width);
                if cell.col + cw <= left || cell.col >= left + width {
                    continue;
                }
                let style = if self.selected(cell.pos) {
                    theme::VIEWER_SELECTED
                } else {
                    theme::VIEWER_TEXT
                };
                let x0 = cell.col.max(left) - left;
                // A character cut by an edge, and tabs: blanks.
                let whole = cell.col >= left && cell.col + cw <= left + width;
                if cell.ch == '\t' || !whole {
                    let end = (cell.col + cw).min(left + width) - left;
                    for x in x0..end {
                        buf[(a.x + x as u16, ry)].set_symbol(" ").set_style(style);
                    }
                    continue;
                }
                let x = a.x + x0 as u16;
                if cell.combined {
                    let bytes = self.src.read_vec(cell.pos, cell.len as usize);
                    let mut s = String::new();
                    let mut i = 0;
                    while i < bytes.len() {
                        let (c, n) = self.codec.decode(&bytes[i..]);
                        // The base character as shown, the marks as they are.
                        s.push(if i == 0 { glyph(c) } else { c });
                        i += n.max(1);
                    }
                    buf.set_stringn(x, ry, &s, cw, style);
                } else {
                    let mut tmp = [0u8; 4];
                    buf.set_stringn(x, ry, glyph(cell.ch).encode_utf8(&mut tmp), cw, style);
                }
            }
            if SHOW_ARROWS && !self.wrap {
                if left > 0 && row.cols > 0 {
                    buf[(a.x, ry)]
                        .set_symbol("«")
                        .set_style(theme::VIEWER_ARROWS);
                }
                if row.cols > left + width {
                    buf[(a.right() - 1, ry)]
                        .set_symbol("»")
                        .set_style(theme::VIEWER_ARROWS);
                }
            }
            self.rows.push(row);
        }
    }

    /// Characters of `bytes` (starting at file offset `pos`) for the hex
    /// and dump modes: one column per code unit, a character at its first
    /// unit and `›` over the rest.
    fn unit_chars(&mut self, pos: u64, len: usize) -> Vec<(char, u64)> {
        let u = self.codec.unit();
        let bytes = self.src.read_vec(pos, len + 4);
        let mut out = Vec::new();
        let mut i = 0;
        while i < len.min(bytes.len()) {
            let (ch, n) = self.codec.decode(&bytes[i..]);
            let n = n.max(1);
            let units = n.div_ceil(u);
            let mut c = glyph(ch);
            if layout::display_width(c) != 1 {
                c = if units > 1 && layout::display_width(c) == 2 {
                    c
                } else {
                    '?'
                };
            }
            out.push((c, pos + i as u64));
            for k in 1..units {
                let wide = k == 1 && layout::display_width(c) == 2;
                out.push((if wide { '\0' } else { '›' }, pos + (i + k * u) as u64));
            }
            i += units * u;
        }
        out
    }

    fn draw_dump(&mut self, buf: &mut Buffer) {
        let a = self.area;
        let rb = self.row_bytes();
        let size = self.src.size();
        for y in 0..a.height {
            let pos = self.top + u64::from(y) * rb;
            if pos >= size {
                break;
            }
            let len = (size - pos).min(rb) as usize;
            let chars = self.unit_chars(pos, len);
            for (x, (ch, at)) in chars.into_iter().enumerate().take(usize::from(a.width)) {
                let style = if self.selected(at) {
                    theme::VIEWER_SELECTED
                } else {
                    theme::VIEWER_TEXT
                };
                let cell = &mut buf[(a.x + x as u16, a.y + y)];
                if ch == '\0' {
                    // The second half of a wide character.
                    cell.set_style(style);
                    continue;
                }
                let mut tmp = [0u8; 4];
                if layout::display_width(ch) == 2 && (x as u16) + 1 < a.width {
                    buf.set_stringn(a.x + x as u16, a.y + y, ch.encode_utf8(&mut tmp), 2, style);
                } else {
                    cell.set_symbol(ch.encode_utf8(&mut tmp)).set_style(style);
                }
            }
        }
    }

    fn draw_hex(&mut self, buf: &mut Buffer) {
        let a = self.area;
        let bpl = self.bytes_per_line;
        let size = self.src.size();
        for y in 0..a.height {
            let pos = self.top + u64::from(y) * bpl as u64;
            if pos >= size {
                break;
            }
            let bytes = self.src.read_vec(pos, bpl);
            // (text, selected) pieces of the line.
            let mut line: Vec<(String, bool)> = vec![(format!("{pos:010X}:"), false)];
            for i in 0..bpl {
                if i > 0 && i % 8 == 0 {
                    line.push((" │".into(), false));
                }
                match bytes.get(i) {
                    Some(b) => {
                        line.push((" ".into(), false));
                        line.push((format!("{b:02X}"), self.selected(pos + i as u64)));
                    }
                    None => line.push(("   ".into(), false)),
                }
            }
            line.push(("  ".into(), false));
            let chars = self.unit_chars(pos, bytes.len());
            for (ch, _) in chars {
                if ch != '\0' {
                    line.push((ch.to_string(), false));
                }
            }
            // Horizontal scroll by `left` columns.
            let mut skip = self.left;
            let mut x = a.x;
            for (piece, sel) in line {
                let style = if sel {
                    theme::VIEWER_SELECTED
                } else {
                    theme::VIEWER_TEXT
                };
                for ch in piece.chars() {
                    if skip > 0 {
                        skip -= 1;
                        continue;
                    }
                    if x >= a.right() {
                        break;
                    }
                    let w = layout::display_width(ch) as u16;
                    if x + w > a.right() {
                        break;
                    }
                    let mut tmp = [0u8; 4];
                    buf.set_stringn(x, a.y + y, ch.encode_utf8(&mut tmp), w.into(), style);
                    x += w;
                }
            }
        }
    }

    fn draw_scrollbar(&mut self, bar: Rect, buf: &mut Buffer) {
        let h = bar.height;
        let style = theme::VIEWER_SCROLLBAR;
        if h < 2 {
            return;
        }
        buf[(bar.x, bar.y)].set_symbol("▲").set_style(style);
        buf[(bar.x, bar.bottom() - 1)]
            .set_symbol("▼")
            .set_style(style);
        let field = h.saturating_sub(2);
        if field == 0 {
            return;
        }
        let size = self.src.size().max(1);
        let end = self.visible_end().min(size);
        let shown = end.saturating_sub(self.top).max(1);
        let thumb = ((u64::from(field) * shown).div_ceil(size)).clamp(1, u64::from(field)) as u16;
        let start = if self.last_page {
            field - thumb
        } else {
            ((u64::from(field) * self.top / size) as u16).min(field - thumb)
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

    /// Far's status line (`FileViewer::ShowStatus`).
    fn draw_status(&mut self, line: Rect, buf: &mut Buffer, clock: u16) {
        let size = self.src.size();
        let percent = if self.last_page {
            100
        } else if size == 0 {
            0
        } else {
            (u128::from(self.top) * 100 / u128::from(size)) as u64
        };
        let col = crate::tr!("MViewerStatusCol");
        let col: String = col.chars().take(3).collect();
        let mut status = format!(
            "│{}│{:5.5}│{:<10}│{} {:<3}│{:4}",
            self.mode.letter(),
            self.codec.short_name(),
            size,
            col,
            self.left,
            format!("{percent}%"),
        );
        let width = usize::from(line.width);
        let clock = usize::from(clock);
        let available = width.saturating_sub(clock + usize::from(clock > 0));
        if status.chars().count() > available {
            status = status.chars().take(available).collect();
        }
        let name_w = available - status.chars().count();
        let name = crate::panel::truncate_path(&self.path().display().to_string(), name_w);
        let mut text = format!("{name:<name_w$}{status}");
        if clock > 0 {
            text.push('│');
        }
        buf.set_style(line, theme::VIEWER_STATUS);
        buf.set_stringn(line.x, line.y, &text, width, theme::VIEWER_STATUS);
    }

    /// The key bar labels F1..F12 for the current state (Far's
    /// `UpdateViewKeyBar`); `group` is the modifiers held (`""`, `"Shift"`,
    /// `"CtrlAlt"`, …), as in Far's message ids.
    pub fn keybar_labels(&self, next_cp: u32, group: &str) -> [String; 12] {
        let t = |id: &str| crate::i18n::plain(&crate::tr!(id));
        if !group.is_empty() {
            let mut labels: [String; 12] =
                std::array::from_fn(|i| t(&format!("MView{group}F{}", i + 1)));
            if group == "Shift" {
                labels[1] = match self.mode {
                    Mode::Text if self.word_wrap => t("MViewF2"),
                    Mode::Text => t("MViewShiftF2"),
                    _ => String::new(),
                };
            }
            return labels;
        }
        let f2 = match self.mode {
            Mode::Text if self.wrap => t("MViewF2Unwrap"),
            Mode::Text if self.word_wrap => t("MViewShiftF2"),
            Mode::Text => t("MViewF2"),
            Mode::Hex | Mode::Dump if self.base == Mode::Dump && self.mode == Mode::Hex => {
                t("MViewF4Text")
            }
            Mode::Hex => t("MViewF4Dump"),
            Mode::Dump => t("MViewF4Text"),
        };
        let f4 = match self.mode {
            Mode::Hex if self.base == Mode::Dump => t("MViewF4Dump"),
            Mode::Hex => t("MViewF4Text"),
            _ => t("MViewF4"),
        };
        let f8 = if next_cp == codepage::ansi() {
            t("MViewF8")
        } else if next_cp == codepage::oem() {
            t("MViewF8DOS")
        } else {
            codepage::short_name(next_cp)
        };
        [
            t("MViewF1"),
            f2,
            t("MViewF3"),
            f4,
            String::new(),
            t("MViewF6"),
            t("MViewF7"),
            f8,
            String::new(),
            t("MViewF10"),
            t("MViewF11"),
            t("MViewF12"),
        ]
    }

    /// The F8 order: ANSI ↔ OEM; from a page not in the list — ANSI
    /// (Far's default `F8CPs`).
    pub fn next_f8_codepage(&self) -> u32 {
        let (ansi, oem) = (codepage::ansi(), codepage::oem());
        if self.codec.cp() == ansi { oem } else { ansi }
    }
}

/// Far's `isBinaryFile`: a zero byte (a zero unit in UTF-16).
fn is_binary(head: &[u8], unit: usize) -> bool {
    if unit == 2 {
        head.as_chunks::<2>().0.contains(&[0, 0])
    } else {
        head.contains(&0)
    }
}

/// Columns of a hex line with `bytes` per line (address, bytes with a
/// separator every 8, two blanks, the characters).
fn hex_line_width(bytes: usize, unit: usize) -> usize {
    11 + 3 * bytes + 2 * ((bytes - 1) / 8) + 2 + bytes / unit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_line_is_79_wide() {
        assert_eq!(hex_line_width(16, 1), 79);
    }

    #[test]
    fn binary_files() {
        assert!(is_binary(b"MZ\0\0", 1));
        assert!(!is_binary(b"text", 1));
        let utf16: Vec<u8> = "text".encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert!(!is_binary(&utf16, 2));
    }
}

/// One element of Far's go-to input (`GetRowCol`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GotoValue {
    pub value: u64,
    /// `+` or `-`: relative to the current place.
    pub sign: Option<char>,
    pub percent: bool,
}

impl GotoValue {
    /// The absolute value from the current one and the whole (for `%`).
    pub fn resolve(self, current: u64, whole: u64) -> u64 {
        let v = if self.percent {
            (u128::from(whole) * u128::from(self.value.min(100)) / 100) as u64
        } else {
            self.value
        };
        match self.sign {
            Some('+') => current.saturating_add(v),
            Some('-') => current.saturating_sub(v),
            _ => v,
        }
    }
}

/// Far's go-to input: `[+|-]value[%]`, then optionally a separator
/// (` .,;:`) and the column. Hex with `0x`, `$`, a trailing `h` or the
/// Hex check box; a trailing `m` means decimal. An empty element keeps
/// that coordinate. `None`: not a valid input.
pub fn parse_goto(text: &str, hex: bool) -> Option<(Option<GotoValue>, Option<GotoValue>)> {
    let text = text.trim();
    let (first, second) = match text.find([' ', '.', ',', ';', ':']) {
        Some(i) => (&text[..i], Some(&text[i + 1..])),
        None => (text, None),
    };
    let element = |s: &str| -> Option<Option<GotoValue>> {
        let mut s = s.trim();
        if s.is_empty() {
            return Some(None);
        }
        let sign = s.chars().next().filter(|c| *c == '+' || *c == '-');
        if sign.is_some() {
            s = &s[1..];
        }
        let percent = s.ends_with('%');
        if percent {
            s = &s[..s.len() - 1];
        }
        let lower = s.to_ascii_lowercase();
        let (digits, radix) = if let Some(d) = lower.strip_prefix("0x").or(lower.strip_prefix('$'))
        {
            (d.to_string(), 16)
        } else if let Some(d) = lower.strip_suffix('h') {
            (d.to_string(), 16)
        } else if let Some(d) = lower.strip_suffix('m') {
            (d.to_string(), 10)
        } else {
            // A percentage is decimal even with the Hex box.
            (lower.clone(), if hex && !percent { 16 } else { 10 })
        };
        let value = u64::from_str_radix(&digits, radix).ok()?;
        Some(Some(GotoValue {
            value,
            sign,
            percent,
        }))
    };
    let row = element(first)?;
    let col = match second {
        Some(s) => element(s)?,
        None => None,
    };
    if row.is_none() && col.is_none() {
        return None;
    }
    Some((row, col))
}

#[cfg(test)]
mod goto_tests {
    use super::*;

    #[test]
    fn parses_far_goto() {
        let v = |value, sign, percent| {
            Some(GotoValue {
                value,
                sign,
                percent,
            })
        };
        assert_eq!(
            parse_goto("1000", false),
            Some((v(1000, None, false), None))
        );
        assert_eq!(parse_goto("50%", false), Some((v(50, None, true), None)));
        assert_eq!(
            parse_goto("+0x10", false),
            Some((v(16, Some('+'), false), None))
        );
        assert_eq!(parse_goto("ffh", false), Some((v(255, None, false), None)));
        assert_eq!(parse_goto("10", true), Some((v(16, None, false), None)));
        assert_eq!(parse_goto("10m", true), Some((v(10, None, false), None)));
        assert_eq!(
            parse_goto("100 20", false),
            Some((v(100, None, false), v(20, None, false)))
        );
        assert_eq!(parse_goto(",5", false), Some((None, v(5, None, false))));
        assert_eq!(parse_goto("zz", false), None);
        assert_eq!(v(50, None, true).unwrap().resolve(0, 1000), 500);
        assert_eq!(v(10, Some('-'), false).unwrap().resolve(5, 0), 0);
    }
}
