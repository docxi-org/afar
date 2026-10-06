//! Panel commands with their dialogs and menus, as in Far: selecting by
//! mask (Gray +, Gray -, Gray * and their Ctrl/Alt/Shift variants, Ctrl+M),
//! the sort menu (Ctrl+F12), the change-drive menu (Alt+F1, Alt+F2).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use ratatui::layout::Rect;

use super::App;
use super::fileops::{Overlay, Purpose};
use crate::dialog::{Dialog, input_at};
use crate::drives::Drive;
use crate::masks::FileMasks;
use crate::menu::{Item, Menu, Outcome as MenuOutcome};
use crate::panel::{SORT_MODES, SelectMode, size_float};
use crate::tr;
use crate::wm::WinId;

impl App {
    /// Far's select dialog: 55×7, the mask (last one used, "*.*" at
    /// first), OK / Filter / Cancel.
    pub(super) fn select_dialog(&mut self, add: bool) {
        let title = if add {
            tr!("MSelectTitle")
        } else {
            tr!("MUnselectTitle")
        };
        let dialog = Dialog::far(title, 55)
            .row(vec![input_at(5, 45, self.select_mask.clone(), true)])
            .separator()
            .button_row(vec![
                crate::dialog::Button::new(tr!("MOk")).default(),
                crate::dialog::Button::new(tr!("MSelectFilter")).disabled(),
                crate::dialog::Button::new(tr!("MCancel")),
            ]);
        self.overlays.push(Overlay::Dialog {
            dialog,
            purpose: Purpose::Select {
                side: self.active,
                add,
            },
        });
    }

    pub(super) fn select_from_dialog(&mut self, side: usize, add: bool, dialog: Dialog) {
        let text = dialog.input_value(0);
        match FileMasks::parse(&text) {
            Some(masks) => {
                self.select_mask = text;
                let mode = if add {
                    SelectMode::Add
                } else {
                    SelectMode::Remove
                };
                self.panels[side].select_masked(
                    Some(&masks),
                    mode,
                    self.config.panels.select_folders,
                );
                if self.panels[side].sort.selected_first {
                    self.panels[side].resort();
                }
                self.selection_changed[side] = Some(std::time::Instant::now());
            }
            None => {
                // Far keeps the dialog open over the error.
                self.overlays.push(Overlay::Dialog {
                    dialog,
                    purpose: Purpose::Select { side, add },
                });
                self.message(&tr!("MWarning"), &[tr!("MIncorrectMask")], true);
            }
        }
    }

    /// Ctrl+Gray +/- (same extension) and Alt+Gray +/- (same name) as the
    /// item under the cursor.
    pub(super) fn select_like_current(&mut self, add: bool, by_ext: bool) {
        let side = self.active;
        let Some(name) = self.panels[side].current().map(|e| e.name.clone()) else {
            return;
        };
        let (stem, ext) = match name.rfind('.') {
            Some(i) => name.split_at(i),
            None => (name.as_str(), ""),
        };
        // Quoted for separators in names; brackets taken literally.
        let mask = match (by_ext, ext.is_empty()) {
            (true, true) => "*.".to_string(),
            (true, false) => format!("\"*{}\"", literal_brackets(ext)),
            (false, _) => format!("\"{}.*\"", literal_brackets(stem)),
        };
        if let Some(masks) = FileMasks::parse(&mask) {
            let mode = if add {
                SelectMode::Add
            } else {
                SelectMode::Remove
            };
            self.panels[side].select_masked(Some(&masks), mode, self.config.panels.select_folders);
        }
    }
}

/// `[` and `]` as themselves in a mask: `[[]`, `[]]`.
fn literal_brackets(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c == '[' || c == ']' {
            out.push('[');
            out.push(c);
            out.push(']');
        } else {
            out.push(c);
        }
    }
    out
}

/// What a menu was opened for.
pub(super) enum MenuPurpose {
    /// Ctrl+F12.
    Sort { side: usize },
    /// Alt+F1 / Alt+F2: the drives in menu order.
    Drives { side: usize, drives: Vec<Drive> },
}

/// Items of the sort menu after the modes: separator, then these.
const SORT_OPTIONS: usize = 1;

impl App {
    /// Ctrl+F12, Far's sort menu: the modes with their Ctrl+F keys, the
    /// current one marked ▲/▼; then the options. In the menu `+` and `-`
    /// choose ascending/descending (or switch an option on/off), `*` is
    /// like Enter.
    pub(super) fn sort_menu(&mut self) {
        let side = self.active;
        let sort = self.panels[side].sort;
        let mut items: Vec<Item> = SORT_MODES
            .iter()
            .map(|m| {
                let mark = if sort.reverse { '▼' } else { '▲' };
                let mut item = Item::new(tr!(m.label))
                    .checked((m.mode == sort.mode).then_some(mark))
                    .disabled(!m.supported);
                if let Some(n) = m.key {
                    item = item.accel(KeyCode::F(n), KeyModifiers::CONTROL);
                }
                item
            })
            .collect();
        let check = |on: bool| on.then_some('√');
        items.push(Item::separator());
        items.push(
            Item::new(tr!("MMenuSortUseGroups"))
                .accel(KeyCode::F(11), KeyModifiers::SHIFT)
                .disabled(true),
        );
        items.push(
            Item::new(tr!("MMenuSortSelectedFirst"))
                .checked(check(sort.selected_first))
                .accel(KeyCode::F(12), KeyModifiers::SHIFT),
        );
        items.push(Item::new(tr!("MMenuSortDirectoriesFirst")).checked(check(sort.dirs_first)));
        let current = SORT_MODES
            .iter()
            .position(|m| m.mode == sort.mode)
            .unwrap_or(0);
        let x = self.panel_rect(side).x + 4;
        let menu = Menu::new(tr!("MMenuSortTitle"), items)
            .bottom_title("+ - * F4")
            .at_column(x)
            .select(current);
        self.overlays.push(Overlay::Menu {
            menu,
            purpose: MenuPurpose::Sort { side },
        });
    }

    /// Alt+F1 / Alt+F2, Far's change-drive menu: letter, type, total and
    /// free space (Far's default columns); the cursor on the panel's drive.
    pub(super) fn drive_menu(&mut self, side: usize) {
        let drives = crate::drives::list();
        let types: Vec<String> = drives
            .iter()
            .map(|d| d.kind.label_id().map(|id| tr!(id)).unwrap_or_default())
            .collect();
        let sizes: Vec<(String, String)> = drives
            .iter()
            .map(|d| {
                d.sizes
                    .map(|(t, f)| (size_float(t), size_float(f)))
                    .unwrap_or_default()
            })
            .collect();
        let tw = types.iter().map(|t| t.chars().count()).max().unwrap_or(0);
        let aw = sizes.iter().map(|s| s.0.chars().count()).max().unwrap_or(0);
        let bw = sizes.iter().map(|s| s.1.chars().count()).max().unwrap_or(0);
        let items = drives
            .iter()
            .zip(&types)
            .zip(&sizes)
            .map(|((d, t), (total, free))| {
                let mut text = format!("&{}: {t:<tw$}", d.letter);
                if aw + bw > 0 {
                    text += &format!(" │ {total:>aw$} │ {free:>bw$}");
                }
                Item::new(text)
            })
            .collect();
        let current = crate::drives::letter_of(&self.panels[side].path)
            .and_then(|l| drives.iter().position(|d| d.letter == l))
            .unwrap_or(0);
        let x = self.panel_rect(side).x + 5;
        let menu = Menu::new(tr!("MChangeDriveTitle"), items)
            .bottom_title("Del Shift+Del F3 F4 F9")
            .at_column(x)
            .select(current);
        self.overlays.push(Overlay::Menu {
            menu,
            purpose: MenuPurpose::Drives { side, drives },
        });
    }

    /// Where a panel is (or would be, when hidden) on the screen.
    fn panel_rect(&self, side: usize) -> Rect {
        let Some(l) = self.last_layout.as_ref() else {
            return Rect::default();
        };
        let r = l.panels[side];
        if r.width > 0 {
            return r;
        }
        // Hidden: the left or the right half of the panels' area.
        let half = l.top.width / 2;
        let x = if side == 0 { l.top.x } else { l.top.x + half };
        Rect::new(x, l.top.y, half, l.top.height)
    }

    pub(super) fn menu_key(&mut self, key: KeyEvent) {
        let Some(Overlay::Menu { menu, purpose }) = self.overlays.last_mut() else {
            return;
        };
        if matches!(purpose, MenuPurpose::Sort { .. })
            && let KeyCode::Char(c @ ('+' | '-' | '*')) = key.code
        {
            let i = menu.selected;
            self.close_menu(Some(i), Some(c).filter(|c| *c != '*'));
            return;
        }
        if let MenuOutcome::Closed(choice) = menu.handle_key(&key) {
            self.close_menu(choice, None);
        }
    }

    pub(super) fn menu_mouse(&mut self, ev: &MouseEvent) {
        if let Some(Overlay::Menu { menu, .. }) = self.overlays.last_mut()
            && let MenuOutcome::Closed(choice) = menu.handle_mouse(ev)
        {
            self.close_menu(choice, None);
        }
    }

    /// `order`: `+` or `-` pressed in the sort menu.
    fn close_menu(&mut self, choice: Option<usize>, order: Option<char>) {
        let Some(Overlay::Menu { purpose, .. }) = self.overlays.pop() else {
            return;
        };
        let Some(i) = choice else { return };
        match purpose {
            MenuPurpose::Sort { side } => {
                let p = &mut self.panels[side];
                let switch = |on: bool| match order {
                    Some('+') => true,
                    Some('-') => false,
                    _ => !on,
                };
                match i.checked_sub(SORT_MODES.len() + SORT_OPTIONS) {
                    None => {
                        let mode = SORT_MODES[i].mode;
                        match order {
                            Some(c) => {
                                p.sort.mode = mode;
                                p.sort.reverse = c == '-';
                            }
                            None => p.sort.set_mode(mode),
                        }
                    }
                    Some(1) => p.sort.selected_first = switch(p.sort.selected_first),
                    Some(2) => p.sort.dirs_first = switch(p.sort.dirs_first),
                    _ => {}
                }
                p.resort();
            }
            MenuPurpose::Drives { side, drives } => {
                let Some(drive) = drives.get(i) else { return };
                let other = &self.panels[1 - side].path;
                let path = self
                    .drive_paths
                    .get(&drive.letter)
                    .filter(|p| p.is_dir())
                    .cloned()
                    .or_else(|| {
                        (crate::drives::letter_of(other) == Some(drive.letter))
                            .then(|| other.clone())
                    })
                    .unwrap_or_else(|| drive.root.clone());
                self.wm.set_hidden(WinId::Panel(side), false);
                self.active = side;
                self.change_dir(side, &path);
            }
        }
    }
}
