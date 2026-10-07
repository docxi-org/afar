//! Commands: every action of the panels has a stable name (`panel.swap`,
//! `fileop.copy`) that the key map (`keymap.rs`) binds keys to; later the
//! F9 menu, macros and the agent use the same names (docs/02-architecture.md,
//! "Слой команд"). Keys are bound per context: the panels (with the
//! command line under them) and the viewer.

use crate::panel::{SortMode, ViewMode};

/// Where a key binding works.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Ctx {
    Panels,
    Viewer,
}

impl Ctx {
    /// The section of `keymaps/far.toml`.
    pub fn section(self) -> &'static str {
        match self {
            Ctx::Panels => "panels",
            Ctx::Viewer => "viewer",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    // The application and the layout.
    Quit,
    MainMenu,
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
    /// Ctrl+Q: the quick view on the passive panel.
    QuickView,
    /// Ctrl+A: the attributes and times of files.
    Attributes,
    /// Alt+F7: find files.
    FindFile,
    // History menus.
    CommandHistory,
    ViewHistory,
    FolderHistory,
    // Screens (F12): panels, viewers.
    Screens,
    NextScreen,
    PrevScreen,
    ViewFile,
    /// Alt+F3: the other of the built-in and the external viewer.
    ViewFileAlt,
    /// Ctrl+Shift+F3: always the built-in viewer.
    ViewInternal,
    Viewer(ViewerCmd),
}

/// Commands of the viewer (Far's viewer.cpp / fileview.cpp keys).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewerCmd {
    Close,
    Wrap,
    WordWrap,
    Hex,
    ModeMenu,
    Edit,
    Search,
    SearchNext,
    SearchPrev,
    NextCodepage,
    CodepageMenu,
    Goto,
    GoFile,
    NextFile,
    PrevFile,
    Copy,
    Unselect,
    Undo,
    GotoBookmark(u8),
    SetBookmark(u8),
    Up,
    Down,
    PageUp,
    PageDown,
    Left,
    Right,
    /// Ctrl+Left/Right: 20 columns in text, one byte in hex and dump.
    LeftMore,
    RightMore,
    LeftStart,
    RightEnd,
    Home,
    End,
    StartKeepLeft,
    EndKeepLeft,
    BytesLess,
    BytesMore,
    BytesLess16,
    BytesMore16,
    Scrollbar,
    StatusLine,
    KeyBar,
    UserScreen,
    /// Ctrl+Enter: a reference to the selected (or shown) lines into the
    /// agent's input (IDE protocol `at_mentioned`).
    AskAgent,
    /// Alt+Shift+F9: the viewer's settings.
    Settings,
}

/// A command's name, its default (Far) keys, and whether it works with the
/// panels hidden (Far's list in filelist.cpp ProcessKey).
pub struct Def {
    pub name: &'static str,
    pub command: Command,
    pub keys: &'static [&'static str],
    pub with_panels_hidden: bool,
    pub ctx: &'static [Ctx],
}

/// A command of the panels.
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
        ctx: &[Ctx::Panels],
    }
}

/// A command of every screen.
const fn global(name: &'static str, command: Command, keys: &'static [&'static str]) -> Def {
    Def {
        name,
        command,
        keys,
        with_panels_hidden: true,
        ctx: &[Ctx::Panels, Ctx::Viewer],
    }
}

/// A command of the viewer.
const fn vdef(name: &'static str, command: ViewerCmd, keys: &'static [&'static str]) -> Def {
    Def {
        name,
        command: Command::Viewer(command),
        keys,
        with_panels_hidden: true,
        ctx: &[Ctx::Viewer],
    }
}

use Command::*;
use ViewerCmd as V;

/// All commands with Far's keys.
#[rustfmt::skip]
pub const COMMANDS: &[Def] = &[
    def("app.quit", Quit, &["F10"], true),
    def("app.menu", MainMenu, &["F9"], true),
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
    def("panel.quick_view", QuickView, &["Ctrl+Q"], false),
    def("file.attributes", Attributes, &["Ctrl+A"], false),
    def("file.find", FindFile, &["Alt+F7"], true),
    def("history.commands", CommandHistory, &["Alt+F8"], true),
    def("history.views", ViewHistory, &["Alt+F11"], true),
    def("history.folders", FolderHistory, &["Alt+F12"], true),
    def("cmdline.history_next", HistoryNext, &["Ctrl+X"], true),
    def("fileop.view", ViewFile, &["F3"], false),
    def("fileop.view_alt", ViewFileAlt, &["Alt+F3"], false),
    def("fileop.view_internal", ViewInternal, &["Ctrl+Shift+F3"], false),
    global("screens.list", Screens, &["F12"]),
    global("screens.next", NextScreen, &["Ctrl+Tab"]),
    global("screens.prev", PrevScreen, &["Ctrl+Shift+Tab"]),
    vdef("viewer.close", V::Close, &["F3", "F10", "Esc"]),
    vdef("viewer.wrap", V::Wrap, &["F2"]),
    vdef("viewer.word_wrap", V::WordWrap, &["Shift+F2"]),
    vdef("viewer.hex", V::Hex, &["F4"]),
    vdef("viewer.mode_menu", V::ModeMenu, &["Shift+F4"]),
    vdef("viewer.edit", V::Edit, &["F6"]),
    vdef("viewer.search", V::Search, &["F7"]),
    vdef("viewer.search_next", V::SearchNext, &["Shift+F7", "Space"]),
    vdef("viewer.search_prev", V::SearchPrev, &["Alt+F7"]),
    vdef("viewer.next_codepage", V::NextCodepage, &["F8"]),
    vdef("viewer.codepage_menu", V::CodepageMenu, &["Shift+F8"]),
    vdef("viewer.goto", V::Goto, &["Alt+F8"]),
    vdef("viewer.go_file", V::GoFile, &["Ctrl+F10"]),
    vdef("viewer.next_file", V::NextFile, &["Gray+"]),
    vdef("viewer.prev_file", V::PrevFile, &["Gray-"]),
    vdef("viewer.copy", V::Copy, &["Ctrl+C", "Ctrl+Ins"]),
    vdef("viewer.unselect", V::Unselect, &["Ctrl+U"]),
    vdef("viewer.undo", V::Undo, &["Alt+BS", "Ctrl+Z"]),
    vdef("viewer.bookmark_0", V::GotoBookmark(0), &["Ctrl+0"]),
    vdef("viewer.bookmark_1", V::GotoBookmark(1), &["Ctrl+1"]),
    vdef("viewer.bookmark_2", V::GotoBookmark(2), &["Ctrl+2"]),
    vdef("viewer.bookmark_3", V::GotoBookmark(3), &["Ctrl+3"]),
    vdef("viewer.bookmark_4", V::GotoBookmark(4), &["Ctrl+4"]),
    vdef("viewer.bookmark_5", V::GotoBookmark(5), &["Ctrl+5"]),
    vdef("viewer.bookmark_6", V::GotoBookmark(6), &["Ctrl+6"]),
    vdef("viewer.bookmark_7", V::GotoBookmark(7), &["Ctrl+7"]),
    vdef("viewer.bookmark_8", V::GotoBookmark(8), &["Ctrl+8"]),
    vdef("viewer.bookmark_9", V::GotoBookmark(9), &["Ctrl+9"]),
    vdef("viewer.set_bookmark_0", V::SetBookmark(0), &["Ctrl+Shift+0"]),
    vdef("viewer.set_bookmark_1", V::SetBookmark(1), &["Ctrl+Shift+1"]),
    vdef("viewer.set_bookmark_2", V::SetBookmark(2), &["Ctrl+Shift+2"]),
    vdef("viewer.set_bookmark_3", V::SetBookmark(3), &["Ctrl+Shift+3"]),
    vdef("viewer.set_bookmark_4", V::SetBookmark(4), &["Ctrl+Shift+4"]),
    vdef("viewer.set_bookmark_5", V::SetBookmark(5), &["Ctrl+Shift+5"]),
    vdef("viewer.set_bookmark_6", V::SetBookmark(6), &["Ctrl+Shift+6"]),
    vdef("viewer.set_bookmark_7", V::SetBookmark(7), &["Ctrl+Shift+7"]),
    vdef("viewer.set_bookmark_8", V::SetBookmark(8), &["Ctrl+Shift+8"]),
    vdef("viewer.set_bookmark_9", V::SetBookmark(9), &["Ctrl+Shift+9"]),
    vdef("viewer.up", V::Up, &["Up"]),
    vdef("viewer.down", V::Down, &["Down"]),
    vdef("viewer.page_up", V::PageUp, &["PgUp", "Ctrl+Up"]),
    vdef("viewer.page_down", V::PageDown, &["PgDn", "Ctrl+Down"]),
    vdef("viewer.left", V::Left, &["Left"]),
    vdef("viewer.right", V::Right, &["Right"]),
    vdef("viewer.left_more", V::LeftMore, &["Ctrl+Left"]),
    vdef("viewer.right_more", V::RightMore, &["Ctrl+Right"]),
    vdef("viewer.left_start", V::LeftStart, &["Ctrl+Shift+Left"]),
    vdef("viewer.right_end", V::RightEnd, &["Ctrl+Shift+Right"]),
    vdef("viewer.home", V::Home, &["Home", "Ctrl+Home"]),
    vdef("viewer.end", V::End, &["End", "Ctrl+End"]),
    vdef("viewer.start_keep_left", V::StartKeepLeft, &["Ctrl+PgUp"]),
    vdef("viewer.end_keep_left", V::EndKeepLeft, &["Ctrl+PgDn"]),
    vdef("viewer.bytes_less", V::BytesLess, &["Alt+Left"]),
    vdef("viewer.bytes_more", V::BytesMore, &["Alt+Right"]),
    vdef("viewer.bytes_less_16", V::BytesLess16, &["Ctrl+Alt+Left"]),
    vdef("viewer.bytes_more_16", V::BytesMore16, &["Ctrl+Alt+Right"]),
    vdef("viewer.scrollbar", V::Scrollbar, &["Ctrl+S"]),
    vdef("viewer.status_line", V::StatusLine, &["Ctrl+Shift+B"]),
    vdef("viewer.keybar", V::KeyBar, &["Ctrl+B"]),
    vdef("viewer.user_screen", V::UserScreen, &["Ctrl+O"]),
    vdef("viewer.ask_agent", V::AskAgent, &["Ctrl+Enter"]),
    vdef("viewer.settings", V::Settings, &["Alt+Shift+F9"]),
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
