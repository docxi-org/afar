//! A menu as Far draws it (VMenu2 in "full" box mode; far/vmenu.cpp,
//! far/vmenu2.cpp; docs/10-far-menus-reference.md): a list in a double frame with
//! a cyan margin and a shadow, check marks, `&` hotkeys, accelerator keys
//! appended to the item text, separators joined to `│` columns, a
//! scrollbar in place of the right frame.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;

use crate::theme;

pub struct Item {
    /// Text with an optional `&` before the hotkey letter.
    pub text: String,
    /// `√`, or a custom mark such as `▲`/`▼`.
    pub check: Option<char>,
    pub separator: bool,
    /// Cannot be chosen; the cursor skips it.
    pub disabled: bool,
    accel: Option<(KeyCode, KeyModifiers)>,
}

impl Item {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            check: None,
            separator: false,
            disabled: false,
            accel: None,
        }
    }

    pub fn separator() -> Self {
        Self {
            separator: true,
            ..Self::new("")
        }
    }

    pub fn checked(mut self, mark: Option<char>) -> Self {
        self.check = mark;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// A key that chooses the item; its name is shown after the text.
    pub fn accel(mut self, code: KeyCode, modifiers: KeyModifiers) -> Self {
        self.accel = Some((code, modifiers));
        self
    }

    fn selectable(&self) -> bool {
        !self.separator && !self.disabled
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    Pending,
    /// Chosen item, or `None` when cancelled.
    Closed(Option<usize>),
}

pub struct Menu {
    title: String,
    bottom_title: String,
    items: Vec<Item>,
    pub selected: usize,
    top: usize,
    /// Column of the list's frame; `None` centres the menu.
    column: Option<u16>,
    /// Geometry of the last drawing.
    outer: Rect,
    list: Rect,
    rows: usize,
    pressed: Option<usize>,
}

impl Menu {
    /// A menu with the accelerator keys appended to the item texts (Far's
    /// DecorateItemsWithHotkeys): left-aligned one space after the longest
    /// text.
    pub fn new(title: impl Into<String>, mut items: Vec<Item>) -> Self {
        let longest = items
            .iter()
            .map(|i| visible_len(&i.text))
            .max()
            .unwrap_or(0);
        for item in items.iter_mut().filter(|i| !i.separator) {
            if let Some((code, mods)) = item.accel {
                let pad = longest + 1 - visible_len(&item.text);
                item.text = format!("{}{}{}", item.text, " ".repeat(pad), key_name(code, mods));
            }
        }
        let mut menu = Self {
            title: title.into(),
            bottom_title: String::new(),
            items,
            selected: 0,
            top: 0,
            column: None,
            outer: Rect::default(),
            list: Rect::default(),
            rows: 1,
            pressed: None,
        };
        menu.selected = menu.first_selectable().unwrap_or(0);
        menu
    }

    pub fn bottom_title(mut self, text: impl Into<String>) -> Self {
        self.bottom_title = text.into();
        self
    }

    /// Puts the list's frame at column `x` (Far's SetPosition({x, -1})).
    pub fn at_column(mut self, x: u16) -> Self {
        self.column = Some(x);
        self
    }

    pub fn select(mut self, index: usize) -> Self {
        if self.items.get(index).is_some_and(Item::selectable) {
            self.selected = index;
        }
        self
    }

    pub fn rect(&self) -> Rect {
        self.outer
    }

    fn first_selectable(&self) -> Option<usize> {
        self.items.iter().position(Item::selectable)
    }

    fn last_selectable(&self) -> Option<usize> {
        self.items.iter().rposition(Item::selectable)
    }

    /// The next selectable item from `from` in direction `dir`; wraps
    /// around when `wrap`.
    fn step(&self, from: usize, dir: isize, wrap: bool) -> Option<usize> {
        let n = self.items.len() as isize;
        let mut i = from as isize;
        for _ in 0..n {
            i += dir;
            if i < 0 || i >= n {
                if !wrap {
                    return None;
                }
                i = i.rem_euclid(n);
            }
            if self.items[i as usize].selectable() {
                return Some(i as usize);
            }
        }
        None
    }

    /// Moves `count` items (PgUp/PgDn), landing on a selectable one.
    fn page(&mut self, dir: isize) {
        let n = self.items.len() as isize;
        let target = (self.selected as isize + dir * self.rows as isize).clamp(0, n - 1) as usize;
        if self.items[target].selectable() {
            self.selected = target;
        } else if let Some(i) = self
            .step(target, dir, false)
            .or_else(|| self.step(target, -dir, false))
        {
            self.selected = i;
        }
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> Outcome {
        if self.items.is_empty() {
            return Outcome::Closed(None);
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc | KeyCode::F(10) => return Outcome::Closed(None),
            KeyCode::Enter => {
                if self.items[self.selected].selectable() {
                    return Outcome::Closed(Some(self.selected));
                }
            }
            KeyCode::Up | KeyCode::Left => {
                if let Some(i) = self.step(self.selected, -1, true) {
                    self.selected = i;
                }
            }
            KeyCode::Down | KeyCode::Right => {
                if let Some(i) = self.step(self.selected, 1, true) {
                    self.selected = i;
                }
            }
            KeyCode::Home => self.selected = self.first_selectable().unwrap_or(0),
            KeyCode::End => self.selected = self.last_selectable().unwrap_or(0),
            KeyCode::PageUp if ctrl => self.selected = self.first_selectable().unwrap_or(0),
            KeyCode::PageDown if ctrl => self.selected = self.last_selectable().unwrap_or(0),
            KeyCode::PageUp => self.page(-1),
            KeyCode::PageDown => self.page(1),
            _ => {
                if let Some(i) = self.accel_item(key).or_else(|| self.hotkey_item(key)) {
                    self.selected = i;
                    return Outcome::Closed(Some(i));
                }
            }
        }
        Outcome::Pending
    }

    fn accel_item(&self, key: &KeyEvent) -> Option<usize> {
        self.items
            .iter()
            .position(|i| i.selectable() && i.accel == Some((key.code, key.modifiers)))
    }

    /// A letter, or Alt+letter, in any keyboard layout.
    fn hotkey_item(&self, key: &KeyEvent) -> Option<usize> {
        let KeyCode::Char(c) = key.code else {
            return None;
        };
        if key.modifiers.intersects(KeyModifiers::CONTROL) {
            return None;
        }
        let c = c.to_lowercase().next()?;
        let c = crate::keys::latin_equivalent(c).unwrap_or(c);
        self.items
            .iter()
            .position(|i| i.selectable() && crate::dialog::hotkey(&i.text) == Some(c))
    }

    /// The item drawn at `pos`, if it can be chosen.
    fn item_at(&self, pos: Position) -> Option<usize> {
        let l = self.list;
        if pos.x <= l.x || pos.x + 1 >= l.right() || pos.y <= l.y {
            return None;
        }
        let row = usize::from(pos.y - l.y - 1);
        let i = self.top + row;
        (row < self.rows && self.items.get(i).is_some_and(Item::selectable)).then_some(i)
    }

    /// Like Far: the cursor follows the mouse, an item is chosen on release
    /// where it was pressed, a click outside cancels (the middle button
    /// chooses the current item), the wheel scrolls.
    pub fn handle_mouse(&mut self, ev: &MouseEvent) -> Outcome {
        let pos = Position::new(ev.column, ev.row);
        let under = self.item_at(pos);
        match ev.kind {
            MouseEventKind::Down(button) if !self.outer.contains(pos) => {
                return match button {
                    MouseButton::Middle => Outcome::Closed(Some(self.selected)),
                    _ => Outcome::Closed(None),
                };
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.pressed = under;
                if let Some(i) = under {
                    self.selected = i;
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(i) = under.filter(|i| self.pressed.take() == Some(*i)) {
                    return Outcome::Closed(Some(i));
                }
            }
            MouseEventKind::Moved | MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(i) = under {
                    self.selected = i;
                }
            }
            MouseEventKind::ScrollUp => {
                if let Some(i) = self.step(self.selected, -1, false) {
                    self.selected = i;
                    self.top = self.top.saturating_sub(1);
                }
            }
            MouseEventKind::ScrollDown => {
                if let Some(i) = self.step(self.selected, 1, false) {
                    self.selected = i;
                    self.top += 1;
                }
            }
            _ => {}
        }
        Outcome::Pending
    }

    pub fn draw(&mut self, area: Rect, buf: &mut Buffer) {
        let n = self.items.len();
        let longest = self
            .items
            .iter()
            .map(|i| visible_len(&i.text))
            .max()
            .unwrap_or(0);
        let titles = self
            .title
            .chars()
            .count()
            .max(self.bottom_title.chars().count());
        let wn = (longest + 5).max(titles + 6) as u16;
        let w = (wn + 4).min(area.width);
        let wn = w.saturating_sub(4);
        // Far lets the blank row below go off screen before cutting items.
        let h = (n as u16 + 4).min(area.height + 1);
        let mut x = match self.column {
            Some(c) if c > 1 => area.x + c - 2,
            Some(c) => area.x + c,
            None => area.x + (area.width - w) / 2,
        };
        if x > area.x && x + w > area.right().saturating_sub(1) {
            x = area.right().saturating_sub(1).saturating_sub(w);
        }
        let y = area.y + area.height.saturating_sub(h) / 2;
        let outer = Rect::new(x, y, w, h);
        let list = Rect::new(x + 2, y + 1, wn, h.saturating_sub(2));
        self.outer = outer;
        self.list = list;
        let rows = usize::from(list.height.saturating_sub(2)).max(1);
        self.rows = rows;
        if wn < 6 {
            return;
        }

        // Keep the cursor on the page.
        self.top = self.top.min(n.saturating_sub(rows));
        if self.selected >= self.top + rows {
            self.top = self.selected + 1 - rows;
        } else if self.selected < self.top {
            self.top = self.selected;
        }

        let put = |buf: &mut Buffer, cx: u16, cy: u16, s: &str, style: Style| {
            if area.contains(Position::new(cx, cy)) {
                buf[(cx, cy)].set_symbol(s).set_style(style);
            }
        };
        crate::dialog::draw_shadow(buf, outer.intersection(area), area);
        for cy in outer.top()..outer.bottom() {
            for cx in outer.left()..outer.right() {
                put(buf, cx, cy, " ", theme::MENU_BOX);
            }
        }
        // The list's frame and titles.
        let (lx, ly, rx, by) = (list.x, list.y, list.right() - 1, list.bottom() - 1);
        for cx in lx + 1..rx {
            put(buf, cx, ly, "═", theme::MENU_BOX);
            put(buf, cx, by, "═", theme::MENU_BOX);
        }
        for cy in ly + 1..by {
            put(buf, lx, cy, "║", theme::MENU_BOX);
            put(buf, rx, cy, "║", theme::MENU_BOX);
        }
        put(buf, lx, ly, "╔", theme::MENU_BOX);
        put(buf, rx, ly, "╗", theme::MENU_BOX);
        put(buf, lx, by, "╚", theme::MENU_BOX);
        put(buf, rx, by, "╝", theme::MENU_BOX);
        for (title, ty) in [(&self.title, ly), (&self.bottom_title, by)] {
            if title.is_empty() {
                continue;
            }
            let t = title.chars().count().min(usize::from(wn).saturating_sub(4));
            let tx = lx + (wn - 2 - t as u16) / 2;
            let text: String = std::iter::once(' ')
                .chain(title.chars().take(t))
                .chain(std::iter::once(' '))
                .collect();
            for (k, ch) in text.chars().enumerate() {
                put(buf, tx + k as u16, ty, &ch.to_string(), theme::MENU_TITLE);
            }
        }

        let text_w = usize::from(wn - 5);
        for r in 0..rows {
            let cy = ly + 1 + r as u16;
            let i = self.top + r;
            let Some(item) = self.items.get(i) else {
                for cx in lx + 1..rx {
                    put(buf, cx, cy, " ", theme::MENU_TEXT);
                }
                continue;
            };
            if item.separator {
                put(buf, lx, cy, "╟", theme::MENU_BOX);
                put(buf, rx, cy, "╢", theme::MENU_BOX);
                for cx in lx + 1..rx {
                    let k = usize::from(cx - lx);
                    let above = k >= 3 && i > 0 && self.char_at(i - 1, k - 3) == Some('│');
                    let below = k >= 3 && self.char_at(i + 1, k - 3) == Some('│');
                    let s = match (above, below) {
                        (true, true) => "┼",
                        (true, false) => "┴",
                        (false, true) => "┬",
                        _ => "─",
                    };
                    put(buf, cx, cy, s, theme::MENU_BOX);
                }
                continue;
            }
            let (text, hot) = if item.disabled {
                (theme::MENU_DISABLED, theme::MENU_DISABLED)
            } else if i == self.selected {
                (theme::MENU_SELECTED, theme::MENU_SELECTED_HIGHLIGHT)
            } else {
                (theme::MENU_TEXT, theme::MENU_HIGHLIGHT)
            };
            for cx in lx + 1..rx {
                put(buf, cx, cy, " ", text);
            }
            if let Some(mark) = item.check {
                put(buf, lx + 1, cy, &mark.to_string(), text);
            }
            let (cells, cut) = label_cells(&item.text, text_w);
            for (k, (ch, is_hot)) in cells.into_iter().enumerate() {
                put(
                    buf,
                    lx + 3 + k as u16,
                    cy,
                    &ch.to_string(),
                    if is_hot { hot } else { text },
                );
            }
            if cut {
                put(buf, rx - 1, cy, "»", hot);
            }
        }

        // Scrollbar in place of the right frame.
        if n > rows && rows >= 2 {
            let field = rows - 2;
            let rnd = |a: usize, b: usize| {
                ((a as f64 / b as f64 * field as f64 * 10.0).floor() / 10.0).round() as usize
            };
            let mut begin = rnd(self.top, n).max(usize::from(self.top > 0));
            if begin >= field {
                begin = field.saturating_sub(1);
            }
            let size = rnd(rows, n).max(1);
            let mut end = if self.top + rows >= n {
                field
            } else {
                begin + size
            };
            if end >= field && self.top + rows < n {
                end = field - 1;
                if begin > 1 {
                    begin -= 1;
                }
            }
            put(buf, rx, ly + 1, "▲", theme::MENU_SCROLLBAR);
            for k in 0..field {
                let s = if (begin..end).contains(&k) {
                    "█"
                } else {
                    "░"
                };
                put(buf, rx, ly + 2 + k as u16, s, theme::MENU_SCROLLBAR);
            }
            put(buf, rx, ly + rows as u16, "▼", theme::MENU_SCROLLBAR);
        }
    }

    /// The character of item `i`'s text at visible position `k`.
    fn char_at(&self, i: usize, k: usize) -> Option<char> {
        let item = self.items.get(i).filter(|i| !i.separator)?;
        label_cells(&item.text, usize::MAX)
            .0
            .get(k)
            .map(|(c, _)| *c)
    }
}

/// Text length without the `&` markers.
fn visible_len(text: &str) -> usize {
    label_cells(text, usize::MAX).0.len()
}

/// The cells of a label (`&&` is `&`, `&x` marks the hotkey), at most
/// `max`; and whether it was cut.
fn label_cells(text: &str, max: usize) -> (Vec<(char, bool)>, bool) {
    let mut out = Vec::new();
    let mut chars = text.chars();
    let mut hot_done = false;
    while let Some(c) = chars.next() {
        let cell = match c {
            '&' => match chars.next() {
                Some('&') => ('&', false),
                Some(h) if !hot_done => {
                    hot_done = true;
                    (h, true)
                }
                Some(h) => (h, false),
                None => break,
            },
            '\t' => (' ', false),
            c => (c, false),
        };
        if out.len() == max {
            return (out, true);
        }
        out.push(cell);
    }
    (out, false)
}

/// Far's key names (keyboard.cpp): modifiers Ctrl, Alt, Shift, then the key.
pub fn key_name(code: KeyCode, mods: KeyModifiers) -> String {
    let mut s = String::new();
    for (m, name) in [
        (KeyModifiers::CONTROL, "Ctrl"),
        (KeyModifiers::ALT, "Alt"),
        (KeyModifiers::SHIFT, "Shift"),
    ] {
        if mods.contains(m) {
            s.push_str(name);
            s.push('+');
        }
    }
    let key = match code {
        KeyCode::F(n) => format!("F{n}"),
        KeyCode::Char(c) => c.to_uppercase().to_string(),
        KeyCode::Delete => "Del".into(),
        KeyCode::Insert => "Ins".into(),
        KeyCode::Enter => "Enter".into(),
        KeyCode::Esc => "Esc".into(),
        KeyCode::Tab => "Tab".into(),
        KeyCode::Backspace => "BS".into(),
        other => format!("{other:?}"),
    };
    s + &key
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(buf: &Buffer, y: u16, x0: u16, x1: u16) -> String {
        (x0..x1).map(|x| buf[(x, y)].symbol().to_string()).collect()
    }

    fn sort_like() -> Menu {
        Menu::new(
            "Sort by",
            vec![
                Item::new("&Name")
                    .checked(Some('▲'))
                    .accel(KeyCode::F(3), KeyModifiers::CONTROL),
                Item::new("Name only"),
                Item::separator(),
                Item::new("Show selected f&irst").accel(KeyCode::F(12), KeyModifiers::SHIFT),
            ],
        )
        .bottom_title("+ - * F4")
        .at_column(4)
    }

    #[test]
    fn far_menu_geometry() {
        let mut m = sort_like();
        let area = Rect::new(0, 0, 80, 25);
        let mut buf = Buffer::empty(area);
        m.draw(area, &mut buf);
        // Longest text: "Show selected first Shift+F12" (29) → Wn 34.
        assert_eq!(m.list, Rect::new(4, 9, 34, 6));
        assert_eq!(m.outer, Rect::new(2, 8, 38, 8));
        assert_eq!(row(&buf, 9, 4, 38), "╔═══════════ Sort by ════════════╗");
        assert_eq!(
            row(&buf, 10, 2, 40),
            "  ║▲ Name                Ctrl+F3   ║  "
        );
        assert_eq!(row(&buf, 12, 4, 38), "╟────────────────────────────────╢");
        assert_eq!(row(&buf, 13, 4, 38), "║  Show selected first Shift+F12 ║");
        assert_eq!(row(&buf, 14, 4, 38), "╚═══════════ + - * F4 ═══════════╝");
        assert_eq!(buf[(6, 10)].bg, theme::MENU_SELECTED.bg.unwrap());
        assert_eq!(buf[(7, 10)].fg, theme::MENU_SELECTED_HIGHLIGHT.fg.unwrap());
    }

    #[test]
    fn keys_skip_separators_and_wrap() {
        let mut m = sort_like();
        let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        m.handle_key(&down);
        assert_eq!(m.selected, 1);
        m.handle_key(&down);
        assert_eq!(m.selected, 3, "skips the separator");
        m.handle_key(&down);
        assert_eq!(m.selected, 0, "wraps");
        // Hotkey in the Russian layout: "ш" is the key of "i".
        let key = KeyEvent::new(KeyCode::Char('ш'), KeyModifiers::NONE);
        assert_eq!(m.handle_key(&key), Outcome::Closed(Some(3)));
        let key = KeyEvent::new(KeyCode::F(3), KeyModifiers::CONTROL);
        assert_eq!(m.handle_key(&key), Outcome::Closed(Some(0)));
    }

    #[test]
    fn mouse_chooses_on_release_and_cancels_outside() {
        let mut m = sort_like();
        let area = Rect::new(0, 0, 80, 25);
        let mut buf = Buffer::empty(area);
        m.draw(area, &mut buf);
        let ev = |kind, column, row| MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        let down = MouseEventKind::Down(MouseButton::Left);
        let up = MouseEventKind::Up(MouseButton::Left);
        assert_eq!(m.handle_mouse(&ev(down, 10, 11)), Outcome::Pending);
        assert_eq!(m.selected, 1);
        assert_eq!(m.handle_mouse(&ev(up, 12, 11)), Outcome::Closed(Some(1)));
        assert_eq!(m.handle_mouse(&ev(down, 60, 2)), Outcome::Closed(None));
    }
}
