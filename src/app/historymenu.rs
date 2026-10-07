//! Far's history menus (docs/14 §7, docs/12 §10): commands (Alt+F8),
//! files viewed (Alt+F11), folders (Alt+F12) — the oldest at the top, the
//! cursor on the newest, a dated separator before each day's first entry,
//! locked entries checked. Enter acts (runs, opens, goes), Ctrl+Enter puts
//! the text into the command line; Ins locks, Shift+Del deletes, Del
//! clears (asks), Ctrl+C copies; F3 — a command's details, Ctrl+R — the
//! folders that are gone.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::App;
use super::Focus;
use super::fileops::{Overlay, Purpose};
use super::panelcmds::MenuPurpose;
use crate::dialog::Dialog;
use crate::history::{Entry, Kind};
use crate::menu::{Item, Menu};
use crate::tr;

/// Which history a menu shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HistoryMenu {
    Commands,
    Views,
    Folders,
}

impl HistoryMenu {
    fn kind(self) -> Kind {
        match self {
            HistoryMenu::Commands => Kind::Command,
            HistoryMenu::Views => Kind::View,
            HistoryMenu::Folders => Kind::Folder,
        }
    }

    fn title(self) -> String {
        match self {
            HistoryMenu::Commands => tr!("MHistoryTitle"),
            HistoryMenu::Views => tr!("MViewHistoryTitle"),
            HistoryMenu::Folders => tr!("MFolderHistoryTitle"),
        }
    }
}

/// A view entry's text: Far's "View: path" / "Edit: path" / "Ext.: path".
fn view_label(e: &Entry) -> String {
    let ty = e
        .data
        .as_deref()
        .and_then(|d| serde_json::from_str::<serde_json::Value>(d).ok())
        .and_then(|v| v.get("far_type").and_then(|t| t.as_i64()))
        .unwrap_or(0);
    let (what, sep) = match ty {
        1 => (tr!("MHistoryEdit"), ':'),
        4 => (tr!("MHistoryEdit"), '-'),
        2 | 3 => (tr!("MHistoryExt"), ':'),
        _ => (tr!("MHistoryView"), ':'),
    };
    format!("{what}{sep} {}", e.text)
}

/// The local date and time of a time in milliseconds.
fn local_time(ms: i64) -> String {
    use chrono::TimeZone as _;
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format("%d.%m.%Y %H:%M:%S").to_string())
        .unwrap_or_default()
}

/// The local date of a time in milliseconds.
fn day_of(ms: i64) -> String {
    use chrono::TimeZone as _;
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format("%d.%m.%Y").to_string())
        .unwrap_or_default()
}

impl App {
    /// Opens a history menu, the cursor on `select` (an entry's text) or
    /// on the newest entry.
    pub(super) fn history_menu(&mut self, which: HistoryMenu, select: Option<&str>) {
        let mut entries = self.store.list(which.kind(), "");
        if entries.is_empty() {
            return;
        }
        entries.sort_by_key(|e| (e.last_used, e.id));
        let mut items = Vec::new();
        let mut rows = Vec::new();
        let mut day = String::new();
        for (i, e) in entries.iter().enumerate() {
            let d = day_of(e.last_used);
            if d != day {
                items.push(Item::titled_separator(d.clone()));
                rows.push(None);
                day = d;
            }
            let text = match which {
                HistoryMenu::Views => view_label(e),
                _ => e.text.clone(),
            };
            items.push(Item::new(text.replace('&', "&&")).checked(e.locked.then_some('√')));
            rows.push(Some(i));
        }
        let at = select
            .and_then(|t| entries.iter().position(|e| e.text == t))
            .unwrap_or(entries.len() - 1);
        let row = rows.iter().position(|r| *r == Some(at)).unwrap_or(0);
        // The three menus look alike: as wide as the longest entry of any of
        // them, a row free above and below.
        let widest = [
            HistoryMenu::Commands,
            HistoryMenu::Views,
            HistoryMenu::Folders,
        ]
        .into_iter()
        .flat_map(|w| {
            self.store
                .list(w.kind(), "")
                .into_iter()
                .map(move |e| match w {
                    HistoryMenu::Views => view_label(&e),
                    _ => e.text,
                })
        })
        .map(|t| t.chars().count())
        .max()
        .unwrap_or(0);
        let titles = [
            HistoryMenu::Commands,
            HistoryMenu::Views,
            HistoryMenu::Folders,
        ]
        .into_iter()
        .map(|w| w.title().chars().count())
        .max()
        .unwrap_or(0);
        let menu = Menu::new(which.title(), items)
            .select(row)
            .min_text_width((widest + 3).max(titles + 1))
            .margin_rows(1)
            .margin_cols(5);
        self.overlays.push(Overlay::Menu {
            menu,
            purpose: MenuPurpose::History {
                which,
                entries,
                rows,
            },
        });
    }

    /// Keys of a history menu before the menu's own; `true`: handled.
    pub(super) fn history_menu_key(&mut self, key: &KeyEvent) -> bool {
        let Some(Overlay::Menu {
            menu,
            purpose:
                MenuPurpose::History {
                    which,
                    entries,
                    rows,
                },
        }) = self.overlays.last()
        else {
            return false;
        };
        let which = *which;
        let Some(entry) = rows
            .get(menu.selected)
            .copied()
            .flatten()
            .and_then(|i| entries.get(i))
            .cloned()
        else {
            return false;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let kind = which.kind();
        match key.code {
            KeyCode::Enter if ctrl && shift && which == HistoryMenu::Folders => {
                self.overlays.pop();
                let side = 1 - self.active;
                self.change_dir(side, std::path::Path::new(&entry.text));
            }
            KeyCode::Enter if ctrl => {
                self.overlays.pop();
                self.focus = Focus::Panels;
                let text = if which == HistoryMenu::Commands || !entry.text.contains(' ') {
                    entry.text.clone()
                } else {
                    format!("\"{}\"", entry.text)
                };
                self.cmdline_insert(&text);
            }
            KeyCode::Enter => {
                self.overlays.pop();
                self.history_menu_open(which, &entry);
            }
            KeyCode::Insert if !ctrl && !shift => {
                self.store.set_locked(kind, "", &entry.text, !entry.locked);
                self.overlays.pop();
                self.history_menu(which, Some(&entry.text));
            }
            KeyCode::Delete if shift => {
                if !entry.locked {
                    self.store.delete(kind, "", &entry.text);
                }
                self.overlays.pop();
                self.history_menu(which, None);
            }
            KeyCode::Delete => {
                self.overlays.pop();
                let dialog = Dialog::message(
                    &tr!("MHistoryTitle"),
                    &[tr!("MHistoryClear")],
                    &[&tr!("MClear"), &tr!("MCancel")],
                    true,
                );
                self.overlays.push(Overlay::Dialog {
                    dialog,
                    purpose: Purpose::HistoryMenuClear { which },
                });
            }
            KeyCode::Char('c') | KeyCode::Insert if ctrl => {
                if let Err(e) = crate::clipboard::set_text(&entry.text) {
                    self.say(e);
                }
            }
            KeyCode::F(3) if which == HistoryMenu::Commands => self.command_info(&entry),
            KeyCode::F(3) | KeyCode::F(4) if which == HistoryMenu::Views => {
                self.overlays.pop();
                self.focus = Focus::Panels;
                self.history_open_file(&entry, Some(key.code == KeyCode::F(4)));
            }
            KeyCode::Char('r') if ctrl && which == HistoryMenu::Folders => {
                for e in entries_of(&self.store, kind) {
                    if !e.locked && !std::path::Path::new(&e.text).is_dir() && !is_network(&e.text)
                    {
                        self.store.delete(kind, "", &e.text);
                    }
                }
                self.overlays.pop();
                self.history_menu(which, Some(&entry.text));
            }
            _ => return false,
        }
        true
    }

    /// Enter on an entry: run the command, open the file, go to the folder.
    pub(super) fn history_menu_open(&mut self, which: HistoryMenu, entry: &Entry) {
        self.focus = Focus::Panels;
        match which {
            HistoryMenu::Commands => {
                self.clear_cmdline();
                self.execute(entry.text.clone());
            }
            HistoryMenu::Folders => {
                let side = self.active;
                self.change_dir(side, std::path::Path::new(&entry.text));
            }
            HistoryMenu::Views => self.history_open_file(entry, None),
        }
    }

    /// A file of the view history: in the viewer or the editor (`edit`;
    /// `None`: as it was last opened — Far's Enter).
    fn history_open_file(&mut self, entry: &Entry, edit: Option<bool>) {
        let path = std::path::PathBuf::from(&entry.text);
        let edited = entry
            .data
            .as_deref()
            .and_then(|d| serde_json::from_str::<serde_json::Value>(d).ok())
            .and_then(|v| v.get("far_type").and_then(|t| t.as_i64()))
            .is_some_and(|t| t == 1 || t == 4);
        if edit.unwrap_or(edited) {
            self.edit_file(&path, None);
        } else if path.is_file() {
            self.record_view(&path);
            self.open_viewer(&path, vec![path.clone()]);
        } else {
            self.say(tr!("link-not-found", path = entry.text.as_str()));
        }
    }

    /// F3 on a command: where it ran, and (afar's) its exit code and time.
    fn command_info(&mut self, entry: &Entry) {
        let mut lines = vec![format!("{} {}", tr!("MHistoryInfoFolder"), entry.folder)];
        if let Some(v) = entry
            .data
            .as_deref()
            .and_then(|d| serde_json::from_str::<serde_json::Value>(d).ok())
        {
            if let Some(code) = v.get("exit").and_then(|c| c.as_i64()) {
                lines.push(tr!("history-info-exit", code = code));
            }
            if let Some(ms) = v.get("ms").and_then(|c| c.as_i64()) {
                lines.push(tr!(
                    "history-info-time",
                    seconds = format!("{:.1}", ms as f64 / 1000.0)
                ));
            }
        }
        lines.push(tr!("history-info-used", when = local_time(entry.last_used)));
        let dialog = Dialog::message(&tr!("MHistoryInfoTitle"), &lines, &[&tr!("MOk")], false);
        self.overlays.push(Overlay::Dialog {
            dialog,
            purpose: Purpose::Message,
        });
    }

    /// A file opened in the viewer goes into the views history (Alt+F11).
    pub(super) fn record_view(&mut self, path: &std::path::Path) {
        let folder = self.panels[self.active].path.display().to_string();
        let text = path.display().to_string();
        self.store.add(Kind::View, "", &text, &folder, "user");
        self.store
            .set_data(Kind::View, "", &text, "{\"far_type\":0}");
        self.journal.push(
            crate::journal::Actor::User,
            crate::journal::Event::FileViewed {
                path: path.to_path_buf(),
            },
        );
    }
}

fn entries_of(store: &crate::history::History, kind: Kind) -> Vec<Entry> {
    store.list(kind, "")
}

/// A network path: not checked (it may be slow or offline).
fn is_network(path: &str) -> bool {
    path.starts_with("\\\\") || path.starts_with("//")
}
