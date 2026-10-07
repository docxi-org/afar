//! Viewer screens (F3) in the application: opening from the panel, the
//! viewer's keys, Far's screen list (F12, Ctrl+Tab), the code page and
//! mode menus, Gray+/- through the panel's files, Ctrl+F10.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossterm::event::KeyEvent;

use super::fileops::{Overlay, Purpose};
use super::panelcmds::MenuPurpose;
use super::{App, AppMsg};
use crate::command::{Command, Ctx, ViewerCmd};
use crate::dialog::{Button, Dialog, check_at, input_at, radio_at, text_at};
use crate::keymap::{Chord, Key};
use crate::menu::{Item, Menu};
use crate::tr;
use crate::viewer::search::{self, Found, Query};
use crate::viewer::{self, Mode, Outcome, Viewer, codepage};
use crate::wm::{ScreenId, WinId};

/// Far ignores F3 right after opening, so key repeat does not close the
/// viewer at once.
const F3_GRACE: Duration = Duration::from_millis(500);

/// A viewer's search in the background.
pub(super) struct RunningSearch {
    id: u32,
    cancel: Arc<AtomicBool>,
}

/// The result of a viewer's search.
pub struct SearchDone {
    id: u32,
    result: Result<Found, String>,
    backward: bool,
    /// Where the search started (for wrapping around).
    origin: u64,
    /// This was the second pass, from the other end.
    wrapped: bool,
}

/// An item of the code page menu.
#[derive(Clone, Copy, Debug)]
pub(super) enum CpChoice {
    Detect,
    Page(u32),
}

impl App {
    fn positions_file(&self) -> PathBuf {
        self.data_dir.join("history").join("viewer.json")
    }

    /// The viewer on screen, if a viewer screen is current.
    pub(super) fn shown_viewer(&self) -> Option<usize> {
        match self.wm.current_screen() {
            ScreenId::Viewer(id) => self.viewers.iter().position(|v| v.id == id),
            _ => None,
        }
    }

    /// F3 on the panel: views the file under the cursor; Gray+/- in the
    /// viewer go through the panel's files. `external`: the external
    /// viewer if one is set (F3 / Alt+F3 by the setting); `None`: always
    /// the built-in one (Ctrl+Shift+F3).
    pub(super) fn view_current(&mut self, external: Option<bool>) -> bool {
        let panel = &self.panels[self.active];
        let Some(entry) = panel.current() else {
            return false;
        };
        if entry.is_dir {
            self.count_dir_sizes();
            return true;
        }
        let path = panel.path.join(&entry.name);
        let command = self.config.viewer.external_command.trim().to_string();
        if external == Some(true) && !command.is_empty() {
            let name = format!("\"{}\"", path.display());
            let line = if command.contains("!.!") {
                command.replace("!.!", &name)
            } else {
                format!("{command} {name}")
            };
            self.execute(line);
            return true;
        }
        let list: Vec<PathBuf> = panel
            .entries
            .iter()
            .filter(|e| !e.is_dir)
            .map(|e| panel.path.join(&e.name))
            .collect();
        self.open_viewer(&path, list);
        true
    }

    /// F3 on a folder (Far's CountDirSize): the sizes of the selected
    /// folders — or of the current one, or of all of them on `..` — are
    /// counted in the background and shown in the size column.
    fn count_dir_sizes(&mut self) {
        let panel = &self.panels[self.active];
        let selected: Vec<String> = panel
            .selected()
            .filter(|e| e.is_dir && !e.link)
            .map(|e| e.name.clone())
            .collect();
        let names = if !selected.is_empty() {
            selected
        } else {
            match panel.current() {
                Some(e) if e.name == ".." => panel
                    .entries
                    .iter()
                    .filter(|e| e.is_dir && !e.link && e.name != "..")
                    .map(|e| e.name.clone())
                    .collect(),
                Some(e) if !e.link => vec![e.name.clone()],
                _ => Vec::new(),
            }
        };
        if names.is_empty() {
            return;
        }
        let dir = panel.path.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let sizes = names
                .into_iter()
                .map(|n| {
                    let size = tree_size(&dir.join(&n));
                    (n, size)
                })
                .collect();
            let _ = tx.send(AppMsg::DirSizes(dir, sizes));
        });
    }

    pub(super) fn dir_sizes(&mut self, dir: &Path, sizes: Vec<(String, u64)>) {
        for panel in &mut self.panels {
            if panel.path != dir {
                continue;
            }
            for (name, size) in &sizes {
                if let Some(e) = panel.entries.iter_mut().find(|e| e.name == *name) {
                    e.dir_size = Some(*size);
                }
            }
        }
    }

    pub(super) fn open_viewer(&mut self, path: &Path, list: Vec<PathBuf>) -> Option<u32> {
        let id = self.next_viewer_id;
        let remembered = self
            .viewer_positions
            .get(path)
            .cloned()
            .map(|r| r.filtered(&self.config.viewer));
        match Viewer::open(
            id,
            path,
            &self.viewer_defaults,
            remembered.as_ref(),
            &self.config.viewer,
        ) {
            Ok(mut v) => {
                self.next_viewer_id += 1;
                v.list = list;
                self.viewers.push(v);
                self.wm.add_screen(ScreenId::Viewer(id), WinId::Viewer(id));
                Some(id)
            }
            Err(e) => {
                self.message(
                    &tr!("MViewerTitle"),
                    &[
                        tr!("MViewerCannotOpenFile"),
                        path.display().to_string(),
                        e.to_string(),
                    ],
                    true,
                );
                None
            }
        }
    }

    fn remember(&mut self, i: usize) {
        let v = &self.viewers[i];
        self.viewer_defaults = v.defaults();
        let (path, r) = (v.path().to_path_buf(), v.remembered());
        self.viewer_positions.put(&path, r);
        let file = self.positions_file();
        self.viewer_positions.save(&file);
    }

    pub(super) fn close_viewer(&mut self, i: usize) {
        self.remember(i);
        let v = self.viewers.remove(i);
        self.wm.remove_screen(ScreenId::Viewer(v.id));
    }

    /// Saves the positions of all open viewers (on quit).
    pub(super) fn remember_viewers(&mut self) {
        for i in 0..self.viewers.len() {
            self.remember(i);
        }
    }

    /// Keys of a viewer screen.
    pub(super) fn viewer_key(&mut self, i: usize, key: KeyEvent) {
        if self.viewer_peek {
            // Ctrl+O showed the user screen until a key.
            self.viewer_peek = false;
            return;
        }
        if key.code == crossterm::event::KeyCode::Esc
            && let Some(s) = self.viewer_search.take()
        {
            s.cancel.store(true, Ordering::Relaxed);
            return;
        }
        let Some(chord) = Chord::from_event(&key) else {
            return;
        };
        let Some(command) = self.keymap.get(Ctx::Viewer, &chord) else {
            return;
        };
        if command == Command::Viewer(ViewerCmd::Close)
            && chord.key == Key::F(3)
            && self.viewers[i].opened.elapsed() < F3_GRACE
        {
            return;
        }
        match command {
            Command::Viewer(cmd) => self.run_viewer(i, cmd),
            other => {
                self.run_command(other);
            }
        }
    }

    pub(super) fn run_viewer(&mut self, i: usize, cmd: ViewerCmd) {
        let Outcome::App(cmd) = self.viewers[i].command(cmd) else {
            return;
        };
        use ViewerCmd::*;
        match cmd {
            Close => self.close_viewer(i),
            NextCodepage => {
                let v = &mut self.viewers[i];
                let cp = v.next_f8_codepage();
                v.set_codepage(cp);
            }
            CodepageMenu => self.codepage_menu(i),
            ModeMenu => self.view_mode_menu(i),
            GoFile => self.go_file(i),
            NextFile | PrevFile => self.step_file(i, cmd == NextFile),
            KeyBar => self.viewer_keybar = !self.viewer_keybar,
            UserScreen => self.viewer_peek = true,
            Search => self.viewer_search_dialog(i),
            SearchNext | SearchPrev if self.viewer_query.text.is_empty() => {
                self.viewer_search_dialog(i)
            }
            SearchNext => self.viewer_search_continue(i, false),
            SearchPrev => self.viewer_search_continue(i, true),
            Goto => self.viewer_goto_dialog(i),
            Copy => {
                if let Some(text) = self.viewers[i].selected_text()
                    && let Err(e) = crate::clipboard::set_text(&text)
                {
                    self.say(e);
                }
            }
            AskAgent => self.ide_mention(i),
            Settings => self.viewer_settings_dialog(),
            Edit => self.say(tr!("viewer-not-yet")),
            _ => {}
        }
    }

    /// Ctrl+F10: the active panel goes to the file (the viewer stays).
    fn go_file(&mut self, i: usize) {
        let path = self.viewers[i].path().to_path_buf();
        let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
            return;
        };
        let side = self.active;
        if self.panels[side].path != dir {
            self.change_dir(side, dir);
        }
        self.panels[side].set_cursor_by_name(&name.to_string_lossy());
    }

    /// Gray+ / Gray-: the next or previous file of the panel in the same
    /// viewer, keeping its mode.
    fn step_file(&mut self, i: usize, forward: bool) {
        let v = &self.viewers[i];
        let here = v.path().to_path_buf();
        let Some(pos) = v.list.iter().position(|p| *p == here) else {
            return;
        };
        let next = if forward {
            pos + 1
        } else {
            match pos.checked_sub(1) {
                Some(p) => p,
                None => return,
            }
        };
        let Some(path) = v.list.get(next).cloned() else {
            return;
        };
        self.remember(i);
        let old = &self.viewers[i];
        let (id, list, mode) = (old.id, old.list.clone(), old.mode);
        let remembered = self
            .viewer_positions
            .get(&path)
            .cloned()
            .map(|r| r.filtered(&self.config.viewer));
        match Viewer::open(
            id,
            &path,
            &self.viewer_defaults,
            remembered.as_ref(),
            &self.config.viewer,
        ) {
            Ok(mut v) => {
                v.list = list;
                if v.mode != mode {
                    v.set_mode(mode);
                }
                self.viewers[i] = v;
            }
            Err(e) => self.message(
                &tr!("MViewerTitle"),
                &[
                    tr!("MViewerCannotOpenFile"),
                    path.display().to_string(),
                    e.to_string(),
                ],
                true,
            ),
        }
    }

    /// Shift+F8: Far's code page menu — detection, the system pages,
    /// Unicode, the rest of the installed ones.
    fn codepage_menu(&mut self, i: usize) {
        let current = self.viewers[i].codepage();
        let (ansi, oem) = (codepage::ansi(), codepage::oem());
        let mut items = vec![Item::new(tr!("MEditOpenAutoDetect"))];
        let mut choices = vec![None];
        let mut group = |title: String, pages: Vec<u32>, items: &mut Vec<Item>| {
            if pages.is_empty() {
                return;
            }
            items.push(Item::titled_separator(title));
            choices.push(None);
            for cp in pages {
                let name = codepage::long_name(cp);
                items.push(Item::new(name).checked((cp == current).then_some('√')));
                choices.push(Some(CpChoice::Page(cp)));
            }
        };
        group(tr!("MGetCodePageSystem"), vec![ansi, oem], &mut items);
        group(
            tr!("MGetCodePageUnicode"),
            vec![codepage::UTF8, codepage::UTF16LE, codepage::UTF16BE],
            &mut items,
        );
        let other: Vec<u32> = codepage::installed()
            .into_iter()
            .filter(|cp| ![ansi, oem, codepage::UTF8].contains(cp))
            .collect();
        group(tr!("MGetCodePageOther"), other, &mut items);
        choices[0] = Some(CpChoice::Detect);
        let selected = choices
            .iter()
            .position(|c| matches!(c, Some(CpChoice::Page(cp)) if *cp == current))
            .unwrap_or(0);
        let menu = Menu::new(tr!("MGetCodePageTitle"), items).select(selected);
        let id = self.viewers[i].id;
        self.overlays.push(Overlay::Menu {
            menu,
            purpose: MenuPurpose::Codepage { id, choices },
        });
    }

    /// Shift+F4: text, hex, dump.
    fn view_mode_menu(&mut self, i: usize) {
        let mode = self.viewers[i].mode;
        let items = [
            ("MViewF4Text", Mode::Text),
            ("MViewF4", Mode::Hex),
            ("MViewF4Dump", Mode::Dump),
        ]
        .into_iter()
        .map(|(id, m)| Item::new(tr!(id)).checked((m == mode).then_some('√')))
        .collect();
        let selected = match mode {
            Mode::Text => 0,
            Mode::Hex => 1,
            Mode::Dump => 2,
        };
        let menu = Menu::new(tr!("MViewMode"), items).select(selected);
        let id = self.viewers[i].id;
        self.overlays.push(Overlay::Menu {
            menu,
            purpose: MenuPurpose::ViewMode { id },
        });
    }

    pub(super) fn codepage_chosen(&mut self, id: u32, choice: CpChoice) {
        let Some(v) = self.viewers.iter_mut().find(|v| v.id == id) else {
            return;
        };
        let cp = match choice {
            CpChoice::Detect => v.detect_codepage(),
            CpChoice::Page(cp) => cp,
        };
        v.set_codepage(cp);
    }

    pub(super) fn view_mode_chosen(&mut self, id: u32, item: usize) {
        if let Some(v) = self.viewers.iter_mut().find(|v| v.id == id) {
            v.set_mode([Mode::Text, Mode::Hex, Mode::Dump][item.min(2)]);
        }
    }

    // --------------------------------------------------------------- goto

    /// Alt+F8: Far's go-to dialog (stddlg.cpp `GoToRowCol`): a byte
    /// offset (or `%`), optionally a column.
    fn viewer_goto_dialog(&mut self, i: usize) {
        // The Hex box starts as "the hex mode" and is then remembered.
        let hex = self
            .viewer_goto
            .1
            .unwrap_or(self.viewers[i].mode == Mode::Hex);
        let dialog = Dialog::new(tr!("MGoTo"), 29)
            .row(vec![input_at(5, 28, self.viewer_goto.0.clone(), true)])
            .separator()
            .row(vec![check_at(5, tr!("MGoToHex"), hex)])
            .separator()
            .buttons(&[&tr!("MOk"), &tr!("MCancel")], 0);
        let id = self.viewers[i].id;
        self.overlays.push(Overlay::Dialog {
            dialog,
            purpose: Purpose::ViewerGoto { id },
        });
    }

    pub(super) fn viewer_goto_closed(&mut self, id: u32, dialog: &Dialog) {
        let (text, hex) = (dialog.input_value(0), dialog.checked(0));
        self.viewer_goto = (text.clone(), Some(hex));
        let Some((row, col)) = viewer::parse_goto(&text, hex) else {
            return;
        };
        let Some(v) = self.viewers.iter_mut().find(|v| v.id == id) else {
            return;
        };
        let pos = row.map_or(v.top, |r| r.resolve(v.top, v.size()));
        let left = col.map(|c| c.resolve(v.left as u64, 0) as usize);
        v.goto(pos, left);
    }

    // ------------------------------------------------------------- search

    /// F7: Far's search dialog (stddlg.cpp `GetSearchReplaceString`, 76×12).
    fn viewer_search_dialog(&mut self, i: usize) {
        let q = &self.viewer_query;
        let hex = q.hex || self.viewers[i].mode == Mode::Hex && q.text.is_empty();
        // The label loses its hot key: the radio buttons have their own.
        let label = tr!("MSearchReplaceSearchFor").replace('&', "");
        let text_label = tr!("MSearchReplaceText");
        let x_text = 5 + crate::dialog::visible(&label).chars().count() as u16 + 2;
        let x_hex = x_text + 4 + crate::dialog::visible(&text_label).chars().count() as u16 + 2;
        let mut d = Dialog::far(tr!("MSearchReplaceSearchTitle"), 76);
        let group = d.new_group();
        let dialog = d
            .row(vec![
                text_at(5, label),
                radio_at(x_text, text_label, !hex, group),
                radio_at(x_hex, tr!("MSearchReplaceHex"), hex, group),
            ])
            .row(vec![input_at(5, 65, q.text.clone(), true)])
            .separator()
            .row(vec![
                check_at(5, tr!("MSearchReplaceCase"), q.case),
                check_at(39, tr!("MSearchReplaceRegexp"), q.regex),
            ])
            .row(vec![check_at(5, tr!("MSearchReplaceWholeWords"), q.words)])
            .row(vec![
                check_at(5, tr!("MSearchReplaceFuzzy"), false).disabled(),
            ])
            .separator()
            .button_row(vec![
                Button::new(tr!("MSearchReplaceFindPrev")),
                Button::new(tr!("MSearchReplaceFindNext")).default(),
                Button::new(tr!("MSearchReplaceCancel")),
            ])
            .focus_item(2);
        let id = self.viewers[i].id;
        self.overlays.push(Overlay::Dialog {
            dialog,
            purpose: Purpose::ViewerSearch { id },
        });
    }

    pub(super) fn viewer_search_dialog_closed(
        &mut self,
        id: u32,
        button: Option<usize>,
        dialog: &Dialog,
    ) {
        let backward = match button {
            Some(0) => true,
            Some(1) => false,
            _ => return,
        };
        let query = Query {
            text: dialog.input_value(0),
            hex: dialog.radio(0) == 1,
            case: dialog.checked(0),
            regex: dialog.checked(1),
            words: dialog.checked(2),
        };
        if query.text.is_empty() {
            return;
        }
        if query.hex && query.hex_bytes().is_none() {
            self.not_found(&query);
            return;
        }
        self.viewer_query = query;
        let Some(i) = self.viewers.iter().position(|v| v.id == id) else {
            return;
        };
        // A new search: from the top of the screen forward, from its end
        // backward.
        let v = &mut self.viewers[i];
        let from = if backward { v.visible_end() } else { v.top };
        self.start_search(id, from, backward, from, false);
    }

    /// Shift+F7 / Space, Alt+F7: on from the found text.
    fn viewer_search_continue(&mut self, i: usize, backward: bool) {
        let v = &mut self.viewers[i];
        let from = match (v.selection, backward) {
            (Some((s, _)), false) => s + v.unit(),
            (Some((s, _)), true) => s,
            (None, false) => v.top,
            (None, true) => v.visible_end(),
        };
        let id = v.id;
        self.start_search(id, from, backward, from, false);
    }

    fn start_search(&mut self, id: u32, from: u64, backward: bool, origin: u64, wrapped: bool) {
        let Some(v) = self.viewers.iter().find(|v| v.id == id) else {
            return;
        };
        if let Some(old) = self.viewer_search.take() {
            old.cancel.store(true, Ordering::Relaxed);
        }
        let (path, cp, query) = (
            v.path().to_path_buf(),
            v.codepage(),
            self.viewer_query.clone(),
        );
        // The second pass stops where the first began.
        let limit = match (wrapped, backward) {
            (false, false) => u64::MAX,
            (false, true) => 0,
            (true, _) => origin,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        self.viewer_search = Some(RunningSearch {
            id,
            cancel: cancel.clone(),
        });
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = search::find(&path, cp, &query, from, limit, backward, &cancel);
            let _ = tx.send(AppMsg::ViewerFound(SearchDone {
                id,
                result,
                backward,
                origin,
                wrapped,
            }));
        });
    }

    pub(super) fn viewer_found(&mut self, done: SearchDone) {
        if self.viewer_search.as_ref().is_none_or(|s| s.id != done.id) {
            return;
        }
        self.viewer_search = None;
        let Some(i) = self.viewers.iter().position(|v| v.id == done.id) else {
            return;
        };
        match done.result {
            Ok(Found::At(start, end)) => {
                let v = &mut self.viewers[i];
                v.selection = Some((start, end));
                v.show_pos(start, end - start);
            }
            Ok(Found::Edge) if !done.wrapped => {
                // Far's SearchWrapStop: ask at the end of the file.
                let (edge, question) = if done.backward {
                    ("MViewSearchBod", "MViewSearchFromEnd")
                } else {
                    ("MViewSearchEod", "MViewSearchFromBegin")
                };
                let dialog = Dialog::message(
                    &tr!("MSearchReplaceSearchTitle"),
                    &[tr!(edge), tr!(question)],
                    &[&tr!("MYes"), &tr!("MCancel")],
                    false,
                );
                self.overlays.push(Overlay::Dialog {
                    dialog,
                    purpose: Purpose::ViewerSearchWrap {
                        id: done.id,
                        backward: done.backward,
                        origin: done.origin,
                    },
                });
            }
            Ok(Found::Edge) => {
                let q = self.viewer_query.clone();
                self.not_found(&q);
            }
            Ok(Found::Cancelled) => {}
            Err(e) => self.message(&tr!("MSearchReplaceSearchTitle"), &[e], true),
        }
    }

    pub(super) fn viewer_search_wrap(&mut self, id: u32, backward: bool, origin: u64) {
        let Some(v) = self.viewers.iter().find(|v| v.id == id) else {
            return;
        };
        let from = if backward { v.size() } else { 0 };
        self.start_search(id, from, backward, origin, true);
    }

    fn not_found(&mut self, q: &Query) {
        let what = if q.hex {
            "MViewSearchCannotFindHex"
        } else {
            "MViewSearchCannotFind"
        };
        self.message(
            &tr!("MSearchReplaceSearchTitle"),
            &[tr!(what), format!("\"{}\"", q.text)],
            true,
        );
    }

    // ------------------------------------------------------------ screens

    /// Screens of the F12 list: the panels and the viewers (not the user
    /// screen, which belongs to the panels).
    fn switchable_screens(&self) -> Vec<ScreenId> {
        self.wm
            .screens()
            .into_iter()
            .filter(|s| *s != ScreenId::UserScreen)
            .collect()
    }

    /// The screen the panels' user screen counts as.
    fn current_switchable(&self) -> ScreenId {
        match self.wm.current_screen() {
            ScreenId::UserScreen => ScreenId::Panels,
            s => s,
        }
    }

    /// F12: Far's screen list (manager.cpp `WindowsMenu`): hot key, type,
    /// name.
    pub(super) fn screens_menu(&mut self) {
        let screens = self.switchable_screens();
        let rows: Vec<(String, String)> = screens
            .iter()
            .map(|s| match s {
                ScreenId::Viewer(id) => (
                    tr!("MScreensView"),
                    self.viewers
                        .iter()
                        .find(|v| v.id == *id)
                        .map(|v| v.path().display().to_string())
                        .unwrap_or_default(),
                ),
                _ => (
                    tr!("MScreensPanels"),
                    self.panels[self.active].path.display().to_string(),
                ),
            })
            .collect();
        let type_w = rows.iter().map(|r| r.0.chars().count()).max().unwrap_or(0);
        let items = rows
            .iter()
            .enumerate()
            .map(|(i, (kind, name))| {
                let hotkey = match i {
                    0..=9 => format!("&{i}"),
                    10..=35 => format!("&{}", (b'A' + (i - 10) as u8) as char),
                    _ => " ".into(),
                };
                let name = name.replace('&', "&&");
                Item::new(format!("{hotkey}  {kind:<type_w$}   {name}"))
            })
            .collect();
        let current = self.current_switchable();
        let selected = screens.iter().position(|s| *s == current).unwrap_or(0);
        let menu = Menu::new(tr!("MScreensTitle"), items).select(selected);
        self.overlays.push(Overlay::Menu {
            menu,
            purpose: MenuPurpose::Screens { screens },
        });
    }

    pub(super) fn switch_screen(&mut self, screen: ScreenId) {
        self.viewer_peek = false;
        self.wm.switch_to(screen);
    }

    /// Ctrl+Tab / Ctrl+Shift+Tab.
    pub(super) fn cycle_screens(&mut self, forward: bool) {
        let screens = self.switchable_screens();
        if screens.len() < 2 {
            return;
        }
        let current = self.current_switchable();
        let i = screens.iter().position(|s| *s == current).unwrap_or(0);
        let n = screens.len();
        let next = if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        };
        self.switch_screen(screens[next]);
    }

    /// Far's reload timer: the shown viewer checks its file once a second.
    pub(super) fn viewer_tick(&mut self) {
        if let Some(i) = self.shown_viewer()
            && self.viewer_checked.elapsed() >= Duration::from_secs(1)
        {
            self.viewer_checked = std::time::Instant::now();
            self.viewers[i].check_changed();
        }
    }

    /// Viewers for the restart state.
    pub(super) fn viewer_states(&self) -> (Vec<crate::dev::ViewerState>, Option<usize>) {
        let states = self
            .viewers
            .iter()
            .map(|v| {
                let mut remembered = v.remembered();
                remembered.mode = Some(v.mode);
                crate::dev::ViewerState {
                    path: v.path().to_path_buf(),
                    remembered,
                    mode: v.mode,
                    wrap: v.wrap,
                    word_wrap: v.word_wrap,
                    list: v.list.clone(),
                }
            })
            .collect();
        (states, self.shown_viewer())
    }

    /// Reopens the viewers of the previous instance (development mode).
    pub(super) fn restore_viewers(
        &mut self,
        states: Vec<crate::dev::ViewerState>,
        shown: Option<usize>,
    ) {
        let screen = self.wm.current_screen();
        let mut shown_id = None;
        for (n, s) in states.into_iter().enumerate() {
            let defaults = viewer::Defaults {
                wrap: s.wrap,
                word_wrap: s.word_wrap,
                ..self.viewer_defaults
            };
            let id = self.next_viewer_id;
            if let Ok(mut v) = Viewer::open(
                id,
                &s.path,
                &defaults,
                Some(&s.remembered),
                &self.config.viewer,
            ) {
                self.next_viewer_id += 1;
                v.list = s.list;
                if v.mode != s.mode {
                    v.set_mode(s.mode);
                }
                self.viewers.push(v);
                self.wm.add_screen(ScreenId::Viewer(id), WinId::Viewer(id));
                if shown == Some(n) {
                    shown_id = Some(id);
                }
            }
        }
        self.wm.switch_to(match shown_id {
            Some(id) => ScreenId::Viewer(id),
            None => screen,
        });
    }

    /// The key bar of the shown viewer.
    pub(super) fn viewer_keybar_labels(&self, i: usize, group: &str) -> Vec<String> {
        let v = &self.viewers[i];
        v.keybar_labels(v.next_f8_codepage(), group).to_vec()
    }
}

/// The size of the files in a folder and below it; links are not followed.
fn tree_size(dir: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in read.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total += meta.len();
            }
        }
    }
    total
}
