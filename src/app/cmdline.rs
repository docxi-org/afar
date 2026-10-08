//! The command line as in Far (far/cmdline.cpp, far/edit.cpp,
//! far/panelmix.cpp MakePathForUI): history (Ctrl+E / Ctrl+X, Ctrl+End
//! completes from it), word editing, inserting paths and names.
//!
//! Panels see keys first: Home/End and the arrows move the panel cursor;
//! the line gets Ctrl+←/→ only when it has text, and everything when the
//! panels are hidden.

use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{App, quote};
use crate::command::Command;
use crate::lineedit::{self, Done};

const HISTORY_SIZE: usize = 1000;

/// Commands run from the command line, oldest first; kept in the history
/// database (`crate::history`), this is its copy for browsing.
pub(super) struct History {
    items: Vec<String>,
    /// Position while browsing with Ctrl+E / Ctrl+X; `None`: below the
    /// newest, at the line being typed.
    pos: Option<usize>,
    /// The line being typed when browsing started.
    typed: String,
    /// Ctrl+End completion: the typed prefix and the last match.
    complete: Option<(String, usize)>,
}

impl History {
    /// The commands of the history database; the old `commands.txt` (if
    /// any) goes into it once.
    pub(super) fn load(store: &crate::history::History, legacy: &Path) -> Self {
        if let Ok(text) = std::fs::read_to_string(legacy) {
            let lines: Vec<String> = text
                .lines()
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect();
            store.import_commands(&lines);
        }
        let mut items: Vec<String> = store
            .recent(crate::history::Kind::Command, "")
            .into_iter()
            .collect();
        if items.len() > HISTORY_SIZE {
            items.drain(..items.len() - HISTORY_SIZE);
        }
        Self::new(items)
    }

    pub(super) fn new(items: Vec<String>) -> Self {
        Self {
            items,
            pos: None,
            typed: String::new(),
            complete: None,
        }
    }

    /// A command was run: it becomes the newest entry (no duplicates).
    pub(super) fn add(&mut self, cmd: &str) {
        let cmd = cmd.trim();
        if cmd.is_empty() || cmd.contains('\n') {
            return;
        }
        self.items.retain(|c| !c.eq_ignore_ascii_case(cmd));
        self.items.push(cmd.to_string());
        if self.items.len() > HISTORY_SIZE {
            self.items.drain(..self.items.len() - HISTORY_SIZE);
        }
        self.pos = None;
        self.complete = None;
    }

    /// Ctrl+E (`back`) / Ctrl+X: the line to show.
    fn step(&mut self, back: bool, current: &str) -> Option<String> {
        if self.items.is_empty() {
            return None;
        }
        if self.pos.is_none() {
            self.typed = current.to_string();
        }
        let last = self.items.len() - 1;
        self.pos = match (self.pos, back) {
            (None, true) => Some(last),
            (None, false) => None,
            (Some(0), true) => Some(0),
            (Some(i), true) => Some(i - 1),
            (Some(i), false) if i == last => None,
            (Some(i), false) => Some(i + 1),
        };
        Some(match self.pos {
            Some(i) => self.items[i].clone(),
            None => self.typed.clone(),
        })
    }

    /// Ctrl+End at the end of the line: the next older command starting
    /// with what was typed (Far's GetSimilar).
    fn complete(&mut self, current: &str) -> Option<String> {
        let (prefix, from) = match &self.complete {
            Some((prefix, last)) if self.items.get(*last).is_some_and(|c| c == current) => {
                (prefix.clone(), *last)
            }
            _ => (current.to_string(), self.items.len()),
        };
        let lower = prefix.to_lowercase();
        let found = (0..from).rev().find(|&i| {
            let c = &self.items[i];
            c.len() > prefix.len() && c.to_lowercase().starts_with(&lower)
        });
        match found {
            Some(i) => {
                self.complete = Some((prefix, i));
                Some(self.items[i].clone())
            }
            None => {
                // Past the oldest: back to what was typed.
                self.complete = None;
                (from < self.items.len()).then_some(prefix)
            }
        }
    }
}

impl App {
    /// The line's own keys; `false` when it does not take `key`.
    pub(super) fn cmdline_key(&mut self, key: &KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let len = self.cmdline.chars().count();
        let cur = self.cmd_cursor.min(len);
        // Panels hidden: Up/Down browse the history, as in Far.
        let browse = match key.code {
            KeyCode::Up if !self.panels_visible() && !ctrl => Some(true),
            KeyCode::Down if !self.panels_visible() && !ctrl => Some(false),
            _ => None,
        };
        if let Some(back) = browse {
            self.history_step(back);
            return true;
        }
        if key.code == KeyCode::End && ctrl && cur == len {
            let line = self.cmdline.clone();
            if let Some(text) = self.cmd_history.complete(&line) {
                self.set_cmdline(&text);
            }
            return true;
        }
        self.cmd_history.complete = None;
        if key.code == KeyCode::Esc {
            self.clear_cmdline();
            return true;
        }
        // The selection (Far's `Edit`), then the editing keys.
        match lineedit::key(
            &mut self.cmdline,
            &mut self.cmd_cursor,
            &mut self.cmd_anchor,
            key,
        ) {
            Done::Yes => return true,
            Done::Copy(text) => {
                if let Err(e) = self.clip_set(&text) {
                    self.say(e);
                }
                return true;
            }
            Done::Paste => {
                if let Some((text, _)) = self.clip_get_block() {
                    self.cmdline_insert(&text);
                }
                return true;
            }
            Done::No => {}
        }
        lineedit::edit(&mut self.cmdline, &mut self.cmd_cursor, key)
    }

    fn set_cmdline(&mut self, text: &str) {
        self.cmdline = text.to_string();
        self.cmd_cursor = self.cmdline.chars().count();
        self.cmd_anchor = None;
    }

    /// Ctrl+E / Ctrl+X: the previous / next command of the history.
    pub(super) fn history_step(&mut self, back: bool) {
        let line = self.cmdline.clone();
        if let Some(text) = self.cmd_history.step(back, &line) {
            self.set_cmdline(&text);
        }
    }

    /// Far's commands that put paths and names into the command line.
    pub(super) fn insert_for(&mut self, command: Command) {
        let a = self.active;
        let text = match command {
            // The left / right / active / passive panel's folder.
            Command::InsertLeftPath => Some(folder_text(&self.panels[0].path)),
            Command::InsertRightPath => Some(folder_text(&self.panels[1].path)),
            Command::InsertActivePath => Some(folder_text(&self.panels[a].path)),
            Command::InsertPassivePath => Some(folder_text(&self.panels[1 - a].path)),
            // The full name of the current file, here or on the passive panel.
            Command::InsertFullName => self.full_name_text(a),
            Command::InsertPassiveFullName => self.full_name_text(1 - a),
            // The passive panel's current name.
            Command::InsertPassiveName => match self.panels[1 - a].current() {
                Some(e) if e.name != ".." => Some(format!("{} ", quote(&e.name))),
                _ => None,
            },
            _ => None,
        };
        if let Some(text) = text {
            self.cmdline_insert(&text);
        }
    }

    fn full_name_text(&self, side: usize) -> Option<String> {
        let p = &self.panels[side];
        let e = p.current()?;
        let full = if e.name == ".." {
            p.path.clone()
        } else {
            p.path.join(&e.name)
        };
        Some(format!("{} ", quote(&full.to_string_lossy())))
    }
}

/// A panel's folder with a trailing separator, quoted when it has spaces.
pub(super) fn folder_text(path: &Path) -> String {
    let mut s = path.to_string_lossy().into_owned();
    if !s.ends_with(['\\', '/']) {
        s.push(std::path::MAIN_SEPARATOR);
    }
    quote(&s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_browses_and_completes() {
        let mut h = History::new(Vec::new());
        h.add("dir");
        h.add("git status");
        h.add("git log");
        h.add("dir");
        assert_eq!(h.step(true, "typed").as_deref(), Some("dir"));
        assert_eq!(h.step(true, "").as_deref(), Some("git log"));
        assert_eq!(h.step(false, "").as_deref(), Some("dir"));
        assert_eq!(h.step(false, "").as_deref(), Some("typed"));
        assert_eq!(h.complete("git").as_deref(), Some("git log"));
        assert_eq!(h.complete("git log").as_deref(), Some("git status"));
        assert_eq!(h.complete("git status").as_deref(), Some("git"));
    }
}
