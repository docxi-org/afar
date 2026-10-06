//! Settings: `config.toml` in the roaming profile (`%APPDATA%\afar`). The
//! first run writes it from a commented template in the interface language;
//! afar's own changes (the settings dialogs) are written with `toml_edit`,
//! so the comments stay. The state of the last run (panels, layout) is not
//! here but in `state.json` (see `App::save_state`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub general: General,
    pub panels: Panels,
    pub confirm: Confirm,
    pub agent: Agent,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    /// `en`, `ru`, …; empty: the Windows interface language.
    pub language: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Panels {
    /// Gray + and Gray * select folders too (Far's "Select folders").
    pub select_folders: bool,
}

/// Far's confirmations (Options → Confirmations).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Confirm {
    pub delete_folder: bool,
    pub read_only: bool,
    /// Esc during an operation asks before cancelling.
    pub esc: bool,
}

impl Default for Confirm {
    fn default() -> Self {
        Self {
            delete_folder: true,
            read_only: true,
            esc: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Agent {
    /// The agent's program and extra arguments (before afar's own).
    pub command: String,
    pub args: Vec<String>,
    /// Start in live observation mode (journal entries go with prompts).
    pub live: bool,
    pub permissions: Permissions,
}

impl Default for Agent {
    fn default() -> Self {
        Self {
            command: "claude".into(),
            args: Vec::new(),
            live: false,
            permissions: Permissions::default(),
        }
    }
}

/// What the agent may do through afar without asking.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Allow,
    Confirm,
    Deny,
}

/// The agent's permissions by kind of action (docs/02-architecture.md,
/// "Политика разрешений").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Permissions {
    /// Moving the panels, selecting, showing files.
    pub navigate: Level,
    pub mkdir: Level,
    pub copy: Level,
    #[serde(rename = "move")]
    pub move_: Level,
    /// Deleting to the recycle bin.
    pub delete: Level,
    pub delete_permanent: Level,
    pub run_command: Level,
}

impl Default for Permissions {
    fn default() -> Self {
        Self {
            navigate: Level::Allow,
            mkdir: Level::Allow,
            copy: Level::Confirm,
            move_: Level::Confirm,
            delete: Level::Confirm,
            delete_permanent: Level::Deny,
            run_command: Level::Confirm,
        }
    }
}

/// `%APPDATA%\afar` (or `~/.config/afar`).
pub fn config_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("afar")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// The commented template in a language (English when there is none).
pub fn template(lang: &str) -> &'static str {
    match lang {
        "ru" => include_str!("../i18n/config/ru.toml"),
        _ => include_str!("../i18n/config/en.toml"),
    }
}

impl Config {
    /// Reads `path`; a missing file is created from the template in
    /// `lang`. Returns the settings and a problem to tell the user (a
    /// broken file leaves the defaults in force).
    pub fn load(path: &Path, lang: impl FnOnce() -> String) -> (Self, Option<String>) {
        match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str(&text) {
                Ok(config) => (config, None),
                Err(e) => (Self::default(), Some(format!("{}: {e}", path.display()))),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let text = template(&lang());
                let written = path
                    .parent()
                    .map_or(Ok(()), std::fs::create_dir_all)
                    .and_then(|()| std::fs::write(path, text));
                let config = toml::from_str(text).unwrap_or_default();
                (
                    config,
                    written.err().map(|e| format!("{}: {e}", path.display())),
                )
            }
            Err(e) => (Self::default(), Some(format!("{}: {e}", path.display()))),
        }
    }

    /// Writes the settings into `path`, keeping its comments and layout.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = std::fs::read_to_string(path).unwrap_or_else(|_| template("en").to_string());
        let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| format!("{e}"))?;
        let new: toml_edit::DocumentMut = toml::to_string(self)
            .map_err(|e| e.to_string())?
            .parse()
            .map_err(|e| format!("{e}"))?;
        merge(doc.as_table_mut(), new.as_table());
        std::fs::write(path, doc.to_string()).map_err(|e| e.to_string())
    }
}

/// Sets the values of `new` in `old`, keeping `old`'s comments.
fn merge(old: &mut toml_edit::Table, new: &toml_edit::Table) {
    for (key, item) in new.iter() {
        match (old.get_mut(key), item) {
            (Some(toml_edit::Item::Table(o)), toml_edit::Item::Table(n)) => merge(o, n),
            (Some(toml_edit::Item::Value(o)), toml_edit::Item::Value(n)) => {
                if o.to_string().trim() != n.to_string().trim() {
                    let decor = o.decor().clone();
                    *o = n.clone();
                    *o.decor_mut() = decor;
                }
            }
            (Some(_), _) => {}
            (None, item) => {
                old.insert(key, item.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_are_the_defaults() {
        for lang in ["en", "ru"] {
            let config: Config = toml::from_str(template(lang)).unwrap();
            assert_eq!(config, Config::default(), "{lang}");
        }
    }

    #[test]
    fn saving_keeps_comments() {
        let dir = std::env::temp_dir().join(format!("afar-config-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("config.toml");
        let (mut config, problem) = Config::load(&path, || "ru".into());
        assert!(problem.is_none());
        config.agent.permissions.copy = Level::Allow;
        config.confirm.esc = false;
        config.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# "), "comments kept");
        let (back, _) = Config::load(&path, || unreachable!());
        assert_eq!(back, config);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
