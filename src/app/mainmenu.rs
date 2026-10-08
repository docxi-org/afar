//! F9: Far's main menu (config.cpp ShowMenuBar) — Left, Files, Commands,
//! Options, Right with Far's items and keys. What afar does is linked to
//! its command; the rest is shown disabled until it exists.

use crossterm::event::{KeyEvent, MouseEvent};

use super::App;
use super::fileops::Overlay;
use crate::command::Command;
use crate::keymap::Chord;
use crate::menu::Item;
use crate::menubar::{MenuBar, Outcome, Title};
use crate::panel::ViewMode;
use crate::tr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MainAction {
    Run(Command),
    /// A command for one panel (Left / Right menus), whichever is active.
    OnSide(usize, Command),
    Confirmations,
    AgentSettings,
    ViewerSettings,
    EditorSettings,
    AutocompleteSettings,
    HintSettings,
    FarImport,
}

/// The F9 menu's text for a command, if it has an item (hints say what a
/// key does with it).
pub(super) fn menu_label(command: Command) -> Option<&'static str> {
    let menus = [panel_menu(0), files_menu(), commands_menu()];
    menus
        .iter()
        .flatten()
        .find_map(|(label, _, action)| match action {
            Some(MainAction::Run(c) | MainAction::OnSide(_, c)) if *c == command => Some(*label),
            _ => None,
        })
}

/// An item: Far's text id, the key shown, what it does (`None`: disabled).
type Entry = (&'static str, Option<&'static str>, Option<MainAction>);

const SEP: Entry = ("", None, None);

fn panel_menu(side: usize) -> Vec<Entry> {
    use Command::*;
    use MainAction::{OnSide, Run};
    let right = side == 1;
    vec![
        (
            "MMenuBriefView",
            Some("Ctrl+1"),
            Some(OnSide(side, View(ViewMode::Brief))),
        ),
        (
            "MMenuMediumView",
            Some("Ctrl+2"),
            Some(OnSide(side, View(ViewMode::Medium))),
        ),
        (
            "MMenuFullView",
            Some("Ctrl+3"),
            Some(OnSide(side, View(ViewMode::Full))),
        ),
        (
            "MMenuWideView",
            Some("Ctrl+4"),
            Some(OnSide(side, View(ViewMode::Wide))),
        ),
        (
            "MMenuDetailedView",
            Some("Ctrl+5"),
            Some(OnSide(side, View(ViewMode::Detailed))),
        ),
        (
            "MMenuDizView",
            Some("Ctrl+6"),
            Some(OnSide(side, View(ViewMode::Descriptions))),
        ),
        (
            "MMenuLongDizView",
            Some("Ctrl+7"),
            Some(OnSide(side, View(ViewMode::LongDescriptions))),
        ),
        (
            "MMenuOwnersView",
            Some("Ctrl+8"),
            Some(OnSide(side, View(ViewMode::Owners))),
        ),
        (
            "MMenuLinksView",
            Some("Ctrl+9"),
            Some(OnSide(side, View(ViewMode::Links))),
        ),
        (
            "MMenuAlternativeView",
            Some("Ctrl+0"),
            Some(OnSide(side, View(ViewMode::AltFull))),
        ),
        SEP,
        // Shown on this side: the command shows it on the other one.
        (
            "MMenuInfoPanel",
            Some("Ctrl+L"),
            Some(OnSide(1 - side, InfoPanel)),
        ),
        ("MMenuTreePanel", Some("Ctrl+T"), None),
        (
            "MMenuQuickView",
            Some("Ctrl+Q"),
            Some(OnSide(1 - side, QuickView)),
        ),
        SEP,
        (
            "MMenuSortModes",
            Some("Ctrl+F12"),
            Some(OnSide(side, SortMenu)),
        ),
        ("MMenuLongNames", Some("Ctrl+N"), None),
        if right {
            (
                "MMenuTogglePanelRight",
                Some("Ctrl+F2"),
                Some(Run(ToggleRight)),
            )
        } else {
            ("MMenuTogglePanel", Some("Ctrl+F1"), Some(Run(ToggleLeft)))
        },
        ("MMenuReread", Some("Ctrl+R"), Some(OnSide(side, Refresh))),
        if right {
            (
                "MMenuChangeDriveRight",
                Some("Alt+F2"),
                Some(Run(DriveMenuRight)),
            )
        } else {
            ("MMenuChangeDrive", Some("Alt+F1"), Some(Run(DriveMenuLeft)))
        },
    ]
}

fn files_menu() -> Vec<Entry> {
    use Command::*;
    use MainAction::Run;
    vec![
        ("MMenuView", Some("F3"), Some(Run(ViewFile))),
        ("MMenuEdit", Some("F4"), Some(Run(EditFile))),
        ("MMenuCopy", Some("F5"), Some(Run(Copy))),
        ("MMenuMove", Some("F6"), Some(Run(Move))),
        ("MMenuLink", Some("Alt+F6"), None),
        ("MMenuCreateFolder", Some("F7"), Some(Run(MkDir))),
        ("MMenuDelete", Some("F8"), Some(Run(Delete))),
        ("MMenuWipe", Some("Alt+Del"), Some(Run(Wipe))),
        SEP,
        ("MMenuAdd", Some("Shift+F1"), None),
        ("MMenuExtract", Some("Shift+F2"), None),
        ("MMenuArchiveCommands", Some("Shift+F3"), None),
        SEP,
        ("MMenuAttributes", Some("Ctrl+A"), Some(Run(Attributes))),
        ("MMenuApplyCommand", Some("Ctrl+G"), None),
        ("MMenuDescribe", Some("Ctrl+Z"), None),
        SEP,
        ("MMenuSelectGroup", Some("Gray+"), Some(Run(SelectDialog))),
        (
            "MMenuUnselectGroup",
            Some("Gray-"),
            Some(Run(UnselectDialog)),
        ),
        (
            "MMenuInvertSelection",
            Some("Gray*"),
            Some(Run(InvertSelection)),
        ),
        (
            "MMenuRestoreSelection",
            Some("Ctrl+M"),
            Some(Run(RestoreSelection)),
        ),
    ]
}

fn commands_menu() -> Vec<Entry> {
    use Command::*;
    use MainAction::Run;
    vec![
        ("MMenuFindFile", Some("Alt+F7"), Some(Run(FindFile))),
        ("MMenuHistory", Some("Alt+F8"), Some(Run(CommandHistory))),
        ("MMenuVideoMode", Some("Alt+F9"), None),
        ("MMenuFindFolder", Some("Alt+F10"), Some(Run(FindFolder))),
        ("MMenuViewHistory", Some("Alt+F11"), Some(Run(ViewHistory))),
        (
            "MMenuFoldersHistory",
            Some("Alt+F12"),
            Some(Run(FolderHistory)),
        ),
        // afar's own.
        ("menu-import-far-history", None, Some(MainAction::FarImport)),
        SEP,
        ("MMenuSwapPanels", Some("Ctrl+U"), Some(Run(SwapPanels))),
        ("MMenuTogglePanels", Some("Ctrl+O"), Some(Run(TogglePanels))),
        ("MMenuCompareFolders", None, None),
        SEP,
        ("MMenuUserMenu", None, None),
        ("MMenuFileAssociations", None, None),
        ("MMenuFolderShortcuts", None, None),
        ("MMenuFilter", Some("Ctrl+I"), None),
        SEP,
        ("MMenuPluginCommands", Some("F11"), None),
        ("MMenuWindowsList", Some("F12"), Some(Run(Screens))),
        ("MMenuProcessList", Some("Ctrl+W"), None),
        ("MMenuHotPlugList", None, None),
    ]
}

fn options_menu() -> Vec<Entry> {
    use MainAction::{
        AgentSettings, AutocompleteSettings, Confirmations, EditorSettings, HintSettings,
        ViewerSettings,
    };
    vec![
        ("MMenuSystemSettings", None, None),
        ("MMenuPanelSettings", None, None),
        ("MMenuTreeSettings", None, None),
        ("MMenuInterface", None, None),
        ("MMenuLanguages", None, None),
        ("MMenuPluginsConfig", None, None),
        ("MMenuPluginsManagerSettings", None, None),
        ("MMenuDialogSettings", None, None),
        ("MMenuVMenuSettings", None, None),
        ("MMenuCmdlineSettings", None, None),
        (
            "MMenuAutoCompleteSettings",
            None,
            Some(AutocompleteSettings),
        ),
        ("MMenuInfoPanelSettings", None, None),
        ("MMenuMaskGroups", None, None),
        SEP,
        ("MMenuConfirmation", None, Some(Confirmations)),
        ("MMenuFilePanelModes", None, None),
        ("MMenuFileDescriptions", None, None),
        ("MMenuFolderInfoFiles", None, None),
        SEP,
        ("MMenuViewer", None, Some(ViewerSettings)),
        ("MMenuEditor", None, Some(EditorSettings)),
        ("MMenuCodePages", None, None),
        SEP,
        ("MMenuColors", None, None),
        ("MMenuFilesHighlighting", None, None),
        // afar's own: the agent.
        SEP,
        ("menu-agent-settings", None, Some(AgentSettings)),
        ("menu-hint-settings", None, Some(HintSettings)),
        SEP,
        ("MMenuSaveSetup", Some("Shift+F9"), None),
    ]
}

fn title(text: &str, entries: Vec<Entry>, check: Option<usize>) -> Title<MainAction> {
    let mut items = Vec::new();
    let mut actions = Vec::new();
    for (i, (id, key, action)) in entries.into_iter().enumerate() {
        if id.is_empty() {
            items.push(Item::separator());
            actions.push(None);
            continue;
        }
        let mut item = Item::new(tr!(id))
            .disabled(action.is_none())
            .checked((check == Some(i)).then_some('√'));
        if let Some(chord) = key.and_then(Chord::parse) {
            item = item.accel_chord(chord);
        }
        items.push(item);
        actions.push(action);
    }
    let selected = check.unwrap_or(0);
    Title {
        text: text.to_string(),
        items,
        actions,
        selected,
    }
}

impl App {
    /// F9: the bar opens on Left, or on Right when the right panel is
    /// active (as in Far).
    pub(super) fn main_menu(&mut self) {
        let view_item = |mode: ViewMode| match mode {
            ViewMode::Brief => 0,
            ViewMode::Medium => 1,
            ViewMode::Full => 2,
            ViewMode::Wide => 3,
            ViewMode::Detailed => 4,
            ViewMode::Descriptions => 5,
            ViewMode::LongDescriptions => 6,
            ViewMode::Owners => 7,
            ViewMode::Links => 8,
            ViewMode::AltFull => 9,
        };
        let titles = vec![
            title(
                &tr!("MMenuLeftTitle"),
                panel_menu(0),
                Some(view_item(self.panels[0].view)),
            ),
            title(&tr!("MMenuFilesTitle"), files_menu(), None),
            title(&tr!("MMenuCommandsTitle"), commands_menu(), None),
            title(&tr!("MMenuOptionsTitle"), options_menu(), None),
            title(
                &tr!("MMenuRightTitle"),
                panel_menu(1),
                Some(view_item(self.panels[1].view)),
            ),
        ];
        let start = if self.active == 1 && !self.panel_hidden(1) {
            4
        } else {
            0
        };
        self.overlays
            .push(Overlay::MenuBar(MenuBar::new(titles, start)));
    }

    /// A click on the top row of the screen (Far's FilePanels::ProcessMouse):
    /// in the left corner it is Ctrl+O, elsewhere it opens the menu bar —
    /// with the title under the mouse open.
    pub(super) fn top_row_click(&mut self, ev: &MouseEvent) {
        if ev.column == 0 {
            self.run_command(Command::TogglePanels);
            return;
        }
        self.main_menu();
        let pos = ratatui::layout::Position::new(ev.column, ev.row);
        if let Some(Overlay::MenuBar(bar)) = self.overlays.last_mut() {
            bar.set_row(ev.row);
        }
        if let Some(Overlay::MenuBar(bar)) = self.overlays.last_mut()
            && bar.has_title_at(pos)
        {
            let mut press = *ev;
            press.kind =
                crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left);
            bar.handle_mouse(&press);
        }
    }

    pub(super) fn menubar_key(&mut self, key: KeyEvent) {
        if let Some(Overlay::MenuBar(bar)) = self.overlays.last_mut() {
            let outcome = bar.handle_key(&key);
            self.menubar_outcome(outcome);
        }
    }

    pub(super) fn menubar_mouse(&mut self, ev: &MouseEvent) {
        if let Some(Overlay::MenuBar(bar)) = self.overlays.last_mut() {
            let outcome = bar.handle_mouse(ev);
            self.menubar_outcome(outcome);
        }
    }

    fn menubar_outcome(&mut self, outcome: Outcome<MainAction>) {
        let action = match outcome {
            Outcome::Pending => return,
            Outcome::Closed => None,
            Outcome::Chosen(a) => Some(a),
        };
        self.overlays.pop();
        match action {
            Some(MainAction::Run(command)) => {
                self.run_command(command);
            }
            Some(MainAction::OnSide(side, command)) => {
                // Like Far: the panel of the menu, without moving the focus.
                let active = self.active;
                self.active = side;
                self.run_command(command);
                self.active = active;
            }
            Some(MainAction::Confirmations) => self.confirmations_dialog(),
            Some(MainAction::AgentSettings) => self.agent_settings_dialog(),
            Some(MainAction::ViewerSettings) => self.viewer_settings_dialog(),
            Some(MainAction::EditorSettings) => self.editor_settings_dialog(None),
            Some(MainAction::AutocompleteSettings) => self.autocomplete_settings_dialog(),
            Some(MainAction::HintSettings) => self.hint_settings_dialog(),
            Some(MainAction::FarImport) => self.far_import_menu(),
            None => {}
        }
    }
}
