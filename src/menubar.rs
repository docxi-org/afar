//! Far's menu bar (F9; far/hmenu.cpp): titles on the top row, the selected
//! one's submenu below it in a thin box. Left/Right go between titles (and
//! their submenus once one is open), Enter/Down open a submenu, a title's
//! hotkey opens it, Esc closes the submenu and then the bar.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};

use crate::menu::{Item, Menu, Outcome as MenuOutcome};
use crate::theme;

/// One title of the bar with its submenu; `actions` match `items`.
pub struct Title<T> {
    pub text: String,
    pub items: Vec<Item>,
    pub actions: Vec<Option<T>>,
    /// The item selected when the submenu opens.
    pub selected: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome<T> {
    Pending,
    Closed,
    Chosen(T),
}

pub struct MenuBar<T> {
    titles: Vec<Title<T>>,
    selected: usize,
    open: Option<Menu>,
    /// Column of each title as last drawn.
    xpos: Vec<u16>,
    row: u16,
}

impl<T: Clone> MenuBar<T> {
    pub fn new(titles: Vec<Title<T>>, selected: usize) -> Self {
        let selected = selected.min(titles.len().saturating_sub(1));
        Self {
            titles,
            selected,
            open: None,
            xpos: Vec::new(),
            row: 0,
        }
    }

    fn open_submenu(&mut self) {
        let t = &mut self.titles[self.selected];
        let items = std::mem::take(&mut t.items);
        let x = self.xpos.get(self.selected).copied().unwrap_or(2);
        let menu = Menu::new("", items).at(x, self.row + 1).select(t.selected);
        // Keep the decorated items for the next opening.
        self.open = Some(menu);
    }

    fn close_submenu(&mut self) {
        if let Some(menu) = self.open.take() {
            let t = &mut self.titles[self.selected];
            t.selected = menu.selected;
            t.items = menu.into_items();
        }
    }

    fn select(&mut self, index: usize) {
        let was_open = self.open.is_some();
        self.close_submenu();
        self.selected = index;
        if was_open {
            self.open_submenu();
        }
    }

    fn hotkey(&self, key: &KeyEvent) -> Option<usize> {
        let KeyCode::Char(c) = key.code else {
            return None;
        };
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return None;
        }
        let c = c.to_lowercase().next()?;
        let c = crate::keys::latin_equivalent(c).unwrap_or(c);
        self.titles
            .iter()
            .position(|t| crate::dialog::hotkey(&t.text) == Some(c))
    }

    fn chosen(&mut self, index: usize) -> Outcome<T> {
        let action = self.titles[self.selected]
            .actions
            .get(index)
            .cloned()
            .flatten();
        self.close_submenu();
        self.titles[self.selected].selected = index;
        match action {
            Some(a) => Outcome::Chosen(a),
            None => Outcome::Closed,
        }
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> Outcome<T> {
        let n = self.titles.len();
        match key.code {
            KeyCode::Left => {
                self.select((self.selected + n - 1) % n);
                return Outcome::Pending;
            }
            KeyCode::Right => {
                self.select((self.selected + 1) % n);
                return Outcome::Pending;
            }
            KeyCode::F(10) => return Outcome::Closed,
            KeyCode::Esc => {
                if self.open.is_some() {
                    self.close_submenu();
                    return Outcome::Pending;
                }
                return Outcome::Closed;
            }
            _ => {}
        }
        if let Some(menu) = &mut self.open {
            return match menu.handle_key(key) {
                MenuOutcome::Closed(Some(i)) => self.chosen(i),
                MenuOutcome::Closed(None) => {
                    self.close_submenu();
                    Outcome::Pending
                }
                MenuOutcome::Pending => Outcome::Pending,
            };
        }
        match key.code {
            KeyCode::Enter | KeyCode::Down | KeyCode::Up => self.open_submenu(),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = n - 1,
            KeyCode::Tab => self.selected = if self.selected == 0 { n - 1 } else { 0 },
            _ => {
                if let Some(i) = self.hotkey(key) {
                    self.selected = i;
                    self.open_submenu();
                }
            }
        }
        Outcome::Pending
    }

    /// The title under `pos` on the bar.
    fn title_at(&self, pos: Position) -> Option<usize> {
        if pos.y != self.row {
            return None;
        }
        self.xpos.iter().enumerate().position(|(i, &x)| {
            let w = self.titles[i].text.chars().filter(|c| *c != '&').count() as u16 + 4;
            (x..x + w).contains(&pos.x)
        })
    }

    pub fn handle_mouse(&mut self, ev: &MouseEvent) -> Outcome<T> {
        let pos = Position::new(ev.column, ev.row);
        if let Some(i) = self.title_at(pos) {
            if let MouseEventKind::Down(MouseButton::Left) = ev.kind {
                if self.open.is_some() && i == self.selected {
                    self.close_submenu();
                } else {
                    self.close_submenu();
                    self.selected = i;
                    self.open_submenu();
                }
            }
            return Outcome::Pending;
        }
        if let Some(menu) = &mut self.open {
            return match menu.handle_mouse(ev) {
                MenuOutcome::Closed(Some(i)) => self.chosen(i),
                MenuOutcome::Closed(None) => Outcome::Closed,
                MenuOutcome::Pending => Outcome::Pending,
            };
        }
        match ev.kind {
            MouseEventKind::Down(_) => Outcome::Closed,
            _ => Outcome::Pending,
        }
    }

    /// The bar on the top row of `area`, the open submenu below it.
    pub fn draw(&mut self, area: Rect, buf: &mut Buffer) {
        self.row = area.y;
        buf.set_style(Rect::new(area.x, area.y, area.width, 1), theme::HMENU_TEXT);
        for x in area.x..area.right() {
            buf[(x, area.y)].set_symbol(" ");
        }
        self.xpos.clear();
        let mut x = area.x + 2;
        for (i, t) in self.titles.iter().enumerate() {
            self.xpos.push(x);
            let selected = i == self.selected;
            let (text, hot) = if selected {
                (theme::HMENU_SELECTED, theme::HMENU_SELECTED_HIGHLIGHT)
            } else {
                (theme::HMENU_TEXT, theme::HMENU_HIGHLIGHT)
            };
            let label = format!("  {}  ", t.text);
            let mut chars = label.chars().peekable();
            let mut hot_done = false;
            while let Some(c) = chars.next() {
                let (c, style) = if c == '&' && !hot_done {
                    match chars.next() {
                        Some(h) => {
                            hot_done = true;
                            (h, hot)
                        }
                        None => break,
                    }
                } else {
                    (c, text)
                };
                if x < area.right() {
                    buf[(x, area.y)].set_symbol(&c.to_string()).set_style(style);
                }
                x += 1;
            }
        }
        if let Some(menu) = &mut self.open {
            menu.draw(area, buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar() -> MenuBar<u8> {
        let title = |text: &str, items: &[&str]| Title {
            text: text.into(),
            items: items.iter().map(|i| Item::new(*i)).collect(),
            actions: (0..items.len() as u8).map(Some).collect(),
            selected: 0,
        };
        MenuBar::new(
            vec![
                title("&Left", &["&Brief", "&Medium"]),
                title("&Files", &["&Copy"]),
                title("&Right", &["&Brief"]),
            ],
            0,
        )
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn opens_moves_and_chooses() {
        let mut b = bar();
        let area = Rect::new(0, 0, 80, 25);
        let mut buf = Buffer::empty(area);
        b.draw(area, &mut buf);
        let row: String = (0..30).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert_eq!(row, "    Left    Files    Right    ");
        assert!(b.open.is_none(), "F9 shows the bar only");
        b.handle_key(&key(KeyCode::Enter));
        assert!(b.open.is_some());
        // Right with a submenu open opens the next one.
        b.handle_key(&key(KeyCode::Right));
        assert_eq!(b.selected, 1);
        assert!(b.open.is_some());
        assert_eq!(b.handle_key(&key(KeyCode::Enter)), Outcome::Chosen(0));
        // Esc closes the submenu, then the bar.
        b.handle_key(&key(KeyCode::Char('r')));
        assert_eq!(b.selected, 2);
        assert_eq!(b.handle_key(&key(KeyCode::Esc)), Outcome::Pending);
        assert_eq!(b.handle_key(&key(KeyCode::Esc)), Outcome::Closed);
    }
}
