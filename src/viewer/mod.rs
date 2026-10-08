//! The viewer (F3), Far's way (viewer.cpp, fileview.cpp; docs/12): the top
//! of the screen is a byte offset in the file, rows are found by scanning
//! from it, so a file of any size opens at once. Text, hex and dump modes,
//! code pages with detection, wrapping, Far's status line.

pub mod codepage;
pub mod highlight;
pub mod layout;
pub mod lines;
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
    /// Wrapping and word wrapping.
    #[serde(default)]
    pub wrap: Option<(bool, bool)>,
}

impl Remembered {
    /// What the settings let come back (Far's "Save file position", …).
    pub fn filtered(mut self, s: &crate::config::Viewer) -> Self {
        if !s.save_position {
            self.top = 0;
            self.left = 0;
        }
        if !s.save_position && !s.save_codepage {
            self.cp = 0;
        }
        if !s.save_bookmarks {
            self.bookmarks.clear();
        }
        if !s.save_mode {
            self.mode = None;
        }
        if !s.save_wrap {
            self.wrap = None;
        }
        self
    }
}

/// The settings a viewer works with (from `[viewer]`).
#[derive(Clone, Copy, Debug)]
struct Opts {
    tab: usize,
    max_line: usize,
    arrows: bool,
    zero: bool,
    persistent: bool,
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

/// What a mark on the viewer's lines is (the agent's pointers, the
/// changes of the file).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkKind {
    Info,
    Warning,
    Error,
    /// Lines that changed on the disk while the file was open.
    Changed,
}

/// Lines marked in the viewer (docs/11, "Агент открывает и показывает"):
/// lines from 1, inclusive.
#[derive(Clone, Debug)]
pub struct Mark {
    pub from: u64,
    pub to: u64,
    pub label: String,
    pub kind: MarkKind,
    /// Made by the agent.
    pub agent: bool,
    /// Blinks until then.
    pub flash_until: Option<Instant>,
    /// Goes away then.
    pub expires: Option<Instant>,
    /// Its place changed (the lines were rewritten): shown dimmed.
    pub stale: bool,
}

/// Files up to this size are remembered to show what changed in them.
const SNAPSHOT_LIMIT: u64 = 4 << 20;
/// Bytes of a grown file looked through per reread for its first
/// non-ASCII text (`Viewer::redetect_grown`).
const REDETECT_SCAN: u64 = 4 << 20;
/// The end of a grown file looked at first: where the new text is.
const REDETECT_TAIL: u64 = 1 << 20;
/// Bytes compared at the start and before the old end of a changed file:
/// the same — it only grew, the line index goes on.
const EDGE: u64 = 4096;
/// Bytes the line index goes on per frame while line numbers wait for it.
const INDEX_BUDGET: u64 = 16 << 20;

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
    /// The scroll bar's thumb is being dragged.
    dragging_bar: bool,
    /// The first Shift+click of a selection (Far: the next one ends it).
    shift_anchor: Option<u64>,
    opts: Opts,
    /// Lines marked by the agent, and the changed ones.
    pub marks: Vec<Mark>,
    lines: lines::LineIndex,
    /// The file's bytes as last read (small files), for what changed.
    snapshot: Option<Vec<u8>>,
    /// "Follow the agent" is on (shown in the status line).
    pub follow: bool,
    /// The label of the first mark the last frame showed.
    shown_label: Option<String>,
    /// Syntax highlighting (Alt+F3).
    highlight: highlight::Highlight,
    /// The code page is guessed, not chosen: while the file is ASCII up to
    /// this offset, text added later decides it.
    ascii_until: Option<u64>,
    /// Line numbers at the left (Ctrl+F3, text mode).
    pub line_numbers: bool,
    /// Columns of the line numbers in the last frame.
    gutter: u16,
    /// The size, the first and the last bytes when last read: tells a file
    /// that only grew from one rewritten.
    edges: (u64, Vec<u8>, Vec<u8>),
    /// The search of this window: every match on the screen is marked.
    search: Option<(search::Query, search::Pattern)>,
    /// Its matches in the file: how many, and where the first ones start
    /// (the counter "3/17").
    found_count: Option<(usize, Vec<u64>)>,
}

impl Viewer {
    /// Opens `path`; `remembered` restores the last position.
    pub fn open(
        id: u32,
        path: &Path,
        defaults: &Defaults,
        remembered: Option<&Remembered>,
        settings: &crate::config::Viewer,
    ) -> std::io::Result<Self> {
        let mut src = Source::open(path)?;
        let head = src.read_vec(0, DETECT_PROBE);
        let whole = head.len() as u64 == src.size();
        let default_cp = match settings.default_codepage {
            0 => codepage::ansi(),
            cp => cp,
        };
        // A BOM always counts; the rest of the detection by the setting.
        let detected = if settings.autodetect_codepage {
            codepage::detect(&head, whole)
        } else {
            codepage::bom(&head).map(|(cp, _)| cp)
        };
        let cp = remembered
            .map(|r| r.cp)
            .filter(|cp| *cp != 0 && Codec::new(*cp).is_some())
            .or(detected)
            .unwrap_or(default_cp);
        let codec = Codec::new(cp)
            .or_else(|| Codec::new(default_cp))
            .or_else(|| Codec::new(codepage::UTF8))
            .expect("UTF-8 is always supported");
        // Plain ASCII so far: the default page is only a guess.
        let guessed = settings.autodetect_codepage
            && head.is_ascii()
            && codec.cp() == default_cp
            && remembered.is_none_or(|r| r.cp == 0 || r.cp == default_cp);
        let ascii_until = guessed.then_some(head.len() as u64);
        let binary =
            settings.detect_dump && is_binary(&head[..head.len().min(BINARY_PROBE)], codec.unit());
        let (wrap, word_wrap) = remembered
            .and_then(|r| r.wrap)
            .unwrap_or((defaults.wrap, defaults.word_wrap));
        let restored_mode = remembered.and_then(|r| r.mode);
        let mode = restored_mode.unwrap_or(if binary { Mode::Dump } else { Mode::Text });
        let highlight = highlight::Highlight::new(settings.syntax, path, &head, &codec);
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
            wrap,
            word_wrap,
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
            dragging_bar: false,
            shift_anchor: None,
            opts: Opts {
                tab: settings.tab_size.clamp(1, 512),
                max_line: settings.max_line.clamp(100, 100_000),
                arrows: settings.show_arrows,
                zero: settings.show_zero,
                persistent: settings.persistent_selection,
            },
            marks: Vec::new(),
            lines: lines::LineIndex::new(),
            snapshot: None,
            follow: false,
            shown_label: None,
            highlight,
            ascii_until,
            line_numbers: settings.line_numbers,
            gutter: 0,
            edges: (0, Vec::new(), Vec::new()),
            search: None,
            found_count: None,
        };
        v.edges = v.read_edges();
        v.snapshot = v.read_snapshot();
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
            // A guessed page is not remembered: the next opening guesses
            // again from what the file has then.
            cp: if self.ascii_until.is_some() {
                0
            } else {
                self.codec.cp()
            },
            mode: self.mode_touched.then_some(self.mode),
            bookmarks: self.bookmarks.to_vec(),
            wrap: Some((self.wrap, self.word_wrap)),
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

    /// The text area of the last frame.
    pub fn area(&self) -> Rect {
        self.area
    }

    fn height(&self) -> usize {
        usize::from(self.area.height.max(1))
    }

    fn width(&self) -> usize {
        usize::from(self.area.width.max(1))
    }

    /// The text area inside `area`: below the status line, left of the
    /// scroll bar, right of the line numbers.
    fn text_area(&self, area: Rect) -> Rect {
        let status = u16::from(self.status_line && area.height > 1);
        let bar = u16::from(self.scrollbar && area.width > 1);
        let gutter = self.gutter.min(area.width.saturating_sub(bar + 1));
        Rect::new(
            area.x + gutter,
            area.y + status,
            area.width - bar - gutter,
            area.height - status,
        )
    }

    /// Columns for the line numbers: the digits of the most lines known
    /// (at least three) and a space; none outside the text mode.
    fn gutter_width(&self) -> u16 {
        if !self.line_numbers || self.mode != Mode::Text {
            return 0;
        }
        let digits = self.lines.known_lines().to_string().len().max(3);
        digits as u16 + 1
    }

    /// The line index is still being built for the line numbers (the
    /// application then draws sooner).
    pub fn indexing(&self) -> bool {
        self.line_numbers && self.mode == Mode::Text && !self.lines.done()
    }

    fn opts(&self) -> TextOpts {
        let w = self.width();
        TextOpts {
            tab: self.opts.tab,
            max_line: self.opts.max_line,
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
        let pos = pos.min(self.src.size());
        // From the line index: a scan from the start on every move cost
        // seconds deep in a big file.
        let line = self.line_of(pos);
        let line_start = self.line_start(line).min(pos);
        let line = line - 1;
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
    /// Goes to the start of line `n` (from 1), counting line feeds from
    /// the start of the file (links to `file#L10`, docs/16).
    pub fn goto_line(&mut self, n: u64) {
        let pos = self.line_start(n);
        self.goto(pos, Some(0));
    }

    /// Where line `n` (from 1) starts.
    pub fn line_start(&mut self, n: u64) -> u64 {
        self.lines.line_start(&mut self.src, &self.codec, n)
    }

    /// The line (from 1) of a byte offset.
    pub fn line_of(&mut self, offset: u64) -> u64 {
        self.lines.line_of(&mut self.src, &self.codec, offset)
    }

    /// How many lines the file has.
    pub fn line_count(&mut self) -> u64 {
        self.lines.count(&mut self.src, &self.codec)
    }

    /// Shows line `n` with a few lines of context above it.
    pub fn show_line(&mut self, n: u64) {
        // Past the end: the last line, not an empty screen after it.
        let n = n.min(self.line_count().max(1));
        let pos = self.line_start(n.saturating_sub(3).max(1));
        self.goto(pos, Some(0));
    }

    /// The lines shown in the last frame.
    /// `None` before the viewer has been drawn.
    pub fn visible_lines(&mut self) -> Option<(u64, u64)> {
        let (first, last) = (self.rows.first()?.start, self.rows.last()?.start);
        Some((self.line_of(first), self.line_of(last)))
    }

    /// The line at the top of the screen.
    pub fn top_line(&mut self) -> u64 {
        self.line_of(self.top)
    }

    /// The selection as lines.
    pub fn selection_lines(&mut self) -> Option<(u64, u64)> {
        let (from, to) = self.selection?;
        Some((
            self.line_of(from),
            self.line_of(to.saturating_sub(1).max(from)),
        ))
    }

    /// The text of lines `from..=to` (at most `limit` bytes).
    pub fn lines_text(&mut self, from: u64, to: u64, limit: usize) -> String {
        let start = self.line_start(from);
        let end = self.line_start(to + 1).max(start);
        let bytes = self
            .src
            .read_vec(start, ((end - start) as usize).min(limit));
        self.decode(&bytes)
    }

    fn decode(&self, bytes: &[u8]) -> String {
        let mut out = String::new();
        let mut i = 0;
        while i < bytes.len() {
            let (ch, n) = self.codec.decode(&bytes[i..]);
            out.push(ch);
            i += n.max(1);
        }
        out
    }

    /// The first line from `from` matching `re`, going through the file.
    pub fn find_line(&mut self, re: &regex::Regex, from: u64) -> Option<u64> {
        let total = self.line_count();
        let mut n = from.max(1);
        while n <= total {
            // A batch of lines at a time.
            let last = (n + 255).min(total);
            let text = self.lines_text(n, last, 8 << 20);
            for (k, line) in text.split('\n').enumerate() {
                if re.is_match(line) {
                    return Some(n + k as u64);
                }
            }
            n = last + 1;
        }
        None
    }

    /// To the next (previous) marked place from the top line.
    fn jump_mark(&mut self, forward: bool) {
        let here = self.line_of(self.top);
        // The place shown with context: it is `here + 3` when jumped to.
        let at = here + 3;
        let target = if forward {
            self.marks.iter().map(|m| m.from).filter(|f| *f > at).min()
        } else {
            self.marks.iter().map(|m| m.from).filter(|f| *f < at).max()
        };
        if let Some(line) = target {
            self.show_line(line);
        }
    }

    /// Adds marks; `replace` drops the agent's earlier ones first.
    pub fn add_marks(&mut self, marks: Vec<Mark>, replace: bool) {
        if replace {
            self.marks.retain(|m| !m.agent);
        }
        self.marks.extend(marks);
    }

    /// Drops the marks whose time ran out; `true`: something is drawn
    /// differently now (also while a mark blinks).
    pub fn marks_tick(&mut self) -> bool {
        let now = Instant::now();
        let before = self.marks.len();
        self.marks.retain(|m| m.expires.is_none_or(|t| t > now));
        before != self.marks.len()
            || self
                .marks
                .iter()
                .any(|m| m.flash_until.is_some_and(|t| t > now))
    }

    fn read_snapshot(&mut self) -> Option<Vec<u8>> {
        let size = self.src.size();
        (size <= SNAPSHOT_LIMIT).then(|| self.src.read_vec(0, size as usize))
    }

    /// The file changed: what lines are new or rewritten (in the new
    /// text), the marks moved with their lines (or stale).
    fn changed_lines(&mut self) -> Vec<(u64, u64)> {
        let new = self.read_snapshot();
        let (Some(old), Some(new_bytes)) = (self.snapshot.take(), new.clone()) else {
            self.snapshot = new;
            return Vec::new();
        };
        self.snapshot = new;
        let (old_t, new_t) = (self.decode(&old), self.decode(&new_bytes));
        let diff = similar::TextDiff::from_lines(&old_t, &new_t);
        let mut changed = Vec::new();
        // Old line → new line (from 0), for moving marks.
        let mut map: Vec<Option<u64>> = vec![None; old_t.lines().count() + 1];
        for op in diff.ops() {
            use similar::DiffTag;
            let (o, n) = (op.old_range(), op.new_range());
            match op.tag() {
                DiffTag::Equal => {
                    for (k, ol) in o.clone().enumerate() {
                        if let Some(slot) = map.get_mut(ol) {
                            *slot = Some((n.start + k) as u64);
                        }
                    }
                }
                DiffTag::Insert | DiffTag::Replace => {
                    if !n.is_empty() {
                        changed.push((n.start as u64 + 1, n.end as u64));
                    }
                }
                DiffTag::Delete => {
                    // A deletion shows on the line after it.
                    let l = n.start as u64 + 1;
                    changed.push((l, l));
                }
            }
        }
        for m in &mut self.marks {
            let f = map.get((m.from - 1) as usize).copied().flatten();
            let t = map.get((m.to - 1) as usize).copied().flatten();
            match (f, t) {
                (Some(f), Some(t)) if t >= f => {
                    m.from = f + 1;
                    m.to = t + 1;
                }
                _ => m.stale = true,
            }
        }
        changed
    }

    pub fn goto(&mut self, pos: u64, left: Option<usize>) {
        self.push_undo();
        self.top = pos.min(self.src.size());
        self.adjust_top();
        if let Some(l) = left {
            self.left = l;
        }
    }

    /// The user's choice: kept when the file grows.
    pub fn set_codepage(&mut self, cp: u32) -> bool {
        self.ascii_until = None;
        self.use_codepage(cp)
    }

    /// The page "Automatic detection" found: still a guess, the text the
    /// file gets later may change it.
    pub fn set_detected_codepage(&mut self, cp: u32) -> bool {
        let ok = self.use_codepage(cp);
        self.ascii_until = Some(0);
        ok
    }

    fn use_codepage(&mut self, cp: u32) -> bool {
        let Some(codec) = Codec::new(cp) else {
            return false;
        };
        self.codec = codec;
        // Line feeds are other bytes in another page.
        self.lines = lines::LineIndex::new();
        self.highlight.reset();
        self.adjust_top();
        true
    }

    /// The search shown in this window (every match marked); whether it is
    /// a new one (to count).
    pub fn set_search(&mut self, query: &search::Query) -> bool {
        if self.search.as_ref().is_some_and(|(q, _)| q == query) {
            return false;
        }
        self.search = query.pattern().ok().map(|p| (query.clone(), p));
        self.found_count = None;
        self.search.is_some()
    }

    /// The matches counted for `query` (dropped if the search changed).
    pub fn set_found_count(&mut self, query: &search::Query, total: usize, starts: Vec<u64>) {
        if self.search.as_ref().is_some_and(|(q, _)| q == query) {
            self.found_count = Some((total, starts));
        }
    }

    /// "3/17": the found text among the matches.
    fn found_counter(&self) -> Option<String> {
        let (total, starts) = self.found_count.as_ref()?;
        let at = self
            .selection
            .and_then(|(s, _)| starts.binary_search(&s).ok())
            .map_or("-".to_string(), |k| (k + 1).to_string());
        Some(format!("{at}/{total}"))
    }

    /// Syntax highlighting is on, and the file's syntax.
    pub fn syntax(&self) -> (bool, Option<&'static str>) {
        (self.highlight.on, self.highlight.name())
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
    pub fn check_changed(&mut self) -> Option<Vec<(u64, u64)>> {
        let was_last = self.last_page;
        let (old_size, head, tail) = std::mem::take(&mut self.edges);
        if !self.src.refresh() {
            self.edges = (old_size, head, tail);
            return None;
        }
        // Only grew (a log): the line index goes on; else it starts over.
        let size = self.src.size();
        let grew = size >= old_size
            && self.src.read_vec(0, head.len()) == head
            && self.src.read_vec(old_size - tail.len() as u64, tail.len()) == tail;
        if grew {
            self.lines.grown();
        } else {
            self.lines = lines::LineIndex::new();
        }
        self.edges = self.read_edges();
        self.found_count = None;
        self.redetect_grown();
        self.highlight.reset();
        let changed = self.changed_lines();
        let size = self.src.size();
        if self.top > size || was_last {
            self.go_end();
        }
        Some(changed)
    }

    fn read_edges(&mut self) -> (u64, Vec<u8>, Vec<u8>) {
        let size = self.src.size();
        let head = self.src.read_vec(0, EDGE.min(size) as usize);
        let tail_from = size.saturating_sub(EDGE);
        let tail = self.src.read_vec(tail_from, (size - tail_from) as usize);
        (size, head, tail)
    }

    /// A file that was ASCII when its page was guessed: its first non-ASCII
    /// text decides the page (a log that starts in English and goes on in
    /// UTF-8). Far keeps the first guess.
    fn redetect_grown(&mut self) {
        let Some(from) = self.ascii_until else {
            return;
        };
        let size = self.src.size();
        // Rewritten shorter: looked through again from the start.
        let from = if from > size { 0 } else { from };
        // The new text is at the end: looked at first, then on from where
        // the last look stopped.
        let tail = size.saturating_sub(REDETECT_TAIL).max(from);
        let found = match self.first_non_ascii(tail, size) {
            Ok(at) => Some(at),
            Err(_) => self
                .first_non_ascii(from, tail.min(from + REDETECT_SCAN))
                .map_err(|pos| self.ascii_until = Some(if pos == tail { size } else { pos }))
                .ok(),
        };
        let Some(at) = found else {
            return;
        };
        // Some text before it (from a character's start), then the new one.
        let mut start = at.saturating_sub(1024);
        for _ in 0..3 {
            match self.src.byte(start) {
                Some(b) if start > 0 && b & 0xC0 == 0x80 => start -= 1,
                _ => break,
            }
        }
        let probe = self.src.read_vec(start, DETECT_PROBE);
        let whole = start + probe.len() as u64 == size;
        if let Some(cp) = codepage::detect(&probe, whole)
            && cp != self.codec.cp()
        {
            self.use_codepage(cp);
        }
        self.ascii_until = None;
    }

    /// The first non-ASCII byte in `from..to`, or where the look ended.
    fn first_non_ascii(&mut self, from: u64, to: u64) -> Result<u64, u64> {
        let mut pos = from;
        while pos < to {
            let data = self.src.read_vec(pos, (to - pos).min(1 << 16) as usize);
            if data.is_empty() {
                break;
            }
            if let Some(i) = data.iter().position(|b| !b.is_ascii()) {
                return Ok(pos + i as u64);
            }
            pos += data.len() as u64;
        }
        Err(pos)
    }

    /// Runs a viewer command; those needing dialogs or the panels go back
    /// to the application.
    pub fn command(&mut self, cmd: ViewerCmd) -> Outcome {
        use ViewerCmd::*;
        // Without persistent selection keys drop it (except copying and
        // searching on).
        if !self.opts.persistent
            && !matches!(cmd, Copy | SearchNext | SearchPrev | Search | AskAgent)
        {
            self.selection = None;
        }
        let text = self.mode == Mode::Text;
        match cmd {
            NextMark => self.jump_mark(true),
            PrevMark => self.jump_mark(false),
            Up => self.up(1),
            Down => self.down(1),
            PageUp => self.up(self.height().saturating_sub(1).max(1)),
            PageDown => self.page_down(),
            Left if text && !self.wrap || self.mode == Mode::Hex => {
                self.left = self.left.saturating_sub(1)
            }
            Right if text && !self.wrap => self.left = (self.left + 1).min(self.opts.max_line),
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
                    self.left = (self.left + 20).min(self.opts.max_line);
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
            // The marks of the search go too.
            Unselect => {
                self.selection = None;
                self.search = None;
                self.found_count = None;
            }
            Scrollbar => self.scrollbar = !self.scrollbar,
            StatusLine => self.status_line = !self.status_line,
            LineNumbers if text => self.line_numbers = !self.line_numbers,
            // The window says what it is now.
            Syntax => {
                self.highlight.on = !self.highlight.on;
                return Outcome::App(cmd);
            }
            other => return Outcome::App(other),
        }
        Outcome::Done
    }

    /// The scroll bar's column and its first and last rows, when shown.
    fn scrollbar_at(&self) -> Option<(u16, u16, u16)> {
        (self.scrollbar && self.area.height >= 2)
            .then(|| (self.area.right(), self.area.y, self.area.bottom() - 1))
    }

    /// The byte offset of the character shown at a screen cell (text and
    /// dump modes).
    fn pos_at(&self, x: u16, y: u16) -> Option<u64> {
        let a = self.area;
        if !a.contains(ratatui::layout::Position::new(x, y)) {
            return None;
        }
        let (dx, dy) = (usize::from(x - a.x), usize::from(y - a.y));
        match self.mode {
            Mode::Text => {
                let row = self.rows.get(dy)?;
                let col = dx + if self.wrap { 0 } else { self.left };
                row.cells
                    .iter()
                    .find(|c| col >= c.col && col < c.col + usize::from(c.width))
                    .map(|c| c.pos)
            }
            Mode::Dump => {
                let pos = self.top
                    + dy as u64 * (self.width() * self.codec.unit()) as u64
                    + (dx * self.codec.unit()) as u64;
                (pos < self.src.size()).then_some(pos)
            }
            Mode::Hex => None,
        }
    }

    /// A mouse press (Far's `Viewer::ProcessMouse`). On the scroll bar: the
    /// arrows scroll a row, the cells next to them go to the start or the
    /// end, elsewhere the thumb goes there (and follows a drag). Shift+click
    /// starts a selection, the next Shift+click ends it. Returns whether it
    /// was taken.
    pub fn mouse_down(&mut self, x: u16, y: u16, shift: bool) -> bool {
        if let Some((bx, top, bottom)) = self.scrollbar_at()
            && x == bx
            && (top..=bottom).contains(&y)
        {
            if y == top {
                self.up(1);
            } else if y == bottom {
                self.down(1);
            } else if y == top + 1 {
                self.command(ViewerCmd::Home);
            } else if y + 1 == bottom {
                self.command(ViewerCmd::End);
            } else {
                self.dragging_bar = true;
                self.thumb_to(y, top, bottom);
            }
            return true;
        }
        if shift && let Some(pos) = self.pos_at(x, y) {
            let len = self.char_len(pos);
            match self.shift_anchor.take() {
                None => {
                    self.shift_anchor = Some(pos);
                    self.selection = Some((pos, pos + len));
                }
                Some(a) => {
                    let (from, to) = if a <= pos { (a, pos) } else { (pos, a) };
                    let end = to + self.char_len(to);
                    self.selection = Some((from, end));
                }
            }
            return true;
        }
        false
    }

    pub fn mouse_drag(&mut self, y: u16) {
        if self.dragging_bar
            && let Some((_, top, bottom)) = self.scrollbar_at()
        {
            self.thumb_to(y, top, bottom);
        }
    }

    pub fn mouse_up(&mut self) {
        self.dragging_bar = false;
    }

    /// The top at the scroll bar's row `y` (between the cells for the start
    /// and the end).
    fn thumb_to(&mut self, y: u16, top: u16, bottom: u16) {
        let (first, last) = (top + 2, bottom.saturating_sub(2));
        let field = u64::from(last.saturating_sub(first)).max(1);
        let k = u64::from(y.clamp(first, last.max(first)) - first);
        let size = self.src.size();
        self.top = (u128::from(size) * u128::from(k) / u128::from(field)) as u64;
        if self.top >= size {
            self.go_end();
        } else {
            self.adjust_top();
        }
    }

    fn char_len(&mut self, pos: u64) -> u64 {
        layout::char_at(&mut self.src, &self.codec, pos).map_or(1, |(_, n)| n as u64)
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
        if self.indexing() {
            self.lines.advance(&mut self.src, &self.codec, INDEX_BUDGET);
        }
        self.gutter = self.gutter_width();
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
        // The line of each row, for the marks.
        let mut line = if self.marks.is_empty() {
            0
        } else {
            self.line_of(p)
        };
        self.shown_label = None;
        let mut prev = p;
        let now = Instant::now();
        // The rows first: the syntax colors are found for their bytes.
        let mut rows = Vec::new();
        while rows.len() < usize::from(a.height) && p < self.src.size() {
            let row = layout::read_row(&mut self.src, &self.codec, p, &opts);
            p = row.end;
            rows.push(row);
        }
        let syn = match (rows.first(), rows.last()) {
            (Some(f), Some(l)) => self
                .highlight
                .ranges(&mut self.src, &self.codec, f.start, l.end),
            _ => Vec::new(),
        };
        // Every match of the window's search on the screen.
        let found = match (&self.search, rows.first(), rows.last()) {
            (Some((_, p)), Some(f), Some(l)) => {
                p.matches(&mut self.src, &self.codec, f.start, l.end)
            }
            _ => Vec::new(),
        };
        let numbers = self.row_numbers(&rows);
        for (y, row) in rows.into_iter().enumerate() {
            if self.gutter > 0 {
                let text = match numbers[y] {
                    Some(n) => format!("{n:>w$} ", w = usize::from(self.gutter) - 1),
                    None => String::new(),
                };
                buf.set_stringn(
                    a.x - self.gutter,
                    a.y + y as u16,
                    &text,
                    usize::from(self.gutter),
                    theme::VIEWER_LINE_NUMBERS,
                );
            }
            if !self.marks.is_empty() && row.start > prev {
                line += lines::count_feeds(&mut self.src, &self.codec, prev, row.start);
                prev = row.start;
            }
            let ry = a.y + y as u16;
            // Changed lines get a bar in the first column only: their text
            // keeps its colors.
            let changed = self
                .marks
                .iter()
                .any(|m| m.kind == MarkKind::Changed && line >= m.from && line <= m.to);
            let mark_style = self
                .marks
                .iter()
                .rev()
                .find(|m| m.kind != MarkKind::Changed && line >= m.from && line <= m.to)
                .map(|m| {
                    let mut st = match m.kind {
                        MarkKind::Info => theme::VIEWER_MARK_INFO,
                        MarkKind::Warning => theme::VIEWER_MARK_WARNING,
                        MarkKind::Error => theme::VIEWER_MARK_ERROR,
                        MarkKind::Changed => theme::VIEWER_MARK_CHANGED,
                    };
                    if m.stale {
                        st = st.add_modifier(ratatui::style::Modifier::DIM);
                    }
                    // Blinking: the colors swap four times a second.
                    let phase = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_millis() / 250);
                    if m.flash_until.is_some_and(|t| t > now) && phase.is_multiple_of(2) {
                        st = st.add_modifier(ratatui::style::Modifier::REVERSED);
                    }
                    st
                });
            if let Some(st) = mark_style {
                for x in a.left()..a.right() {
                    buf[(x, ry)].set_style(st);
                }
                if self.shown_label.is_none() {
                    self.shown_label = self
                        .marks
                        .iter()
                        .rev()
                        .find(|m| line >= m.from && line <= m.to && !m.label.is_empty())
                        .map(|m| m.label.clone());
                }
            }
            for cell in &row.cells {
                let cw = usize::from(cell.width);
                if cell.col + cw <= left || cell.col >= left + width {
                    continue;
                }
                // Selection over the agent's marks over the matches over
                // the syntax.
                let style = if self.selected(cell.pos) {
                    theme::VIEWER_SELECTED
                } else if let Some(st) = mark_style {
                    st
                } else if found.iter().any(|(s, e)| cell.pos >= *s && cell.pos < *e) {
                    theme::VIEWER_FOUND
                } else {
                    match highlight::color_at(&syn, cell.pos) {
                        Some(c) => theme::VIEWER_TEXT.fg(c),
                        None => theme::VIEWER_TEXT,
                    }
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
                    let ch = if cell.ch == '\0' && self.opts.zero {
                        '·'
                    } else {
                        glyph(cell.ch)
                    };
                    buf.set_stringn(x, ry, ch.encode_utf8(&mut tmp), cw, style);
                }
            }
            if self.opts.arrows && !self.wrap {
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
            // Over the scrolling arrow too: the bar stays seen.
            if changed {
                buf[(a.x, ry)].set_style(theme::VIEWER_MARK_CHANGED);
            }
            self.rows.push(row);
        }
    }

    /// The line number of each row that starts a line (rows going on with
    /// a wrapped or cut line get none); none while the line index has not
    /// reached the screen. Lines are counted by line feeds, as the agent's
    /// tools count them.
    fn row_numbers(&mut self, rows: &[Row]) -> Vec<Option<u64>> {
        let mut out = vec![None; rows.len()];
        if self.gutter == 0 || rows.is_empty() {
            return out;
        }
        let Some(mut line) = self
            .lines
            .known_line_of(&mut self.src, &self.codec, rows[0].start)
        else {
            return out;
        };
        for (k, row) in rows.iter().enumerate() {
            let starts = row.start == 0
                || layout::unit_before(&mut self.src, &self.codec, row.start) == Some('\n');
            if starts {
                if k > 0 {
                    line += 1;
                }
                out[k] = Some(line);
            }
        }
        out
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
        let counter = self
            .found_counter()
            .map_or(String::new(), |c| format!("│{c}"));
        let mut status = format!(
            "{counter}│{}│{:5.5}│{:<10}│{} {:<3}│{:4}",
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
        // A mark on the screen: its label instead of the name; following
        // the agent: a sign in front.
        let shown = match &self.shown_label {
            Some(label) => format!("◆ {label}"),
            None => self.path().display().to_string(),
        };
        let shown = if self.follow {
            format!("⇢ {shown}")
        } else {
            shown
        };
        let name = crate::panel::truncate_path(&shown, name_w);
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
            if group == "Alt" {
                labels[2] = t("editor-keybar-syntax");
            }
            if group == "Ctrl" && self.mode == Mode::Text {
                labels[2] = t(if self.line_numbers {
                    "MEditCtrlF3Hide"
                } else {
                    "MEditCtrlF3"
                });
            }
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
    /// A plain decimal number (no `0x`, `h`, `m`, `%`, Hex box): a line
    /// number while the line numbers are shown.
    pub plain: bool,
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
        let mut plain = false;
        let (digits, radix) = if let Some(d) = lower.strip_prefix("0x").or(lower.strip_prefix('$'))
        {
            (d.to_string(), 16)
        } else if let Some(d) = lower.strip_suffix('h') {
            (d.to_string(), 16)
        } else if let Some(d) = lower.strip_suffix('m') {
            (d.to_string(), 10)
        } else {
            plain = !hex && !percent;
            // A percentage is decimal even with the Hex box.
            (lower.clone(), if hex && !percent { 16 } else { 10 })
        };
        let value = u64::from_str_radix(&digits, radix).ok()?;
        Some(Some(GotoValue {
            value,
            sign,
            percent,
            plain,
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
    fn the_line_index_goes_on_when_the_file_grows() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("afar-vidx-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("idx.log");
        std::fs::write(&path, "one\ntwo\nthr").unwrap();
        let settings = crate::config::Viewer::default();
        let mut v = Viewer::open(1, &path, &Defaults::default(), None, &settings).unwrap();
        assert_eq!(v.line_count(), 3);
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(b"ee\nfour\n").unwrap();
        drop(f);
        assert!(v.check_changed().is_some());
        assert_eq!(v.line_count(), 5);
        let four = v.line_start(4);
        assert_eq!(v.line_of(four), 4);
        assert_eq!(v.line_start(3), 8);
        // Rewritten: counted afresh.
        std::fs::write(&path, "a\nb\nc\nd\ne\nf\n").unwrap();
        assert!(v.check_changed().is_some());
        assert_eq!(v.line_count(), 7);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn line_col_counts_from_the_line_index() {
        let dir = std::env::temp_dir().join(format!("afar-vlc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("lc.txt");
        std::fs::write(&path, "ab\r\nцве\nx").unwrap();
        let settings = crate::config::Viewer::default();
        let mut v = Viewer::open(1, &path, &Defaults::default(), None, &settings).unwrap();
        assert_eq!(v.line_col(0), (0, 0));
        assert_eq!(v.line_col(2), (0, 2));
        assert_eq!(v.line_col(4), (1, 0));
        // "цв" is four bytes, two characters.
        assert_eq!(v.line_col(8), (1, 2));
        assert_eq!(v.line_col(11), (2, 0));
        assert_eq!(v.line_col(12), (2, 1));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_grown_ascii_file_gets_its_page_from_new_text() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("afar-vgrow-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("grow.log");
        std::fs::write(&path, b"start\n").unwrap();
        let settings = crate::config::Viewer {
            default_codepage: 1251,
            ..Default::default()
        };
        let open = || Viewer::open(1, &path, &Defaults::default(), None, &settings).unwrap();
        let mut v = open();
        assert_eq!(v.codepage(), 1251);
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all("ещё строка\n".as_bytes()).unwrap();
        drop(f);
        assert!(v.check_changed().is_some());
        assert_eq!(v.codepage(), codepage::UTF8);
        // A page the user chose stays.
        let mut v = open();
        v.set_codepage(1251);
        std::fs::write(&path, "start\nи ещё\n".as_bytes()).unwrap();
        assert!(v.check_changed().is_some());
        assert_eq!(v.codepage(), 1251);
        // New text far past what one look goes through: found at the end.
        let line = "x".repeat(99)
            + "
";
        std::fs::write(&path, line.repeat(60_000)).unwrap();
        let mut v = open();
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(
            "конец
"
            .as_bytes(),
        )
        .unwrap();
        drop(f);
        assert!(v.check_changed().is_some());
        assert_eq!(v.codepage(), codepage::UTF8);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn parses_far_goto() {
        let v = |value, sign, percent| {
            Some(GotoValue {
                value,
                sign,
                percent,
                plain: false,
            })
        };
        let p = |value, sign| {
            Some(GotoValue {
                value,
                sign,
                percent: false,
                plain: true,
            })
        };
        assert_eq!(parse_goto("1000", false), Some((p(1000, None), None)));
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
            Some((p(100, None), p(20, None)))
        );
        assert_eq!(parse_goto(",5", false), Some((None, p(5, None))));
        assert_eq!(parse_goto("-3", false), Some((p(3, Some('-')), None)));
        assert_eq!(parse_goto("zz", false), None);
        assert_eq!(v(50, None, true).unwrap().resolve(0, 1000), 500);
        assert_eq!(v(10, Some('-'), false).unwrap().resolve(5, 0), 0);
    }
}
