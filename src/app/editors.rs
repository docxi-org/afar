//! Editor windows (F4; docs/11 «Редактор», docs/17): opening (F4,
//! Shift+F4, F6 from the viewer, the history), keys, saving with Far's
//! questions, the status line and key bar, the mouse, positions, and the
//! file watched on the disk (afar's: Far checks only on save).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use serde::{Deserialize, Serialize};

use super::fileops::{Overlay, Purpose};
use super::{App, Focus};
use crate::command::{Command, Ctx, EditorCmd};
use crate::dialog::{Dialog, check_at, combo_at, input_at, text_at};
use crate::editor::{Editor, Eol, Outcome, Pos, text};
use crate::keymap::{Chord, Key};
use crate::theme;
use crate::tr;
use crate::viewer::codepage;
use crate::wm::{ScreenId, WinId};

/// Far: F4 is ignored for half a second after opening (key repeat from
/// the panel would close the window).
const F4_GRACE: Duration = Duration::from_millis(500);

/// Files from this size are opened in the editor only when asked to.
const LARGE_FILE: u64 = 64 * 1024 * 1024;

/// "Save as" with another code page, byte order mark or line endings:
/// they become the window's only once the file is written (Far).
#[derive(Clone, Copy, Debug)]
pub(super) struct SaveFormat {
    cp: u32,
    bom: bool,
    eol: Option<Eol>,
}

/// The code pages offered: UTF-8 and UTF-16 first, then the system's
/// (ANSI, OEM and the installed ones), the file's own always among them.
fn codepage_list(current: u32) -> Vec<u32> {
    let mut list = vec![
        codepage::UTF8,
        codepage::UTF16LE,
        codepage::UTF16BE,
        codepage::ansi(),
        codepage::oem(),
    ];
    for cp in codepage::installed() {
        if !list.contains(&cp) {
            list.push(cp);
        }
    }
    if current != 0 && !list.contains(&current) {
        list.insert(0, current);
    }
    list
}

/// What to do once a save (or the question about it) is done.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum After {
    Stay,
    Close,
    /// F6: on to the viewer at the same place.
    View,
}

/// The editor's questions (dialogs), answered in `editor_dialog_closed`.
pub(super) enum Ask {
    /// Shift+F4: the file to open or create, and the code pages listed.
    Open { cps: Vec<u32> },
    /// The folder of a new file does not exist yet.
    NewPath { path: PathBuf, cp: Option<u32> },
    /// A file of 64 MB or more: the editor, or the viewer.
    Large { path: PathBuf, cp: Option<u32> },
    /// Shift+F2.
    SaveAs { id: u32, then: After, cps: Vec<u32> },
    /// Leaving a modified file: save?
    Save { id: u32, then: After },
    /// The file changed on the disk since it was read (on save).
    External { id: u32, then: After },
    /// The file is read-only: overwrite?
    ReadOnly {
        id: u32,
        then: After,
        path: PathBuf,
        fmt: Option<SaveFormat>,
    },
    /// "Save as" onto another existing file.
    Overwrite {
        id: u32,
        then: After,
        path: PathBuf,
        fmt: Option<SaveFormat>,
    },
    /// The file (or its folder) is gone: save?
    Deleted { id: u32, then: After },
    /// The file changed on the disk while edited (afar's): read it again?
    Reload { id: u32 },
    /// F7 / Ctrl+F7.
    Search { id: u32, replace: bool },
    /// Replace this match?
    Replace {
        run: super::editsearch::ReplaceRun,
        found: crate::editor::Found,
    },
    /// Alt+F8.
    Goto { id: u32 },
    /// F8 / Shift+F8 needing the file read again: unsaved changes go.
    ReloadCp { id: u32, cp: u32 },
    /// The new page cannot read the text as it is: Show / OK / Cancel.
    SwitchCp { id: u32, cp: u32, at: Pos },
    /// The file has bytes its page cannot read: another page, or as it is.
    BadCp { id: u32, cps: Vec<u32> },
    /// Saving a file with unreadable bytes loses them: save anyway?
    DataLost { id: u32, then: After },
    /// The file is open already: the open window, a new one, read again.
    Reedit {
        id: u32,
        path: PathBuf,
        cp: Option<u32>,
    },
}

/// Where a file was left (Far's editor position cache).
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct EditorPlace {
    pub line: usize,
    pub col: usize,
    pub top: usize,
    pub left: usize,
    pub cp: u32,
    #[serde(default)]
    pub bookmarks: [Option<crate::editor::Bookmark>; 10],
}

/// The position cache of the editor: `history/editor.json`.
#[derive(Default, Serialize, Deserialize)]
pub struct EditorPlaces {
    entries: Vec<(String, EditorPlace)>,
}

fn place_key(path: &Path) -> String {
    let s = path.display().to_string();
    if cfg!(windows) { s.to_lowercase() } else { s }
}

impl EditorPlaces {
    pub fn load(file: &Path) -> Self {
        std::fs::read(file)
            .ok()
            .and_then(|d| serde_json::from_slice(&d).ok())
            .unwrap_or_default()
    }

    fn get(&self, path: &Path) -> Option<&EditorPlace> {
        let k = place_key(path);
        self.entries.iter().find(|(p, _)| *p == k).map(|(_, r)| r)
    }

    fn put(&mut self, path: &Path, place: EditorPlace, file: &Path) {
        let k = place_key(path);
        self.entries.retain(|(p, _)| *p != k);
        self.entries.insert(0, (k, place));
        self.entries.truncate(1000);
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec(self) {
            let _ = std::fs::write(file, json);
        }
    }
}

/// A file's time and size (changes made outside the editor).
fn stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

impl App {
    fn places_file(&self) -> PathBuf {
        self.data_dir.join("history").join("editor.json")
    }

    /// The editor on screen, if an editor screen is current.
    pub(super) fn shown_editor(&self) -> Option<usize> {
        match self.wm.current_screen() {
            ScreenId::Editor(id) => self.editors.iter().position(|e| e.id == id),
            _ => None,
        }
    }

    pub(super) fn editor_index(&self, id: u32) -> Option<usize> {
        self.editors.iter().position(|e| e.id == id)
    }

    /// F4 / Ctrl+Shift+F4 on the panel; Shift+F4: the "open or create"
    /// dialog. F4 on a folder is Far's attributes dialog.
    pub(super) fn edit_current(&mut self, ask_name: bool) -> bool {
        if ask_name {
            self.editor_open_dialog();
            return true;
        }
        let panel = &self.panels[self.active];
        let Some(entry) = panel.current() else {
            return false;
        };
        if entry.is_dir {
            if entry.name != ".." {
                self.attributes_dialog();
            }
            return true;
        }
        let path = panel.path.join(&entry.name);
        // Far's UseExternalEditor: the configured program instead.
        let command = self.config.editor.external_command.trim().to_string();
        if self.config.editor.external_f4 && !command.is_empty() {
            let name = format!("\"{}\"", path.display());
            let line = if command.contains("!.!") {
                command.replace("!.!", &name)
            } else {
                format!("{command} {name}")
            };
            self.execute_external(line);
            return true;
        }
        self.edit_file(&path, None);
        true
    }

    /// Opens `path` in the editor; a large file asks first.
    pub(super) fn edit_file(&mut self, path: &Path, cp: Option<u32>) {
        if std::fs::metadata(path).is_ok_and(|m| m.len() >= LARGE_FILE) {
            let size = crate::panel::size_float(std::fs::metadata(path).map_or(0, |m| m.len()));
            let dialog = Dialog::message(
                &tr!("MEditTitle"),
                &[path.display().to_string(), tr!("editor-large", size = size)],
                &[
                    &tr!("editor-large-edit"),
                    &tr!("editor-large-view"),
                    &tr!("MCancel"),
                ],
                true,
            );
            self.overlays.push(Overlay::Dialog {
                dialog,
                purpose: Purpose::Editor(Ask::Large {
                    path: path.to_path_buf(),
                    cp,
                }),
            });
            return;
        }
        // Open already (Far's FindWindowByFile): asked how, when modified
        // or when the confirmation is on; else that window.
        if let Some(e) = self
            .editors
            .iter()
            .find(|e| place_key(e.path()) == place_key(path))
        {
            let id = e.id;
            if e.modified() || self.config.confirm.reedit {
                let dialog = Dialog::message(
                    &tr!("MEditTitle"),
                    &[path.display().to_string(), tr!("MAskReload")],
                    &[
                        &tr!("MCurrent"),
                        &tr!("MNewOpen"),
                        &tr!("MReload"),
                        &tr!("MCancel"),
                    ],
                    false,
                );
                self.overlays.push(Overlay::Dialog {
                    dialog,
                    purpose: Purpose::Editor(Ask::Reedit {
                        id,
                        path: path.to_path_buf(),
                        cp,
                    }),
                });
            } else {
                self.wm.switch_to(ScreenId::Editor(id));
            }
            return;
        }
        if let Some(id) = self.open_editor(path, cp, None) {
            self.journal_user_edit(path);
            self.editor_user_opened(id);
        }
    }

    /// After the user opened a file: bytes its page cannot read are
    /// asked about (Far's `BadCodepageDialog`) — another page, or as it is.
    pub(super) fn editor_user_opened(&mut self, id: u32) {
        let Some(i) = self.editor_index(id) else {
            return;
        };
        let e = &self.editors[i];
        let Some(bytes) = e.bad_conversion.clone() else {
            return;
        };
        let cps = codepage_list(e.cp);
        let selected = cps.iter().position(|cp| *cp == e.cp).unwrap_or(0);
        let items = cps
            .iter()
            .map(|cp| Some(codepage::long_name(*cp)))
            .collect();
        let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02X}")).collect();
        let dialog = Dialog::far(tr!("MWarning"), 64)
            .row(vec![text_at(
                5,
                tr!("MUnsupportedCodePageSelectedCodepage"),
            )])
            .row(vec![combo_at(5, 53, items, selected)])
            .row(vec![text_at(
                5,
                tr!(
                    "MUnsupportedCodePageDoesNotSupport",
                    p0 = tr!("MUnsupportedCodePageByteSequence")
                ),
            )])
            .row(vec![text_at(5, format!("[{}]", hex.join(" ")))])
            .row(vec![text_at(5, tr!("MEditorSaveNotRecommended"))])
            .separator()
            .buttons(&[&tr!("MOk"), &tr!("MCancel")], 0)
            .warning();
        self.overlays.push(Overlay::Dialog {
            dialog,
            purpose: Purpose::Editor(Ask::BadCp { id, cps }),
        });
    }

    /// The window goes without saving and without remembering its place
    /// (Far's "Reload", a cancelled bad open).
    fn editor_drop(&mut self, i: usize) {
        let e = self.editors.remove(i);
        self.wm.remove_screen(ScreenId::Editor(e.id));
    }

    /// Opens an editor screen (or shows the one already open on the file);
    /// `line`: where to put the cursor (from 0).
    pub(super) fn open_editor(
        &mut self,
        path: &Path,
        cp: Option<u32>,
        line: Option<usize>,
    ) -> Option<u32> {
        self.open_editor_as(path, cp, line, false)
    }

    /// `new_copy`: another window on a file already open (Far's "New
    /// instance").
    fn open_editor_as(
        &mut self,
        path: &Path,
        cp: Option<u32>,
        line: Option<usize>,
        new_copy: bool,
    ) -> Option<u32> {
        if path.is_dir() {
            self.message(&tr!("MEditTitle"), &[tr!("MEditCanNotEditDirectory")], true);
            return None;
        }
        let key = place_key(path);
        if let Some(e) = self
            .editors
            .iter_mut()
            .find(|e| !new_copy && place_key(e.path()) == key)
        {
            let id = e.id;
            if let Some(l) = line {
                e.cursor = Pos::new(l.min(e.line_count() - 1), 0);
                e.top = e.cursor.line.saturating_sub(3);
            }
            self.wm.switch_to(ScreenId::Editor(id));
            return Some(id);
        }
        let remembered = self.editor_places.get(path).cloned();
        // A file that does not show its code page (plain ASCII, a new
        // one) is taken as UTF-8 — Far takes ANSI, and a Cyrillic word
        // typed into it then goes to the disk in 1251, which other tools
        // (the agent's Read and Edit) read as UTF-8 and break. A page set
        // in the settings wins.
        let default_cp = match self.config.editor.default_codepage {
            0 => codepage::UTF8,
            cp => cp,
        };
        let id = self.next_editor_id;
        let mut editor = match std::fs::read(path) {
            Ok(data) => {
                // A page asked for wins; a remembered one only when the file
                // has no byte order mark of its own (a mark is proof).
                let remembered_cp = remembered
                    .as_ref()
                    .map(|r| r.cp)
                    .filter(|cp| *cp != 0 && codepage::bom(&data).is_none());
                let want = cp.or(remembered_cp);
                let l = text::load(
                    &data,
                    want,
                    self.config.editor.autodetect_codepage,
                    default_cp,
                );
                let mut e = Editor::new(id, path, l.lines, l.cp, l.bom, l.eol.unwrap_or(Eol::CrLf));
                e.stamp = stamp(path);
                e.bad_conversion = l.bad;
                e
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let mut e = Editor::new(
                    id,
                    path,
                    Vec::new(),
                    cp.unwrap_or(default_cp),
                    false,
                    Eol::CrLf,
                );
                e.new_file = true;
                e
            }
            Err(err) => {
                self.message(
                    &tr!("MEditTitle"),
                    &[
                        tr!("MEditCannotOpen"),
                        path.display().to_string(),
                        err.to_string(),
                    ],
                    true,
                );
                return None;
            }
        };
        editor.settings = self.editor_settings();
        editor.line_numbers = self.config.editor.line_numbers;
        if editor.settings.expand_tabs == 2 {
            editor.expand_all_tabs(false);
        }
        let ed_config = &self.config.editor;
        if let Some(r) = remembered.as_ref().filter(|_| ed_config.save_bookmarks) {
            editor.bookmarks = r.bookmarks;
        }
        if let Some(r) = remembered.filter(|_| line.is_none() && ed_config.save_position) {
            let last = editor.line_count() - 1;
            editor.cursor = Pos::new(r.line.min(last), r.col);
            editor.top = r.top.min(last);
            editor.left = r.left;
        }
        if let Some(l) = line {
            let l = l.min(editor.line_count() - 1);
            editor.cursor = Pos::new(l, 0);
            editor.top = l;
        }
        self.next_editor_id += 1;
        self.editors.push(editor);
        self.wm.add_screen(ScreenId::Editor(id), WinId::Editor(id));
        self.focus = Focus::Panels;
        self.record_edit(path);
        Some(id)
    }

    /// Far's history: an edited file (`far_type` 1).
    fn record_edit(&mut self, path: &Path) {
        use crate::history::Kind;
        let folder = self.panels[self.active].path.display().to_string();
        let text = path.display().to_string();
        self.store.add(Kind::View, "", &text, &folder, "user");
        self.store
            .set_data(Kind::View, "", &text, "{\"far_type\":1}");
    }

    /// The user opened a file in the editor (the journal).
    fn journal_user_edit(&mut self, path: &Path) {
        self.journal.push(
            crate::journal::Actor::User,
            crate::journal::Event::EditorOpened {
                path: path.to_path_buf(),
            },
        );
    }

    /// The configured settings of a new editor window.
    pub(super) fn editor_settings(&self) -> crate::editor::Settings {
        use crate::config::{ExpandTabs, ShowWhitespace};
        let c = &self.config.editor;
        crate::editor::Settings {
            tab_size: c.tab_size.clamp(1, 512),
            expand_tabs: match c.expand_tabs {
                ExpandTabs::Keep => 0,
                ExpandTabs::New => 1,
                ExpandTabs::All => 2,
            },
            cursor_beyond_eol: c.cursor_beyond_eol,
            persistent_blocks: c.persistent_blocks,
            del_removes_blocks: c.del_removes_blocks,
            search_cursor_at_end: c.search_cursor_at_end,
            search_select_found: c.search_select_found,
            auto_indent: c.auto_indent,
            show_whitespace: match c.show_whitespace {
                ShowWhitespace::Off => 0,
                ShowWhitespace::All => 1,
                ShowWhitespace::NoEol => 2,
            },
            scrollbar: c.scrollbar,
            ..crate::editor::Settings::default()
        }
    }

    fn remember_editor(&mut self, i: usize) {
        let e = &self.editors[i];
        if e.new_file {
            return;
        }
        let place = EditorPlace {
            line: e.cursor.line,
            col: e.cursor.col,
            top: e.top,
            left: e.left,
            // Far does not keep the page of a file it could not read.
            cp: if e.bad_conversion.is_some() { 0 } else { e.cp },
            bookmarks: e.bookmarks,
        };
        let (path, file) = (e.path().to_path_buf(), self.places_file());
        self.editor_places.put(&path, place, &file);
    }

    /// Saves the positions of all open editors (on quit).
    pub(super) fn remember_editors(&mut self) {
        for i in 0..self.editors.len() {
            self.remember_editor(i);
        }
    }

    pub(super) fn close_editor(&mut self, i: usize) {
        self.remember_editor(i);
        let e = self.editors.remove(i);
        self.wm.remove_screen(ScreenId::Editor(e.id));
    }

    // --------------------------------------------------------------- keys

    pub(super) fn editor_key(&mut self, i: usize, key: KeyEvent) {
        if self.viewer_peek {
            self.viewer_peek = false;
            return;
        }
        let e = &mut self.editors[i];
        // Ctrl+Q: the next key as it is (a Ctrl+letter as its control
        // character).
        if e.quote_next {
            e.quote_next = false;
            if let KeyCode::Char(c) = key.code {
                let c = if key.modifiers.contains(KeyModifiers::CONTROL) && c.is_ascii_alphabetic()
                {
                    char::from((c.to_ascii_lowercase() as u8) & 0x1f)
                } else {
                    c
                };
                e.type_char(c);
                e.scroll_to_cursor();
            }
            return;
        }
        let chord = Chord::from_event(&key);
        if let Some(command) = chord.and_then(|c| self.keymap.get(Ctx::Editor, &c)) {
            if command == Command::Editor(EditorCmd::Quit)
                && chord.is_some_and(|c| c.key == Key::F(4))
                && self.editors[i].opened.elapsed() < F4_GRACE
            {
                return;
            }
            match command {
                Command::Editor(cmd) => self.run_editor(i, cmd),
                other => {
                    self.run_command(other);
                }
            }
            return;
        }
        // Typing: plain and shifted characters, AltGr (Ctrl+Alt).
        if let KeyCode::Char(c) = key.code {
            let m = key.modifiers;
            let ctrl_alt = KeyModifiers::CONTROL | KeyModifiers::ALT;
            if !m.intersects(ctrl_alt) || m.contains(ctrl_alt) {
                let e = &mut self.editors[i];
                e.type_char(c);
                e.scroll_to_cursor();
            }
        }
    }

    pub(super) fn run_editor(&mut self, i: usize, cmd: EditorCmd) {
        let Outcome::App(cmd) = self.editors[i].command(cmd) else {
            return;
        };
        use EditorCmd::*;
        match cmd {
            Copy => {
                let e = &self.editors[i];
                // Far: without a block, the current line with its ending
                // (the cursor stays); a vertical block goes as a column.
                let (text, vertical) = match e.block_text() {
                    Some(b) => (b.text, b.vertical),
                    None => (e.line_with_eol(e.cursor.line), false),
                };
                if let Err(err) = self.clip_set_block(&text, vertical) {
                    self.say(err);
                }
            }
            Cut => {
                if let Some(b) = self.editors[i].block_text() {
                    match self.clip_set_block(&b.text, b.vertical) {
                        Ok(()) => {
                            self.editors[i].delete_selection();
                        }
                        Err(err) => self.say(err),
                    }
                }
            }
            Paste => {
                // A column (Far's mark on the clipboard) goes in as one.
                match self.clip_get_block() {
                    Some((text, true)) => self.editors[i].paste_vertical(&text),
                    Some((text, false)) => self.editors[i].insert_text(&text),
                    None => {}
                }
            }
            InsertFileName => {
                let name = self.editors[i].path().display().to_string();
                self.editors[i].insert_text(&name);
            }
            Save => self.editor_save(i, After::Stay),
            SaveAs => self.editor_save_as_dialog(i, After::Stay),
            SaveQuit => self.editor_save(i, After::Close),
            Quit => self.editor_leave(i, After::Close),
            View => self.editor_leave(i, After::View),
            OpenFile => self.editor_open_dialog(),
            GoFile => {
                let path = self.editors[i].path().to_path_buf();
                if let (Some(dir), Some(name)) = (path.parent(), path.file_name()) {
                    let side = self.active;
                    self.change_dir(side, dir);
                    self.panels[side].set_cursor_by_name(&name.to_string_lossy());
                }
            }
            StatusLine => self.editor_status = !self.editor_status,
            KeyBar => self.editor_keybar = !self.editor_keybar,
            UserScreen => self.viewer_peek = true,
            Search => self.editor_search_dialog(i, false),
            Replace => self.editor_search_dialog(i, true),
            SearchNext => self.editor_search_continue(i, false),
            SearchPrev => self.editor_search_continue(i, true),
            Goto => self.editor_goto_dialog(i),
            NextCodepage => self.editor_next_codepage(i),
            Settings => {
                let id = self.editors[i].id;
                self.editor_settings_dialog(Some(id));
            }
            InsertActiveName | InsertPassiveName | InsertLeftPath | InsertRightPath
            | InsertActivePath | InsertPassivePath => {
                // Far's MakePathForUI: names and folders, quoted when they
                // have spaces.
                let a = self.active;
                let folder = |side: usize| super::cmdline::folder_text(&self.panels[side].path);
                let text = match cmd {
                    InsertActiveName | InsertPassiveName => {
                        let side = if cmd == InsertActiveName { a } else { 1 - a };
                        self.panels[side].current().map(|e| super::quote(&e.name))
                    }
                    InsertLeftPath => Some(folder(0)),
                    InsertRightPath => Some(folder(1)),
                    InsertActivePath => Some(folder(a)),
                    _ => Some(folder(1 - a)),
                };
                if let Some(text) = text {
                    self.editors[i].insert_text(&text);
                }
            }
            CodepageMenu => self.editor_codepage_menu(i),
            _ => {}
        }
        if let Some(i) = self.shown_editor() {
            self.editors[i].scroll_to_cursor();
        }
    }

    // ------------------------------------------------------------ opening

    /// Shift+F4: Far's "Open/create file" dialog (`dlgOpenEditor`).
    pub(super) fn editor_open_dialog(&mut self) {
        let cps = codepage_list(0);
        let mut items = vec![Some(tr!("MDefaultCP")), Some(tr!("MEditOpenAutoDetect"))];
        items.extend(cps.iter().map(|cp| Some(codepage::long_name(*cp))));
        let dialog = Dialog::far(tr!("MEditTitle"), 76)
            .row(vec![text_at(5, tr!("MEditOpenCreateLabel"))])
            .row(vec![input_at(5, 66, "", Some("NewEdit")).use_last()])
            .separator()
            .row(vec![
                text_at(5, tr!("MEditCodePage")),
                combo_at(25, 46, items, 0),
            ])
            .separator()
            .buttons(&[&tr!("MOk"), &tr!("MCancel")], 0);
        self.overlays.push(Overlay::Dialog {
            dialog,
            purpose: Purpose::Editor(Ask::Open { cps }),
        });
    }

    /// A new file's name (Far's `MNewFileName`): the localized prefix and
    /// the time.
    fn new_file_name(&self) -> String {
        let pattern = tr!("MNewFileName");
        let prefix = pattern.split('{').next().unwrap_or("New_File_").to_string();
        let now = chrono::Local::now();
        format!("{prefix}{}.txt", now.format("%Y-%m-%d_%H.%M.%S.%3f"))
    }

    // ------------------------------------------------------------- saving

    /// F2: Far's checks in order (changed outside, read-only), then writes.
    pub(super) fn editor_save(&mut self, i: usize, then: After) {
        let e = &self.editors[i];
        let id = e.id;
        let path = e.path().to_path_buf();
        // A file not saved yet whose folder is gone, or a fresh name: Save as.
        if e.new_file
            && path
                .parent()
                .is_some_and(|d| !d.as_os_str().is_empty() && !d.exists())
        {
            self.editor_save_as_dialog(i, then);
            return;
        }
        if let Some(old) = e.stamp
            && let Some(now) = stamp(&path)
            && now != old
        {
            let dialog = Dialog::message(
                &tr!("MEditTitle"),
                &[tr!("MEditAskSaveExt")],
                &[&tr!("MHYes"), &tr!("MEditBtnSaveAs"), &tr!("MHCancel")],
                true,
            );
            self.overlays.push(Overlay::Dialog {
                dialog,
                purpose: Purpose::Editor(Ask::External { id, then }),
            });
            return;
        }
        self.editor_save_checked(i, then);
    }

    /// Far's warning before saving a file read with unreadable bytes.
    fn editor_save_checked(&mut self, i: usize, then: After) {
        let e = &self.editors[i];
        if e.bad_conversion.is_some() {
            let id = e.id;
            let dialog = Dialog::message(
                &tr!("MWarning"),
                &[tr!("MEditDataLostWarn"), tr!("MEditorSaveNotRecommended")],
                &[&tr!("MEditorSave"), &tr!("MCancel")],
                true,
            );
            self.overlays.push(Overlay::Dialog {
                dialog,
                purpose: Purpose::Editor(Ask::DataLost { id, then }),
            });
            return;
        }
        let path = e.path().to_path_buf();
        self.editor_write(i, path, then, false, None);
    }

    /// Writes the text to `path` (read-only asked about unless `force`);
    /// `fmt`: "Save as" settings, the window's after a successful write.
    fn editor_write(
        &mut self,
        i: usize,
        path: PathBuf,
        then: After,
        force: bool,
        fmt: Option<SaveFormat>,
    ) {
        let id = self.editors[i].id;
        let readonly = std::fs::metadata(&path).is_ok_and(|m| m.permissions().readonly());
        if readonly && !force {
            let dialog = Dialog::message(
                &tr!("MEditTitle"),
                &[path.display().to_string(), tr!("MEditRO"), tr!("MEditOvr")],
                &[&tr!("MYes"), &tr!("MNo")],
                true,
            );
            self.overlays.push(Overlay::Dialog {
                dialog,
                purpose: Purpose::Editor(Ask::ReadOnly {
                    id,
                    then,
                    path,
                    fmt,
                }),
            });
            return;
        }
        let e = &self.editors[i];
        let (cp, bom) = fmt.map_or((e.cp, e.bom), |f| (f.cp, f.bom));
        let converted: Vec<crate::editor::Line>;
        let lines = match fmt.and_then(|f| f.eol) {
            Some(eol) => {
                converted = e
                    .lines()
                    .iter()
                    .map(|l| {
                        let mut l = l.clone();
                        if l.eol != Eol::None {
                            l.eol = eol;
                        }
                        l
                    })
                    .collect();
                &converted[..]
            }
            None => e.lines(),
        };
        let data = match text::encode(lines, cp, bom) {
            Ok(d) => d,
            Err(c) => {
                let cp = codepage::long_name(cp);
                self.message(
                    &tr!("MEditTitle"),
                    &[tr!("editor-cannot-encode", ch = c.to_string(), cp = cp)],
                    true,
                );
                return;
            }
        };
        // The panels' watcher: this write and its temporary file are
        // afar's own.
        self.own_paths(u64::MAX - 1, &[path.clone(), text::temp_path(&path)]);
        let result = (|| -> std::io::Result<()> {
            if readonly {
                let mut p = std::fs::metadata(&path)?.permissions();
                #[allow(clippy::permissions_set_readonly_false)]
                p.set_readonly(false);
                std::fs::set_permissions(&path, p)?;
            }
            let r = text::write_file(&path, &data);
            if readonly && let Ok(m) = std::fs::metadata(&path) {
                let mut p = m.permissions();
                p.set_readonly(true);
                let _ = std::fs::set_permissions(&path, p);
            }
            r
        })();
        self.note_own_change(u64::MAX - 1);
        if let Err(err) = result {
            self.message(
                &tr!("MEditTitle"),
                &[
                    tr!("MEditCannotSave"),
                    path.display().to_string(),
                    err.to_string(),
                ],
                true,
            );
            return;
        }
        let e = &mut self.editors[i];
        if let Some(f) = fmt {
            e.cp = f.cp;
            e.bom = f.bom;
            if let Some(eol) = f.eol {
                e.set_all_eols(eol);
            }
        }
        e.set_path(&path);
        e.saved();
        e.stamp = stamp(&path);
        e.ignored_stamp = None;
        let codepage = codepage::long_name(e.cp);
        self.journal.push(
            crate::journal::Actor::User,
            crate::journal::Event::FileSaved {
                path: path.clone(),
                codepage,
            },
        );
        for p in &mut self.panels {
            p.reload(None);
        }
        let _ = id;
        self.editor_after(i, then);
    }

    fn editor_after(&mut self, i: usize, then: After) {
        match then {
            After::Stay => {}
            After::Close => self.close_editor(i),
            After::View => {
                let e = &self.editors[i];
                let (path, top, cp) = (e.path().to_path_buf(), e.top, e.cp);
                self.close_editor(i);
                // Far: the viewer gets the editor's code page and place.
                if let Some(id) = self.open_viewer(&path, vec![path.clone()])
                    && let Some(v) = self.viewers.iter_mut().find(|v| v.id == id)
                {
                    v.set_codepage(cp);
                    v.show_line(top as u64 + 1 + 3);
                }
            }
        }
    }

    /// Shift+F2: Far's "Save file as" (`dlgSaveFileAs`).
    pub(super) fn editor_save_as_dialog(&mut self, i: usize, then: After) {
        let e = &self.editors[i];
        let path = e.path();
        let dir = &self.panels[self.active].path;
        let name = if path.parent() == Some(dir.as_path()) {
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            path.display().to_string()
        };
        let cps = codepage_list(e.cp);
        let selected = cps.iter().position(|cp| *cp == e.cp).unwrap_or(0);
        let items: Vec<Option<String>> = cps
            .iter()
            .map(|cp| Some(codepage::long_name(*cp)))
            .collect();
        let utf = matches!(e.cp, codepage::UTF8 | codepage::UTF16LE | codepage::UTF16BE);
        let dialog = Dialog::far(tr!("MEditTitle"), 76)
            .row(vec![text_at(5, tr!("MEditSaveAs"))])
            .row(vec![input_at(5, 66, name, Some("NewEdit"))])
            .separator()
            .row(vec![
                text_at(5, tr!("MEditCodePage")),
                combo_at(25, 46, items, selected),
            ])
            .row(vec![check_at(5, tr!("MEditAddSignature"), utf && e.bom)])
            .separator()
            .row(vec![text_at(5, tr!("MEditSaveAsFormatTitle"))])
            .radios(
                &[
                    &tr!("MEditSaveOriginal"),
                    &tr!("MEditSaveDOS"),
                    &tr!("MEditSaveUnix"),
                    &tr!("MEditSaveMac"),
                ],
                0,
            )
            .separator()
            .buttons(&[&tr!("MEditorSave"), &tr!("MCancel")], 0);
        let id = e.id;
        self.overlays.push(Overlay::Dialog {
            dialog,
            purpose: Purpose::Editor(Ask::SaveAs { id, then, cps }),
        });
    }

    /// F4 / F10 / Esc / F6: Far's questions before leaving.
    pub(super) fn editor_leave(&mut self, i: usize, then: After) {
        let e = &self.editors[i];
        let id = e.id;
        if !e.new_file && !e.path().exists() {
            let line1 = if e.modified() {
                tr!("MEditSavedChangedNonFile")
            } else {
                tr!("MEditSavedChangedNonFile1")
            };
            let dialog = Dialog::message(
                &tr!("MEditTitle"),
                &[line1, tr!("MEditSavedChangedNonFile2")],
                &[&tr!("MHYes"), &tr!("MHNo"), &tr!("MHCancel")],
                true,
            );
            self.overlays.push(Overlay::Dialog {
                dialog,
                purpose: Purpose::Editor(Ask::Deleted { id, then }),
            });
            return;
        }
        // F6 on a new file: saved without asking (there is nothing to view
        // otherwise).
        if then == After::View && e.new_file {
            self.editor_save(i, then);
            return;
        }
        if e.modified() {
            let dialog = Dialog::message(
                &tr!("MEditTitle"),
                &[tr!("MEditAskSave")],
                &[&tr!("MHYes"), &tr!("MHNo"), &tr!("MHCancel")],
                true,
            );
            self.overlays.push(Overlay::Dialog {
                dialog,
                purpose: Purpose::Editor(Ask::Save { id, then }),
            });
            return;
        }
        self.editor_after(i, then);
    }

    /// An editor dialog closed with `button` (`None`: Esc).
    pub(super) fn editor_dialog_closed(
        &mut self,
        ask: Ask,
        button: Option<usize>,
        dialog: &Dialog,
    ) {
        match ask {
            Ask::Open { cps } => {
                if button != Some(0) {
                    return;
                }
                let name = dialog.input_value(0).trim().trim_matches('"').to_string();
                let cp = match dialog.combo(0) {
                    0 | 1 => None,
                    k => cps.get(k - 2).copied(),
                };
                let base = self.panels[self.active].path.clone();
                let name = if name.is_empty() {
                    self.new_file_name()
                } else {
                    crate::complete::expand_env(&name)
                };
                let path = base.join(name);
                if let Some(dir) = path.parent()
                    && !dir.exists()
                {
                    let dialog = Dialog::message(
                        &tr!("MWarning"),
                        &[
                            tr!("MEditNewPath1"),
                            tr!("MEditNewPath2"),
                            tr!("MEditNewPath3"),
                        ],
                        &[&tr!("MHYes"), &tr!("MHNo")],
                        true,
                    );
                    self.overlays.push(Overlay::Dialog {
                        dialog,
                        purpose: Purpose::Editor(Ask::NewPath { path, cp }),
                    });
                    return;
                }
                self.edit_file(&path, cp);
            }
            Ask::NewPath { path, cp } => {
                if button == Some(0)
                    && let Some(id) = self.open_editor(&path, cp, None)
                {
                    self.journal_user_edit(&path);
                    self.editor_user_opened(id);
                }
            }
            Ask::Large { path, cp } => match button {
                Some(0) => {
                    if let Some(id) = self.open_editor(&path, cp, None) {
                        self.journal_user_edit(&path);
                        self.editor_user_opened(id);
                    }
                }
                Some(1) => {
                    self.record_view(&path);
                    self.open_viewer(&path, vec![path.clone()]);
                }
                _ => {}
            },
            Ask::SaveAs { id, then, cps } => {
                let Some(i) = self.editor_index(id) else {
                    return;
                };
                if button != Some(0) {
                    return;
                }
                let name = dialog.input_value(0).trim().trim_matches('"').to_string();
                if name.is_empty() {
                    return;
                }
                let base = self.panels[self.active].path.clone();
                let path = base.join(crate::complete::expand_env(&name));
                let e = &self.editors[i];
                let cp = cps.get(dialog.combo(0)).copied().unwrap_or(e.cp);
                let utf = matches!(cp, codepage::UTF8 | codepage::UTF16LE | codepage::UTF16BE);
                let eol = match dialog.radio(0) {
                    1 => Some(Eol::CrLf),
                    2 => Some(Eol::Lf),
                    3 => Some(Eol::Cr),
                    _ => None,
                };
                let fmt = Some(SaveFormat {
                    cp,
                    bom: utf && dialog.checked(0),
                    eol,
                });
                let same = place_key(&path) == place_key(e.path());
                if !same && path.exists() {
                    let dialog = Dialog::message(
                        &tr!("MEditTitle"),
                        &[
                            path.display().to_string(),
                            tr!("MEditExists"),
                            tr!("MEditOvr"),
                        ],
                        &[&tr!("MYes"), &tr!("MNo")],
                        true,
                    );
                    self.overlays.push(Overlay::Dialog {
                        dialog,
                        purpose: Purpose::Editor(Ask::Overwrite {
                            id,
                            then,
                            path,
                            fmt,
                        }),
                    });
                    return;
                }
                // Another name: what the disk has there is not the old file's.
                if !same {
                    self.editors[i].stamp = None;
                }
                self.editor_write(i, path, then, false, fmt);
            }
            Ask::Overwrite {
                id,
                then,
                path,
                fmt,
            } => {
                if let Some(i) = self.editor_index(id)
                    && button == Some(0)
                {
                    self.editors[i].stamp = None;
                    self.editor_write(i, path, then, false, fmt);
                }
            }
            Ask::Save { id, then } | Ask::Deleted { id, then } => {
                let Some(i) = self.editor_index(id) else {
                    return;
                };
                match button {
                    Some(0) => self.editor_save(i, then),
                    Some(1) => self.editor_after(i, then),
                    _ => {}
                }
            }
            Ask::External { id, then } => {
                let Some(i) = self.editor_index(id) else {
                    return;
                };
                match button {
                    Some(0) => self.editor_save_checked(i, then),
                    Some(1) => self.editor_save_as_dialog(i, then),
                    _ => {}
                }
            }
            Ask::ReadOnly {
                id,
                then,
                path,
                fmt,
            } => {
                if let Some(i) = self.editor_index(id)
                    && button == Some(0)
                {
                    self.editor_write(i, path, then, true, fmt);
                }
            }
            Ask::Search { id, replace } => self.editor_search_closed(id, replace, button, dialog),
            Ask::Replace { run, found } => self.editor_replace_answer(run, found, button),
            Ask::Goto { id } => {
                if button == Some(0) {
                    self.editor_goto_closed(id, dialog);
                }
            }
            Ask::ReloadCp { id, cp } => {
                if button == Some(0)
                    && let Some(i) = self.editor_index(id)
                {
                    self.editor_reload_cp(i, cp);
                }
            }
            Ask::SwitchCp { id, cp, at } => self.editor_switch_cp_answer(id, cp, at, button),
            Ask::DataLost { id, then } => {
                if button == Some(0)
                    && let Some(i) = self.editor_index(id)
                {
                    let path = self.editors[i].path().to_path_buf();
                    self.editor_write(i, path, then, false, None);
                }
            }
            Ask::BadCp { id, cps } => {
                let Some(i) = self.editor_index(id) else {
                    return;
                };
                if button != Some(0) {
                    // Far does not open it.
                    self.editor_drop(i);
                    return;
                }
                let cp = cps
                    .get(dialog.combo(0))
                    .copied()
                    .unwrap_or(self.editors[i].cp);
                if cp != self.editors[i].cp {
                    self.editor_reload_cp(i, cp);
                    // Still unreadable in that page: asked again.
                    self.editor_user_opened(id);
                }
            }
            Ask::Reedit { id, path, cp } => match button {
                Some(0) => {
                    if self.editor_index(id).is_some() {
                        self.wm.switch_to(ScreenId::Editor(id));
                    }
                }
                Some(1) => {
                    if let Some(new) = self.open_editor_as(&path, cp, None, true) {
                        self.journal_user_edit(&path);
                        self.editor_user_opened(new);
                    }
                }
                Some(2) => {
                    if let Some(i) = self.editor_index(id) {
                        self.editor_drop(i);
                    }
                    if let Some(new) = self.open_editor(&path, cp, None) {
                        self.journal_user_edit(&path);
                        self.editor_user_opened(new);
                    }
                }
                _ => {}
            },
            Ask::Reload { id } => {
                let Some(i) = self.editor_index(id) else {
                    return;
                };
                let path = self.editors[i].path().to_path_buf();
                let outcome = if button == Some(0) {
                    self.editor_reload(i);
                    "the user read it again (the unsaved changes are gone)"
                } else {
                    self.editors[i].disk_changed = true;
                    "the user kept the buffer (saving will overwrite the disk's version)"
                };
                self.journal.push(
                    crate::journal::Actor::User,
                    crate::journal::Event::EditorDiskChanged {
                        path,
                        outcome: outcome.to_string(),
                    },
                );
            }
        }
    }

    // -------------------------------------------------------- the disk

    /// Reads the file again (keeping its code page and the cursor).
    fn editor_reload(&mut self, i: usize) {
        let cp = self.editors[i].cp;
        self.editor_reload_cp(i, cp);
    }

    /// The file read again from the disk, in code page `cp`.
    pub(super) fn editor_reload_cp(&mut self, i: usize, cp: u32) {
        let e = &self.editors[i];
        let path = e.path().to_path_buf();
        let Ok(data) = std::fs::read(&path) else {
            return;
        };
        let l = text::load(&data, Some(cp), false, cp);
        let e = &mut self.editors[i];
        e.reload(l.lines, l.cp, l.bom, l.eol.unwrap_or(e.default_eol));
        e.bad_conversion = l.bad;
        e.stamp = stamp(&path);
        e.ignored_stamp = None;
        e.scroll_to_cursor();
    }

    /// Once a second: the shown editor's file changed on the disk? Not
    /// modified here — read again; modified — asked once per change.
    pub(super) fn editor_tick(&mut self) {
        let Some(i) = self.shown_editor() else { return };
        if self.has_overlay() {
            return;
        }
        let e = &self.editors[i];
        let (Some(old), Some(now)) = (e.stamp, stamp(e.path())) else {
            return;
        };
        if now == old || e.ignored_stamp == Some(now) {
            return;
        }
        if !e.modified() {
            let path = e.path().to_path_buf();
            self.editor_reload(i);
            self.say(tr!("editor-reloaded", path = path.display().to_string()));
            self.journal.push(
                crate::journal::Actor::External,
                crate::journal::Event::EditorDiskChanged {
                    path,
                    outcome: "the buffer had no unsaved changes and was read again".to_string(),
                },
            );
            return;
        }
        let id = e.id;
        let lines = vec![
            e.path().display().to_string(),
            tr!("editor-changed-on-disk"),
        ];
        self.editors[i].ignored_stamp = Some(now);
        self.editors[i].disk_changed = true;
        let dialog = Dialog::message(
            &tr!("MEditTitle"),
            &lines,
            &[&tr!("editor-reload"), &tr!("editor-keep-mine")],
            true,
        );
        self.overlays.push(Overlay::Dialog {
            dialog,
            purpose: Purpose::Editor(Ask::Reload { id }),
        });
    }

    // ------------------------------------------------------------ drawing

    /// The editor screen: Far's status line on top (Ctrl+Shift+B), the
    /// text below. Returns the cursor's cell.
    pub(super) fn draw_editor(
        &mut self,
        i: usize,
        area: Rect,
        buf: &mut Buffer,
        clock: bool,
    ) -> Option<Position> {
        let status = self.editor_status && area.height > 1;
        let text_area = if status {
            Rect::new(area.x, area.y + 1, area.width, area.height - 1)
        } else {
            area
        };
        let cursor = self.editors[i].draw(text_area, buf);
        if status {
            let row = Rect::new(area.x, area.y, area.width, 1);
            let line = self.editor_status_line(i, row.width, clock);
            buf.set_style(row, theme::EDITOR_STATUS);
            buf.set_stringn(
                row.x,
                row.y,
                &line,
                usize::from(row.width),
                theme::EDITOR_STATUS,
            );
        }
        cursor
    }

    /// Far's status line (`FileEditor::ShowStatus`): the name, then
    /// `│*-│cp│Стр n/N│Кол c│С k│RHS│code`.
    fn editor_status_line(&self, i: usize, width: u16, clock: bool) -> String {
        let e = &self.editors[i];
        let lines = e.line_count();
        let line = e.cursor.line + 1;
        let total = format!("{lines}/{lines}").chars().count();
        let pos = format!("{line}/{lines}");
        let vcol = e.vcol(e.cursor.line, e.cursor.col) + 1;
        let short = |id: &str| tr!(id).chars().take(3).collect::<String>();
        let modified = if e.modified() { '*' } else { ' ' };
        let mode = if e.locked {
            '-'
        } else if e.quote_next {
            '"'
        } else {
            ' '
        };
        let cp: String = format!("{:<5}", codepage::short_name(e.cp))
            .chars()
            .take(5)
            .collect();
        let attrs = std::fs::metadata(e.path())
            .ok()
            .map(|m| {
                let mut s = String::new();
                if m.permissions().readonly() {
                    s.push('R');
                }
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt as _;
                    let a = m.file_attributes();
                    if a & 0x2 != 0 {
                        s.push('H');
                    }
                    if a & 0x4 != 0 {
                        s.push('S');
                    }
                }
                s
            })
            .unwrap_or_default();
        let attrs = if attrs.is_empty() {
            String::new()
        } else {
            format!("│{attrs}")
        };
        let code = e
            .lines()
            .get(e.cursor.line)
            .and_then(|l| l.text.chars().nth(e.cursor.col))
            .map(|c| format!("{:<5}", c as u32))
            .unwrap_or_else(|| " ".repeat(5));
        let tail = format!(
            "│{modified}{mode}│{cp}│{} {pos:>total$}│{} {vcol:<3}│{} {:<3}{attrs}│{code}",
            short("MEditStatusLine"),
            short("MEditStatusCol"),
            short("MEditStatusChar"),
            e.cursor.col + 1,
        );
        let reserve = if clock { 6 } else { 0 };
        let room = usize::from(width).saturating_sub(tail.chars().count() + reserve + 1);
        let name = e.path().display().to_string();
        let n = name.chars().count();
        let name = if n > room {
            let keep: String = name.chars().skip(n - room.saturating_sub(1)).collect();
            format!("…{keep}")
        } else {
            name
        };
        format!("{name:<room$} {tail}")
    }

    /// Far's editor key bar (`MEditF1…`), keys not made yet left blank.
    pub(super) fn editor_keybar_labels(&self, i: usize, group: &str) -> Vec<String> {
        let t = |id: &str| crate::i18n::plain(&tr!(id));
        let not_yet: &[(&str, u8)] = &[("", 11), ("Alt", 9), ("Alt", 11)];
        (1..=12u8)
            .map(|n| {
                if not_yet.contains(&(group, n)) {
                    return String::new();
                }
                if group == "Ctrl" && n == 3 && self.editors[i].line_numbers {
                    return t("MEditCtrlF3Hide");
                }
                // F8: the page it goes to (Far: "ANSI" / "OEM").
                if group.is_empty() && n == 8 {
                    let next = super::editcp::next_f8(self.editors[i].cp);
                    return if next == codepage::ansi() {
                        t("MEditF8")
                    } else {
                        t("MEditF8DOS")
                    };
                }
                t(&format!("MEdit{group}F{n}"))
            })
            .collect()
    }

    // -------------------------------------------------------------- mouse

    pub(super) fn editor_mouse(&mut self, i: usize, ev: &MouseEvent, wheel: i32) {
        let e = &mut self.editors[i];
        match ev.kind {
            MouseEventKind::ScrollUp => e.wheel(-(wheel as isize)),
            MouseEventKind::ScrollDown => e.wheel(wheel as isize),
            MouseEventKind::Down(MouseButton::Left) => {
                let now = Instant::now();
                let double = self.editor_last_click.is_some_and(|(t, x, y)| {
                    now.duration_since(t) < Duration::from_millis(500)
                        && x == ev.column
                        && y == ev.row
                });
                if e.scrollbar_press(ev.column, ev.row) {
                    self.editor_last_click = None;
                } else if double {
                    e.select_word(ev.column, ev.row);
                    self.editor_last_click = None;
                } else {
                    let shift = ev.modifiers.contains(KeyModifiers::SHIFT);
                    e.click(ev.column, ev.row, shift);
                    self.editor_last_click = Some((now, ev.column, ev.row));
                }
                self.focus = Focus::Panels;
            }
            MouseEventKind::Drag(MouseButton::Left) if e.dragging_bar => e.thumb_to(ev.row),
            MouseEventKind::Drag(MouseButton::Left) => e.drag(ev.column, ev.row),
            MouseEventKind::Up(_) => e.dragging_bar = false,
            MouseEventKind::Down(_) => self.focus = Focus::Panels,
            _ => {}
        }
    }

    /// Development mode: the open editors for the next instance.
    pub(super) fn editor_states(&self) -> (Vec<crate::dev::EditorState>, Option<usize>) {
        let states = self
            .editors
            .iter()
            .filter(|e| !e.new_file)
            .map(|e| crate::dev::EditorState {
                path: e.path().to_path_buf(),
                cp: e.cp,
                line: e.cursor.line,
                col: e.cursor.col,
                top: e.top,
                left: e.left,
                line_numbers: e.line_numbers,
                bookmarks: e.bookmarks,
            })
            .collect();
        let shown = self.shown_editor().and_then(|i| {
            self.editors[..=i]
                .iter()
                .filter(|e| !e.new_file)
                .count()
                .checked_sub(1)
        });
        (states, shown)
    }

    pub(super) fn restore_editors(
        &mut self,
        states: Vec<crate::dev::EditorState>,
        shown: Option<usize>,
    ) {
        let screen = self.wm.current_screen();
        let mut shown_id = None;
        for (n, s) in states.into_iter().enumerate() {
            let Some(id) = self.open_editor(&s.path, Some(s.cp), None) else {
                continue;
            };
            if let Some(e) = self.editors.iter_mut().find(|e| e.id == id) {
                let last = e.line_count() - 1;
                e.cursor = Pos::new(s.line.min(last), s.col);
                e.top = s.top.min(last);
                e.left = s.left;
                e.line_numbers = s.line_numbers;
                e.bookmarks = s.bookmarks;
            }
            if shown == Some(n) {
                shown_id = Some(id);
            }
        }
        self.wm.switch_to(match shown_id {
            Some(id) => ScreenId::Editor(id),
            None => screen,
        });
    }

    /// Development mode: a modified editor keeps a restart waiting.
    pub(super) fn editors_modified(&self) -> bool {
        self.editors.iter().any(Editor::modified)
    }
}
