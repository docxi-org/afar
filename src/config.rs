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
    pub viewer: Viewer,
    pub autocomplete: Autocomplete,
    pub history: History,
}

/// How a history list is ordered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryOrder {
    /// By how often and how lately, more weight in the current folder.
    #[default]
    Frecency,
    /// The newest first (Far).
    Recent,
}

/// What to do with the entries the agent made.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentEntries {
    /// Marked, after the user's, never put into an empty field.
    #[default]
    Marked,
    /// Like the user's.
    Mixed,
}

/// Histories (docs/15).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct History {
    pub dialogs: bool,
    pub commands: bool,
    /// A command starting with a blank is not kept (bash's ignorespace).
    pub skip_leading_space: bool,
    /// Passwords, tokens and keys become `***` in the history.
    pub redact_secrets: bool,
    pub order: HistoryOrder,
    pub agent_entries: AgentEntries,
}

impl Default for History {
    fn default() -> Self {
        Self {
            dialogs: true,
            commands: true,
            skip_leading_space: true,
            redact_secrets: true,
            order: HistoryOrder::Frecency,
            agent_entries: AgentEntries::Marked,
        }
    }
}

/// When a completion source is used (Far's three states).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Use {
    #[default]
    Always,
    /// Only on Ctrl+Space.
    CtrlSpace,
    Never,
}

impl Use {
    /// Whether the source counts: `manual` — the list was asked for.
    pub fn on(self, manual: bool) -> bool {
        match self {
            Use::Always => true,
            Use::CtrlSpace => manual,
            Use::Never => false,
        }
    }
}

/// Autocompletion in input fields and the command line (Far's
/// "AutoComplete settings", docs/15).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Autocomplete {
    /// In dialogs' fields with a history or a path.
    pub dialogs: bool,
    pub command_line: bool,
    /// The list of matches appears as you type.
    pub show_list: bool,
    /// The list takes the keys: moving in it does not change the field.
    pub modal: bool,
    pub history: Use,
    pub files: Use,
    pub variables: Use,
    pub programs: Use,
}

impl Default for Autocomplete {
    fn default() -> Self {
        Self {
            dialogs: true,
            command_line: true,
            show_list: true,
            modal: false,
            history: Use::Always,
            files: Use::Always,
            variables: Use::Always,
            programs: Use::Always,
        }
    }
}

/// The viewer (F3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Viewer {
    /// The command line stays under the viewer (Far hides it); the agent
    /// pane then keeps its place when a viewer opens.
    pub command_line: bool,
    /// F3 runs the external viewer, Alt+F3 the built-in one (Far's
    /// `UseExternalViewer`).
    pub external_f3: bool,
    /// The external viewer; `!.!` is replaced by the file name, otherwise
    /// the name goes at the end.
    pub external_command: String,
    /// Keys do not drop the selection (Far's `PersistentBlocks`).
    pub persistent_selection: bool,
    pub tab_size: usize,
    pub show_arrows: bool,
    /// The zero character shown as `·`.
    pub show_zero: bool,
    pub scrollbar: bool,
    pub save_position: bool,
    pub save_codepage: bool,
    pub save_bookmarks: bool,
    pub max_line: usize,
    pub save_mode: bool,
    pub save_wrap: bool,
    /// A zero byte near the start opens the dump.
    pub detect_dump: bool,
    pub autodetect_codepage: bool,
    /// 0: the system's ANSI code page.
    pub default_codepage: u32,
}

impl Default for Viewer {
    fn default() -> Self {
        Self {
            command_line: true,
            external_f3: false,
            external_command: String::new(),
            persistent_selection: true,
            tab_size: 8,
            show_arrows: true,
            show_zero: false,
            scrollbar: false,
            save_position: true,
            save_codepage: true,
            save_bookmarks: true,
            max_line: 10_000,
            save_mode: true,
            save_wrap: false,
            detect_dump: true,
            autodetect_codepage: true,
            default_codepage: 0,
        }
    }
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
    /// The agent menu asks before losing context or ending the agent
    /// (`/compact`, `/clear`, new / other session, restart).
    pub agent: bool,
}

impl Default for Confirm {
    fn default() -> Self {
        Self {
            delete_folder: true,
            read_only: true,
            esc: true,
            agent: true,
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
    /// Where the agent pane is: below the panels or above them.
    pub position: AgentPosition,
    /// afar is the agent's IDE (Claude Code's IDE protocol): it sees the
    /// viewer's selection, edits can be reviewed in afar.
    pub ide: bool,
    /// afar wakes the agent with events (Claude Code's Channels, research
    /// preview): `afar channel` is its channel server.
    pub channels: bool,
    pub permissions: Permissions,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentPosition {
    #[default]
    Bottom,
    Top,
}

impl Default for Agent {
    fn default() -> Self {
        Self {
            command: "claude".into(),
            args: Vec::new(),
            live: false,
            position: AgentPosition::Bottom,
            ide: false,
            channels: false,
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
