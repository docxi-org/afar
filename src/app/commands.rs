//! Running commands (`command.rs`): what each named command does. Keys
//! reach here through the key map; a command that does not apply returns
//! `false` and the key goes to the command line (as in Far, where the
//! panel sees a key first and passes on what it does not take).

use std::path::Path;
use std::time::{Duration, Instant};

use super::{App, Focus, quote};
use crate::command::Command;
use crate::journal::Actor;
use crate::ops::DeleteMode;
use crate::panel::SelectMode;
use crate::tr;
use crate::wm::{self, WinId};

impl App {
    /// Runs `command` for the user; `false`: it does not apply now.
    pub(super) fn run_command(&mut self, command: Command) -> bool {
        use Command::*;
        let a = self.active;
        let cmdline_empty = self.cmdline.is_empty();
        match command {
            // Far asks about each modified editor before leaving.
            Quit if self.editors_modified() => {
                if let Some(i) = self.editors.iter().position(|e| e.modified()) {
                    let id = self.editors[i].id;
                    self.wm.switch_to(crate::wm::ScreenId::Editor(id));
                    self.editor_leave(i, super::editors::After::Close);
                }
            }
            Quit => {
                if !self.agent_alive() && self.running.is_none()
                    || self
                        .quit_armed
                        .is_some_and(|t| t.elapsed() < Duration::from_secs(3))
                {
                    self.quit = true;
                } else {
                    self.quit_armed = Some(Instant::now());
                    self.say(tr!("quit-confirm"));
                }
            }
            MainMenu => self.main_menu(),
            TogglePanels => {
                if self.running.is_none() {
                    self.cycle_hiding();
                } else if !self.panels_visible() {
                    // A running command: Ctrl+O goes to it.
                    self.focus = Focus::Command;
                } else {
                    self.set_panels_visible(false);
                    self.focus = Focus::Command;
                }
            }
            // Like Far: Ctrl+arrows move the boundaries between windows;
            // left and right only with an empty command line.
            AgentTaller | AgentShorter => {
                // The boundary moves away from the agent pane's side.
                let up = (command == AgentTaller) != self.wm.agent_on_top();
                self.move_splitter(wm::MAIN_SPLIT, if up { -1 } else { 1 });
            }
            SplitterLeft if cmdline_empty => self.move_splitter(wm::PANELS_SPLIT, -1),
            SplitterRight if cmdline_empty => self.move_splitter(wm::PANELS_SPLIT, 1),
            SplitterLeft | SplitterRight => return false,
            DevRestart => self.request_restart(),
            // Tab to the quick view: the keys go to it.
            NextPanel if self.quick_view.as_ref().is_some_and(|q| q.side == 1 - a) => {
                if let Some(q) = &mut self.quick_view {
                    q.focused = !q.focused;
                }
            }
            NextPanel => {
                if !self.panel_hidden(1 - a) {
                    self.active = 1 - a;
                }
            }
            SwapPanels => self.swap_panels(),
            HidePassive => self.toggle_panel(1 - a),
            ToggleLeft | ToggleRight => {
                let side = usize::from(command == ToggleRight);
                if self.panels_visible() {
                    self.toggle_panel(side);
                } else {
                    // Both hidden (the user screen): just that one returns.
                    self.set_panels_visible(true);
                    self.wm.set_hidden(WinId::Panel(1 - side), true);
                    self.active = side;
                }
            }
            DriveMenuLeft => self.drive_menu(0),
            DriveMenuRight => self.drive_menu(1),
            CursorUp => self.panels[a].move_cursor(-1),
            CursorDown => self.panels[a].move_cursor(1),
            PageUp => self.panels[a].move_cursor(-(self.last_page() as isize)),
            PageDown => self.panels[a].move_cursor(self.last_page() as isize),
            Home => self.panels[a].move_cursor(isize::MIN / 2),
            End => self.panels[a].move_cursor(isize::MAX / 2),
            // The panel's, unless one column of names and text in the
            // command line (Far's ShellRightLeftArrowsRule 0).
            Left | Right if !self.panels[a].multi_column() && !cmdline_empty => return false,
            Left => self.panels[a].move_column(-1),
            Right => self.panels[a].move_column(1),
            Parent => {
                if let Some(parent) = self.panels[a].path.parent().map(Path::to_path_buf) {
                    self.change_dir(a, &parent);
                }
            }
            Root => {
                if let Some(root) = self.panels[a]
                    .path
                    .ancestors()
                    .last()
                    .map(Path::to_path_buf)
                {
                    self.change_dir(a, &root);
                }
            }
            Enter => {
                if self.cmdline.trim().is_empty() {
                    if self.panels_visible() {
                        self.enter();
                    }
                } else {
                    let text = self.cmdline.clone();
                    self.execute(text);
                }
            }
            Refresh => self.panels[a].reload(None),
            ToggleHidden => {
                let show = !self.config.panels.show_hidden;
                self.config.panels.show_hidden = show;
                crate::panel::set_show_hidden(show);
                for p in &mut self.panels {
                    p.reload(None);
                }
                self.say(tr!(if show {
                    "panels-hidden-shown"
                } else {
                    "panels-hidden-hidden"
                }));
                let path = crate::config::config_path();
                if let Err(e) = self.config.save(&path) {
                    self.say(tr!("settings-save-failed", error = e));
                }
            }
            View(mode) => self.panels[a].view = mode,
            Sort(mode) => self.panels[a].set_sort_mode(mode),
            SortMenu => self.sort_menu(),
            SelectedFirst => {
                let p = &mut self.panels[a];
                p.sort.selected_first = !p.sort.selected_first;
                p.resort();
            }
            SelectToggle => {
                self.panels[a].toggle_selection();
                self.panels[a].move_cursor(1);
                self.mark_selection_changed();
            }
            // One column over only with several columns (else the command
            // line's selection, as in Far).
            SelectLeft | SelectRight if !self.panels[a].multi_column() => return false,
            SelectUp | SelectDown | SelectHome | SelectEnd | SelectLeft | SelectRight => {
                let p = &self.panels[a];
                let (n, cur) = (p.entries.len() as isize, p.cursor as isize);
                let to = match command {
                    SelectUp => cur - 1,
                    SelectDown => cur + 1,
                    SelectHome => 0,
                    SelectEnd => n - 1,
                    SelectLeft => cur - p.page_rows() as isize,
                    _ => cur + p.page_rows() as isize,
                };
                self.shift_select_to(to, matches!(command, SelectHome | SelectEnd));
            }
            SelectDialog => self.select_dialog(true),
            UnselectDialog => self.select_dialog(false),
            SelectAll | UnselectAll => {
                let folders = self.config.panels.select_folders;
                self.panels[a].select_all(command == SelectAll, folders);
                self.mark_selection_changed();
            }
            SelectSameExt | UnselectSameExt => {
                self.select_like_current(command == SelectSameExt, true);
                self.mark_selection_changed();
            }
            SelectSameName | UnselectSameName => {
                self.select_like_current(command == SelectSameName, false);
                self.mark_selection_changed();
            }
            InvertSelection | InvertAll | InvertFiles => {
                let mode = match command {
                    InvertAll => SelectMode::InvertAll,
                    InvertFiles => SelectMode::InvertFiles,
                    _ => SelectMode::Invert,
                };
                let folders = self.config.panels.select_folders;
                self.panels[a].select_masked(None, mode, folders);
                self.mark_selection_changed();
            }
            RestoreSelection => {
                self.panels[a].restore_selection();
                self.mark_selection_changed();
            }
            Copy => self.copy_dialog(false, false),
            CopyCurrent => self.copy_dialog(false, true),
            Move => self.copy_dialog(true, false),
            Rename => self.copy_dialog(true, true),
            MkDir => self.mkdir_dialog(),
            Delete | DeleteCurrent => {
                let targets = self.op_sources(command == DeleteCurrent);
                self.delete_dialog(targets, DeleteMode::Trash, Actor::User, None);
            }
            Del if !cmdline_empty => return false,
            Del => {
                let targets = self.op_sources(false);
                self.delete_dialog(targets, DeleteMode::Trash, Actor::User, None);
            }
            DeletePermanent | Wipe => {
                let targets = self.op_sources(false);
                let mode = if command == Wipe {
                    DeleteMode::Wipe
                } else {
                    DeleteMode::Permanent
                };
                self.delete_dialog(targets, mode, Actor::User, None);
            }
            InsertName => {
                if let Some(e) = self.panels[a].current() {
                    let name = if e.name == ".." {
                        "..".to_string()
                    } else {
                        quote(&e.name)
                    };
                    self.cmdline_insert(&format!("{name} "));
                }
            }
            InsertFullName
            | InsertPassiveFullName
            | InsertPassiveName
            | InsertLeftPath
            | InsertRightPath
            | InsertActivePath
            | InsertPassivePath => self.insert_for(command),
            HistoryPrev => self.history_step(true),
            HistoryNext => self.history_step(false),
            ScreenLineUp | ScreenLineDown | ScreenPageUp | ScreenPageDown | ScreenTop
            | ScreenBottom => return self.scroll_user_screen_by(command),
            QuickView => self.toggle_quick_view(),
            Attributes => self.attributes_dialog(),
            InfoPanel => self.toggle_info_panel(),
            FindFolder => self.folder_tree(),
            FindFile => self.find_dialog("", ""),
            CommandHistory => self.history_menu(super::historymenu::HistoryMenu::Commands, None),
            ViewHistory => self.history_menu(super::historymenu::HistoryMenu::Views, None),
            FolderHistory => self.history_menu(super::historymenu::HistoryMenu::Folders, None),
            Screens => self.screens_menu(),
            NextScreen => self.cycle_screens(true),
            PrevScreen => self.cycle_screens(false),
            ViewFile => return self.view_current(Some(self.config.viewer.external_f3)),
            ViewFileAlt => return self.view_current(Some(!self.config.viewer.external_f3)),
            ViewInternal => return self.view_current(None),
            EditFile => return self.edit_current(false),
            EditNew => return self.edit_current(true),
            Viewer(_) | Editor(_) => return false,
        }
        true
    }
}
