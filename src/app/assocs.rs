//! F9 → Commands → File associations (Far's `filetype.cpp`): the list —
//! Ins, Del, F4 / Enter, Ctrl+Up / Down — and an association's dialog:
//! the masks, the description, the commands for Enter, Ctrl+PgDn, F3 and
//! F4. Kept in `[[associations]]` of config.toml.

use super::App;
use super::fileops::{Overlay, Purpose};
use super::panelcmds::MenuPurpose;
use crate::config::Association;
use crate::dialog::{Dialog, input_at, text_at};
use crate::menu::{Item, Menu};
use crate::tr;

/// What a dialog of the associations was for.
#[derive(Clone, Copy, Debug)]
pub(super) enum AssocEdit {
    /// The association's dialog (`new`: inserted before `at`).
    Item {
        at: usize,
        new: bool,
    },
    Delete {
        at: usize,
    },
}

impl App {
    /// The list of associations, the cursor on `select`.
    pub(super) fn assoc_menu(&mut self, select: usize) {
        let items: Vec<Item> = self
            .config
            .associations
            .iter()
            .map(|a| {
                let text = if a.description.trim().is_empty() {
                    a.mask.clone()
                } else {
                    format!("{} ({})", a.description, a.mask)
                };
                Item::new(text.replace('&', "&&"))
            })
            .collect();
        let n = items.len();
        let mut menu = Menu::new(tr!("MAssocTitle"), items)
            .bottom_title(tr!("assoc-bottom"))
            .min_text_width(40);
        if n > 0 {
            menu = menu.select(select.min(n - 1));
        }
        self.overlays.push(Overlay::Menu {
            menu,
            purpose: MenuPurpose::Associations,
        });
    }

    /// Enter on an association: its dialog.
    pub(super) fn assoc_chosen(&mut self, at: usize) {
        if let Some(a) = self.config.associations.get(at).cloned() {
            self.assoc_dialog(at, false, &a);
        }
    }

    /// Ins, Del, F4, Ctrl+Up / Down in the list; whether the key was one.
    pub(super) fn assoc_menu_key(&mut self, key: &crossterm::event::KeyEvent) -> bool {
        use crossterm::event::{KeyCode, KeyModifiers};
        let Some(Overlay::Menu {
            menu,
            purpose: MenuPurpose::Associations,
        }) = self.overlays.last()
        else {
            return false;
        };
        let at = menu.selected;
        let n = self.config.associations.len();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let plain = key.modifiers.is_empty();
        match key.code {
            KeyCode::Insert if plain => {
                self.overlays.pop();
                let at = at.min(n);
                self.assoc_dialog(at, true, &Association::default());
            }
            KeyCode::Delete if plain && at < n => {
                self.overlays.pop();
                let mask = self.config.associations[at].mask.clone();
                let dialog = Dialog::message(
                    &tr!("MAssocTitle"),
                    &[tr!("MAskDelAssoc"), format!("\"{mask}\"")],
                    &[&tr!("MDelete"), &tr!("MCancel")],
                    true,
                );
                self.overlays.push(Overlay::Dialog {
                    dialog,
                    purpose: Purpose::AssocEdit(AssocEdit::Delete { at }),
                });
            }
            KeyCode::F(4) if at < n && !ctrl => {
                self.overlays.pop();
                self.assoc_chosen(at);
            }
            KeyCode::Up | KeyCode::Down if ctrl && at < n => {
                self.overlays.pop();
                let to = if key.code == KeyCode::Up {
                    at.checked_sub(1)
                } else {
                    Some(at + 1).filter(|k| *k < n)
                };
                let shown = match to {
                    Some(to) => {
                        self.config.associations.swap(at, to);
                        self.assoc_save();
                        to
                    }
                    None => at,
                };
                self.assoc_menu(shown);
            }
            _ => return false,
        }
        true
    }

    /// Far's association dialog.
    fn assoc_dialog(&mut self, at: usize, new: bool, a: &Association) {
        let field = |label: String, value: &str, history: Option<&str>| {
            vec![
                vec![text_at(5, label)],
                vec![input_at(5, 66, value.to_string(), history).exec()],
            ]
        };
        let mut d = Dialog::new(tr!("MFileAssocTitle"), 66);
        let rows = [
            field(tr!("MFileAssocMasks"), &a.mask, Some("Masks")),
            field(tr!("MFileAssocDescr"), &a.description, None),
            field(tr!("MFileAssocExec"), &a.enter, None),
            field(tr!("MFileAssocAltExec"), &a.alt_enter, None),
            field(tr!("MFileAssocView"), &a.view, None),
            field(tr!("MFileAssocEdit"), &a.edit, None),
        ];
        for (k, pair) in rows.into_iter().enumerate() {
            if k == 2 {
                d = d.separator();
            }
            for row in pair {
                d = d.row(row);
            }
        }
        let dialog = d.separator().buttons(&[&tr!("MOk"), &tr!("MCancel")], 0);
        self.overlays.push(Overlay::Dialog {
            dialog,
            purpose: Purpose::AssocEdit(AssocEdit::Item { at, new }),
        });
    }

    /// A dialog of the associations closed: saved, the list again.
    pub(super) fn assoc_edited(&mut self, edit: AssocEdit, button: Option<usize>, dialog: &Dialog) {
        match edit {
            AssocEdit::Item { at, new } => {
                if button != Some(0) {
                    self.assoc_menu(at);
                    return;
                }
                let a = Association {
                    mask: dialog.input_value(0).trim().to_string(),
                    description: dialog.input_value(1).trim().to_string(),
                    enter: dialog.input_value(2).trim().to_string(),
                    alt_enter: dialog.input_value(3).trim().to_string(),
                    view: dialog.input_value(4).trim().to_string(),
                    edit: dialog.input_value(5).trim().to_string(),
                };
                // A mask Far would not take: said, and asked again.
                if crate::masks::FileMasks::parse(&a.mask).is_none() {
                    self.assoc_dialog(at, new, &a);
                    self.message(&tr!("MFileAssocTitle"), &[tr!("MIncorrectMask")], true);
                    return;
                }
                let list = &mut self.config.associations;
                let at = at.min(list.len());
                if new {
                    list.insert(at, a);
                } else if at < list.len() {
                    list[at] = a;
                }
                self.assoc_save();
                self.assoc_menu(at);
            }
            AssocEdit::Delete { at } => {
                if button == Some(0) && at < self.config.associations.len() {
                    self.config.associations.remove(at);
                    self.assoc_save();
                }
                self.assoc_menu(at);
            }
        }
    }

    fn assoc_save(&mut self) {
        let path = crate::config::config_path();
        if let Err(e) = self.config.save(&path) {
            self.say(tr!("settings-save-failed", error = e));
        }
    }
}
