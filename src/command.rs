//! Commands: every action of the panels has a stable name (`panel.swap`,
//! `fileop.copy`) that the key map (`keymap.rs`) binds keys to; later the
//! F9 menu, macros and the agent use the same names (docs/02-architecture.md,
//! "Слой команд").

use crate::panel::{SortMode, ViewMode};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    // The application and the layout.
    Quit,
    TogglePanels,
    AgentTaller,
    AgentShorter,
    SplitterLeft,
    SplitterRight,
    DevRestart,
    // Panels.
    NextPanel,
    SwapPanels,
    HidePassive,
    ToggleLeft,
    ToggleRight,
    DriveMenuLeft,
    DriveMenuRight,
    CursorUp,
    CursorDown,
    PageUp,
    PageDown,
    Home,
    End,
    Left,
    Right,
    Parent,
    Root,
    Enter,
    Refresh,
    View(ViewMode),
    Sort(SortMode),
    SortMenu,
    SelectedFirst,
    // Selection.
    SelectToggle,
    SelectUp,
    SelectDown,
    SelectDialog,
    UnselectDialog,
    SelectAll,
    UnselectAll,
    SelectSameExt,
    UnselectSameExt,
    SelectSameName,
    UnselectSameName,
    InvertSelection,
    InvertAll,
    InvertFiles,
    RestoreSelection,
    // File operations.
    Copy,
    CopyCurrent,
    Move,
    Rename,
    MkDir,
    Delete,
    DeleteCurrent,
    /// Del: like F8 when the command line is empty.
    Del,
    DeletePermanent,
    Wipe,
    // The command line.
    InsertName,
    InsertFullName,
    InsertPassiveFullName,
    InsertPassiveName,
    InsertLeftPath,
    InsertRightPath,
    InsertActivePath,
    InsertPassivePath,
    HistoryPrev,
    HistoryNext,
}

/// A command's name, its default (Far) keys, and whether it works with the
/// panels hidden (Far's list in filelist.cpp ProcessKey).
pub struct Def {
    pub name: &'static str,
    pub command: Command,
    pub keys: &'static [&'static str],
    pub with_panels_hidden: bool,
}

const fn def(
    name: &'static str,
    command: Command,
    keys: &'static [&'static str],
    with_panels_hidden: bool,
) -> Def {
    Def {
        name,
        command,
        keys,
        with_panels_hidden,
    }
}

use Command::*;

/// All commands with Far's keys.
#[rustfmt::skip]
pub const COMMANDS: &[Def] = &[
    def("app.quit", Quit, &["F10"], true),
    def("layout.toggle_panels", TogglePanels, &["Ctrl+O"], true),
    def("layout.agent_taller", AgentTaller, &["Ctrl+Up"], true),
    def("layout.agent_shorter", AgentShorter, &["Ctrl+Down"], true),
    def("layout.splitter_left", SplitterLeft, &["Ctrl+Left"], true),
    def("layout.splitter_right", SplitterRight, &["Ctrl+Right"], true),
    def("app.dev_restart", DevRestart, &["Ctrl+Shift+R"], true),
    def("panel.next", NextPanel, &["Tab"], false),
    def("panel.swap", SwapPanels, &["Ctrl+U"], false),
    def("panel.hide_passive", HidePassive, &["Ctrl+P"], false),
    def("panel.toggle_left", ToggleLeft, &["Ctrl+F1"], true),
    def("panel.toggle_right", ToggleRight, &["Ctrl+F2"], true),
    def("panel.drive_menu_left", DriveMenuLeft, &["Alt+F1"], true),
    def("panel.drive_menu_right", DriveMenuRight, &["Alt+F2"], true),
    def("panel.cursor_up", CursorUp, &["Up"], false),
    def("panel.cursor_down", CursorDown, &["Down"], false),
    def("panel.page_up", PageUp, &["PgUp"], false),
    def("panel.page_down", PageDown, &["PgDn"], false),
    def("panel.home", Home, &["Home"], false),
    def("panel.end", End, &["End"], false),
    def("panel.left", Left, &["Left"], false),
    def("panel.right", Right, &["Right"], false),
    def("panel.parent", Parent, &["Ctrl+PgUp"], false),
    def("panel.root", Root, &["Ctrl+\\"], false),
    def("panel.enter", Enter, &["Enter"], true),
    def("panel.refresh", Refresh, &["Ctrl+R"], false),
    def("view.brief", View(ViewMode::Brief), &["Ctrl+1"], false),
    def("view.medium", View(ViewMode::Medium), &["Ctrl+2"], false),
    def("view.full", View(ViewMode::Full), &["Ctrl+3"], false),
    def("view.wide", View(ViewMode::Wide), &["Ctrl+4"], false),
    def("sort.by_name", Sort(SortMode::Name), &["Ctrl+F3"], false),
    def("sort.by_ext", Sort(SortMode::Ext), &["Ctrl+F4"], false),
    def("sort.by_modified", Sort(SortMode::Modified), &["Ctrl+F5"], false),
    def("sort.by_size", Sort(SortMode::Size), &["Ctrl+F6"], false),
    def("sort.unsorted", Sort(SortMode::Unsorted), &["Ctrl+F7"], false),
    def("sort.by_created", Sort(SortMode::Created), &["Ctrl+F8"], false),
    def("sort.by_accessed", Sort(SortMode::Accessed), &["Ctrl+F9"], false),
    def("sort.by_name_only", Sort(SortMode::NameOnly), &[], false),
    def("sort.menu", SortMenu, &["Ctrl+F12"], false),
    def("sort.selected_first", SelectedFirst, &["Shift+F12"], false),
    def("select.toggle", SelectToggle, &["Ins"], false),
    def("select.up", SelectUp, &["Shift+Up"], false),
    def("select.down", SelectDown, &["Shift+Down"], false),
    def("select.dialog", SelectDialog, &["Gray+"], false),
    def("select.unselect_dialog", UnselectDialog, &["Gray-"], false),
    def("select.all", SelectAll, &["Shift+Gray+"], false),
    def("select.none", UnselectAll, &["Shift+Gray-"], false),
    def("select.same_ext", SelectSameExt, &["Ctrl+Gray+"], false),
    def("select.unselect_same_ext", UnselectSameExt, &["Ctrl+Gray-"], false),
    def("select.same_name", SelectSameName, &["Alt+Gray+"], false),
    def("select.unselect_same_name", UnselectSameName, &["Alt+Gray-"], false),
    def("select.invert", InvertSelection, &["Gray*"], false),
    def("select.invert_all", InvertAll, &["Ctrl+Gray*"], false),
    def("select.invert_files", InvertFiles, &["Alt+Gray*"], false),
    def("select.restore", RestoreSelection, &["Ctrl+M"], false),
    def("fileop.copy", Copy, &["F5"], false),
    def("fileop.copy_current", CopyCurrent, &["Shift+F5"], false),
    def("fileop.move", Move, &["F6"], false),
    def("fileop.rename", Rename, &["Shift+F6"], false),
    def("fileop.mkdir", MkDir, &["F7"], true),
    def("fileop.delete", Delete, &["F8"], false),
    def("fileop.delete_current", DeleteCurrent, &["Shift+F8"], false),
    def("fileop.del", Del, &["Del"], false),
    def("fileop.delete_permanent", DeletePermanent, &["Shift+Del"], false),
    def("fileop.wipe", Wipe, &["Alt+Del"], false),
    def("cmdline.insert_name", InsertName, &["Ctrl+Enter"], true),
    def("cmdline.insert_full_name", InsertFullName, &["Ctrl+F", "Ctrl+Alt+F"], true),
    def("cmdline.insert_passive_full_name", InsertPassiveFullName, &["Ctrl+;"], true),
    def("cmdline.insert_passive_name", InsertPassiveName, &["Ctrl+Shift+Enter"], true),
    def("cmdline.insert_left_path", InsertLeftPath, &["Ctrl+["], true),
    def("cmdline.insert_right_path", InsertRightPath, &["Ctrl+]"], true),
    def("cmdline.insert_active_path", InsertActivePath, &["Ctrl+Shift+["], true),
    def("cmdline.insert_passive_path", InsertPassivePath, &["Ctrl+Shift+]"], true),
    def("cmdline.history_prev", HistoryPrev, &["Ctrl+E"], true),
    def("cmdline.history_next", HistoryNext, &["Ctrl+X"], true),
];

impl Command {
    pub fn def(self) -> &'static Def {
        COMMANDS
            .iter()
            .find(|d| d.command == self)
            .expect("every command is in COMMANDS")
    }

    pub fn name(self) -> &'static str {
        self.def().name
    }

    pub fn from_name(name: &str) -> Option<Self> {
        COMMANDS.iter().find(|d| d.name == name).map(|d| d.command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_round_trip() {
        let mut names: Vec<&str> = COMMANDS.iter().map(|d| d.name).collect();
        names.sort();
        let n = names.len();
        names.dedup();
        assert_eq!(names.len(), n, "duplicate command names");
        for d in COMMANDS {
            assert_eq!(Command::from_name(d.name), Some(d.command));
            assert_eq!(d.command.name(), d.name);
        }
    }
}
