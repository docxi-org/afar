//! F2, the user menu, and the file associations (Enter, Ctrl+PgDn, F3,
//! F4 by a file's mask): commands with Far's metasymbols
//! (`crate::metasym`), the questions `!?…!` asked in one dialog first;
//! `@agent text` goes to the agent, `@view` / `@edit` open afar's windows,
//! the rest runs as one command in the command line.

use std::path::{Path, PathBuf};

use super::App;
use super::fileops::{Overlay, Purpose};
use super::panelcmds::MenuPurpose;
use crate::dialog::{Dialog, input_at, text_at};
use crate::menu::{Item, Menu};
use crate::metasym::{PanelFiles, Panels};
use crate::tr;
use crate::usermenu;

/// What a file's association runs on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Assoc {
    Enter,
    AltEnter,
    View,
    Edit,
}

impl App {
    /// F2: the folder's `FarMenu.ini`, else the user's own menu.
    pub(super) fn user_menu(&mut self) {
        let dir = self.panels[self.active].path.clone();
        let (path, items) = match usermenu::local_file(&dir) {
            Some(p) => {
                let items = usermenu::load(&p);
                (p, items)
            }
            None => usermenu::global(),
        };
        self.show_user_menu(path, items);
    }

    fn show_user_menu(&mut self, path: PathBuf, items: Vec<usermenu::Item>) {
        let shown: Vec<Item> = items
            .iter()
            .map(|i| {
                if i.is_separator() {
                    return Item::separator();
                }
                let sub = if i.submenu.is_some() { " »" } else { "" };
                let label = i.label.replace('&', "&&");
                // A one-letter hotkey is the menu's; a key such as F5 its
                // accelerator.
                let mut chars = i.hotkey.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) if c != '&' => Item::new(format!("&{c}  {label}{sub}")),
                    _ => {
                        let item = Item::new(format!("{:<3}{label}{sub}", i.hotkey));
                        match crate::keymap::Chord::parse(&i.hotkey) {
                            Some(chord) => item.accel_chord(chord),
                            None => item,
                        }
                    }
                }
            })
            .collect();
        let menu = Menu::new(tr!("usermenu-title"), shown).bottom_title(tr!("usermenu-bottom"));
        self.overlays.push(Overlay::Menu {
            menu,
            purpose: MenuPurpose::UserMenu { path, items },
        });
    }

    /// An item chosen: its submenu, or its commands.
    pub(super) fn user_menu_chosen(&mut self, path: PathBuf, items: Vec<usermenu::Item>, i: usize) {
        let Some(item) = items.get(i) else {
            return;
        };
        match &item.submenu {
            Some(sub) => self.show_user_menu(path, sub.clone()),
            None => self.run_far_commands(item.commands.clone()),
        }
    }

    /// F4 in the user menu: its file in the editor.
    pub(super) fn user_menu_edit(&mut self, path: &Path) {
        self.overlays.pop();
        self.open_editor(path, None, None);
    }

    /// Commands with metasymbols: their questions first, then run.
    pub(super) fn run_far_commands(&mut self, lines: Vec<String>) {
        let prompts: Vec<(String, String)> = lines
            .iter()
            .flat_map(|l| crate::metasym::prompts(l))
            .collect();
        if prompts.is_empty() {
            self.run_expanded(&lines, &[]);
            return;
        }
        // Far's way: one dialog, a field for each question.
        let mut d = Dialog::new(tr!("usermenu-ask-title"), 60);
        for (title, init) in &prompts {
            d = d
                .row(vec![text_at(5, title.replace('&', "&&"))])
                .row(vec![input_at(5, 60, init.clone(), Some("UserVar"))]);
        }
        let dialog = d.separator().buttons(&[&tr!("MOk"), &tr!("MCancel")], 0);
        self.overlays.push(Overlay::Dialog {
            dialog,
            purpose: Purpose::FarCommands { lines },
        });
    }

    /// The questions answered: the commands run.
    pub(super) fn far_commands_answered(&mut self, lines: Vec<String>, dialog: &Dialog) {
        let n = lines
            .iter()
            .map(|l| crate::metasym::prompts(l).len())
            .sum::<usize>();
        let answers: Vec<String> = (0..n).map(|k| dialog.input_value(k)).collect();
        self.run_expanded(&lines, &answers);
    }

    fn run_expanded(&mut self, lines: &[String], answers: &[String]) {
        let panels = self.metasym_panels();
        let lists = self.journal.dir().join("lists");
        let mut shell = Vec::new();
        let mut agent = Vec::new();
        // Answers are taken by the lines in turn.
        let mut used = 0;
        for line in lines {
            let asked = crate::metasym::prompts(line).len();
            let mine = answers
                .get(used..(used + asked).min(answers.len()))
                .unwrap_or(&[]);
            used += asked;
            let text = crate::metasym::expand(line, &panels, mine, &lists).text;
            let base = &panels.active.dir;
            if let Some(t) = text.strip_prefix("@agent ") {
                agent.push(t.to_string());
            } else if let Some(f) = text.strip_prefix("@view ") {
                let p = base.join(f.trim().trim_matches('"'));
                self.open_viewer(&p, vec![p.clone()]);
            } else if let Some(f) = text.strip_prefix("@edit ") {
                let p = base.join(f.trim().trim_matches('"'));
                self.open_editor(&p, None, None);
            } else if !text.trim().is_empty() {
                shell.push(text);
            }
        }
        if !agent.is_empty() {
            if self.agent_alive() {
                self.agent_type(&agent.join("\n"));
            } else {
                self.say(tr!("usermenu-agent-off"));
            }
        }
        if !shell.is_empty() {
            self.execute(shell.join(" & "));
        }
    }

    /// The panels for the metasymbols.
    fn metasym_panels(&self) -> Panels {
        let files = |side: usize| {
            let p = &self.panels[side];
            let current = p
                .current()
                .filter(|e| e.name != "..")
                .map(|e| e.name.clone());
            let mut selected: Vec<String> = p.selected().map(|e| e.name.clone()).collect();
            if selected.is_empty() {
                selected.extend(current.clone());
            }
            PanelFiles {
                dir: p.path.clone(),
                description: p.description_of(p.cursor),
                current,
                selected,
            }
        };
        Panels {
            active: files(self.active),
            passive: files(1 - self.active),
            left_active: self.active == 0,
        }
    }

    /// A file's association for `what` runs instead (`false`: there is
    /// none, afar does its own).
    pub(super) fn run_association(&mut self, what: Assoc) -> bool {
        let p = &self.panels[self.active];
        let Some(e) = p.current().filter(|e| !e.is_dir) else {
            return false;
        };
        let name = e.name.clone();
        let command = self.config.associations.iter().find_map(|a| {
            let cmd = match what {
                Assoc::Enter => &a.enter,
                Assoc::AltEnter => &a.alt_enter,
                Assoc::View => &a.view,
                Assoc::Edit => &a.edit,
            };
            (!cmd.trim().is_empty()
                && crate::masks::FileMasks::parse(&a.mask).is_some_and(|m| m.matches(&name)))
            .then(|| cmd.clone())
        });
        let Some(command) = command else {
            return false;
        };
        self.run_far_commands(command.lines().map(str::to_string).collect());
        true
    }
}
