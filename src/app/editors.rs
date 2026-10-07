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
    ReadOnly { id: u32, then: After, path: PathBuf },
    /// "Save as" onto another existing file.
    Overwrite { id: u32, then: After, path: PathBuf },
    /// The file (or its folder) is gone: save?
    Deleted { id: u32, then: After },
    /// The file changed on the disk while edited (afar's): read it again?
    Reload { id: u32 },
}

/// Where a file was left (Far's editor position cache).
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct EditorPlace {
    pub line: usize,
    pub col: usize,
    pub top: usize,
    pub left: usize,
    pub cp: u32,
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

    fn editor_index(&self, id: u32) -> Option<usize> {
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
        self.open_editor(path, cp, None);
    }

    /// Opens an editor screen (or shows the one already open on the file);
    /// `line`: where to put the cursor (from 0).
    pub(super) fn open_editor(
        &mut self,
        path: &Path,
        cp: Option<u32>,
        line: Option<usize>,
    ) -> Option<u32> {
        if path.is_dir() {
            self.message(&tr!("MEditTitle"), &[tr!("MEditCanNotEditDirectory")], true);
            return None;
        }
        let key = place_key(path);
        if let Some(e) = self.editors.iter_mut().find(|e| place_key(e.path()) == key) {
            let id = e.id;
            if let Some(l) = line {
                e.cursor = Pos::new(l.min(e.line_count() - 1), 0);
                e.top = e.cursor.line.saturating_sub(3);
            }
            self.wm.switch_to(ScreenId::Editor(id));
            return Some(id);
        }
        let remembered = self.editor_places.get(path).cloned();
        let default_cp = match self.config.viewer.default_codepage {
            0 => codepage::ansi(),
            cp => cp,
        };
        let id = self.next_editor_id;
        let mut editor = match std::fs::read(path) {
            Ok(data) => {
                let want = cp.or(remembered.as_ref().map(|r| r.cp).filter(|cp| *cp != 0));
                let l = text::load(
                    &data,
                    want,
                    self.config.viewer.autodetect_codepage,
                    default_cp,
                );
                let mut e = Editor::new(id, path, l.lines, l.cp, l.bom, l.eol.unwrap_or(Eol::CrLf));
                e.stamp = stamp(path);
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
        if let Some(r) = remembered.filter(|_| line.is_none()) {
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
            cp: e.cp,
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
                let e = &mut self.editors[i];
                // Far: without a block, the current line is copied (with
                // its ending) and stays selected.
                if e.selection().is_none() {
                    e.select_line();
                }
                if let Some(text) = e.selected_text()
                    && let Err(err) = crate::clipboard::set_text(&text)
                {
                    self.say(err);
                }
            }
            Cut => {
                if let Some(text) = self.editors[i].selected_text() {
                    match crate::clipboard::set_text(&text) {
                        Ok(()) => {
                            self.editors[i].delete_selection();
                        }
                        Err(err) => self.say(err),
                    }
                }
            }
            Paste => {
                if let Some(text) = crate::clipboard::get_text() {
                    self.editors[i].insert_text(&text);
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
            _ => {}
        }
        if let Some(i) = self.shown_editor() {
            self.editors[i].scroll_to_cursor();
        }
    }

    // ------------------------------------------------------------ opening

    /// Shift+F4: Far's "Open/create file" dialog (`dlgOpenEditor`).
    pub(super) fn editor_open_dialog(&mut self) {
        let cps = codepage::installed();
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
        self.editor_write(i, path, then, false);
    }

    /// Writes the text to `path` (read-only asked about unless `force`).
    fn editor_write(&mut self, i: usize, path: PathBuf, then: After, force: bool) {
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
                purpose: Purpose::Editor(Ask::ReadOnly { id, then, path }),
            });
            return;
        }
        let e = &self.editors[i];
        let data = match text::encode(e.lines(), e.cp, e.bom) {
            Ok(d) => d,
            Err(c) => {
                let cp = codepage::long_name(e.cp);
                self.message(
                    &tr!("MEditTitle"),
                    &[tr!("editor-cannot-encode", ch = c.to_string(), cp = cp)],
                    true,
                );
                return;
            }
        };
        // The panels' watcher: this write is afar's own.
        self.own_paths(u64::MAX - 1, std::slice::from_ref(&path));
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
        e.set_path(&path);
        e.saved();
        e.stamp = stamp(&path);
        e.ignored_stamp = None;
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
        let cps = codepage::installed();
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
                if button == Some(0) {
                    self.open_editor(&path, cp, None);
                }
            }
            Ask::Large { path, cp } => match button {
                Some(0) => {
                    self.open_editor(&path, cp, None);
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
                let e = &mut self.editors[i];
                if let Some(cp) = cps.get(dialog.combo(0)) {
                    e.cp = *cp;
                }
                let utf = matches!(e.cp, codepage::UTF8 | codepage::UTF16LE | codepage::UTF16BE);
                e.bom = utf && dialog.checked(0);
                let eol = match dialog.radio(0) {
                    1 => Some(Eol::CrLf),
                    2 => Some(Eol::Lf),
                    3 => Some(Eol::Cr),
                    _ => None,
                };
                if let Some(eol) = eol {
                    e.set_all_eols(eol);
                }
                let same = place_key(&path) == place_key(e.path());
                e.new_file = false;
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
                        purpose: Purpose::Editor(Ask::Overwrite { id, then, path }),
                    });
                    return;
                }
                // Another name: what the disk has there is not the old file's.
                if !same {
                    self.editors[i].stamp = None;
                }
                self.editor_write(i, path, then, false);
            }
            Ask::Overwrite { id, then, path } => {
                if let Some(i) = self.editor_index(id)
                    && button == Some(0)
                {
                    self.editors[i].stamp = None;
                    self.editor_write(i, path, then, false);
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
                    Some(0) => {
                        let path = self.editors[i].path().to_path_buf();
                        self.editor_write(i, path, then, false);
                    }
                    Some(1) => self.editor_save_as_dialog(i, then),
                    _ => {}
                }
            }
            Ask::ReadOnly { id, then, path } => {
                if let Some(i) = self.editor_index(id)
                    && button == Some(0)
                {
                    self.editor_write(i, path, then, true);
                }
            }
            Ask::Reload { id } => {
                if let Some(i) = self.editor_index(id)
                    && button == Some(0)
                {
                    self.editor_reload(i);
                }
            }
        }
    }

    // -------------------------------------------------------- the disk

    /// Reads the file again (keeping its code page and the cursor).
    fn editor_reload(&mut self, i: usize) {
        let e = &self.editors[i];
        let path = e.path().to_path_buf();
        let Ok(data) = std::fs::read(&path) else {
            return;
        };
        let l = text::load(&data, Some(e.cp), false, e.cp);
        let e = &mut self.editors[i];
        e.reload(l.lines, l.cp, l.bom, l.eol.unwrap_or(e.default_eol));
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
            let name = e.path().display().to_string();
            self.editor_reload(i);
            self.say(tr!("editor-reloaded", path = name));
            return;
        }
        let id = e.id;
        let lines = vec![
            e.path().display().to_string(),
            tr!("editor-changed-on-disk"),
        ];
        self.editors[i].ignored_stamp = Some(now);
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
        let not_yet: &[(&str, u8)] = &[
            ("", 7),
            ("", 8),
            ("", 11),
            ("Shift", 7),
            ("Shift", 8),
            ("Alt", 7),
            ("Alt", 8),
            ("Alt", 9),
            ("Alt", 11),
            ("Ctrl", 7),
            ("AltShift", 9),
        ];
        (1..=12u8)
            .map(|n| {
                if not_yet.contains(&(group, n)) {
                    return String::new();
                }
                if group == "Ctrl" && n == 3 && self.editors[i].line_numbers {
                    return t("MEditCtrlF3Hide");
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
                if double {
                    e.select_word(ev.column, ev.row);
                    self.editor_last_click = None;
                } else {
                    let shift = ev.modifiers.contains(KeyModifiers::SHIFT);
                    e.click(ev.column, ev.row, shift);
                    self.editor_last_click = Some((now, ev.column, ev.row));
                }
                self.focus = Focus::Panels;
            }
            MouseEventKind::Drag(MouseButton::Left) => e.drag(ev.column, ev.row),
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
