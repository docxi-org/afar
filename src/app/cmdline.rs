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

/// Far's WordDiv (config.cpp) plus blanks.
const WORD_DIV: &str = "~!%^&*()+|{}:\"<>?`-=\\[];',./";
const HISTORY_SIZE: usize = 1000;

fn is_div(c: char) -> bool {
    c.is_whitespace() || WORD_DIV.contains(c)
}

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
        let m = key.modifiers;
        let ctrl = m.contains(KeyModifiers::CONTROL);
        let alt = m.contains(KeyModifiers::ALT);
        let shift = m.contains(KeyModifiers::SHIFT);
        let mut chars: Vec<char> = self.cmdline.chars().collect();
        let len = chars.len();
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
        match key.code {
            // Ctrl+Alt is AltGr on many layouts.
            KeyCode::Char(c) if !(ctrl || alt) || (ctrl && alt) => {
                self.cmdline_insert(&c.to_string());
                return true;
            }
            KeyCode::Backspace if ctrl && shift => {
                chars.drain(..cur);
                self.cmd_cursor = 0;
            }
            KeyCode::Backspace if ctrl => {
                let start = word_start_for_delete(&chars, cur);
                chars.drain(start..cur);
                self.cmd_cursor = start;
            }
            KeyCode::Backspace if cur > 0 => {
                chars.remove(cur - 1);
                self.cmd_cursor = cur - 1;
            }
            KeyCode::Delete if ctrl && cur < len => {
                let end = word_end_for_delete(&chars, cur);
                chars.drain(cur..end);
            }
            KeyCode::Char('t') if ctrl && cur < len => {
                let end = word_end_for_delete(&chars, cur);
                chars.drain(cur..end);
            }
            KeyCode::Delete if cur < len => {
                chars.remove(cur);
            }
            KeyCode::Char('y') if ctrl => {
                chars.clear();
                self.cmd_cursor = 0;
            }
            KeyCode::Char('k') if ctrl => chars.truncate(cur),
            KeyCode::Left if ctrl => self.cmd_cursor = word_left(&chars, cur),
            KeyCode::Right if ctrl => self.cmd_cursor = word_right(&chars, cur),
            KeyCode::Left => self.cmd_cursor = cur.saturating_sub(1),
            KeyCode::Char('s') if ctrl => self.cmd_cursor = cur.saturating_sub(1),
            KeyCode::Right => self.cmd_cursor = (cur + 1).min(len),
            KeyCode::Char('d') if ctrl => self.cmd_cursor = (cur + 1).min(len),
            KeyCode::Home => self.cmd_cursor = 0,
            KeyCode::End => self.cmd_cursor = len,
            KeyCode::Esc => {
                chars.clear();
                self.cmd_cursor = 0;
            }
            _ => return false,
        }
        self.cmdline = chars.into_iter().collect();
        self.cmd_cursor = self.cmd_cursor.min(self.cmdline.chars().count());
        true
    }

    fn set_cmdline(&mut self, text: &str) {
        self.cmdline = text.to_string();
        self.cmd_cursor = self.cmdline.chars().count();
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
fn folder_text(path: &Path) -> String {
    let mut s = path.to_string_lossy().into_owned();
    if !s.ends_with(['\\', '/']) {
        s.push(std::path::MAIN_SEPARATOR);
    }
    quote(&s)
}

/// Far's Ctrl+←: to the start of the previous word.
fn word_left(s: &[char], cur: usize) -> usize {
    let mut p = cur.min(s.len()).saturating_sub(1);
    while p > 0 && !(!is_div(s[p]) && is_div(s[p - 1]) && !s[p].is_whitespace()) {
        if !s[p].is_whitespace() && s[p - 1].is_whitespace() {
            break;
        }
        p -= 1;
    }
    p
}

/// Far's Ctrl+→: to the end of the word.
fn word_right(s: &[char], cur: usize) -> usize {
    if cur >= s.len() {
        return cur;
    }
    let mut p = cur + 1;
    while p < s.len() && !(is_div(s[p]) && !is_div(s[p - 1])) {
        if !s[p].is_whitespace() && s[p - 1].is_whitespace() {
            break;
        }
        p += 1;
    }
    p
}

/// Far's Ctrl+Backspace: deletes back to a word boundary.
fn word_start_for_delete(s: &[char], cur: usize) -> usize {
    let mut p = cur.min(s.len());
    while p > 0 {
        let stop = p > 1 && s[p - 1].is_whitespace() != s[p - 2].is_whitespace();
        p -= 1;
        if p == 0 || stop || is_div(s[p - 1]) {
            break;
        }
    }
    p
}

/// Far's Ctrl+T / Ctrl+Del: deletes forward to a word boundary.
fn word_end_for_delete(s: &[char], cur: usize) -> usize {
    let mut end = cur;
    while end < s.len() {
        let stop = end + 1 < s.len() && s[end].is_whitespace() && !s[end + 1].is_whitespace();
        end += 1;
        if end >= s.len() || stop || is_div(s[end]) {
            break;
        }
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn words_like_far() {
        let s = chars("copy foo.txt d:\\x");
        assert_eq!(word_left(&s, s.len()), 16);
        assert_eq!(word_left(&s, 16), 13);
        assert_eq!(word_left(&s, 9), 5);
        assert_eq!(word_left(&s, 5), 0);
        assert_eq!(word_right(&s, 0), 4);
        assert_eq!(word_right(&s, 4), 5);
        assert_eq!(word_right(&s, 5), 8);
        let s = chars("git commit");
        assert_eq!(word_start_for_delete(&s, s.len()), 4);
        assert_eq!(word_end_for_delete(&s, 0), 3);
    }

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
