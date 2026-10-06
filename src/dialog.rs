//! Far-style dialogs: a modal box with text, input fields, check boxes and
//! buttons, drawn over the window layout (the window manager's overlay).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Style};

use crate::panel::{draw_frame, put, put_title};

pub enum Item {
    Text(String),
    /// Text centered in the dialog.
    Center(String),
    Input {
        value: String,
        cursor: usize,
    },
    Check {
        label: String,
        checked: bool,
    },
    /// A radio button; consecutive radio buttons form one group.
    Radio {
        label: String,
        selected: bool,
    },
    /// A line across the frame.
    Separator,
    /// A row of buttons; the buttons of all rows are numbered in order.
    Buttons(Vec<String>),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Target {
    Item(usize),
    /// Button: item index, global button number.
    Button(usize, usize),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Pending,
    /// Closed with a button (its number) or cancelled (`None`).
    Closed(Option<usize>),
}

struct Colors {
    body: Style,
    frame: Style,
    field: Style,
    focused_button: Style,
}

const NORMAL: Colors = Colors {
    body: Style::new().fg(Color::Black).bg(Color::Gray),
    frame: Style::new().fg(Color::White).bg(Color::Gray),
    field: Style::new().fg(Color::Black).bg(Color::Cyan),
    focused_button: Style::new().fg(Color::Black).bg(Color::Cyan),
};

const WARNING: Colors = Colors {
    body: Style::new().fg(Color::White).bg(Color::Red),
    frame: Style::new().fg(Color::White).bg(Color::Red),
    field: Style::new().fg(Color::Black).bg(Color::Cyan),
    focused_button: Style::new().fg(Color::Black).bg(Color::Gray),
};

pub struct Dialog {
    title: String,
    items: Vec<Item>,
    /// Width of the content area.
    width: u16,
    warning: bool,
    default_button: usize,
    focus: Option<usize>,
    /// Clickable areas from the last draw.
    hits: Vec<(Rect, Target)>,
}

impl Dialog {
    pub fn new(title: impl Into<String>, width: u16) -> Self {
        Self {
            title: title.into(),
            items: Vec::new(),
            width,
            warning: false,
            default_button: 0,
            focus: None,
            hits: Vec::new(),
        }
    }

    /// Red warning style (deletion and other dangerous actions).
    pub fn warning(mut self) -> Self {
        self.warning = true;
        self
    }

    pub fn text(mut self, s: impl Into<String>) -> Self {
        self.items.push(Item::Text(s.into()));
        self
    }

    /// Text wrapped at word boundaries to the dialog's width.
    pub fn wrapped(mut self, s: &str) -> Self {
        for line in wrap(s, usize::from(self.width)) {
            self.items.push(Item::Text(line));
        }
        self
    }

    pub fn center(mut self, s: impl Into<String>) -> Self {
        self.items.push(Item::Center(s.into()));
        self
    }

    pub fn input(mut self, value: impl Into<String>) -> Self {
        let value = value.into();
        let cursor = value.chars().count();
        self.items.push(Item::Input { value, cursor });
        self
    }

    pub fn check(mut self, label: impl Into<String>, checked: bool) -> Self {
        self.items.push(Item::Check {
            label: label.into(),
            checked,
        });
        self
    }

    /// A group of radio buttons with `selected` chosen.
    pub fn radios(mut self, labels: &[&str], selected: usize) -> Self {
        for (i, l) in labels.iter().enumerate() {
            self.items.push(Item::Radio {
                label: l.to_string(),
                selected: i == selected,
            });
        }
        self
    }

    pub fn separator(mut self) -> Self {
        self.items.push(Item::Separator);
        self
    }

    /// Adds a row of buttons; `default` (a global button number) is pressed
    /// by Enter outside the buttons.
    pub fn buttons(mut self, labels: &[&str], default: usize) -> Self {
        self.items.push(Item::Buttons(
            labels.iter().map(|s| s.to_string()).collect(),
        ));
        self.default_button = default;
        self
    }

    /// Text of the `n`-th input field.
    pub fn input_value(&self, n: usize) -> &str {
        self.items
            .iter()
            .filter_map(|i| match i {
                Item::Input { value, .. } => Some(value.as_str()),
                _ => None,
            })
            .nth(n)
            .unwrap_or("")
    }

    /// State of the `n`-th check box.
    pub fn checked(&self, n: usize) -> bool {
        self.items
            .iter()
            .filter_map(|i| match i {
                Item::Check { checked, .. } => Some(*checked),
                _ => None,
            })
            .nth(n)
            .unwrap_or(false)
    }

    /// Index of the selected button in the `n`-th radio group.
    pub fn radio(&self, n: usize) -> usize {
        let mut group = None;
        let mut index = 0;
        let mut prev_radio = false;
        for item in &self.items {
            match item {
                Item::Radio { selected, .. } => {
                    if !prev_radio {
                        group = Some(group.map_or(0, |g| g + 1));
                        index = 0;
                    }
                    if group == Some(n) && *selected {
                        return index;
                    }
                    index += 1;
                    prev_radio = true;
                }
                _ => prev_radio = false,
            }
        }
        0
    }

    /// Selects radio button `i`, unselecting the rest of its group.
    fn select_radio(&mut self, i: usize) {
        let is_radio = |item: &Item| matches!(item, Item::Radio { .. });
        let mut start = i;
        while start > 0 && is_radio(&self.items[start - 1]) {
            start -= 1;
        }
        let mut j = start;
        while j < self.items.len() && is_radio(&self.items[j]) {
            if let Item::Radio { selected, .. } = &mut self.items[j] {
                *selected = j == i;
            }
            j += 1;
        }
    }

    fn targets(&self) -> Vec<Target> {
        let mut out = Vec::new();
        let mut button = 0;
        for (i, item) in self.items.iter().enumerate() {
            match item {
                Item::Input { .. } | Item::Check { .. } | Item::Radio { .. } => {
                    out.push(Target::Item(i))
                }
                Item::Buttons(labels) => {
                    for _ in labels {
                        out.push(Target::Button(i, button));
                        button += 1;
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// The focused control (chosen on first use).
    fn focus(&mut self) -> Option<Target> {
        let targets = self.targets();
        if self.focus.is_none() {
            // Like Far: the first input field, otherwise the first other
            // control, otherwise the default button.
            let is_input = |t: &Target| matches!(t, Target::Item(i) if matches!(self.items[*i], Item::Input { .. }));
            self.focus = targets
                .iter()
                .position(is_input)
                .or_else(|| targets.iter().position(|t| matches!(t, Target::Item(_))))
                .or_else(|| {
                    targets.iter().position(
                        |t| matches!(t, Target::Button(_, b) if *b == self.default_button),
                    )
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
        let cur = self
            .focus()
            .map(|_| self.focus.unwrap_or(0) as isize)
            .unwrap_or(0);
        self.focus = Some((cur + delta).rem_euclid(n) as usize);
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> Outcome {
        let focus = self.focus();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => return Outcome::Closed(None),
            KeyCode::Tab | KeyCode::Down => self.move_focus(1),
            KeyCode::BackTab | KeyCode::Up => self.move_focus(-1),
            KeyCode::Enter => {
                return Outcome::Closed(Some(match focus {
                    Some(Target::Button(_, b)) => b,
                    _ => self.default_button,
                }));
            }
            _ => {}
        }
        match focus {
            Some(Target::Button(_, b)) => match key.code {
                KeyCode::Char(' ') => return Outcome::Closed(Some(b)),
                KeyCode::Left => self.move_focus(-1),
                KeyCode::Right => self.move_focus(1),
                _ => {}
            },
            Some(Target::Item(i)) if matches!(self.items[i], Item::Radio { .. }) => {
                if key.code == KeyCode::Char(' ') {
                    self.select_radio(i);
                }
            }
            Some(Target::Item(i)) => match &mut self.items[i] {
                Item::Check { checked, .. } => {
                    if key.code == KeyCode::Char(' ') {
                        *checked = !*checked;
                    }
                }
                Item::Input { value, cursor } => {
                    let len = value.chars().count();
                    let byte =
                        |s: &str, c: usize| s.char_indices().nth(c).map_or(s.len(), |(i, _)| i);
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
            },
            None => {}
        }
        Outcome::Pending
    }

    /// Returns `None` when the event is outside the dialog.
    pub fn handle_mouse(&mut self, ev: &MouseEvent) -> Option<Outcome> {
        let pos = Position::new(ev.column, ev.row);
        let (rect, target) = self.hits.iter().copied().find(|(r, _)| r.contains(pos))?;
        if ev.kind != MouseEventKind::Down(MouseButton::Left) {
            return Some(Outcome::Pending);
        }
        if let Some(i) = self.targets().iter().position(|t| *t == target) {
            self.focus = Some(i);
        }
        match target {
            Target::Button(_, b) => return Some(Outcome::Closed(Some(b))),
            Target::Item(i) if matches!(self.items[i], Item::Radio { .. }) => self.select_radio(i),
            Target::Item(i) => match &mut self.items[i] {
                Item::Check { checked, .. } => *checked = !*checked,
                Item::Input { value, cursor } => {
                    *cursor = usize::from(ev.column - rect.x).min(value.chars().count());
                }
                _ => {}
            },
        }
        Some(Outcome::Pending)
    }

    /// Draws the dialog centered in `area`; returns the text cursor.
    pub fn draw(&mut self, area: Rect, buf: &mut Buffer) -> Option<Position> {
        let colors = if self.warning { &WARNING } else { &NORMAL };
        // Wide enough for every row of buttons.
        let buttons = self
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Buttons(labels) => Some(
                    labels
                        .iter()
                        .map(|l| l.chars().count() as u16 + 5)
                        .sum::<u16>(),
                ),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        let width = self
            .width
            .max(buttons)
            .min(area.width.saturating_sub(12))
            .max(10);
        let w = width + 10;
        let h = (self.items.len() as u16 + 4).min(area.height);
        let x = area.x + area.width.saturating_sub(w) / 2;
        let y = area.y + area.height.saturating_sub(h) / 2;
        let outer = Rect::new(x, y, w.min(area.width), h);

        // Shadow: two columns to the right and one row below.
        let shadow = Style::new().fg(Color::DarkGray).bg(Color::Black);
        for sy in outer.y + 1..=outer.bottom() {
            for sx in outer.right()..outer.right() + 2 {
                if sx < area.right() && sy < area.bottom() {
                    buf[(sx, sy)].set_style(shadow);
                }
            }
        }
        for sx in outer.x + 2..outer.right() + 2 {
            if sx < area.right() && outer.bottom() < area.bottom() {
                buf[(sx, outer.bottom())].set_style(shadow);
            }
        }
        buf.set_style(outer, colors.body);
        for yy in outer.top()..outer.bottom() {
            for xx in outer.left()..outer.right() {
                buf[(xx, yy)].set_symbol(" ");
            }
        }
        let frame = Rect::new(
            outer.x + 3,
            outer.y + 1,
            outer.width.saturating_sub(6),
            outer.height.saturating_sub(2),
        );
        draw_frame(buf, frame, colors.frame);
        put_title(
            buf,
            frame,
            frame.y,
            &format!(" {} ", self.title),
            colors.frame,
        );

        let focus = self.focus();
        let cx = frame.x + 2;
        let mut cursor = None;
        self.hits.clear();
        let mut button_no = 0;
        for (i, item) in self.items.iter().enumerate() {
            let row = frame.y + 1 + i as u16;
            if row >= frame.bottom() - 1 {
                break;
            }
            match item {
                Item::Text(s) => put(buf, cx, row, width, s, colors.body),
                Item::Center(s) => {
                    let n = (s.chars().count() as u16).min(width);
                    put(buf, cx + (width - n) / 2, row, n, s, colors.body);
                }
                Item::Input { value, cursor: c } => {
                    // Scroll long values so the cursor stays visible.
                    let skip = c.saturating_sub(usize::from(width) - 1);
                    let shown: String = value.chars().skip(skip).collect();
                    put(buf, cx, row, width, &shown, colors.field);
                    let rect = Rect::new(cx, row, width, 1);
                    self.hits.push((rect, Target::Item(i)));
                    if focus == Some(Target::Item(i)) {
                        cursor = Some(Position::new(cx + (c - skip) as u16, row));
                    }
                }
                Item::Check { label, checked } => {
                    let mark = if *checked { "[x] " } else { "[ ] " };
                    put(buf, cx, row, width, &format!("{mark}{label}"), colors.body);
                    let n = (label.chars().count() as u16 + 4).min(width);
                    self.hits.push((Rect::new(cx, row, n, 1), Target::Item(i)));
                    if focus == Some(Target::Item(i)) {
                        cursor = Some(Position::new(cx + 1, row));
                    }
                }
                Item::Radio { label, selected } => {
                    let mark = if *selected { "(•) " } else { "( ) " };
                    put(buf, cx, row, width, &format!("{mark}{label}"), colors.body);
                    let n = (label.chars().count() as u16 + 4).min(width);
                    self.hits.push((Rect::new(cx, row, n, 1), Target::Item(i)));
                    if focus == Some(Target::Item(i)) {
                        cursor = Some(Position::new(cx + 1, row));
                    }
                }
                Item::Separator => {
                    for xx in frame.x + 1..frame.right() - 1 {
                        buf[(xx, row)].set_symbol("─").set_style(colors.frame);
                    }
                    buf[(frame.x, row)].set_symbol("╟");
                    buf[(frame.right() - 1, row)].set_symbol("╢");
                }
                Item::Buttons(labels) => {
                    // The default button is shown as `{ OK }`, like in Far.
                    let texts: Vec<String> = labels
                        .iter()
                        .enumerate()
                        .map(|(j, l)| {
                            if button_no + j == self.default_button {
                                format!("{{ {l} }}")
                            } else {
                                format!("[ {l} ]")
                            }
                        })
                        .collect();
                    let total: u16 = texts
                        .iter()
                        .map(|t| t.chars().count() as u16 + 1)
                        .sum::<u16>()
                        - 1;
                    let mut bx = cx + width.saturating_sub(total) / 2;
                    for (j, t) in texts.iter().enumerate() {
                        let n = t.chars().count() as u16;
                        let target = Target::Button(i, button_no + j);
                        let style = if focus == Some(target) {
                            colors.focused_button
                        } else {
                            colors.body
                        };
                        put(buf, bx, row, n, t, style);
                        self.hits.push((Rect::new(bx, row, n, 1), target));
                        bx += n + 1;
                    }
                    button_no += labels.len();
                }
            }
        }
        cursor
    }
}

/// Splits `s` into lines of at most `width` chars at spaces (long words
/// are cut).
fn wrap(s: &str, width: usize) -> Vec<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_radio_buttons_in_groups() {
        let mut d = Dialog::new("t", 30)
            .radios(&["a", "b", "c"], 0)
            .separator()
            .radios(&["x", "y"], 1)
            .buttons(&["OK"], 0);
        assert_eq!((d.radio(0), d.radio(1)), (0, 1));
        d.handle_key(&key(KeyCode::Down)); // b
        d.handle_key(&key(KeyCode::Down)); // c
        d.handle_key(&key(KeyCode::Char(' ')));
        d.handle_key(&key(KeyCode::Down)); // x
        d.handle_key(&key(KeyCode::Char(' ')));
        assert_eq!((d.radio(0), d.radio(1)), (2, 0));
    }

    #[test]
    fn wraps_words() {
        assert_eq!(wrap("один два три", 8), vec!["один два", "три"]);
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap("", 5), vec![""]);
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn mkdir_dialog() -> Dialog {
        Dialog::new("Создание папки", 40)
            .text("Создать папку:")
            .input("")
            .check("Обработать несколько имён", false)
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
    fn focuses_default_button_without_inputs() {
        let mut d = Dialog::new("Удаление", 30)
            .text("Удалить?")
            .buttons(&["Удалить", "Отмена"], 0);
        assert_eq!(d.handle_key(&key(KeyCode::Right)), Outcome::Pending);
        assert_eq!(d.handle_key(&key(KeyCode::Enter)), Outcome::Closed(Some(1)));
    }

    #[test]
    fn clicks_buttons() {
        let mut d = Dialog::new("Удаление", 30)
            .text("Удалить?")
            .buttons(&["Удалить", "Отмена"], 0);
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 25));
        d.draw(Rect::new(0, 0, 80, 25), &mut buf);
        let (rect, _) = d
            .hits
            .iter()
            .find(|(_, t)| *t == Target::Button(1, 1))
            .copied()
            .unwrap();
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x + 1,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(d.handle_mouse(&click), Some(Outcome::Closed(Some(1))));
    }
}
