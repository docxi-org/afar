//! Far's quick search in a panel (Alt+letter, far/fastfind.cpp): a small
//! "Search" box on the panel's bottom border; the typed text is a name
//! prefix with wildcards; a character that finds nothing is not taken.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};

use super::App;
use crate::masks::cmp_name;
use crate::{theme, tr};

pub(super) struct QuickSearch {
    side: usize,
    text: String,
}

/// Width of Far's search box and of its input field.
const BOX_WIDTH: u16 = 22;
const FIELD: usize = 18;

impl App {
    /// Keys while searching, or Alt+character starting a search; gets the
    /// key as typed (before Cyrillic letters are mapped to Latin keys).
    /// `false`: not taken — the search is closed and the key goes on to
    /// the panel, as in Far.
    pub(super) fn quick_search_key(&mut self, key: &KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let printable = match key.code {
            KeyCode::Char(c) if !ctrl && c >= ' ' => Some(c),
            _ => None,
        };
        let Some(qs) = &mut self.quick_search else {
            return match printable {
                Some(c) if alt => {
                    self.quick_search = Some(QuickSearch {
                        side: self.active,
                        text: String::new(),
                    });
                    self.quick_search_type(c);
                    true
                }
                _ => false,
            };
        };
        let side = qs.side;
        match key.code {
            KeyCode::Esc | KeyCode::F(10) => self.quick_search = None,
            KeyCode::Backspace if !ctrl => {
                qs.text.pop();
                let text = qs.text.clone();
                let from = self.panels[side].cursor;
                if let Some(i) = find(&self.panels[side], &text, from, 1, true) {
                    self.panels[side].set_cursor_centered(i);
                }
            }
            KeyCode::Enter if ctrl => {
                let text = qs.text.clone();
                let p = &self.panels[side];
                let shift = key.modifiers.contains(KeyModifiers::SHIFT);
                let found = if shift {
                    find(p, &text, p.cursor, -1, false)
                } else {
                    find(p, &text, p.cursor, 1, false)
                };
                if let Some(i) = found {
                    self.panels[side].set_cursor_centered(i);
                }
            }
            _ => match printable {
                Some(c) => self.quick_search_type(c),
                None => {
                    self.quick_search = None;
                    return false;
                }
            },
        }
        true
    }

    /// Adds a character if something matches the longer text.
    fn quick_search_type(&mut self, c: char) {
        let Some(qs) = &mut self.quick_search else {
            return;
        };
        let mut text = format!("{}{}", qs.text, c.to_lowercase());
        while text.contains("**") {
            text = text.replace("**", "*");
        }
        let text = text.trim_start_matches('"').to_string();
        let side = qs.side;
        let from = self.panels[side].cursor;
        if let Some(i) = find(&self.panels[side], &text, from, 1, true) {
            self.panels[side].set_cursor_centered(i);
            if let Some(qs) = &mut self.quick_search {
                qs.text = text;
            }
        }
    }

    /// The search box: 22×3 on the panel's bottom border, its title at a
    /// fixed offset; returns where the text cursor goes.
    pub(super) fn draw_quick_search(
        &self,
        screen: Rect,
        panel: Rect,
        buf: &mut Buffer,
    ) -> Option<Position> {
        let qs = self.quick_search.as_ref()?;
        if panel.width == 0 || screen.width < BOX_WIDTH || screen.height < 3 {
            return None;
        }
        let x = (panel.x + 9).min(screen.right().saturating_sub(BOX_WIDTH + 1));
        let y = (panel.bottom() - 1).min(screen.bottom() - 3);
        let area = Rect::new(x, y, BOX_WIDTH, 3);
        crate::panel::draw_frame(buf, area, theme::DIALOG_BOX);
        let title = format!(" {} ", tr!("MSearchFileTitle"));
        let n = title.chars().count().min(usize::from(BOX_WIDTH - 8));
        buf.set_stringn(x + 7, y, &title, n, theme::DIALOG_BOX_TITLE);
        buf.set_stringn(x + 1, y + 1, " ", 1, theme::DIALOG_TEXT);
        // A long text shows its end.
        let len = qs.text.chars().count();
        let shown: String = qs
            .text
            .chars()
            .skip(len.saturating_sub(FIELD - 1))
            .collect();
        crate::panel::put(buf, x + 2, y + 1, FIELD as u16, &shown, theme::DIALOG_EDIT);
        buf.set_stringn(x + 20, y + 1, " ", 1, theme::DIALOG_TEXT);
        Some(Position::new(x + 2 + shown.chars().count() as u16, y + 1))
    }

    pub(super) fn quick_search_side(&self) -> Option<usize> {
        self.quick_search.as_ref().map(|q| q.side)
    }
}

/// Far's FindPartName: the text plus `*` as a mask (brackets taken
/// literally), case-insensitive; a trailing `\` or `/` finds folders only.
/// Searches from `from` (inclusive when `include`) in direction `dir`,
/// wrapping around.
fn find(
    panel: &crate::panel::FilePanel,
    text: &str,
    from: usize,
    dir: isize,
    include: bool,
) -> Option<usize> {
    let (text, dirs_only) = match text.strip_suffix(['\\', '/']) {
        Some(t) => (t, true),
        None => (text, false),
    };
    let mut mask = String::new();
    for c in text.chars() {
        match c {
            '[' => mask.push_str("[[]"),
            ']' => mask.push_str("[]]"),
            c => mask.push(c),
        }
    }
    mask.push('*');
    let n = panel.entries.len() as isize;
    if n == 0 {
        return None;
    }
    let start = from as isize + if include { 0 } else { dir };
    (0..n)
        .map(|k| (start + k * dir).rem_euclid(n) as usize)
        .find(|&i| {
            let e = &panel.entries[i];
            e.name != ".." && (!dirs_only || e.is_dir) && cmp_name(&mask, &e.name)
        })
}
