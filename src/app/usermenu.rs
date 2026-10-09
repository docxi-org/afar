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

/// Command lines of an item's dialog (Far's ten).
const COMMAND_LINES: usize = 10;

/// What a file's association runs on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Assoc {
    Enter,
    AltEnter,
    View,
    Edit,
}

/// The user menu open: its file, its items, the submenus entered.
#[derive(Clone, Debug)]
pub(super) struct MenuState {
    path: PathBuf,
    /// A folder's menu (BS goes to its parent's, Shift+F2 to the main one).
    local: bool,
    file: usermenu::MenuFile,
    /// The submenus entered: indexes from the top.
    level: Vec<usize>,
}

impl MenuState {
    fn open(path: PathBuf, local: bool) -> Self {
        let file = usermenu::load(&path);
        Self {
            path,
            local,
            file,
            level: Vec::new(),
        }
    }

    fn main() -> Self {
        let (path, file) = usermenu::global();
        Self {
            path,
            local: false,
            file,
            level: Vec::new(),
        }
    }

    fn items(&self) -> &Vec<usermenu::Item> {
        let mut items = &self.file.items;
        for &k in &self.level {
            match items.get(k).and_then(|i| i.submenu.as_ref()) {
                Some(sub) => items = sub,
                None => break,
            }
        }
        items
    }

    fn items_mut(&mut self) -> &mut Vec<usermenu::Item> {
        // The levels entered are items with submenus.
        let mut items = &mut self.file.items;
        for &k in &self.level {
            items = items[k].submenu.get_or_insert_with(Vec::new);
        }
        items
    }

    /// The menu's title: main or local, or the submenu's label.
    fn title(&self) -> String {
        let mut items = &self.file.items;
        let mut label = None;
        for &k in &self.level {
            if let Some(item) = items.get(k) {
                label = Some(item.label.clone());
                if let Some(sub) = &item.submenu {
                    items = sub;
                }
            }
        }
        match label {
            Some(l) => l,
            None if self.local => tr!("MLocalMenuTitle"),
            None => tr!("MMainMenuTitle"),
        }
    }
}

/// What a dialog of the user menu was for.
#[derive(Clone, Debug)]
pub(super) enum MenuEdit {
    /// Ins: a command or a submenu, before item `at`.
    InsertKind {
        at: usize,
    },
    /// The item's dialog (`new`: inserted before `at`).
    Item {
        at: usize,
        new: bool,
        submenu: bool,
    },
    Delete {
        at: usize,
    },
}

impl App {
    /// F2: the folder's `FarMenu.ini`, else the main menu.
    pub(super) fn user_menu(&mut self) {
        let dir = self.panels[self.active].path.clone();
        let state = match usermenu::local_file(&dir) {
            Some(p) => MenuState::open(p, true),
            None => MenuState::main(),
        };
        self.show_user_menu(state, 0);
    }

    fn show_user_menu(&mut self, state: MenuState, select: usize) {
        let shown: Vec<Item> = state
            .items()
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
        let empty = shown.is_empty();
        let mut menu = Menu::new(state.title(), shown)
            .bottom_title(tr!("usermenu-bottom"))
            .min_text_width(30);
        if !empty {
            menu = menu.select(select.min(state.items().len().saturating_sub(1)));
        }
        self.overlays.push(Overlay::Menu {
            menu,
            purpose: MenuPurpose::UserMenu(Box::new(state)),
        });
    }

    /// Enter on an item: its submenu, or its commands.
    pub(super) fn user_menu_chosen(&mut self, mut state: MenuState, i: usize) {
        let Some(item) = state.items().get(i).cloned() else {
            return;
        };
        if item.submenu.is_some() {
            state.level.push(i);
            self.show_user_menu(state, 0);
        } else if !item.is_separator() {
            self.run_far_commands(item.commands);
        }
    }

    /// Esc (or Left) in a submenu: the menu above, on the submenu's item.
    pub(super) fn user_menu_up(&mut self, mut state: MenuState) -> bool {
        match state.level.pop() {
            Some(k) => {
                self.show_user_menu(state, k);
                true
            }
            None => false,
        }
    }

    /// A key of the user menu (Far's `usermenu.cpp`): Ins, Del, F4, Ctrl+Up
    /// and Down edit it, Alt+F4 its file, Shift+F2 the main / the folder's
    /// menu, BS the parent folder's, Right / Left into and out of a
    /// submenu. Whether the key was one of them.
    pub(super) fn user_menu_key(&mut self, key: &crossterm::event::KeyEvent) -> bool {
        use crossterm::event::{KeyCode, KeyModifiers};
        let Some(Overlay::Menu {
            menu,
            purpose: MenuPurpose::UserMenu(state),
        }) = self.overlays.last()
        else {
            return false;
        };
        let m = key.modifiers;
        let (ctrl, alt, shift) = (
            m.contains(KeyModifiers::CONTROL),
            m.contains(KeyModifiers::ALT),
            m.contains(KeyModifiers::SHIFT),
        );
        let at = menu.selected;
        let has_item = !state.items().is_empty();
        let on_sub = state.items().get(at).is_some_and(|i| i.submenu.is_some());
        let action = match key.code {
            KeyCode::Insert if !ctrl && !alt && !shift => 1,
            KeyCode::Delete if has_item && m.is_empty() => 2,
            KeyCode::F(4) if has_item && !ctrl && !alt => 3,
            KeyCode::Up if ctrl && has_item => 4,
            KeyCode::Down if ctrl && has_item => 5,
            KeyCode::F(4) if alt || ctrl => 6,
            KeyCode::F(2) if shift => 7,
            KeyCode::Backspace if m.is_empty() && state.local && state.level.is_empty() => 8,
            KeyCode::Right if m.is_empty() && on_sub => 9,
            KeyCode::Left if m.is_empty() && !state.level.is_empty() => 10,
            _ => return false,
        };
        let Some(Overlay::Menu {
            purpose: MenuPurpose::UserMenu(state),
            ..
        }) = self.overlays.pop()
        else {
            return false;
        };
        let mut state = *state;
        match action {
            1 => self.user_menu_ask(
                state,
                MenuEdit::InsertKind { at },
                Dialog::message(
                    &tr!("MUserMenuTitle"),
                    &[tr!("MAskInsertMenuOrCommand")],
                    &[&tr!("MMenuInsertCommand"), &tr!("MMenuInsertMenu")],
                    false,
                ),
            ),
            2 => {
                let item = state.items()[at].clone();
                let question = if item.submenu.is_some() {
                    tr!("MAskDeleteSubMenuItem")
                } else {
                    tr!("MAskDeleteMenuItem")
                };
                let dialog = Dialog::message(
                    &tr!("MUserMenuTitle"),
                    &[question, format!("\"{}\"", item.label)],
                    &[&tr!("MDelete"), &tr!("MCancel")],
                    true,
                );
                self.user_menu_ask(state, MenuEdit::Delete { at }, dialog);
            }
            3 => {
                let submenu = state.items()[at].submenu.is_some();
                let item = state.items()[at].clone();
                self.user_menu_item_dialog(state, at, false, submenu, &item);
            }
            4 | 5 => {
                let to = if action == 4 {
                    at.checked_sub(1)
                } else {
                    Some(at + 1).filter(|k| *k < state.items().len())
                };
                match to {
                    Some(to) => {
                        state.items_mut().swap(at, to);
                        self.user_menu_save(&state);
                        self.show_user_menu(state, to);
                    }
                    None => self.show_user_menu(state, at),
                }
            }
            6 => {
                let path = state.path.clone();
                self.open_editor(&path, None, None);
            }
            7 => {
                // The main menu and the folder's, by turns.
                let dir = self.panels[self.active].path.clone();
                let next = match (state.local, usermenu::local_file(&dir)) {
                    (false, Some(p)) => MenuState::open(p, true),
                    _ => MenuState::main(),
                };
                self.show_user_menu(next, 0);
            }
            8 => {
                // The nearest menu of the folders above, else the main one.
                let above = state
                    .path
                    .parent()
                    .and_then(Path::parent)
                    .and_then(usermenu::find_up);
                let next = match above {
                    Some(p) => MenuState::open(p, true),
                    None => MenuState::main(),
                };
                self.show_user_menu(next, 0);
            }
            9 => {
                state.level.push(at);
                self.show_user_menu(state, 0);
            }
            _ => {
                self.user_menu_up(state);
            }
        }
        true
    }

    fn user_menu_ask(&mut self, state: MenuState, edit: MenuEdit, dialog: Dialog) {
        self.overlays.push(Overlay::Dialog {
            dialog,
            purpose: Purpose::UserMenuEdit(Box::new(state), edit),
        });
    }

    /// Far's item dialog: the hotkey, the label, ten command lines (a
    /// submenu: the hotkey and the label).
    fn user_menu_item_dialog(
        &mut self,
        state: MenuState,
        at: usize,
        new: bool,
        submenu: bool,
        item: &usermenu::Item,
    ) {
        let title = if submenu {
            tr!("MEditSubmenuTitle")
        } else {
            tr!("MEditMenuTitle")
        };
        let mut d = Dialog::new(title, 66)
            .row(vec![text_at(5, tr!("MEditMenuHotKey"))])
            .row(vec![input_at(5, 4, item.hotkey.clone(), None)])
            .row(vec![text_at(5, tr!("MEditMenuLabel"))])
            .row(vec![input_at(5, 66, item.label.clone(), None)]);
        if !submenu {
            d = d
                .separator()
                .row(vec![text_at(5, tr!("MEditMenuCommands"))]);
            for k in 0..COMMAND_LINES {
                let line = item.commands.get(k).cloned().unwrap_or_default();
                d = d.row(vec![input_at(5, 66, line, None).exec().text_line()]);
            }
        }
        let dialog = d.separator().buttons(&[&tr!("MOk"), &tr!("MCancel")], 0);
        self.user_menu_ask(state, MenuEdit::Item { at, new, submenu }, dialog);
    }

    /// A dialog of the user menu closed: the menu changed (and saved), and
    /// shown again.
    pub(super) fn user_menu_edited(
        &mut self,
        mut state: MenuState,
        edit: MenuEdit,
        button: Option<usize>,
        dialog: &Dialog,
    ) {
        match edit {
            MenuEdit::InsertKind { at } => match button {
                Some(b @ (0 | 1)) => {
                    self.user_menu_item_dialog(state, at, true, b == 1, &usermenu::Item::default());
                }
                _ => self.show_user_menu(state, at),
            },
            MenuEdit::Item { at, new, submenu } => {
                if button != Some(0) {
                    self.show_user_menu(state, at);
                    return;
                }
                let mut commands: Vec<String> = (0..COMMAND_LINES)
                    .map(|k| dialog.input_value(2 + k))
                    .collect();
                while commands.last().is_some_and(|c| c.trim().is_empty()) {
                    commands.pop();
                }
                let items = state.items_mut();
                let old = if new {
                    usermenu::Item::default()
                } else {
                    items.get(at).cloned().unwrap_or_default()
                };
                let item = usermenu::Item {
                    hotkey: dialog.input_value(0).trim().to_string(),
                    label: dialog.input_value(1).trim().to_string(),
                    commands: if submenu { Vec::new() } else { commands },
                    submenu: if submenu {
                        Some(old.submenu.unwrap_or_default())
                    } else {
                        None
                    },
                };
                let at = at.min(items.len());
                if new {
                    items.insert(at, item);
                } else {
                    items[at] = item;
                }
                self.user_menu_save(&state);
                self.show_user_menu(state, at);
            }
            MenuEdit::Delete { at } => {
                if button == Some(0) && at < state.items().len() {
                    state.items_mut().remove(at);
                    self.user_menu_save(&state);
                }
                self.show_user_menu(state, at);
            }
        }
    }

    fn user_menu_save(&mut self, state: &MenuState) {
        if let Err(e) = usermenu::save(&state.path, &state.file) {
            self.say(tr!("usermenu-save-failed", error = e.to_string()));
        }
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
