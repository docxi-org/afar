//! The key map: key chords → commands. Far's keys are built in (the `keys`
//! of `command::COMMANDS`); `keymaps/far.toml` next to the settings changes
//! only what it lists:
//!
//! ```toml
//! [panels]
//! "Ctrl+F3" = "sort.by_size"   # rebind
//! "Ctrl+M" = ""                # unbind
//! ```

use std::collections::HashMap;
use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyEventState, KeyModifiers};

use crate::command::{COMMANDS, Command};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Key {
    /// A character key (letters lowercase, symbols unshifted: `[` for `{`).
    Char(char),
    /// A key of the numeric keypad: Gray `+`, `-`, `*`, `/`.
    Gray(char),
    F(u8),
    Up,
    Down,
    Left,
    Right,
    PgUp,
    PgDn,
    Home,
    End,
    Ins,
    Del,
    Enter,
    Tab,
    Esc,
    Bs,
    Space,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Chord {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: Key,
}

const NAMED: &[(&str, Key)] = &[
    ("Up", Key::Up),
    ("Down", Key::Down),
    ("Left", Key::Left),
    ("Right", Key::Right),
    ("PgUp", Key::PgUp),
    ("PgDn", Key::PgDn),
    ("Home", Key::Home),
    ("End", Key::End),
    ("Ins", Key::Ins),
    ("Del", Key::Del),
    ("Enter", Key::Enter),
    ("Tab", Key::Tab),
    ("Esc", Key::Esc),
    ("BS", Key::Bs),
    ("Space", Key::Space),
];

/// US-layout symbols typed with Shift, by their unshifted key.
const SHIFTED: &[(char, char)] = &[
    ('{', '['),
    ('}', ']'),
    (':', ';'),
    ('"', '\''),
    ('<', ','),
    ('>', '.'),
    ('?', '/'),
    ('|', '\\'),
    ('~', '`'),
    ('_', '-'),
    ('!', '1'),
    ('@', '2'),
    ('#', '3'),
    ('$', '4'),
    ('%', '5'),
    ('^', '6'),
    ('&', '7'),
    ('(', '9'),
    (')', '0'),
];

impl Chord {
    /// `Ctrl+Shift+F5`, `Alt+Del`, `Gray+`, `Ctrl+[` (Far's key names).
    pub fn parse(text: &str) -> Option<Self> {
        let mut rest = text.trim();
        let (mut ctrl, mut alt, mut shift) = (false, false, false);
        loop {
            let lower = rest.to_ascii_lowercase();
            if lower.starts_with("ctrl+") && rest.len() > 5 {
                ctrl = true;
                rest = &rest[5..];
            } else if lower.starts_with("alt+") && rest.len() > 4 {
                alt = true;
                rest = &rest[4..];
            } else if lower.starts_with("shift+") && rest.len() > 6 {
                shift = true;
                rest = &rest[6..];
            } else {
                break;
            }
        }
        let key = if let Some(g) = rest.strip_prefix("Gray").filter(|g| g.chars().count() == 1) {
            Key::Gray(g.chars().next()?)
        } else if let Some(n) = rest
            .strip_prefix(['F', 'f'])
            .and_then(|n| n.parse::<u8>().ok())
        {
            Key::F(n)
        } else if let Some((_, k)) = NAMED.iter().find(|(n, _)| n.eq_ignore_ascii_case(rest)) {
            *k
        } else {
            let mut chars = rest.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            Key::Char(c.to_ascii_lowercase())
        };
        Some(Self {
            ctrl,
            alt,
            shift,
            key,
        })
    }

    /// The chord of a key press (after `keys::normalize`).
    pub fn from_event(ev: &KeyEvent) -> Option<Self> {
        let m = ev.modifiers;
        let mut shift = m.contains(KeyModifiers::SHIFT);
        let key = match ev.code {
            KeyCode::Char(c) if ev.state.contains(KeyEventState::KEYPAD) && "+-*/".contains(c) => {
                Key::Gray(c)
            }
            KeyCode::Char(' ') => Key::Space,
            KeyCode::Char(c) => match SHIFTED.iter().find(|(s, _)| *s == c) {
                Some((_, base)) => {
                    shift = true;
                    Key::Char(*base)
                }
                None => Key::Char(c.to_lowercase().next().unwrap_or(c)),
            },
            KeyCode::F(n) => Key::F(n),
            KeyCode::Up => Key::Up,
            KeyCode::Down => Key::Down,
            KeyCode::Left => Key::Left,
            KeyCode::Right => Key::Right,
            KeyCode::PageUp => Key::PgUp,
            KeyCode::PageDown => Key::PgDn,
            KeyCode::Home => Key::Home,
            KeyCode::End => Key::End,
            KeyCode::Insert => Key::Ins,
            KeyCode::Delete => Key::Del,
            KeyCode::Enter => Key::Enter,
            KeyCode::Tab => Key::Tab,
            KeyCode::BackTab => {
                shift = true;
                Key::Tab
            }
            KeyCode::Esc => Key::Esc,
            KeyCode::Backspace => Key::Bs,
            _ => return None,
        };
        Some(Self {
            ctrl: m.contains(KeyModifiers::CONTROL),
            alt: m.contains(KeyModifiers::ALT),
            shift,
            key,
        })
    }

    /// The name Far shows in menus: the keypad keys are "Add",
    /// "Subtract", "Multiply", "Divide" (keyboard.cpp).
    pub fn far_label(&self) -> String {
        let gray = match self.key {
            Key::Gray('+') => "Add",
            Key::Gray('-') => "Subtract",
            Key::Gray('*') => "Multiply",
            Key::Gray('/') => "Divide",
            _ => return self.label(),
        };
        let plain = Chord {
            key: Key::Char('x'),
            ..*self
        }
        .label();
        format!("{}{gray}", plain.trim_end_matches('X'))
    }

    /// The chord as written in key map files (`Gray+`, `Ctrl+[`).
    pub fn label(&self) -> String {
        let mut s = String::new();
        for (on, name) in [
            (self.ctrl, "Ctrl+"),
            (self.alt, "Alt+"),
            (self.shift, "Shift+"),
        ] {
            if on {
                s.push_str(name);
            }
        }
        match self.key {
            Key::Char(c) => s.extend(c.to_uppercase()),
            Key::Gray(c) => {
                s.push_str("Gray");
                s.push(c);
            }
            Key::F(n) => s.push_str(&format!("F{n}")),
            other => s.push_str(
                NAMED
                    .iter()
                    .find(|(_, k)| *k == other)
                    .map_or("?", |(n, _)| n),
            ),
        }
        s
    }
}

/// Chords of the panels (and the command line under them) → commands.
pub struct Keymap {
    panels: HashMap<Chord, Command>,
}

impl Keymap {
    /// Far's keys.
    pub fn far() -> Self {
        let mut panels = HashMap::new();
        for d in COMMANDS {
            for k in d.keys {
                let chord = Chord::parse(k).unwrap_or_else(|| panic!("bad key {k:?}"));
                panels.insert(chord, d.command);
            }
        }
        Self { panels }
    }

    /// Far's keys changed by `file` (if it exists); returns the problems
    /// found in it.
    pub fn load(file: &Path) -> (Self, Vec<String>) {
        let mut map = Self::far();
        let mut problems = Vec::new();
        let Ok(text) = std::fs::read_to_string(file) else {
            return (map, problems);
        };
        let table: toml::Table = match toml::from_str(&text) {
            Ok(t) => t,
            Err(e) => {
                problems.push(format!("{}: {e}", file.display()));
                return (map, problems);
            }
        };
        if let Some(panels) = table.get("panels").and_then(toml::Value::as_table) {
            for (key, value) in panels {
                let Some(chord) = Chord::parse(key) else {
                    problems.push(format!("{}: unknown key {key:?}", file.display()));
                    continue;
                };
                match value.as_str() {
                    Some("") => {
                        map.panels.remove(&chord);
                    }
                    Some(name) => match Command::from_name(name) {
                        Some(c) => {
                            map.panels.insert(chord, c);
                        }
                        None => {
                            problems.push(format!("{}: unknown command {name:?}", file.display()))
                        }
                    },
                    None => problems.push(format!(
                        "{}: {key}: expected a command name",
                        file.display()
                    )),
                }
            }
        }
        (map, problems)
    }

    pub fn panels(&self, chord: &Chord) -> Option<Command> {
        self.panels.get(chord).copied()
    }

    /// The first key of a command (for menus).
    pub fn key_of(&self, command: Command) -> Option<Chord> {
        let mut keys: Vec<Chord> = self
            .panels
            .iter()
            .filter(|(_, c)| **c == command)
            .map(|(k, _)| *k)
            .collect();
        // The documented (first default) key first, then a stable order.
        let default = command.def().keys.first().and_then(|k| Chord::parse(k));
        keys.sort_by_key(|k| (Some(*k) != default, k.label()));
        keys.first().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    #[test]
    fn parses_and_labels() {
        for k in [
            "Ctrl+Shift+F5",
            "Alt+Del",
            "Gray+",
            "Ctrl+Gray*",
            "Ctrl+[",
            "Shift+Up",
            "Ctrl+\\",
            "F10",
        ] {
            assert_eq!(Chord::parse(k).unwrap().label(), k);
        }
        assert_eq!(Chord::parse("ctrl+o").unwrap().label(), "Ctrl+O");
        assert!(Chord::parse("Ctrl+Foo").is_none());
    }

    #[test]
    fn events_match_far_keys() {
        let ctrl = KeyModifiers::CONTROL;
        let map = Keymap::far();
        let find = |e: KeyEvent| map.panels(&Chord::from_event(&e).unwrap());
        assert_eq!(
            find(ev(KeyCode::F(5), KeyModifiers::NONE)),
            Some(Command::Copy)
        );
        assert_eq!(
            find(ev(KeyCode::Char('o'), ctrl)),
            Some(Command::TogglePanels)
        );
        // Ctrl+Shift+[ arrives as '{'.
        assert_eq!(
            find(ev(KeyCode::Char('{'), ctrl | KeyModifiers::SHIFT)),
            Some(Command::InsertActivePath)
        );
        let mut gray = ev(KeyCode::Char('+'), KeyModifiers::NONE);
        assert_eq!(find(gray), None, "the main keyboard's + is typed");
        gray.state = KeyEventState::KEYPAD;
        assert_eq!(find(gray), Some(Command::SelectDialog));
        assert_eq!(find(ev(KeyCode::Char('a'), KeyModifiers::NONE)), None);
    }

    #[test]
    fn user_file_rebinds_and_unbinds() {
        let dir = std::env::temp_dir().join(format!("afar-keymap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("far.toml");
        std::fs::write(
            &file,
            "[panels]\n\"Ctrl+F3\" = \"sort.by_size\"\n\"Ctrl+M\" = \"\"\n\"Ctrl+Q\" = \"nope\"\n",
        )
        .unwrap();
        let (map, problems) = Keymap::load(&file);
        assert_eq!(problems.len(), 1);
        let chord = |k| Chord::parse(k).unwrap();
        assert_eq!(
            map.panels(&chord("Ctrl+F3")),
            Some(Command::Sort(crate::panel::SortMode::Size))
        );
        assert_eq!(map.panels(&chord("Ctrl+M")), None);
        assert_eq!(
            map.key_of(Command::Copy).map(|c| c.label()),
            Some("F5".into())
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
