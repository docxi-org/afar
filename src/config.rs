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
    pub editor: Editor,
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

/// What appears as you type (docs/15, improvement 8).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Suggest {
    /// The rest of the best match in grey after the text; the list only
    /// when asked for (Ctrl+Space in dialogs).
    #[default]
    Ghost,
    /// Far's list of matches.
    List,
    /// Nothing; the list when asked for.
    Off,
}

/// Autocompletion in input fields and the command line (Far's
/// "AutoComplete settings", docs/15).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Autocomplete {
    /// In dialogs' fields with a history or a path.
    pub dialogs: bool,
    pub command_line: bool,
    /// What appears as you type.
    pub suggest: Suggest,
    /// The list takes the keys: moving in it does not change the field.
    pub modal: bool,
    /// Matches by characters in order anywhere, after the ones from the
    /// start (highlighted in the list).
    pub fuzzy: bool,
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
            suggest: Suggest::Ghost,
            modal: false,
            fuzzy: true,
            history: Use::Always,
            files: Use::Always,
            variables: Use::Always,
            programs: Use::Always,
        }
    }
}

/// Far's `ExpandTabs`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpandTabs {
    /// Tabs stay tabs.
    #[default]
    Keep,
    /// A typed Tab goes in as spaces.
    New,
    /// Every tab becomes spaces, those of the file too.
    All,
}

/// Far's `ShowWhiteSpace`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShowWhitespace {
    #[default]
    Off,
    /// Spaces, tabs and line endings.
    All,
    /// Spaces and tabs.
    NoEol,
}

/// The editor (F4; Far's editor settings, docs/17 §9).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Editor {
    /// F4 runs the external editor (Far's `UseExternalEditor`).
    pub external_f4: bool,
    /// The external editor; `!.!` is replaced by the file name, otherwise
    /// the name goes at the end.
    pub external_command: String,
    pub expand_tabs: ExpandTabs,
    pub tab_size: usize,
    /// Blocks stay when the cursor moves and typing does not replace them
    /// (Far's `PersistentBlocks`).
    pub persistent_blocks: bool,
    /// Del and BS remove a block (Far's `DelRemovesBlocks`).
    pub del_removes_blocks: bool,
    pub auto_indent: bool,
    pub show_whitespace: ShowWhitespace,
    pub cursor_beyond_eol: bool,
    /// A match is selected (Far's `SearchSelFound`).
    pub search_select_found: bool,
    /// The cursor after a match, not on it (Far's `SearchCursorAtEnd`).
    pub search_cursor_at_end: bool,
    pub scrollbar: bool,
    pub line_numbers: bool,
    /// Syntax highlighting (Alt+F3 in a window).
    pub syntax: bool,
    pub save_position: bool,
    pub save_bookmarks: bool,
    pub autodetect_codepage: bool,
    /// 0: the system's ANSI code page.
    pub default_codepage: u32,
    /// The agent in the editor (docs/11 «Редактор и агент»).
    pub agent: EditorAgent,
}

/// When a marker line goes to the agent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkerTrigger {
    /// Enter at the end of the line.
    #[default]
    Enter,
    /// Saving the file (every typed marker line).
    Save,
    Both,
}

/// How the agent's changes of the editor's text go in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Apply {
    /// At once (its lines marked, one undo step).
    Direct,
    /// Every change a proposal the user accepts or rejects.
    Propose,
    /// New text at once; a change of the user's text a proposal.
    #[default]
    Mixed,
}

/// `[editor.agent]`: markers in the text, how the agent's changes go in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorAgent {
    /// The agent decides; `<marker>?` — answer only, `<marker>!` — edit.
    pub markers: Vec<String>,
    /// Answer only (a note in the margin, the text stays).
    pub answer_markers: Vec<String>,
    /// Markers count in code comments too (by the file's extension).
    pub marker_in_comments: bool,
    pub marker_trigger: MarkerTrigger,
    /// The marker line leaves the text when it goes to the agent.
    pub marker_remove: bool,
    pub apply: Apply,
}

impl Default for EditorAgent {
    fn default() -> Self {
        Self {
            markers: vec!["!!".into(), "@ai".into()],
            answer_markers: vec!["??".into()],
            marker_in_comments: true,
            marker_trigger: MarkerTrigger::Enter,
            marker_remove: true,
            apply: Apply::Mixed,
        }
    }
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            external_f4: false,
            external_command: String::new(),
            expand_tabs: ExpandTabs::Keep,
            tab_size: 8,
            persistent_blocks: false,
            del_removes_blocks: true,
            auto_indent: false,
            show_whitespace: ShowWhitespace::Off,
            cursor_beyond_eol: true,
            search_select_found: false,
            search_cursor_at_end: false,
            scrollbar: false,
            line_numbers: false,
            syntax: true,
            save_position: true,
            save_bookmarks: true,
            autodetect_codepage: true,
            default_codepage: 0,
            agent: EditorAgent::default(),
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
    /// Syntax highlighting (Alt+F3 in a window).
    pub syntax: bool,
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
            syntax: true,
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
    /// Lines a mouse wheel notch moves in panels and lists; 0: as set in
    /// Windows (Far's System.MsWheelDelta).
    pub wheel_lines: u32,
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
    /// Opening a file already open in the editor asks how (Far's
    /// `AllowReedit`); a modified one always asks.
    pub reedit: bool,
}

impl Default for Confirm {
    fn default() -> Self {
        Self {
            delete_folder: true,
            read_only: true,
            esc: true,
            agent: true,
            reedit: true,
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
    /// afar answers Claude Code's question about development channels at
    /// the agent's start itself (only when afar's channel is the only one).
    pub confirm_channels: bool,
    /// The agent's test tools (`afar_test_input`, `afar_test_screen`): it
    /// presses keys and takes screenshots of afar — past its permissions.
    pub test_tools: bool,
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
            confirm_channels: false,
            test_tools: false,
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
    /// Changing the text of a file open in afar's editor.
    pub edit_buffer: Level,
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
            edit_buffer: Level::Allow,
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
