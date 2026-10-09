//! The user menu (F2), Far's way (`usermenu.cpp`): `FarMenu.ini` of the
//! folder, else the user's own menu (`%APPDATA%\afar\FarMenu.ini`, made
//! from a template the first time). An item is `hotkey: label` at the
//! line's start, then its commands indented; `{` … `}` after an item make
//! it a submenu; `--:` is a separator; `;` starts a comment (afar's). Commands take Far's metasymbols
//! (`crate::metasym`); afar's own: `@agent text` — to the agent, `@view
//! file`, `@edit file` — the built-in viewer and editor.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Item {
    pub hotkey: String,
    pub label: String,
    pub commands: Vec<String>,
    /// A submenu (`{` … `}` after the item).
    pub submenu: Option<Vec<Item>>,
}

impl Item {
    pub fn is_separator(&self) -> bool {
        self.hotkey == "--" && self.label.is_empty()
    }
}

/// The items of a menu file's text.
pub fn parse(text: &str) -> Vec<Item> {
    let lines: Vec<&str> = text.lines().collect();
    let mut at = 0;
    parse_from(&lines, &mut at)
}

/// Items from line `at` up to the closing `}` (or the end).
fn parse_from(lines: &[&str], at: &mut usize) -> Vec<Item> {
    let mut items: Vec<Item> = Vec::new();
    while *at < lines.len() {
        let line = lines[*at].trim_end();
        *at += 1;
        // `;` comments (afar's; Far's files have none).
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        if line.starts_with('{') {
            let sub = parse_from(lines, at);
            if let Some(last) = items.last_mut() {
                last.submenu = Some(sub);
            }
            continue;
        }
        // `}` closes, unless it is a hotkey (`}: label`).
        if line.starts_with('}') && !line[1..].starts_with(':') {
            break;
        }
        if !line.starts_with([' ', '\t']) {
            let Some(mut colon) = line.find(':') else {
                continue;
            };
            // The hotkey `:` itself.
            if line[colon + 1..].starts_with(':') {
                colon += 1;
            }
            items.push(Item {
                hotkey: line[..colon].to_string(),
                label: line[colon + 1..].trim_start().to_string(),
                ..Default::default()
            });
        } else if let Some(item) = items.last_mut() {
            item.commands.push(line.trim_start().to_string());
        }
    }
    items
}

/// The folder's menu file, if it has one.
pub fn local_file(dir: &Path) -> Option<PathBuf> {
    let p = dir.join("FarMenu.ini");
    p.is_file().then_some(p)
}

/// The user's own menu file.
pub fn global_file() -> PathBuf {
    crate::config::config_dir().join("FarMenu.ini")
}

/// A menu file's items (UTF-8 with or without a BOM, else the OEM code
/// page, as Far reads it).
pub fn load(path: &Path) -> Vec<Item> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    let body = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
    let text = match std::str::from_utf8(body) {
        Ok(t) => t.to_string(),
        Err(_) => {
            use crate::viewer::codepage;
            match codepage::Codec::new(codepage::oem()) {
                Some(c) => {
                    let (mut s, mut i) = (String::new(), 0);
                    while i < body.len() {
                        let (ch, n) = c.decode(&body[i..]);
                        s.push(ch);
                        i += n.max(1);
                    }
                    s
                }
                None => String::from_utf8_lossy(body).into_owned(),
            }
        }
    };
    parse(&text)
}

/// The user's menu, written from the template in the interface's language
/// the first time.
pub fn global() -> (PathBuf, Vec<Item>) {
    let path = global_file();
    if !path.exists() {
        let _ = std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")));
        let _ = std::fs::write(&path, template(crate::i18n::lang()));
    }
    let items = load(&path);
    (path, items)
}

/// The menu a new user starts with: the agent's items.
pub fn template(lang: &str) -> &'static str {
    match lang {
        "ru" => include_str!("../i18n/usermenu/ru.ini"),
        _ => include_str!("../i18n/usermenu/en.ini"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_far_menus() {
        let text = "A:  Archive\n    7z a !.!.7z !.!\n--:\nS:  Sub\n{\nX: Inner\n   echo !.!\n}\n}: Brace key\n   echo }\nF5: Five\n  cmd1\n  cmd2\n";
        let m = parse(text);
        assert_eq!(m.len(), 5);
        assert_eq!(m[0].hotkey, "A");
        assert_eq!(m[0].label, "Archive");
        assert_eq!(m[0].commands, vec!["7z a !.!.7z !.!"]);
        assert!(m[1].is_separator());
        let sub = m[2].submenu.as_ref().unwrap();
        assert_eq!(sub[0].label, "Inner");
        assert_eq!(sub[0].commands, vec!["echo !.!"]);
        assert_eq!(m[3].hotkey, "}");
        assert_eq!(m[4].commands, vec!["cmd1", "cmd2"]);
        for lang in ["ru", "en"] {
            assert!(!parse(template(lang)).is_empty(), "{lang}");
        }
    }
}
