//! The autocompletion list under (or over) a field — Far's non-modal
//! `VMenu2` of `EditControl::AutoComplete` (docs/14 §8.4–8.5): the first
//! item is the text as typed; moving in the list puts the item into the
//! field at once; Enter closes the list and goes on to the field's owner.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};

use crate::complete::{Candidate, Group};
use crate::theme;

/// Far's `CBoxMaxHeight`.
const ROWS: usize = 8;

enum Line {
    /// The text as typed.
    Original,
    Item(Candidate),
    /// A source's title between groups.
    Title(&'static str),
}

/// What a key did in the list.
#[derive(Debug, PartialEq, Eq)]
pub enum Reply {
    /// Put this text into the field (the list stays).
    SetText(String),
    /// The key goes to the field; the list stays (it is recomputed after an
    /// edit).
    Pass,
    /// Close the list.
    Close,
    /// Close the list and give the key to the field's owner.
    CloseAndPass,
    /// Modal list: put the text into the field and close.
    Accept(String),
    /// Shift+Del: remove this history entry.
    DeleteHistory(String),
    Ignore,
}

pub struct Completion {
    original: String,
    lines: Vec<Line>,
    title: Option<&'static str>,
    current: usize,
    top: usize,
    modal: bool,
    /// Once above the field, it stays there (Far's `MenuUp`).
    up: bool,
    rect: Rect,
}

impl Completion {
    /// The list for `groups`, or `None` when it would only repeat the typed
    /// text (Far: more than one item, or one that differs).
    pub fn new(original: &str, groups: Vec<Group>, modal: bool, up: bool) -> Option<Self> {
        let count: usize = groups.iter().map(|g| g.items.len()).sum();
        let only_repeats = count == 1
            && groups
                .iter()
                .flat_map(|g| &g.items)
                .all(|c| c.line.to_lowercase() == original.to_lowercase());
        if count == 0 || only_repeats {
            return None;
        }
        let mut lines = vec![Line::Original];
        let title = groups.first().map(|g| g.title);
        for (i, g) in groups.into_iter().enumerate() {
            if i > 0 {
                lines.push(Line::Title(g.title));
            }
            lines.extend(g.items.into_iter().map(Line::Item));
        }
        Some(Self {
            original: original.to_string(),
            lines,
            title,
            current: 0,
            top: 0,
            modal,
            up,
            rect: Rect::default(),
        })
    }

    pub fn is_up(&self) -> bool {
        self.up
    }

    fn text_at(&self, i: usize) -> String {
        match &self.lines[i] {
            Line::Original => self.original.clone(),
            Line::Item(c) => c.line.clone(),
            Line::Title(_) => self.original.clone(),
        }
    }

    /// The next selectable line in a direction, going round.
    fn step(&self, from: usize, delta: isize) -> usize {
        let n = self.lines.len() as isize;
        let mut i = from as isize;
        for _ in 0..n {
            i = (i + delta).rem_euclid(n);
            if !matches!(self.lines[i as usize], Line::Title(_)) {
                return i as usize;
            }
        }
        from
    }

    fn moved(&mut self, to: usize) -> Reply {
        self.current = to;
        if self.modal {
            Reply::Ignore
        } else {
            Reply::SetText(self.text_at(to))
        }
    }

    pub fn key(&mut self, key: &KeyEvent) -> Reply {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let page = ROWS as isize - 1;
        match key.code {
            KeyCode::Modifier(_) => Reply::Ignore,
            KeyCode::Up => self.moved(self.step(self.current, -1)),
            KeyCode::Down => self.moved(self.step(self.current, 1)),
            // Far: Ctrl+End and Ctrl+Space go to the next item too.
            KeyCode::End if ctrl => self.moved(self.step(self.current, 1)),
            KeyCode::Char(' ') if ctrl => self.moved(self.step(self.current, 1)),
            KeyCode::PageUp => {
                let mut i = self.current;
                for _ in 0..page {
                    let j = self.step(i, -1);
                    if j > i {
                        break;
                    }
                    i = j;
                }
                self.moved(i)
            }
            KeyCode::PageDown => {
                let mut i = self.current;
                for _ in 0..page {
                    let j = self.step(i, 1);
                    if j < i {
                        break;
                    }
                    i = j;
                }
                self.moved(i)
            }
            KeyCode::Home if !ctrl => self.moved(0),
            KeyCode::End => self.moved(self.step(0, -1)),
            KeyCode::Delete if shift => match &self.lines[self.current] {
                Line::Item(c) if c.history && !c.locked => Reply::DeleteHistory(c.line.clone()),
                _ => Reply::Ignore,
            },
            KeyCode::Enter if self.modal => Reply::Accept(self.text_at(self.current)),
            KeyCode::Enter => Reply::CloseAndPass,
            KeyCode::Esc | KeyCode::F(10) => Reply::Close,
            // Editing and moving in the field: the list stays.
            KeyCode::Left | KeyCode::Right | KeyCode::Backspace | KeyCode::Delete => Reply::Pass,
            KeyCode::Char(_) if !(ctrl ^ alt) => Reply::Pass,
            _ => Reply::CloseAndPass,
        }
    }

    /// A click on an item takes it; outside the list — `None` (the list
    /// closes, the click goes on).
    pub fn mouse(&mut self, ev: &MouseEvent) -> Option<Reply> {
        let pos = Position::new(ev.column, ev.row);
        if !self.rect.contains(pos) {
            return matches!(ev.kind, MouseEventKind::Down(_)).then_some(Reply::Close);
        }
        let inner_top = self.rect.y + 1;
        if pos.y < inner_top || pos.y >= self.rect.bottom() - 1 {
            return Some(Reply::Ignore);
        }
        let i = self.top + usize::from(pos.y - inner_top);
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) if i < self.lines.len() => {
                if matches!(self.lines[i], Line::Title(_)) {
                    return Some(Reply::Ignore);
                }
                self.current = i;
                Some(Reply::Accept(self.text_at(i)))
            }
            MouseEventKind::ScrollUp => Some(self.moved(self.step(self.current, -1))),
            MouseEventKind::ScrollDown => Some(self.moved(self.step(self.current, 1))),
            _ => Some(Reply::Ignore),
        }
    }

    /// Draws the list at the field `anchor`, within `area`: under the field,
    /// or over it when there is no room below and the field is in the lower
    /// half (and then always over it).
    pub fn draw(&mut self, buf: &mut Buffer, area: Rect, anchor: Rect) {
        let shown = self.lines.len().min(ROWS);
        let h = shown as u16 + 2;
        let below_room = area.bottom().saturating_sub(anchor.y + 1);
        if !self.up && h > below_room && anchor.y > area.y + area.height / 2 {
            self.up = true;
        }
        let y = if self.up {
            anchor.y.saturating_sub(h).max(area.y)
        } else {
            anchor.y + 1
        };
        let w = anchor
            .width
            .max(22)
            .min(area.right().saturating_sub(anchor.x));
        let rect = Rect::new(anchor.x, y, w, h.min(area.bottom().saturating_sub(y)));
        self.rect = rect;
        if rect.width < 4 || rect.height < 3 {
            return;
        }
        // Keep the current line on the page.
        if self.current < self.top {
            self.top = self.current;
        } else if self.current >= self.top + shown {
            self.top = self.current + 1 - shown;
        }
        let (text, selected, frame) = (theme::MENU_TEXT, theme::MENU_SELECTED, theme::MENU_BOX);
        buf.set_style(rect, text);
        for yy in rect.top()..rect.bottom() {
            for xx in rect.left()..rect.right() {
                buf[(xx, yy)].set_symbol(" ");
            }
        }
        let (l, r, t, b) = (rect.left(), rect.right() - 1, rect.top(), rect.bottom() - 1);
        for xx in l + 1..r {
            buf[(xx, t)].set_symbol("─").set_style(frame);
            buf[(xx, b)].set_symbol("─").set_style(frame);
        }
        for yy in t + 1..b {
            buf[(l, yy)].set_symbol("│").set_style(frame);
            buf[(r, yy)].set_symbol("│").set_style(frame);
        }
        buf[(l, t)].set_symbol("┌").set_style(frame);
        buf[(r, t)].set_symbol("┐").set_style(frame);
        buf[(l, b)].set_symbol("└").set_style(frame);
        buf[(r, b)].set_symbol("┘").set_style(frame);
        let inner = usize::from(rect.width.saturating_sub(4));
        if let Some(id) = self.title {
            let title = format!(" {} ", crate::tr!(id));
            let tw = title
                .chars()
                .count()
                .min(usize::from(rect.width.saturating_sub(2)));
            let tx = l + (rect.width.saturating_sub(tw as u16)) / 2;
            buf.set_stringn(tx, t, &title, tw, theme::MENU_TITLE);
        }
        for (k, line) in self.lines.iter().enumerate().skip(self.top).take(shown) {
            let yy = t + 1 + (k - self.top) as u16;
            if yy >= b {
                break;
            }
            match line {
                Line::Title(id) => {
                    buf[(l, yy)].set_symbol("├").set_style(frame);
                    buf[(r, yy)].set_symbol("┤").set_style(frame);
                    for xx in l + 1..r {
                        buf[(xx, yy)].set_symbol("─").set_style(frame);
                    }
                    let title = format!(" {} ", crate::tr!(id));
                    let tw = title
                        .chars()
                        .count()
                        .min(usize::from(rect.width.saturating_sub(2)));
                    let tx = l + (rect.width.saturating_sub(tw as u16)) / 2;
                    buf.set_stringn(tx, yy, &title, tw, theme::MENU_TITLE);
                }
                Line::Original | Line::Item(_) => {
                    let style = if k == self.current { selected } else { text };
                    for xx in l + 1..r {
                        buf[(xx, yy)].set_style(style);
                    }
                    if let Line::Item(c) = line {
                        if c.locked {
                            buf[(l + 1, yy)].set_symbol("√");
                        }
                        buf.set_stringn(l + 2, yy, &c.shown, inner, style);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(s: &str) -> Candidate {
        Candidate {
            line: s.into(),
            shown: s.into(),
            history: true,
            locked: false,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn moves_put_items_into_the_field() {
        let groups = vec![
            Group {
                title: "MCompletionHistoryTitle",
                items: vec![cand("cargo build"), cand("cargo test")],
            },
            Group {
                title: "MCompletionFilesTitle",
                items: vec![cand("cargo.toml")],
            },
        ];
        let mut c = Completion::new("car", groups, false, false).unwrap();
        assert_eq!(
            c.key(&key(KeyCode::Down)),
            Reply::SetText("cargo build".into())
        );
        assert_eq!(
            c.key(&key(KeyCode::Down)),
            Reply::SetText("cargo test".into())
        );
        // The title between groups is skipped.
        assert_eq!(
            c.key(&key(KeyCode::Down)),
            Reply::SetText("cargo.toml".into())
        );
        // Round to the text as typed.
        assert_eq!(c.key(&key(KeyCode::Down)), Reply::SetText("car".into()));
        assert_eq!(
            c.key(&key(KeyCode::Up)),
            Reply::SetText("cargo.toml".into())
        );
        assert_eq!(c.key(&key(KeyCode::Enter)), Reply::CloseAndPass);
        assert_eq!(c.key(&key(KeyCode::Char('x'))), Reply::Pass);
        assert_eq!(c.key(&key(KeyCode::Tab)), Reply::CloseAndPass);
    }

    #[test]
    fn nothing_to_offer() {
        assert!(Completion::new("x", vec![], false, false).is_none());
        let same = vec![Group {
            title: "MCompletionHistoryTitle",
            items: vec![cand("Dir")],
        }];
        assert!(Completion::new("dir", same, false, false).is_none());
    }
}
