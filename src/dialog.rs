//! Far-style dialogs (far/dialog.cpp, far/message.cpp): a modal box over
//! the window layout. Geometry follows Far: the dialog is W×H, the double
//! box is at (3,1)–(W−4,H−2), row `i` is at y = 2 + i, items sit at x
//! positions relative to the dialog (5 is the usual left edge), buttons are
//! centred as a group. Labels carry Far's `&` hotkeys: the letter is
//! highlighted and Alt+letter (in any keyboard layout) activates the item.
//! See docs/09-far-ui-reference.md.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Style};

use crate::complete::fuzzy;
use crate::panel::draw_frame;
use crate::theme;

/// Where a dialog or menu belongs: it is centred there (the manager's
/// screen, or the agent's pane), and goes beyond only when it does not fit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Host {
    Screen,
    Agent,
}

/// The start of `size` cells centred in the parent's `start..start+len`,
/// kept within the area's `area_start..area_start+area_len` (a window
/// bigger than its parent spreads both ways, then is pushed back in).
pub fn centred(start: u16, len: u16, area_start: u16, area_len: u16, size: u16) -> u16 {
    let c = i32::from(start) + (i32::from(len) - i32::from(size)) / 2;
    let max = i32::from(area_start) + i32::from(area_len.saturating_sub(size));
    c.clamp(i32::from(area_start), max) as u16
}

/// Position of an item: from the dialog's left edge, or centred.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum X {
    At(u16),
    Center,
}

pub enum Kind {
    Text {
        text: String,
        /// Show `&` literally instead of as a hotkey marker.
        show_amp: bool,
        /// Drawn in the hotkey colour (e.g. names in a delete prompt).
        highlight: bool,
    },
    Input {
        value: String,
        cursor: usize,
        /// Not edited yet: dimmed, replaced by the first typed character.
        unchanged: bool,
        width: u16,
        /// The history list's name (Far's DIF_HISTORY): Far draws `↓`
        /// after the field; fields with one name share the list.
        history: Option<String>,
        /// An empty field starts with the newest entry (DIF_USELASTHISTORY).
        use_last: bool,
        /// Holds paths: completion offers files (DIF_EDITPATH).
        path: bool,
        /// Holds a command: completion offers programs too
        /// (DIF_EDITPATHEXEC).
        exec: bool,
        readonly: bool,
        disabled: bool,
    },
    Check {
        label: String,
        checked: bool,
        disabled: bool,
        /// A three-state box (Far's BSTATE_3STATE): `Some(true)` — "?",
        /// neither (several files that differ); `None` — two states.
        mixed: Option<bool>,
    },
    Radio {
        label: String,
        selected: bool,
        group: u16,
        disabled: bool,
    },
    /// Far's DIF_DROPDOWNLIST combo box; `None` items are separators.
    Combo {
        items: Vec<Option<String>>,
        selected: usize,
        width: u16,
        disabled: bool,
    },
    /// A button among the items that does not close the dialog (Far's
    /// DIF_BTNNOCLOSE): pressing it gives `Outcome::Pressed`.
    Button { label: String, disabled: bool },
}

pub struct Elem {
    pub x: X,
    pub kind: Kind,
}

pub struct Button {
    pub label: String,
    pub default: bool,
    pub disabled: bool,
    pub hidden: bool,
}

pub enum Row {
    Items(Vec<Elem>),
    Separator,
    /// A separator with text in the middle, like Far's " Всего ".
    Caption(String),
    Buttons(Vec<Button>),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Target {
    /// Row, element.
    Elem(usize, usize),
    /// Row, button index in the row, button number in the dialog.
    Button(usize, usize, usize),
    /// The `↓` of a field with a history: row, element.
    Arrow(usize, usize),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Pending,
    /// Closed with a button (its number in the dialog) or cancelled.
    Closed(Option<usize>),
    /// The focused field's history is needed (the owner keeps it).
    History(HistoryRequest),
    /// An item button (`button_at`) was pressed: its number among the
    /// item buttons; the dialog stays open.
    Pressed(usize),
}

/// The focused input field (see `Dialog::focused_field`).
pub struct FocusedField {
    pub value: String,
    pub history: Option<String>,
    pub path: bool,
    pub exec: bool,
    pub rect: Rect,
    /// The cursor is at the end of the text.
    pub at_end: bool,
}

/// What a field with a history asks of the history's owner.
#[derive(Debug, PartialEq, Eq)]
pub enum HistoryRequest {
    /// Ctrl+Up / Ctrl+Down / `↓`: the list (answer: `show_history`).
    Open {
        list: String,
    },
    /// Ctrl+End: the next entry starting with `prefix` after `after`
    /// (answer: `set_focused_input`).
    Next {
        list: String,
        prefix: String,
        after: String,
    },
    /// In the open list (answer: `refresh_history`).
    Lock {
        list: String,
        text: String,
        locked: bool,
    },
    Delete {
        list: String,
        text: String,
    },
    Clear {
        list: String,
    },
}

/// What the mouse button was pressed on (the action happens on release).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pressed {
    Target(Target, Rect),
    ListItem(usize),
}

/// An open drop-down list of a combo box, or of a field's history.
struct OpenList {
    row: usize,
    elem: usize,
    current: usize,
    /// First item shown (the list scrolls).
    top: usize,
    /// A field's history.
    history: Option<HistoryView>,
}

/// A line of a field's history list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistLine {
    Entry {
        text: String,
        locked: bool,
        /// Made by the agent (marked in the list).
        agent: bool,
        /// A path that does not exist now (greyed).
        missing: bool,
        /// When and where, shown on the right.
        detail: String,
    },
    /// A group's title: another source (the passive panel, folders).
    Title(String),
}

/// History lines as shown, with the characters the filter matched.
type ShownLines = Vec<(HistLine, Vec<usize>)>;

/// A history list as shown: the lines, the filter typed in the list, and
/// the lines it lets through with the matched characters.
struct HistoryView {
    lines: Vec<HistLine>,
    filter: String,
    shown: Vec<(usize, Vec<usize>)>,
}

impl HistoryView {
    fn new(lines: Vec<HistLine>, filter: String) -> Self {
        let mut v = Self {
            lines,
            filter,
            shown: Vec::new(),
        };
        v.apply();
        v
    }

    /// Without a filter: everything; with one — the entries matching it
    /// (characters in order, anywhere), the closest matches first.
    fn apply(&mut self) {
        if self.filter.is_empty() {
            self.shown = (0..self.lines.len()).map(|i| (i, Vec::new())).collect();
            return;
        }
        let mut found: Vec<(usize, usize, Vec<usize>)> = self
            .lines
            .iter()
            .enumerate()
            .filter_map(|(i, l)| match l {
                HistLine::Entry { text, .. } => {
                    fuzzy(text, &self.filter).map(|(score, marks)| (score, i, marks))
                }
                HistLine::Title(_) => None,
            })
            .collect();
        found.sort_by_key(|(score, i, _)| (*score, *i));
        self.shown = found.into_iter().map(|(_, i, m)| (i, m)).collect();
    }

    fn entry(&self, k: usize) -> Option<(String, bool)> {
        match &self.lines[self.shown.get(k)?.0] {
            HistLine::Entry { text, locked, .. } => Some((text.clone(), *locked)),
            HistLine::Title(_) => None,
        }
    }

    fn first_entry(&self) -> usize {
        (0..self.shown.len())
            .find(|k| self.entry(*k).is_some())
            .unwrap_or(0)
    }
}

/// Far's combo and history lists show at most this many items.
const LIST_ROWS: usize = 8;

struct Colors {
    body: Style,
    box_: Style,
    highlight: Style,
    button_focused: Style,
    button_focused_highlight: Style,
    disabled: Style,
}

const NORMAL: Colors = Colors {
    body: theme::DIALOG_TEXT,
    box_: theme::DIALOG_BOX,
    highlight: theme::DIALOG_HIGHLIGHT,
    button_focused: theme::DIALOG_BUTTON_SELECTED,
    button_focused_highlight: theme::DIALOG_BUTTON_SELECTED_HIGHLIGHT,
    disabled: theme::DIALOG_DISABLED,
};

const WARNING: Colors = Colors {
    body: theme::WARN_TEXT,
    box_: theme::WARN_BOX,
    highlight: theme::WARN_HIGHLIGHT,
    button_focused: theme::WARN_BUTTON_SELECTED,
    button_focused_highlight: theme::WARN_BUTTON_SELECTED_HIGHLIGHT,
    disabled: theme::WARN_DISABLED,
};

/// Visible text of a label: `&` markers removed (`&&` is a literal `&`).
pub fn visible(label: &str) -> String {
    let mut out = String::new();
    let mut chars = label.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' {
            if chars.peek() == Some(&'&') {
                out.push('&');
                chars.next();
            }
            continue;
        }
        out.push(c);
    }
    out
}

fn width_of(s: &str) -> u16 {
    s.chars().count() as u16
}

/// Far's window shadow: the row below (from x+2) and two columns to the
/// right keep their characters in dim colours.
pub fn draw_shadow(buf: &mut Buffer, outer: Rect, area: Rect) {
    for sy in outer.y + 1..=outer.bottom() {
        for sx in outer.right()..outer.right() + 2 {
            if sx < area.right() && sy < area.bottom() {
                buf[(sx, sy)].set_style(theme::SHADOW);
            }
        }
    }
    if outer.bottom() < area.bottom() {
        for sx in outer.x + 2..(outer.right() + 2).min(area.right()) {
            buf[(sx, outer.bottom())].set_style(theme::SHADOW);
        }
    }
}

/// The hotkey of a label, as the Latin key at its position (so Alt+К and
/// Alt+R both find "&Копировать").
pub(crate) fn hotkey(label: &str) -> Option<char> {
    let mut chars = label.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' {
            match chars.next() {
                Some('&') => continue,
                Some(h) => {
                    let h = h.to_lowercase().next()?;
                    return Some(crate::keys::latin_equivalent(h).unwrap_or(h));
                }
                None => return None,
            }
        }
    }
    None
}

/// Draws a label at (x, y), at most `max` cells, with its hotkey letter in
/// `hot`; returns the cells used.
fn put_label(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    max: u16,
    label: &str,
    normal: Style,
    hot: Style,
) -> u16 {
    let mut col = 0u16;
    let mut chars = label.chars().peekable();
    let mut next_hot = false;
    while let Some(c) = chars.next() {
        if col >= max {
            break;
        }
        if c == '&' && !next_hot {
            if chars.peek() == Some(&'&') {
                chars.next();
            } else {
                next_hot = true;
                continue;
            }
        }
        let style = if next_hot { hot } else { normal };
        next_hot = false;
        buf[(x + col, y)]
            .set_symbol(&c.to_string())
            .set_style(style);
        col += 1;
    }
    col
}

fn put_plain(buf: &mut Buffer, x: u16, y: u16, max: u16, text: &str, style: Style) -> u16 {
    let mut col = 0;
    for c in text.chars() {
        if col >= max {
            break;
        }
        buf[(x + col, y)]
            .set_symbol(&c.to_string())
            .set_style(style);
        col += 1;
    }
    col
}

/// Splits `s` into lines of at most `width` chars at spaces (long words
/// are cut).
pub fn wrap(s: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in s.split_whitespace() {
        let mut word: Vec<char> = word.chars().collect();
        while word.len() > width {
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            lines.push(word.drain(..width).collect());
        }
        let word: String = word.into_iter().collect();
        let len = line.chars().count();
        if len > 0 && len + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(&word);
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

pub struct Dialog {
    title: String,
    /// Full width W (the box is 3 columns in from each side).
    width: u16,
    rows: Vec<Row>,
    warning: bool,
    focus: Option<usize>,
    hits: Vec<(Rect, Target)>,
    list: Option<OpenList>,
    next_group: u16,
    /// Moved by the user from the centred position.
    offset: (i32, i32),
    /// Being dragged with the mouse: the last mouse position.
    drag: Option<(u16, u16)>,
    /// Where the dialog was last drawn.
    outer: Rect,
    /// Where the open drop-down list was last drawn.
    list_rect: Rect,
    pressed: Option<Pressed>,
    /// Ctrl+End: the text it started from (cleared by other keys).
    cycle_prefix: Option<String>,
    /// `fill_last` has run.
    filled: bool,
    /// Where the focused input field was last drawn.
    focused_rect: Rect,
    /// The ghost suggestion: shown in grey after the focused field's text
    /// while it is this text and the cursor is at its end.
    ghost: Option<Box<(String, String)>>,
    /// Check boxes that mean something only when another is checked:
    /// (dependent, the one it depends on), by their order in the dialog.
    links: Vec<(usize, usize)>,
    /// Where the dialog belongs (set by its owner when first drawn).
    pub host: Option<Host>,
}

impl Dialog {
    /// A dialog of Far's width W (e.g. 76 for copy and make-folder).
    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn far(title: impl Into<String>, width: u16) -> Self {
        Self {
            title: title.into(),
            width,
            rows: Vec::new(),
            warning: false,
            focus: None,
            hits: Vec::new(),
            list: None,
            next_group: 0,
            offset: (0, 0),
            drag: None,
            outer: Rect::default(),
            list_rect: Rect::default(),
            pressed: None,
            cycle_prefix: None,
            filled: false,
            focused_rect: Rect::default(),
            ghost: None,
            links: Vec::new(),
            host: None,
        }
    }

    /// The `child`-th check box is available only while the `parent`-th
    /// is checked (Far greys such a box, e.g. "Modal mode" under "Show
    /// list").
    pub fn check_depends(mut self, child: usize, parent: usize) -> Self {
        self.links.push((child, parent));
        self.sync_links();
        self
    }

    /// Greys or enables the dependent check boxes.
    fn sync_links(&mut self) {
        if self.links.is_empty() {
            return;
        }
        let states: Vec<bool> = self
            .kinds()
            .into_iter()
            .filter_map(|k| match k {
                Kind::Check { checked, .. } => Some(*checked),
                _ => None,
            })
            .collect();
        let links = self.links.clone();
        let mut n = 0;
        for row in &mut self.rows {
            let Row::Items(elems) = row else { continue };
            for elem in elems {
                if let Kind::Check { disabled, .. } = &mut elem.kind {
                    if let Some((_, parent)) = links.iter().find(|(c, _)| *c == n) {
                        *disabled = !states.get(*parent).copied().unwrap_or(false);
                    }
                    n += 1;
                }
            }
        }
    }

    /// The ghost suggestion for the focused field: `(its text, the rest)`.
    pub fn set_ghost(&mut self, ghost: Option<(String, String)>) {
        self.ghost = ghost.map(Box::new);
    }

    pub fn ghost(&self) -> Option<(String, String)> {
        self.ghost.as_deref().cloned()
    }

    /// The dialog is being moved with the mouse.
    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// A dialog with `content` columns for text (W = content + 10).
    pub fn new(title: impl Into<String>, content: u16) -> Self {
        Self::far(title, content + 10)
    }

    /// Far's `Message()`: centred lines, a separator, buttons; the width
    /// from the content (message.cpp). Lines starting with `\x01` are
    /// separators.
    pub fn message(title: &str, lines: &[String], buttons: &[&str], warning: bool) -> Self {
        let cols = crossterm::terminal::size().map_or(80, |(w, _)| w);
        let buttons_len: u16 = buttons
            .iter()
            .map(|b| width_of(&visible(b)) + 5)
            .sum::<u16>()
            .saturating_sub(1);
        let longest = lines
            .iter()
            .map(|l| width_of(l))
            .chain([width_of(title) + 2, buttons_len])
            .max()
            .unwrap_or(0);
        let content = longest.min(cols.saturating_sub(11).max(buttons_len));
        let mut d = Self::new(title, content);
        d.warning = warning;
        for l in lines {
            if l.starts_with('\x01') {
                d = d.separator();
            } else {
                d.rows.push(Row::Items(vec![Elem {
                    x: X::Center,
                    kind: Kind::Text {
                        text: l.clone(),
                        show_amp: true,
                        highlight: false,
                    },
                }]));
            }
        }
        if !buttons.is_empty() {
            if !matches!(d.rows.last(), Some(Row::Separator)) {
                d = d.separator();
            }
            d = d.buttons(buttons, 0);
        }
        d
    }

    /// Red warning style.
    pub fn warning(mut self) -> Self {
        self.warning = true;
        self
    }

    /// Width available to items at x = 5.
    pub fn content_width(&self) -> u16 {
        self.width.saturating_sub(10)
    }

    pub fn row(mut self, elems: Vec<Elem>) -> Self {
        self.rows.push(Row::Items(elems));
        self
    }

    pub fn text(self, s: impl Into<String>) -> Self {
        self.row(vec![text_at(5, s)])
    }

    /// Centred text shown as is (`&` included), like Far's prompts.
    pub fn center(self, s: impl Into<String>) -> Self {
        self.row(vec![text_at(5, s).centered().literal()])
    }

    /// Text wrapped at word boundaries to the content width.
    pub fn wrapped(mut self, s: &str) -> Self {
        for line in wrap(s, usize::from(self.content_width())) {
            self = self.row(vec![text_at(5, line).literal()]);
        }
        self
    }

    /// An input field across the content width (with Far's history arrow).
    pub fn input(self, value: impl Into<String>) -> Self {
        let width = self.content_width().saturating_sub(1);
        self.row(vec![input_at(5, width, value, None)])
    }

    pub fn check(self, label: impl Into<String>, checked: bool) -> Self {
        self.row(vec![check_at(5, label, checked)])
    }

    /// A group of radio buttons, one per row.
    pub fn radios(mut self, labels: &[&str], selected: usize) -> Self {
        let group = self.new_group();
        for (i, l) in labels.iter().enumerate() {
            self = self.row(vec![radio_at(5, *l, i == selected, group)]);
        }
        self
    }

    /// A new radio group id (for radios built with `radio_at`).
    pub fn new_group(&mut self) -> u16 {
        self.next_group += 1;
        self.next_group
    }

    pub fn separator(mut self) -> Self {
        self.rows.push(Row::Separator);
        self
    }

    pub fn caption(mut self, text: impl Into<String>) -> Self {
        self.rows.push(Row::Caption(text.into()));
        self
    }

    /// A row of buttons; the `default` one (an index in this row) is shown
    /// as `{ … }` and pressed by Enter elsewhere.
    pub fn buttons(mut self, labels: &[&str], default: usize) -> Self {
        self.rows.push(Row::Buttons(
            labels
                .iter()
                .enumerate()
                .map(|(i, l)| Button {
                    label: l.to_string(),
                    default: i == default,
                    disabled: false,
                    hidden: false,
                })
                .collect(),
        ));
        self
    }

    pub fn button_row(mut self, buttons: Vec<Button>) -> Self {
        self.rows.push(Row::Buttons(buttons));
        self
    }

    /// Focuses the `n`-th focusable item (in reading order).
    pub fn focus_item(mut self, n: usize) -> Self {
        self.focus = Some(n);
        self
    }

    // ------------------------------------------------------------ values

    fn kinds(&self) -> Vec<&Kind> {
        self.rows
            .iter()
            .flat_map(|r| match r {
                Row::Items(e) => e.iter().map(|e| &e.kind).collect::<Vec<_>>(),
                _ => Vec::new(),
            })
            .collect()
    }

    /// The fields with a history: (list, value), for recording on close.
    pub fn history_values(&self) -> Vec<(String, String)> {
        self.kinds()
            .into_iter()
            .filter_map(|k| match k {
                Kind::Input {
                    history: Some(list),
                    value,
                    readonly: false,
                    ..
                } => Some((list.clone(), value.clone())),
                _ => None,
            })
            .collect()
    }

    pub fn history_filled(&self) -> bool {
        self.filled
    }

    /// Fills the empty fields marked `use_last` with `last(list)` (once).
    pub fn fill_last(&mut self, last: impl Fn(&str) -> Option<String>) {
        self.filled = true;
        for row in &mut self.rows {
            let Row::Items(elems) = row else { continue };
            for e in elems {
                if let Kind::Input {
                    history: Some(list),
                    use_last: true,
                    value,
                    cursor,
                    unchanged,
                    ..
                } = &mut e.kind
                    && value.is_empty()
                    && let Some(text) = last(list)
                {
                    *cursor = text.chars().count();
                    *value = text;
                    *unchanged = true;
                }
            }
        }
    }

    /// The label of button number `n` (as shown, `&` removed).
    pub fn button_label(&self, n: usize) -> Option<String> {
        self.rows
            .iter()
            .filter_map(|r| match r {
                Row::Buttons(b) => Some(b),
                _ => None,
            })
            .flatten()
            .nth(n)
            .map(|b| visible(&b.label))
    }

    /// The focused field with a history: its place and list.
    fn focused_history(&mut self) -> Option<(usize, usize, String)> {
        match self.focus()? {
            Target::Elem(r, e) => match &self.elem(r, e)?.kind {
                Kind::Input {
                    history: Some(list),
                    readonly: false,
                    disabled: false,
                    ..
                } => Some((r, e, list.clone())),
                _ => None,
            },
            _ => None,
        }
    }

    /// Opens the focused field's history list with `lines` (in the
    /// list's order); nothing to show — nothing opens.
    pub fn show_history(&mut self, lines: Vec<HistLine>) {
        let Some((r, e, _)) = self.focused_history() else {
            return;
        };
        if !lines.iter().any(|l| matches!(l, HistLine::Entry { .. })) {
            return;
        }
        let view = HistoryView::new(lines, String::new());
        self.list = Some(OpenList {
            row: r,
            elem: e,
            current: view.first_entry(),
            top: 0,
            history: Some(view),
        });
    }

    /// The open history list after a change (lock, delete): the filter
    /// stays, the cursor follows its entry (a locked one moves up) or
    /// stays at its row when the entry is gone.
    pub fn refresh_history(&mut self, lines: Vec<HistLine>) {
        let Some(list) = &mut self.list else { return };
        let Some(old) = &list.history else { return };
        let text = old.entry(list.current).map(|(t, _)| t);
        let view = HistoryView::new(lines, old.filter.clone());
        if view.shown.is_empty() && view.filter.is_empty() {
            self.list = None;
            return;
        }
        list.current = text
            .and_then(|t| {
                (0..view.shown.len()).find(|k| view.entry(*k).is_some_and(|(e, _)| e == t))
            })
            .unwrap_or(list.current.min(view.shown.len().saturating_sub(1)));
        list.history = Some(view);
    }

    /// The focused input field, for autocompletion: its text, history
    /// list, whether it holds paths or a command, and where it is.
    pub fn focused_field(&mut self) -> Option<FocusedField> {
        let rect = self.focused_rect;
        match self.focus()? {
            Target::Elem(r, e) => match &self.elem(r, e)?.kind {
                Kind::Input {
                    value,
                    cursor,
                    history,
                    path,
                    exec,
                    readonly: false,
                    disabled: false,
                    ..
                } => Some(FocusedField {
                    value: value.clone(),
                    history: history.clone(),
                    path: *path,
                    exec: *exec,
                    rect,
                    at_end: *cursor >= value.chars().count(),
                }),
                _ => None,
            },
            _ => None,
        }
    }

    /// Sets the focused field's text (a history entry), cursor at the end.
    pub fn set_focused_input(&mut self, text: &str) {
        if let Some(Target::Elem(r, e)) = self.focus()
            && let Some(Elem {
                kind:
                    Kind::Input {
                        value,
                        cursor,
                        unchanged,
                        ..
                    },
                ..
            }) = self.elem_mut(r, e)
        {
            *value = text.to_string();
            *cursor = value.chars().count();
            *unchanged = false;
        }
    }

    /// Puts `text` into the `n`-th input field at its cursor (untouched
    /// text is replaced, as typing would) and focuses the field.
    pub fn insert_input(&mut self, n: usize, text: &str) {
        let mut k = 0;
        let mut at = None;
        'rows: for (r, row) in self.rows.iter_mut().enumerate() {
            let Row::Items(elems) = row else { continue };
            for (ei, e) in elems.iter_mut().enumerate() {
                if let Kind::Input {
                    value,
                    cursor,
                    unchanged,
                    ..
                } = &mut e.kind
                {
                    if k == n {
                        if *unchanged {
                            value.clear();
                            *cursor = 0;
                        }
                        let b = value
                            .char_indices()
                            .nth(*cursor)
                            .map_or(value.len(), |(i, _)| i);
                        value.insert_str(b, text);
                        *cursor += text.chars().count();
                        *unchanged = false;
                        at = Some((r, ei));
                        break 'rows;
                    }
                    k += 1;
                }
            }
        }
        if let Some((r, e)) = at {
            self.set_focus(Target::Elem(r, e));
        }
    }

    /// Sets the `n`-th input field's text (the cursor at its end).
    pub fn set_input_value(&mut self, n: usize, text: &str) {
        let mut k = 0;
        for row in &mut self.rows {
            let Row::Items(elems) = row else { continue };
            for e in elems {
                if let Kind::Input {
                    value,
                    cursor,
                    unchanged,
                    ..
                } = &mut e.kind
                {
                    if k == n {
                        *value = text.to_string();
                        *cursor = value.chars().count();
                        *unchanged = false;
                        return;
                    }
                    k += 1;
                }
            }
        }
    }

    pub fn input_value(&self, n: usize) -> String {
        self.kinds()
            .into_iter()
            .filter_map(|k| match k {
                Kind::Input { value, .. } => Some(value.clone()),
                _ => None,
            })
            .nth(n)
            .unwrap_or_default()
    }

    /// The `n`-th check box: on, off, or `None` — "?" (three-state).
    pub fn check_state(&self, n: usize) -> Option<bool> {
        self.kinds()
            .into_iter()
            .filter_map(|k| match k {
                Kind::Check { checked, mixed, .. } => {
                    Some((mixed != &Some(true)).then_some(*checked))
                }
                _ => None,
            })
            .nth(n)
            .flatten()
    }

    /// State of the `n`-th check box.
    pub fn checked(&self, n: usize) -> bool {
        self.kinds()
            .into_iter()
            .filter_map(|k| match k {
                Kind::Check { checked, .. } => Some(*checked),
                _ => None,
            })
            .nth(n)
            .unwrap_or(false)
    }

    /// Index of the selected button in the `n`-th radio group (in order of
    /// appearance).
    pub fn radio(&self, n: usize) -> usize {
        let mut groups: Vec<u16> = Vec::new();
        let mut counts: Vec<usize> = Vec::new();
        for k in self.kinds() {
            if let Kind::Radio {
                group, selected, ..
            } = k
            {
                let gi = match groups.iter().position(|g| g == group) {
                    Some(i) => i,
                    None => {
                        groups.push(*group);
                        counts.push(0);
                        groups.len() - 1
                    }
                };
                if gi == n && *selected {
                    return counts[gi];
                }
                counts[gi] += 1;
            }
        }
        0
    }

    /// Selected item of the `n`-th combo box.
    pub fn combo(&self, n: usize) -> usize {
        self.kinds()
            .into_iter()
            .filter_map(|k| match k {
                Kind::Combo { selected, .. } => Some(*selected),
                _ => None,
            })
            .nth(n)
            .unwrap_or(0)
    }

    // ------------------------------------------------------------- focus

    fn targets(&self) -> Vec<Target> {
        let mut out = Vec::new();
        let mut number = 0;
        for (r, row) in self.rows.iter().enumerate() {
            match row {
                Row::Items(elems) => {
                    for (e, elem) in elems.iter().enumerate() {
                        let focusable = match &elem.kind {
                            Kind::Text { .. } => false,
                            Kind::Input {
                                disabled, readonly, ..
                            } => !disabled && !readonly,
                            Kind::Check { disabled, .. }
                            | Kind::Radio { disabled, .. }
                            | Kind::Combo { disabled, .. }
                            | Kind::Button { disabled, .. } => !disabled,
                        };
                        if focusable {
                            out.push(Target::Elem(r, e));
                        }
                    }
                }
                Row::Buttons(buttons) => {
                    for (b, button) in buttons.iter().enumerate() {
                        if !button.disabled && !button.hidden {
                            out.push(Target::Button(r, b, number));
                        }
                        number += 1;
                    }
                }
                Row::Separator | Row::Caption(_) => {}
            }
        }
        out
    }

    /// The number of the item button at row `r`, element `e` (counted
    /// among the item buttons), if it is one.
    fn item_button(&self, r: usize, e: usize) -> Option<usize> {
        if !matches!(self.elem(r, e)?.kind, Kind::Button { .. }) {
            return None;
        }
        let mut n = 0;
        for (ri, row) in self.rows.iter().enumerate() {
            if let Row::Items(elems) = row {
                for (ei, elem) in elems.iter().enumerate() {
                    if (ri, ei) == (r, e) {
                        return Some(n);
                    }
                    if matches!(elem.kind, Kind::Button { .. }) {
                        n += 1;
                    }
                }
            }
        }
        None
    }

    fn default_button(&self) -> Option<usize> {
        let mut number = 0;
        for row in &self.rows {
            if let Row::Buttons(buttons) = row {
                for b in buttons {
                    if b.default {
                        return Some(number);
                    }
                    number += 1;
                }
            }
        }
        None
    }

    fn elem(&self, r: usize, e: usize) -> Option<&Elem> {
        match self.rows.get(r)? {
            Row::Items(elems) => elems.get(e),
            _ => None,
        }
    }

    fn elem_mut(&mut self, r: usize, e: usize) -> Option<&mut Elem> {
        match self.rows.get_mut(r)? {
            Row::Items(elems) => elems.get_mut(e),
            _ => None,
        }
    }

    fn is_input(&self, t: Target) -> bool {
        matches!(t, Target::Elem(r, e) if matches!(self.elem(r, e).map(|e| &e.kind), Some(Kind::Input { .. })))
    }

    /// The focused item; on first use, like Far: the first input field,
    /// otherwise the first other control, otherwise the default button.
    fn focus(&mut self) -> Option<Target> {
        let targets = self.targets();
        if self.focus.is_none() {
            let default = self.default_button();
            self.focus = targets
                .iter()
                .position(|t| self.is_input(*t))
                .or_else(|| targets.iter().position(|t| matches!(t, Target::Elem(..))))
                .or_else(|| {
                    targets
                        .iter()
                        .position(|t| matches!(t, Target::Button(_, _, n) if Some(*n) == default))
                })
                .or((!targets.is_empty()).then_some(0));
        }
        self.focus.and_then(|f| targets.get(f).copied())
    }

    fn move_focus(&mut self, delta: isize) {
        let n = self.targets().len() as isize;
        if n == 0 {
            return;
        }
        self.focus();
        let cur = self.focus.unwrap_or(0) as isize;
        self.focus = Some((cur + delta).rem_euclid(n) as usize);
    }

    fn set_focus(&mut self, target: Target) {
        if let Some(i) = self.targets().iter().position(|t| *t == target) {
            self.focus = Some(i);
        }
    }

    fn select_radio(&mut self, r: usize, e: usize) {
        let Some(Kind::Radio { group, .. }) = self.elem(r, e).map(|e| &e.kind) else {
            return;
        };
        let group = *group;
        for (ri, row) in self.rows.iter_mut().enumerate() {
            if let Row::Items(elems) = row {
                for (ei, elem) in elems.iter_mut().enumerate() {
                    if let Kind::Radio {
                        group: g, selected, ..
                    } = &mut elem.kind
                        && *g == group
                    {
                        *selected = ri == r && ei == e;
                    }
                }
            }
        }
    }

    /// Activates the item with hotkey `key` (a Latin letter).
    fn activate_hotkey(&mut self, key: char) -> Option<Outcome> {
        let targets = self.targets();
        let mut number = 0usize;
        let mut found: Option<(usize, usize, bool)> = None; // row, elem, is text
        'rows: for (r, row) in self.rows.iter().enumerate() {
            match row {
                Row::Items(elems) => {
                    for (e, elem) in elems.iter().enumerate() {
                        let (label, is_text) = match &elem.kind {
                            Kind::Text {
                                text,
                                show_amp: false,
                                ..
                            } => (text, true),
                            Kind::Check {
                                label,
                                disabled: false,
                                ..
                            }
                            | Kind::Radio {
                                label,
                                disabled: false,
                                ..
                            }
                            | Kind::Button {
                                label,
                                disabled: false,
                            } => (label, false),
                            _ => continue,
                        };
                        if hotkey(label) == Some(key) {
                            found = Some((r, e, is_text));
                            break 'rows;
                        }
                    }
                }
                Row::Buttons(buttons) => {
                    for b in buttons {
                        if !b.disabled && !b.hidden && hotkey(&b.label) == Some(key) {
                            return Some(Outcome::Closed(Some(number)));
                        }
                        number += 1;
                    }
                }
                Row::Separator | Row::Caption(_) => {}
            }
        }
        let (r, e, is_text) = found?;
        if is_text {
            // A label: focus the next focusable item after it.
            if let Some(next) = targets
                .iter()
                .position(|t| matches!(*t, Target::Elem(tr, te) if (tr, te) > (r, e)))
            {
                self.focus = Some(next);
            }
            return Some(Outcome::Pending);
        }
        if let Some(n) = self.item_button(r, e) {
            return Some(Outcome::Pressed(n));
        }
        self.set_focus(Target::Elem(r, e));
        if matches!(self.elem(r, e).map(|e| &e.kind), Some(Kind::Radio { .. })) {
            self.select_radio(r, e);
        } else if let Some(Elem {
            kind: Kind::Check { checked, mixed, .. },
            ..
        }) = self.elem_mut(r, e)
        {
            toggle_check(checked, mixed);
        }
        Some(Outcome::Pending)
    }

    // ------------------------------------------------------------- input

    pub fn handle_key(&mut self, key: &KeyEvent) -> Outcome {
        let outcome = self.handle_key_inner(key);
        self.sync_links();
        outcome
    }

    fn handle_key_inner(&mut self, key: &KeyEvent) -> Outcome {
        if self.list.is_some() {
            return self.list_key(key);
        }
        let focus = self.focus();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        // A field with a history: Ctrl+Up / Ctrl+Down open the list,
        // Ctrl+End goes round the entries starting with the typed text.
        let cycle = self.cycle_prefix.take();
        if ctrl
            && !alt
            && let Some((r, e, list)) = self.focused_history()
        {
            match key.code {
                KeyCode::Up | KeyCode::Down => {
                    return Outcome::History(HistoryRequest::Open { list });
                }
                KeyCode::End => {
                    let value = match &self.elem(r, e).map(|e| &e.kind) {
                        Some(Kind::Input { value, .. }) => value.clone(),
                        _ => String::new(),
                    };
                    let prefix = cycle.unwrap_or_else(|| value.clone());
                    self.cycle_prefix = Some(prefix.clone());
                    return Outcome::History(HistoryRequest::Next {
                        list,
                        prefix,
                        after: value,
                    });
                }
                _ => {}
            }
        }
        let on_input = focus.is_some_and(|t| self.is_input(t));
        // Hotkeys: Alt+letter, or a plain letter when not typing in a field.
        if let KeyCode::Char(c) = key.code
            && ((alt && !ctrl) || (!alt && !ctrl && !on_input && c != ' '))
        {
            let c = c.to_lowercase().next().unwrap_or(c);
            let latin = crate::keys::latin_equivalent(c).unwrap_or(c);
            if let Some(outcome) = self.activate_hotkey(latin) {
                return outcome;
            }
            if alt {
                return Outcome::Pending;
            }
        }
        match key.code {
            KeyCode::Esc => return Outcome::Closed(None),
            KeyCode::Tab => {
                self.move_focus(1);
                return Outcome::Pending;
            }
            KeyCode::BackTab => {
                self.move_focus(-1);
                return Outcome::Pending;
            }
            KeyCode::Down if alt || ctrl => {
                if let Some(Target::Elem(r, e)) = focus {
                    self.open_list(r, e);
                }
                return Outcome::Pending;
            }
            KeyCode::Down => {
                self.move_focus(1);
                return Outcome::Pending;
            }
            KeyCode::Up => {
                self.move_focus(-1);
                return Outcome::Pending;
            }
            KeyCode::Enter | KeyCode::Char(' ')
                if let Some(Target::Elem(r, e)) = focus
                    && let Some(n) = self.item_button(r, e) =>
            {
                return Outcome::Pressed(n);
            }
            KeyCode::Enter => {
                return Outcome::Closed(match focus {
                    Some(Target::Button(_, _, n)) => Some(n),
                    _ => self.default_button(),
                });
            }
            _ => {}
        }
        match focus {
            Some(Target::Button(_, _, n)) => match key.code {
                KeyCode::Char(' ') => return Outcome::Closed(Some(n)),
                KeyCode::Left => self.move_focus(-1),
                KeyCode::Right => self.move_focus(1),
                _ => {}
            },
            Some(Target::Elem(r, e)) => self.elem_key(r, e, key),
            Some(Target::Arrow(..)) | None => {}
        }
        Outcome::Pending
    }

    fn elem_key(&mut self, r: usize, e: usize, key: &KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match self.elem(r, e).map(|e| &e.kind) {
            Some(Kind::Radio { .. }) => {
                match key.code {
                    KeyCode::Char(' ') => self.select_radio(r, e),
                    KeyCode::Left => self.move_focus(-1),
                    KeyCode::Right => self.move_focus(1),
                    _ => {}
                }
                return;
            }
            Some(Kind::Combo { .. }) => {
                if matches!(key.code, KeyCode::F(4) | KeyCode::Char(' ')) {
                    self.open_list(r, e);
                }
                return;
            }
            _ => {}
        }
        let Some(elem) = self.elem_mut(r, e) else {
            return;
        };
        match &mut elem.kind {
            Kind::Check { checked, mixed, .. } => {
                if key.code == KeyCode::Char(' ') {
                    toggle_check(checked, mixed);
                }
            }
            Kind::Input {
                value,
                cursor,
                unchanged,
                ..
            } => {
                let typing = matches!(key.code, KeyCode::Char(_)) && !(ctrl ^ alt);
                // Typing over untouched text replaces it.
                if *unchanged && typing {
                    value.clear();
                    *cursor = 0;
                }
                *unchanged = false;
                let len = value.chars().count();
                let byte = |s: &str, c: usize| s.char_indices().nth(c).map_or(s.len(), |(i, _)| i);
                match key.code {
                    // Ctrl+Alt is AltGr on many layouts.
                    KeyCode::Char(c) if !(ctrl ^ alt) => {
                        let at = byte(value, *cursor);
                        value.insert(at, c);
                        *cursor += 1;
                    }
                    KeyCode::Char('y') if ctrl => {
                        value.clear();
                        *cursor = 0;
                    }
                    KeyCode::Backspace if *cursor > 0 => {
                        let at = byte(value, *cursor - 1);
                        value.remove(at);
                        *cursor -= 1;
                    }
                    KeyCode::Delete if *cursor < len => {
                        let at = byte(value, *cursor);
                        value.remove(at);
                    }
                    KeyCode::Left => *cursor = cursor.saturating_sub(1),
                    KeyCode::Right => *cursor = (*cursor + 1).min(len),
                    KeyCode::Home => *cursor = 0,
                    KeyCode::End => *cursor = len,
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn open_list(&mut self, r: usize, e: usize) {
        if let Some(Elem {
            kind:
                Kind::Combo {
                    selected,
                    disabled: false,
                    ..
                },
            ..
        }) = self.elem(r, e)
        {
            self.list = Some(OpenList {
                row: r,
                elem: e,
                current: *selected,
                top: 0,
                history: None,
            });
        }
    }

    fn list_items(&self) -> Option<Vec<Option<String>>> {
        let list = self.list.as_ref()?;
        if let Some(view) = &list.history {
            return Some(
                (0..view.shown.len())
                    .map(|k| view.entry(k).map(|(t, _)| t))
                    .collect(),
            );
        }
        match &self.elem(list.row, list.elem)?.kind {
            Kind::Combo { items, .. } => Some(items.clone()),
            _ => None,
        }
    }

    fn choose(&mut self, r: usize, e: usize, index: usize) {
        // A history entry goes into its field.
        if let Some(text) = self
            .list
            .as_ref()
            .and_then(|l| l.history.as_ref())
            .and_then(|h| h.entry(index))
            .map(|(t, _)| t)
        {
            self.list = None;
            self.set_focused_input(&text);
            return;
        }
        self.list = None;
        if let Some(Elem {
            kind: Kind::Combo { selected, .. },
            ..
        }) = self.elem_mut(r, e)
        {
            *selected = index;
        }
    }

    fn list_key(&mut self, key: &KeyEvent) -> Outcome {
        let (Some(items), Some(list)) = (self.list_items(), self.list.as_ref()) else {
            self.list = None;
            return Outcome::Pending;
        };
        let (r, e, current) = (list.row, list.elem, list.current);
        let step = |from: usize, delta: isize| {
            let n = items.len() as isize;
            let mut i = from as isize;
            for _ in 0..n {
                i = (i + delta).rem_euclid(n);
                if items[i as usize].is_some() {
                    return i as usize;
                }
            }
            from
        };
        // The history list's own keys (Far's history menu).
        let history = self
            .list
            .as_ref()
            .and_then(|l| l.history.as_ref())
            .map(|v| (v.entry(current), v.shown.len()));
        if let Some((entry, count)) = history {
            let list = match &self.elem(r, e).map(|e| &e.kind) {
                Some(Kind::Input {
                    history: Some(l), ..
                }) => l.clone(),
                _ => String::new(),
            };
            let shift = key.modifiers.contains(KeyModifiers::SHIFT);
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            let (text, locked) = entry.unwrap_or_default();
            let alt = key.modifiers.contains(KeyModifiers::ALT);
            // Typing filters the list; Backspace takes a character back.
            let filter_edit = match key.code {
                KeyCode::Char(c) if !(ctrl ^ alt) => Some(Some(c)),
                KeyCode::Backspace => Some(None),
                _ => None,
            };
            if let Some(edit) = filter_edit {
                if let Some(view) = self.list.as_mut().and_then(|l| l.history.as_mut()) {
                    match edit {
                        Some(c) => view.filter.push(c),
                        None => {
                            view.filter.pop();
                        }
                    }
                    view.apply();
                    let first = view.first_entry();
                    if let Some(l) = &mut self.list {
                        l.current = first;
                        l.top = 0;
                    }
                }
                return Outcome::Pending;
            }
            match key.code {
                KeyCode::Tab => {
                    self.choose(r, e, current);
                    return Outcome::Pending;
                }
                KeyCode::Insert if !ctrl => {
                    return Outcome::History(HistoryRequest::Lock {
                        list,
                        text,
                        locked: !locked,
                    });
                }
                KeyCode::Delete if shift => {
                    return Outcome::History(HistoryRequest::Delete { list, text });
                }
                KeyCode::Delete => {
                    self.list = None;
                    return Outcome::History(HistoryRequest::Clear { list });
                }
                KeyCode::Char('c') | KeyCode::Insert if ctrl => {
                    let _ = crate::clipboard::set_text(&text);
                    return Outcome::Pending;
                }
                KeyCode::PageUp | KeyCode::PageDown => {
                    let n = count.max(1);
                    let moved = if key.code == KeyCode::PageUp {
                        current.saturating_sub(LIST_ROWS - 1)
                    } else {
                        (current + LIST_ROWS - 1).min(n - 1)
                    };
                    if let Some(l) = &mut self.list {
                        l.current = moved;
                    }
                    return Outcome::Pending;
                }
                _ => {}
            }
        }
        let moved = match key.code {
            KeyCode::Esc => {
                self.list = None;
                return Outcome::Pending;
            }
            KeyCode::Enter => {
                self.choose(r, e, current);
                return Outcome::Pending;
            }
            KeyCode::Up => step(current, -1),
            KeyCode::Down => step(current, 1),
            KeyCode::Home => step(items.len() - 1, 1),
            KeyCode::End => step(0, -1),
            KeyCode::Char(c) => {
                let c = c.to_lowercase().next().unwrap_or(c);
                let latin = crate::keys::latin_equivalent(c).unwrap_or(c);
                if let Some(i) = items
                    .iter()
                    .position(|it| it.as_deref().and_then(hotkey) == Some(latin))
                {
                    self.choose(r, e, i);
                }
                return Outcome::Pending;
            }
            _ => current,
        };
        if let Some(l) = &mut self.list {
            l.current = moved;
        }
        Outcome::Pending
    }

    /// Returns `None` when the event is outside the dialog.
    ///
    /// Like Far (and Windows): pressing the button focuses an item, the
    /// action happens on release over the same item.
    pub fn handle_mouse(&mut self, ev: &MouseEvent) -> Option<Outcome> {
        let outcome = self.handle_mouse_inner(ev);
        self.sync_links();
        outcome
    }

    fn handle_mouse_inner(&mut self, ev: &MouseEvent) -> Option<Outcome> {
        let pos = Position::new(ev.column, ev.row);
        // Moving the dialog: grabbed anywhere but on its items.
        if let Some((lx, ly)) = self.drag {
            match ev.kind {
                MouseEventKind::Drag(MouseButton::Left) => {
                    self.offset.0 += i32::from(ev.column) - i32::from(lx);
                    self.offset.1 += i32::from(ev.row) - i32::from(ly);
                    self.drag = Some((ev.column, ev.row));
                }
                _ => self.drag = None,
            }
            return Some(Outcome::Pending);
        }
        // An open list closes on a press anywhere outside it, and that
        // press does nothing else.
        if self.list.is_some()
            && matches!(ev.kind, MouseEventKind::Down(_))
            && !self.list_rect.contains(pos)
        {
            self.list = None;
            self.pressed = None;
            return Some(Outcome::Pending);
        }
        let under = self.under(pos);
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.pressed = under;
                match under {
                    Some(Pressed::Target(target, rect)) => {
                        self.set_focus(target);
                        // The text cursor goes where the field was pressed.
                        if let Target::Elem(r, e) = target
                            && let Some(Elem {
                                kind:
                                    Kind::Input {
                                        value,
                                        cursor,
                                        unchanged,
                                        ..
                                    },
                                ..
                            }) = self.elem_mut(r, e)
                        {
                            *unchanged = false;
                            *cursor = usize::from(ev.column - rect.x).min(value.chars().count());
                        }
                    }
                    Some(Pressed::ListItem(_)) => {}
                    None if self.list.is_none() && self.outer.contains(pos) => {
                        self.drag = Some((ev.column, ev.row));
                    }
                    None if !self.outer.contains(pos) && self.list.is_none() => return None,
                    None => {}
                }
                Some(Outcome::Pending)
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let pressed = self.pressed.take();
                let same = match (pressed, under) {
                    (Some(Pressed::Target(a, _)), Some(Pressed::Target(b, _))) => a == b,
                    (Some(Pressed::ListItem(a)), Some(Pressed::ListItem(b))) => a == b,
                    _ => false,
                };
                if !same {
                    return Some(Outcome::Pending);
                }
                Some(match under {
                    Some(Pressed::Target(target, _)) => self.activate(target),
                    Some(Pressed::ListItem(index)) => {
                        if let Some(list) = &self.list {
                            let (r, e) = (list.row, list.elem);
                            self.choose(r, e, index);
                        }
                        Outcome::Pending
                    }
                    None => Outcome::Pending,
                })
            }
            _ if under.is_some() || self.outer.contains(pos) || self.list_rect.contains(pos) => {
                Some(Outcome::Pending)
            }
            _ => None,
        }
    }

    /// What is under the mouse: an item of the open list, or a dialog item.
    fn under(&self, pos: Position) -> Option<Pressed> {
        if self.list.is_some() && self.list_rect.contains(pos) {
            // The first hit area is the list's items; the frame is nothing.
            let (items, _) = self.hits.first()?;
            if !items.contains(pos) {
                return None;
            }
            let top = self.list.as_ref().map_or(0, |l| l.top);
            let index = top + usize::from(pos.y - items.y);
            return self
                .list_items()
                .is_some_and(|it| it.get(index).is_some_and(Option::is_some))
                .then_some(Pressed::ListItem(index));
        }
        self.hits
            .iter()
            .skip(usize::from(self.list.is_some()))
            .find(|(r, _)| r.contains(pos))
            .map(|(r, t)| Pressed::Target(*t, *r))
    }

    /// A click (press and release) on an item.
    fn activate(&mut self, target: Target) -> Outcome {
        match target {
            Target::Button(_, _, n) => Outcome::Closed(Some(n)),
            Target::Arrow(r, e) => {
                self.set_focus(Target::Elem(r, e));
                match self.focused_history() {
                    Some((_, _, list)) => Outcome::History(HistoryRequest::Open { list }),
                    None => Outcome::Pending,
                }
            }
            Target::Elem(r, e) => {
                if let Some(n) = self.item_button(r, e) {
                    return Outcome::Pressed(n);
                }
                match self.elem(r, e).map(|e| &e.kind) {
                    Some(Kind::Radio { .. }) => self.select_radio(r, e),
                    Some(Kind::Combo { .. }) => self.open_list(r, e),
                    Some(Kind::Check { .. }) => {
                        if let Some(Elem {
                            kind: Kind::Check { checked, mixed, .. },
                            ..
                        }) = self.elem_mut(r, e)
                        {
                            toggle_check(checked, mixed);
                        }
                    }
                    _ => {}
                }
                Outcome::Pending
            }
        }
    }

    // -------------------------------------------------------------- draw

    /// Draws the dialog centred in `area`; returns the text cursor.
    pub fn draw(&mut self, area: Rect, buf: &mut Buffer) -> Option<Position> {
        self.draw_in(area, area, buf)
    }

    /// Draws the dialog centred in `parent` (its window), within `area`.
    pub fn draw_in(&mut self, parent: Rect, area: Rect, buf: &mut Buffer) -> Option<Position> {
        let c = if self.warning { &WARNING } else { &NORMAL };
        let w = self.width.min(area.width);
        let h = (self.rows.len() as u16 + 4).min(area.height);
        // Centred in the parent, moved by the user, kept on the screen.
        let cx = centred(parent.x, parent.width, area.x, area.width, w);
        let cy = centred(parent.y, parent.height, area.y, area.height, h);
        let clamp = |centre: u16, delta: i32, start: u16, room: u16| {
            (i32::from(centre) + delta).clamp(i32::from(start), i32::from(start + room)) as u16
        };
        let x0 = clamp(cx, self.offset.0, area.x, area.width.saturating_sub(w));
        let y0 = clamp(cy, self.offset.1, area.y, area.height.saturating_sub(h));
        // Remember the clamped offset so dragging past the edge does not
        // accumulate.
        self.offset = (i32::from(x0) - i32::from(cx), i32::from(y0) - i32::from(cy));
        let outer = Rect::new(x0, y0, w, h);
        self.outer = outer;

        draw_shadow(buf, outer, area);
        buf.set_style(outer, c.body);
        for yy in outer.top()..outer.bottom() {
            for xx in outer.left()..outer.right() {
                buf[(xx, yy)].set_symbol(" ");
            }
        }
        let frame = Rect::new(x0 + 3, y0 + 1, w.saturating_sub(6), h.saturating_sub(2));
        draw_frame(buf, frame, c.box_);
        let title = format!(" {} ", visible(&self.title));
        let title: String = title
            .chars()
            .take(usize::from(frame.width.saturating_sub(2)))
            .collect();
        let tx = frame.x + frame.width.saturating_sub(width_of(&title)) / 2;
        put_plain(buf, tx, frame.y, width_of(&title), &title, c.box_);

        let focus = self.focus();
        let mut cursor = None;
        self.hits.clear();
        let mut number = 0usize;
        let max_x = x0 + w.saturating_sub(4);
        let rows = std::mem::take(&mut self.rows);
        for (r, row) in rows.iter().enumerate() {
            let y = y0 + 2 + r as u16;
            if y + 1 >= frame.bottom() || y >= area.bottom() {
                break;
            }
            match row {
                Row::Separator | Row::Caption(_) => {
                    for xx in frame.x + 1..frame.right().saturating_sub(1) {
                        buf[(xx, y)].set_symbol("─").set_style(c.box_);
                    }
                    buf[(frame.x, y)].set_symbol("╟").set_style(c.box_);
                    buf[(frame.right() - 1, y)]
                        .set_symbol("╢")
                        .set_style(c.box_);
                    if let Row::Caption(text) = row {
                        let text = format!(" {text} ");
                        let len = width_of(&text);
                        put_plain(buf, x0 + w.saturating_sub(len) / 2, y, len, &text, c.box_);
                    }
                }
                Row::Items(elems) => {
                    for (e, elem) in elems.iter().enumerate() {
                        let focused = focus == Some(Target::Elem(r, e));
                        let target = Target::Elem(r, e);
                        if let Some(pos) =
                            self.draw_elem(buf, c, (x0, y, max_x, w), elem, focused, target)
                        {
                            cursor = Some(pos);
                        }
                    }
                }
                Row::Buttons(buttons) => {
                    let texts: Vec<String> = buttons
                        .iter()
                        .map(|b| {
                            if b.default {
                                format!("{{ {} }}", b.label)
                            } else {
                                format!("[ {} ]", b.label)
                            }
                        })
                        .collect();
                    let total: u16 = buttons
                        .iter()
                        .zip(&texts)
                        .filter(|(b, _)| !b.hidden)
                        .map(|(_, t)| width_of(&visible(t)) + 1)
                        .sum::<u16>()
                        .saturating_sub(1);
                    let mut bx = x0 + w.saturating_sub(total) / 2;
                    for (bi, (b, t)) in buttons.iter().zip(&texts).enumerate() {
                        let n = number;
                        number += 1;
                        if b.hidden {
                            continue;
                        }
                        let target = Target::Button(r, bi, n);
                        let focused = focus == Some(target);
                        let (style, hot) = if b.disabled {
                            (c.disabled, c.disabled)
                        } else if focused {
                            (c.button_focused, c.button_focused_highlight)
                        } else {
                            (c.body, c.highlight)
                        };
                        let len = width_of(&visible(t));
                        put_label(buf, bx, y, len, t, style, hot);
                        self.hits.push((Rect::new(bx, y, len, 1), target));
                        if focused {
                            cursor = Some(Position::new(bx + 2, y));
                        }
                        bx += len + 1;
                    }
                }
            }
        }
        self.rows = rows;
        if self.draw_list(buf, x0, y0, area) {
            return None;
        }
        self.list_rect = Rect::default();
        cursor
    }

    /// `at`: dialog x, row y, right limit, dialog width.
    fn draw_elem(
        &mut self,
        buf: &mut Buffer,
        c: &Colors,
        at: (u16, u16, u16, u16),
        elem: &Elem,
        focused: bool,
        target: Target,
    ) -> Option<Position> {
        let (x0, y, max_x, w) = at;
        let x = |len: u16| match elem.x {
            X::At(x) => x0 + x,
            X::Center => x0 + w.saturating_sub(len) / 2,
        };
        let dim_edit = theme::DIALOG_EDIT_UNCHANGED.fg(Color::DarkGray);
        match &elem.kind {
            Kind::Text {
                text,
                show_amp,
                highlight,
            } => {
                let style = if *highlight { c.highlight } else { c.body };
                if *show_amp {
                    let at = x(width_of(text));
                    put_plain(buf, at, y, max_x.saturating_sub(at), text, style);
                } else {
                    let at = x(width_of(&visible(text)));
                    put_label(
                        buf,
                        at,
                        y,
                        max_x.saturating_sub(at),
                        text,
                        style,
                        c.highlight,
                    );
                }
                None
            }
            Kind::Input {
                value,
                cursor: cur,
                unchanged,
                width,
                history,
                readonly,
                disabled,
                ..
            } => {
                let at = x(*width);
                let skip = cur.saturating_sub(usize::from(*width).saturating_sub(1));
                let shown: String = value.chars().skip(skip).collect();
                let style = if *readonly {
                    c.body
                } else if *disabled {
                    dim_edit
                } else if *unchanged {
                    theme::DIALOG_EDIT_UNCHANGED
                } else {
                    theme::DIALOG_EDIT
                };
                for i in 0..*width {
                    buf[(at + i, y)].set_symbol(" ").set_style(style);
                }
                put_plain(buf, at, y, *width, &shown, style);
                // The ghost after the text, as far as the field goes.
                if focused
                    && !*unchanged
                    && *cur >= value.chars().count()
                    && let Some((base, rest)) = self.ghost.as_deref()
                    && base == value
                {
                    let gx = (cur - skip) as u16;
                    if gx < *width {
                        put_plain(buf, at + gx, y, width - gx, rest, theme::GHOST_EDIT);
                    }
                }
                if history.is_some() && !*readonly {
                    buf[(at + width, y)].set_symbol("↓").set_style(c.body);
                    if let Target::Elem(r, e) = target {
                        self.hits
                            .push((Rect::new(at + width, y, 1, 1), Target::Arrow(r, e)));
                    }
                }
                if !*readonly {
                    self.hits.push((Rect::new(at, y, *width, 1), target));
                }
                if focused {
                    self.focused_rect = Rect::new(at, y, *width, 1);
                }
                (focused && !*readonly).then(|| Position::new(at + (cur - skip) as u16, y))
            }
            Kind::Check {
                label,
                checked,
                disabled,
                ..
            }
            | Kind::Radio {
                label,
                selected: checked,
                disabled,
                ..
            } => {
                let radio = matches!(elem.kind, Kind::Radio { .. });
                let unknown = matches!(
                    elem.kind,
                    Kind::Check {
                        mixed: Some(true),
                        ..
                    }
                );
                let mark = match (radio, *checked) {
                    _ if unknown => "[?] ",
                    (false, true) => "[x] ",
                    (false, false) => "[ ] ",
                    (true, true) => "(•) ",
                    (true, false) => "( ) ",
                };
                let len = width_of(&visible(label)) + 4;
                let at = x(len);
                let (style, hot) = if *disabled {
                    (c.disabled, c.disabled)
                } else {
                    (c.body, c.highlight)
                };
                put_plain(buf, at, y, 4, mark, style);
                put_label(
                    buf,
                    at + 4,
                    y,
                    max_x.saturating_sub(at + 4),
                    label,
                    style,
                    hot,
                );
                self.hits.push((Rect::new(at, y, len, 1), target));
                focused.then(|| Position::new(at + 1, y))
            }
            Kind::Combo {
                items,
                selected,
                width,
                disabled,
            } => {
                let at = x(*width);
                let text = items
                    .get(*selected)
                    .and_then(|i| i.as_deref())
                    .map(visible)
                    .unwrap_or_default();
                let style = if *disabled {
                    dim_edit
                } else if focused {
                    theme::DIALOG_EDIT_SELECTED
                } else {
                    theme::DIALOG_EDIT
                };
                for i in 0..*width {
                    buf[(at + i, y)].set_symbol(" ").set_style(style);
                }
                put_plain(buf, at, y, *width, &text, style);
                buf[(at + width, y)].set_symbol("↓").set_style(c.body);
                self.hits.push((Rect::new(at, y, *width + 1, 1), target));
                focused.then(|| Position::new(at, y))
            }
            Kind::Button { label, disabled } => {
                let t = format!("[ {label} ]");
                let len = width_of(&visible(&t));
                let at = x(len);
                let (style, hot) = if *disabled {
                    (c.disabled, c.disabled)
                } else if focused {
                    (c.button_focused, c.button_focused_highlight)
                } else {
                    (c.body, c.highlight)
                };
                put_label(buf, at, y, len, &t, style, hot);
                self.hits.push((Rect::new(at, y, len, 1), target));
                focused.then(|| Position::new(at + 2, y))
            }
        }
    }

    /// The open drop-down list, below its combo box; returns whether one
    /// is open.
    fn draw_list(&mut self, buf: &mut Buffer, x0: u16, y0: u16, area: Rect) -> bool {
        let Some(list) = &self.list else {
            return false;
        };
        let (r, e, current, top) = (list.row, list.elem, list.current, list.top);
        // A history's lines as shown (marks, details, titles, filter).
        let view: Option<(ShownLines, String)> = list.history.as_ref().map(|v| {
            (
                v.shown
                    .iter()
                    .map(|(i, m)| (v.lines[*i].clone(), m.clone()))
                    .collect(),
                v.filter.clone(),
            )
        });
        // A combo's list is as wide as the combo; a history's, as the field
        // and the arrow, but at least 21 (Far).
        let (fx, list_w) = match self.elem(r, e) {
            Some(Elem {
                x: X::At(fx),
                kind: Kind::Combo { width, .. },
            }) => (*fx, *width + 1),
            Some(Elem {
                x: X::At(fx),
                kind: Kind::Input { width, .. },
            }) => (*fx, (*width + 1).max(21)),
            _ => return false,
        };
        let Some(items) = self.list_items() else {
            return false;
        };
        let shown = items.len().min(LIST_ROWS);
        // Keep the cursor on the page.
        let top = if current < top {
            current
        } else if current >= top + shown {
            current + 1 - shown
        } else {
            top
        }
        .min(items.len() - shown);
        if let Some(l) = &mut self.list {
            l.top = top;
        }
        let h = shown as u16 + 2;
        let field_y = y0 + 2 + r as u16;
        let lx = x0 + fx;
        // Below the field; above it when there is no room below.
        let ly = if field_y + 1 + h > area.bottom() && field_y >= area.y + h {
            field_y - h
        } else {
            field_y + 1
        };
        let rect = Rect::new(
            lx,
            ly,
            list_w.min(area.right().saturating_sub(lx)),
            h.min(area.bottom().saturating_sub(ly)),
        );
        self.list_rect = rect;
        if rect.width < 3 || rect.height < 3 {
            return true;
        }
        buf.set_style(rect, theme::COMBO_TEXT);
        for yy in rect.top()..rect.bottom() {
            for xx in rect.left()..rect.right() {
                buf[(xx, yy)].set_symbol(" ");
            }
        }
        for xx in rect.left()..rect.right() {
            buf[(xx, rect.top())].set_symbol("─");
            buf[(xx, rect.bottom() - 1)].set_symbol("─");
        }
        for yy in rect.top()..rect.bottom() {
            buf[(rect.left(), yy)].set_symbol("│");
            buf[(rect.right() - 1, yy)].set_symbol("│");
        }
        buf[(rect.left(), rect.top())].set_symbol("┌");
        buf[(rect.right() - 1, rect.top())].set_symbol("┐");
        buf[(rect.left(), rect.bottom() - 1)].set_symbol("└");
        buf[(rect.right() - 1, rect.bottom() - 1)].set_symbol("┘");
        let inner = rect.width.saturating_sub(2);
        for (k, item) in items.iter().enumerate().skip(top).take(shown) {
            let y = rect.y + 1 + (k - top) as u16;
            if y + 1 >= rect.bottom() {
                break;
            }
            let line = view.as_ref().and_then(|(lines, _)| lines.get(k));
            match (item, line) {
                (None, _) => {
                    buf[(rect.left(), y)].set_symbol("├");
                    buf[(rect.right() - 1, y)].set_symbol("┤");
                    for xx in rect.left() + 1..rect.right() - 1 {
                        buf[(xx, y)].set_symbol("─");
                    }
                    // A group's title in the middle of the separator.
                    if let Some((HistLine::Title(title), _)) = line {
                        let t = format!(" {title} ");
                        let w = (t.chars().count() as u16).min(inner);
                        let x = rect.x + 1 + (inner - w) / 2;
                        buf.set_stringn(x, y, &t, usize::from(w), theme::COMBO_TEXT);
                    }
                }
                (Some(text), None) => {
                    let (style, hot) = if k == current {
                        (theme::COMBO_SELECTED, theme::COMBO_SELECTED_HIGHLIGHT)
                    } else {
                        (theme::COMBO_TEXT, theme::COMBO_HIGHLIGHT)
                    };
                    for xx in rect.left() + 1..rect.right() - 1 {
                        buf[(xx, y)].set_symbol(" ").set_style(style);
                    }
                    put_label(
                        buf,
                        rect.x + 2,
                        y,
                        inner.saturating_sub(1),
                        text,
                        style,
                        hot,
                    );
                }
                (Some(_), Some((entry, marks))) => {
                    let HistLine::Entry {
                        text,
                        locked,
                        agent,
                        missing,
                        detail,
                    } = entry
                    else {
                        continue;
                    };
                    let selected = k == current;
                    let (style, hot) = match (selected, missing) {
                        (true, _) => (theme::COMBO_SELECTED, theme::COMBO_SELECTED_HIGHLIGHT),
                        (false, true) => (theme::COMBO_DISABLED, theme::COMBO_HIGHLIGHT),
                        (false, false) => (theme::COMBO_TEXT, theme::COMBO_HIGHLIGHT),
                    };
                    for xx in rect.left() + 1..rect.right() - 1 {
                        buf[(xx, y)].set_symbol(" ").set_style(style);
                    }
                    // Marks: locked, made by the agent.
                    if *locked {
                        buf[(rect.x + 1, y)].set_symbol("√");
                    } else if *agent {
                        buf[(rect.x + 1, y)].set_symbol("•");
                    }
                    // The detail on the right, when there is room.
                    let room = usize::from(inner.saturating_sub(2));
                    let dw = detail.chars().count();
                    let text_room = if dw > 0 && room > dw + 12 {
                        let dx = rect.right() - 2 - dw as u16;
                        buf.set_stringn(dx, y, detail, dw, style);
                        room - dw - 1
                    } else {
                        room
                    };
                    // The text with the filter's characters highlighted.
                    for (i, ch) in text.chars().enumerate().take(text_room) {
                        let st = if marks.contains(&i) { hot } else { style };
                        buf[(rect.x + 2 + i as u16, y)]
                            .set_symbol(&ch.to_string())
                            .set_style(st);
                    }
                }
            }
        }
        // The filter typed in the list, on its bottom frame.
        if let Some((_, filter)) = &view
            && !filter.is_empty()
        {
            let t = format!(
                " {} ",
                crate::tr!("history-filter", filter = filter.clone())
            );
            let w = (t.chars().count() as u16).min(inner);
            buf.set_stringn(
                rect.x + 1,
                rect.bottom() - 1,
                &t,
                usize::from(w),
                theme::COMBO_TEXT,
            );
        }
        // A scroll bar on the right frame when not all items fit.
        if items.len() > shown && shown >= 2 {
            let x = rect.right() - 1;
            let field = shown as u16;
            let thumb = ((top * usize::from(field)) / items.len()) as u16;
            for i in 0..field {
                let s = if i == thumb.min(field - 1) {
                    "█"
                } else {
                    "░"
                };
                buf[(x, rect.y + 1 + i)].set_symbol(s);
            }
        }
        // Clicks inside the list go to its field (first hit area).
        self.hits.insert(
            0,
            (
                Rect::new(rect.x + 1, rect.y + 1, inner, shown as u16),
                Target::Elem(r, e),
            ),
        );
        true
    }
}

/// An input field at `x`, `width` wide; `history`: the name of its
/// history list (Far's: `Copy`, `NewFolder`, `Masks`, …).
pub fn input_at(x: u16, width: u16, value: impl Into<String>, history: Option<&str>) -> Elem {
    let value = value.into();
    Elem {
        x: X::At(x),
        kind: Kind::Input {
            cursor: value.chars().count(),
            unchanged: !value.is_empty(),
            value,
            width,
            history: history.map(str::to_string),
            use_last: false,
            path: false,
            exec: false,
            readonly: false,
            disabled: false,
        },
    }
}

pub fn text_at(x: u16, text: impl Into<String>) -> Elem {
    Elem {
        x: X::At(x),
        kind: Kind::Text {
            text: text.into(),
            show_amp: false,
            highlight: false,
        },
    }
}

pub fn check_at(x: u16, label: impl Into<String>, checked: bool) -> Elem {
    Elem {
        x: X::At(x),
        kind: Kind::Check {
            label: label.into(),
            checked,
            disabled: false,
            mixed: None,
        },
    }
}

/// Space or a click on a check box: a three-state one goes
/// "?" → on → off → "?" (Far's order), a two-state one flips.
fn toggle_check(checked: &mut bool, mixed: &mut Option<bool>) {
    match mixed {
        Some(true) => {
            *mixed = Some(false);
            *checked = true;
        }
        Some(false) if *checked => *checked = false,
        Some(false) => *mixed = Some(true),
        None => *checked = !*checked,
    }
}

pub fn radio_at(x: u16, label: impl Into<String>, selected: bool, group: u16) -> Elem {
    Elem {
        x: X::At(x),
        kind: Kind::Radio {
            label: label.into(),
            selected,
            group,
            disabled: false,
        },
    }
}

/// A button among the items that leaves the dialog open
/// (`Outcome::Pressed`).
pub fn button_at(x: u16, label: impl Into<String>) -> Elem {
    Elem {
        x: X::At(x),
        kind: Kind::Button {
            label: label.into(),
            disabled: false,
        },
    }
}

/// The width of an item button with this label.
pub fn button_width(label: &str) -> u16 {
    width_of(&visible(label)) + 4
}

pub fn combo_at(x: u16, width: u16, items: Vec<Option<String>>, selected: usize) -> Elem {
    Elem {
        x: X::At(x),
        kind: Kind::Combo {
            items,
            selected,
            width,
            disabled: false,
        },
    }
}

impl Elem {
    /// Greyed out and not focusable.
    /// A three-state check box, showing "?" when `unknown`.
    pub fn three_state(mut self, unknown: bool) -> Self {
        if let Kind::Check { mixed, .. } = &mut self.kind {
            *mixed = Some(unknown);
        }
        self
    }

    pub fn disabled(mut self) -> Self {
        match &mut self.kind {
            Kind::Input { disabled, .. }
            | Kind::Check { disabled, .. }
            | Kind::Radio { disabled, .. }
            | Kind::Combo { disabled, .. }
            | Kind::Button { disabled, .. } => *disabled = true,
            Kind::Text { .. } => {}
        }
        self
    }

    /// A field for paths: completion offers files and folders.
    pub fn path(mut self) -> Self {
        if let Kind::Input { path, .. } = &mut self.kind {
            *path = true;
        }
        self
    }

    /// A field for a command: completion offers files and programs.
    pub fn exec(mut self) -> Self {
        if let Kind::Input { path, exec, .. } = &mut self.kind {
            *path = true;
            *exec = true;
        }
        self
    }

    /// An empty field starts with its history's newest entry.
    pub fn use_last(mut self) -> Self {
        if let Kind::Input { use_last, .. } = &mut self.kind {
            *use_last = true;
        }
        self
    }

    /// Text in the hotkey colour (Far highlights names this way).
    pub fn highlighted(mut self) -> Self {
        if let Kind::Text { highlight, .. } = &mut self.kind {
            *highlight = true;
        }
        self
    }

    /// Shows the text as is: `&` is not a hotkey marker.
    pub fn literal(mut self) -> Self {
        if let Kind::Text { show_amp, .. } = &mut self.kind {
            *show_amp = true;
        }
        self
    }

    /// An input field that only shows its text (looks like plain text).
    pub fn readonly(mut self) -> Self {
        if let Kind::Input {
            readonly,
            unchanged,
            ..
        } = &mut self.kind
        {
            *readonly = true;
            *unchanged = false;
        }
        self
    }

    pub fn centered(mut self) -> Self {
        self.x = X::Center;
        self
    }
}

impl Button {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            default: false,
            disabled: false,
            hidden: false,
        }
    }

    pub fn default(mut self) -> Self {
        self.default = true;
        self
    }

    pub fn disabled(mut self) -> Self {
        self.disabled = true;
        self
    }

    pub fn hidden(mut self) -> Self {
        self.hidden = true;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn alt(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT)
    }

    fn mkdir_dialog() -> Dialog {
        Dialog::far("Создание папки", 76)
            .text("Создать п&апку:")
            .input("")
            .check("Обрабатыват&ь несколько имён папок", false)
            .separator()
            .buttons(&["OK", "Отмена"], 0)
    }

    #[test]
    fn edits_input_and_closes_with_default_button() {
        let mut d = mkdir_dialog();
        for c in "новая".chars() {
            assert_eq!(d.handle_key(&key(KeyCode::Char(c))), Outcome::Pending);
        }
        d.handle_key(&key(KeyCode::Backspace));
        assert_eq!(d.input_value(0), "нова");
        assert_eq!(d.handle_key(&key(KeyCode::Enter)), Outcome::Closed(Some(0)));
    }

    #[test]
    fn navigates_to_check_box_and_buttons() {
        let mut d = mkdir_dialog();
        d.handle_key(&key(KeyCode::Tab));
        d.handle_key(&key(KeyCode::Char(' ')));
        assert!(d.checked(0));
        d.handle_key(&key(KeyCode::Tab)); // OK
        d.handle_key(&key(KeyCode::Tab)); // Отмена
        assert_eq!(d.handle_key(&key(KeyCode::Enter)), Outcome::Closed(Some(1)));
        assert_eq!(
            mkdir_dialog().handle_key(&key(KeyCode::Esc)),
            Outcome::Closed(None)
        );
    }

    #[test]
    fn hotkeys_work_in_any_layout() {
        let mut d = Dialog::far("t", 40)
            .check("Обрабатыват&ь", false)
            .buttons(&["&Копировать", "&Отменить"], 0);
        // Alt+Ь (Cyrillic layout) and Alt+M (the same key) toggle the box.
        d.handle_key(&alt('ь'));
        assert!(d.checked(0));
        d.handle_key(&alt('m'));
        assert!(!d.checked(0));
        // Alt+К presses "&Копировать"; Alt+J (the key of О) "&Отменить".
        assert_eq!(d.handle_key(&alt('к')), Outcome::Closed(Some(0)));
        assert_eq!(d.handle_key(&alt('j')), Outcome::Closed(Some(1)));
    }

    #[test]
    fn typing_replaces_untouched_text() {
        let mut d = Dialog::far("t", 40).input("old").buttons(&["OK"], 0);
        d.handle_key(&key(KeyCode::Char('n')));
        assert_eq!(d.input_value(0), "n");
        let mut d = Dialog::far("t", 40).input("old").buttons(&["OK"], 0);
        d.handle_key(&key(KeyCode::End));
        d.handle_key(&key(KeyCode::Char('!')));
        assert_eq!(d.input_value(0), "old!");
    }

    #[test]
    fn radio_groups_in_one_row_and_across_rows() {
        let mut d = Dialog::far("t", 76);
        let g = d.new_group();
        let mut d = d
            .row(vec![
                text_at(5, "П&рава доступа:"),
                radio_at(20, "По умол&чанию", true, g),
                radio_at(37, "Копироват&ь", false, g),
            ])
            .radios(&["x", "y"], 1)
            .buttons(&["OK"], 0);
        assert_eq!((d.radio(0), d.radio(1)), (0, 1));
        d.handle_key(&key(KeyCode::Right)); // the second radio of group 0
        d.handle_key(&key(KeyCode::Char(' ')));
        assert_eq!((d.radio(0), d.radio(1)), (1, 1));
    }

    #[test]
    fn combo_list_opens_and_chooses() {
        let items = vec![
            Some("&Запрос действия".to_string()),
            Some("В&место".into()),
            None,
            Some("П&ропустить".into()),
        ];
        let mut d = Dialog::far("t", 76)
            .row(vec![combo_at(29, 42, items, 0)])
            .buttons(&["OK"], 0);
        d.handle_key(&KeyEvent::new(KeyCode::Down, KeyModifiers::ALT));
        d.handle_key(&key(KeyCode::Down));
        d.handle_key(&key(KeyCode::Down)); // skips the separator
        d.handle_key(&key(KeyCode::Enter));
        assert_eq!(d.combo(0), 3);
        assert_eq!(d.handle_key(&key(KeyCode::Enter)), Outcome::Closed(Some(0)));
    }

    #[test]
    fn click_outside_closes_the_list() {
        let items = vec![Some("a".to_string()), Some("b".into())];
        let mut d = Dialog::far("t", 76)
            .row(vec![combo_at(29, 42, items, 0)])
            .buttons(&["OK"], 0);
        let area = Rect::new(0, 0, 100, 30);
        let mut buf = Buffer::empty(area);
        d.handle_key(&KeyEvent::new(KeyCode::Down, KeyModifiers::ALT));
        d.draw(area, &mut buf);
        assert!(d.list.is_some());
        // The list is white on cyan, like Far's combo boxes.
        let r = d.list_rect;
        assert_eq!(buf[(r.x + 3, r.y + 2)].style().bg, theme::COMBO_TEXT.bg);
        let click = |x, y| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        // Outside the dialog altogether.
        assert_eq!(d.handle_mouse(&click(0, 0)), Some(Outcome::Pending));
        assert!(d.list.is_none());
        // Inside the dialog but outside the list (the title row): closes.
        d.handle_key(&KeyEvent::new(KeyCode::Down, KeyModifiers::ALT));
        d.draw(area, &mut buf);
        assert_eq!(
            d.handle_mouse(&click(d.outer.x + 10, d.outer.y + 1)),
            Some(Outcome::Pending)
        );
        assert!(d.list.is_none());
        // The list covers the OK button: a click on its frame there does
        // not press OK.
        d.handle_key(&KeyEvent::new(KeyCode::Down, KeyModifiers::ALT));
        d.draw(area, &mut buf);
        let r = d.list_rect;
        assert_eq!(
            d.handle_mouse(&click(r.x + 5, r.bottom() - 1)),
            Some(Outcome::Pending)
        );
    }

    #[test]
    fn far_geometry_and_buttons() {
        // Far's make-folder dialog is 76 wide with buttons at 29..34 and
        // 36..45.
        let mut d = mkdir_dialog();
        let mut buf = Buffer::empty(Rect::new(0, 0, 76, 25));
        d.draw(Rect::new(0, 0, 76, 25), &mut buf);
        let top = (25 - (5 + 4)) / 2;
        let row = |y: u16| {
            (0..76)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        };
        assert!(row(top + 1).starts_with("   ╔"), "{}", row(top + 1));
        assert_eq!(
            row(top + 6),
            format!("   ║{:25}{{ OK }} [ Отмена ]{:26}║   ", "", "")
        );
        // The hotkey letter is highlighted.
        let y = top + 2;
        let a = (10..76).find(|&x| buf[(x, y)].symbol() == "а").unwrap();
        assert_eq!(buf[(a, y)].style().fg, theme::DIALOG_HIGHLIGHT.fg);
    }

    #[test]
    fn message_is_sized_like_far() {
        let d = Dialog::message(
            "Ошибка",
            &["Ошибка удаления файла".into(), "x".into()],
            &["&Повторить", "Отмена"],
            true,
        );
        // Buttons: (9+5) + (6+5) − 1 = 24 > 21, so W = 24 + 10.
        assert_eq!(d.width, 34);
    }

    #[test]
    fn wraps_words() {
        assert_eq!(wrap("один два три", 8), vec!["один два", "три"]);
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap("", 5), vec![""]);
    }

    #[test]
    fn drags_by_the_frame() {
        let mut d = Dialog::far("t", 40).text("x").buttons(&["OK"], 0);
        let area = Rect::new(0, 0, 80, 25);
        let mut buf = Buffer::empty(area);
        d.draw(area, &mut buf);
        let start = d.outer;
        let ev = |kind, column, row| MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        // Grab the top border, move 5 right and 3 down, release.
        let (gx, gy) = (start.x + 10, start.y + 1);
        assert_eq!(
            d.handle_mouse(&ev(MouseEventKind::Down(MouseButton::Left), gx, gy)),
            Some(Outcome::Pending)
        );
        assert!(d.dragging());
        d.handle_mouse(&ev(MouseEventKind::Drag(MouseButton::Left), gx + 5, gy + 3));
        d.handle_mouse(&ev(MouseEventKind::Up(MouseButton::Left), gx + 5, gy + 3));
        assert!(!d.dragging());
        d.draw(area, &mut buf);
        assert_eq!((d.outer.x, d.outer.y), (start.x + 5, start.y + 3));
        // Not past the screen edge.
        d.handle_mouse(&ev(
            MouseEventKind::Down(MouseButton::Left),
            d.outer.x + 10,
            d.outer.y + 1,
        ));
        d.handle_mouse(&ev(MouseEventKind::Drag(MouseButton::Left), 0, 0));
        d.draw(area, &mut buf);
        assert_eq!((d.outer.x, d.outer.y), (0, 0));
    }

    #[test]
    fn clicks_buttons() {
        let mut d = Dialog::far("Удаление", 40)
            .text("Удалить?")
            .buttons(&["Удалить", "Отмена"], 0);
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 25));
        d.draw(Rect::new(0, 0, 80, 25), &mut buf);
        let (rect, _) = d
            .hits
            .iter()
            .find(|(_, t)| matches!(t, Target::Button(_, _, 1)))
            .copied()
            .unwrap();
        let ev = |kind, column| MouseEvent {
            kind,
            column,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        };
        let down = MouseEventKind::Down(MouseButton::Left);
        let up = MouseEventKind::Up(MouseButton::Left);
        // Pressing only focuses; releasing over the same button presses it.
        assert_eq!(
            d.handle_mouse(&ev(down, rect.x + 1)),
            Some(Outcome::Pending)
        );
        assert_eq!(
            d.handle_mouse(&ev(up, rect.x + 2)),
            Some(Outcome::Closed(Some(1)))
        );
        // Released elsewhere: nothing.
        d.handle_mouse(&ev(down, rect.x + 1));
        assert_eq!(d.handle_mouse(&ev(up, 0)), Some(Outcome::Pending));
    }

    #[test]
    fn check_box_toggles_on_release() {
        let mut d = Dialog::far("t", 40).check("x", false).buttons(&["OK"], 0);
        let area = Rect::new(0, 0, 80, 25);
        let mut buf = Buffer::empty(area);
        d.draw(area, &mut buf);
        let (rect, _) = d.hits[0];
        let ev = |kind| MouseEvent {
            kind,
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        };
        d.handle_mouse(&ev(MouseEventKind::Down(MouseButton::Left)));
        assert!(!d.checked(0));
        d.handle_mouse(&ev(MouseEventKind::Up(MouseButton::Left)));
        assert!(d.checked(0));
    }
}
