//! File operations in the UI, laid out like Far's (docs/09-far-ui-reference.md):
//! the copy/move (F5/F6), make-folder (F7) and delete (F8) dialogs, the
//! "file already exists" warning, error messages and progress windows.
//! Dialogs live in the overlay: modal for input, while the agent pane and
//! background work keep going.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{KeyCode, KeyEvent, MouseEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use tokio::sync::oneshot;

use super::policy::AgentAction;
use super::{App, AppMsg, Focus};
use crate::config::Level;
use crate::dialog::{
    Button, Dialog, Outcome, check_at, combo_at, input_at, radio_at, text_at, visible, wrap,
};
use crate::journal::{Actor, Event};
use crate::mcp::Reply;
use crate::ops::{
    self, ConfirmAnswer, ConflictAction, ConflictAnswer, CopyJob, DeleteMode, ErrorAnswer,
    ErrorContext, FileInfo, OpControl, OpId, OpKind, OpMsg, OpReport, Overwrite, Question,
};
use crate::panel::{group_thousands, size_float};
use crate::tr;

/// Longest path lists kept in journal entries.
const LIST_LIMIT: usize = 20;
/// Far's standard dialog width (copy, make folder, progress).
const FAR_WIDTH: u16 = 76;
/// Text width inside it.
const FAR_TEXT: usize = 66;

/// "Already existing files" in the copy dialog, in Far's order.
const OVERWRITE_CHOICES: [(&str, Overwrite); 6] = [
    ("MCopyAsk", Overwrite::Ask),
    ("MCopyOverwrite", Overwrite::Replace),
    ("MCopySkip", Overwrite::Skip),
    ("MCopyRename", Overwrite::Rename),
    ("MCopyAppend", Overwrite::Append),
    ("MCopyOnlyNewerFiles", Overwrite::ReplaceIfNewer),
];

pub(super) enum Overlay {
    Dialog {
        dialog: Dialog,
        purpose: Purpose,
    },
    Progress(Progress),
    Menu {
        menu: crate::menu::Menu,
        purpose: super::panelcmds::MenuPurpose,
    },
    /// F9.
    MenuBar(crate::menubar::MenuBar<super::mainmenu::MainAction>),
    /// F9 on the agent pane.
    AgentMenu(crate::menubar::MenuBar<super::agentmenu::AgentAction>),
    /// Alt+F7's results.
    Find(Box<super::findfiles::FindView>),
}

/// What a dialog was opened for, i.e. what to do when it closes.
pub(super) enum Purpose {
    MkDir {
        side: usize,
        actor: Actor,
        /// The agent's request, answered when the dialog closes.
        reply: Option<oneshot::Sender<Reply>>,
    },
    Delete {
        targets: Vec<PathBuf>,
        mode: DeleteMode,
        actor: Actor,
        reply: Option<oneshot::Sender<Reply>>,
    },
    Copy {
        sources: Vec<PathBuf>,
        moving: bool,
        /// Panel the sources are in; relative destinations start there.
        side: usize,
        actor: Actor,
        reply: Option<oneshot::Sender<Reply>>,
    },
    OpError {
        reply: mpsc::Sender<ErrorAnswer>,
    },
    /// A question of a running deletion; `answers` maps the buttons.
    OpConfirm {
        reply: mpsc::Sender<ConfirmAnswer>,
        answers: Vec<ConfirmAnswer>,
    },
    /// Esc during an operation: "cancel it?" (the operation is paused).
    AbortOp {
        op: OpId,
    },
    Conflict {
        target: PathBuf,
        reply: mpsc::Sender<ConflictAnswer>,
    },
    /// "&Имя" without "remember": the new name for one file.
    RenameTo {
        reply: mpsc::Sender<ConflictAnswer>,
    },
    Message,
    /// F9 → Options → Confirmations.
    Confirmations,
    /// F9 → Options → the agent and its permissions.
    AgentSettings,
    /// F9 → Options → Viewer settings, Alt+Shift+F9 in a viewer.
    ViewerSettings,
    /// F9 → Options → AutoComplete settings.
    AutocompleteSettings,
    /// Gray + / Gray -: select or unselect by the mask.
    Select {
        side: usize,
        add: bool,
    },
    /// F7 in a viewer.
    ViewerSearch {
        id: u32,
    },
    /// Alt+F8 in a viewer.
    ViewerGoto {
        id: u32,
    },
    /// The agent menu's "Rename…" and "Other model…".
    AgentRename,
    /// Alt+F7: what and where to look.
    FindAsk,
    /// Its "Advanced" options; the mask and text to come back with.
    FindAdvanced {
        mask: String,
        text: String,
    },
    /// Ctrl+A: the attributes dialog and what it started from.
    Attributes(Box<super::attributes::AttrState>),
    /// Del in a history menu (Alt+F8, Alt+F11, Alt+F12): "clear it?".
    HistoryMenuClear {
        which: super::historymenu::HistoryMenu,
    },
    /// Import Far's history from this file?
    FarImport {
        path: PathBuf,
    },
    /// Del in a field's history list: "clear it?" (Far's MHistoryClear).
    HistoryClear {
        list: String,
    },
    /// The agent menu's "Compact context…": instructions for `/compact`.
    AgentCompact,
    /// An agent menu action that asks first.
    AgentConfirm(super::agentmenu::AgentAction),
    /// Continue a session of a folder (the running agent ends).
    AgentResume {
        dir: PathBuf,
        id: String,
    },
    AgentModel,
    /// The agent's edit through the IDE protocol (`openDiff`).
    IdeDiff {
        tab_name: String,
        new_contents: String,
        reply: Option<tokio::sync::oneshot::Sender<crate::ide::DiffAnswer>>,
    },
    /// A viewer's search reached the end (start): continue from the other
    /// end up to where it began?
    ViewerSearchWrap {
        id: u32,
        backward: bool,
        origin: u64,
    },
}

pub(super) struct Progress {
    op: OpId,
    kind: OpKind,
    started: Instant,
    done: usize,
    total: usize,
    bytes_done: u64,
    bytes_total: u64,
    file_done: u64,
    file_total: u64,
    current: String,
    target: String,
}

pub(super) struct RunningOp {
    kind: OpKind,
    actor: Actor,
    reply: Option<oneshot::Sender<Reply>>,
    control: Arc<OpControl>,
    /// Item to put the cursor on when done: panel and name.
    focus: Option<(usize, String)>,
    /// Where things go (copy/move), for the agent's answer.
    dest: Option<PathBuf>,
}

// ------------------------------------------------------------- helpers

fn name_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

fn limited(paths: &[PathBuf]) -> Vec<PathBuf> {
    paths.iter().take(LIST_LIMIT).cloned().collect()
}

fn chars(s: &str) -> usize {
    s.chars().count()
}

/// Far's QuoteOuterSpace: quotes only a name with spaces at its ends.
fn quote_outer_space(s: &str) -> String {
    if s.starts_with(' ') || s.ends_with(' ') {
        format!("\"{s}\"")
    } else {
        s.to_string()
    }
}

/// Cut at the end with "…".
fn truncate_right(s: &str, max: usize) -> String {
    if chars(s) <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
    t.push('…');
    t
}

/// Cut in the middle with "…".
fn truncate_center(s: &str, max: usize) -> String {
    let n = chars(s);
    if n <= max || max < 3 {
        return s.to_string();
    }
    let head = (max - 1) / 2;
    let tail = max - 1 - head;
    let mut t: String = s.chars().take(head).collect();
    t.push('…');
    t.extend(s.chars().skip(n - tail));
    t
}

/// Far's truncate_path: keeps the root, "…" in the middle.
fn truncate_path(p: &str, max: usize) -> String {
    if chars(p) <= max {
        return p.to_string();
    }
    let root_len = p.find(['\\', '/']).map_or(0, |i| i + 1);
    let root: String = p.chars().take(root_len).collect();
    let rest = max.saturating_sub(chars(&root) + 1);
    let tail: String = p.chars().skip(chars(p) - rest).collect();
    format!("{root}…{tail}")
}

fn hms(d: Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

impl App {
    /// The file operations' progress for the taskbar button: percent of the
    /// first one shown; paused (yellow) while a question waits over it.
    pub(super) fn ops_progress(&self) -> Option<(u8, u8)> {
        let p = self.overlays.iter().find_map(|o| match o {
            Overlay::Progress(p) => Some(p),
            _ => None,
        })?;
        let percent = (100 * p.bytes_done)
            .checked_div(p.bytes_total)
            .or_else(|| (100 * p.done as u64).checked_div(p.total as u64))
            .unwrap_or(0)
            .min(100) as u8;
        let waiting = !matches!(self.overlays.last(), Some(Overlay::Progress(_)));
        Some((if waiting { 4 } else { 1 }, percent))
    }
}

/// Far's progress bar: 61 cells and the percentage.
fn bar(done: u64, total: u64) -> String {
    const CELLS: u64 = 61;
    let filled = (CELLS * done).checked_div(total).unwrap_or(0).min(CELLS) as usize;
    let percent = (100 * done).checked_div(total).unwrap_or(0).min(100);
    format!(
        "{}{} {percent:>3}%",
        "█".repeat(filled),
        "░".repeat(CELLS as usize - filled)
    )
}

/// "Файлов:        3 / 10": label and value across 61 cells.
fn counter(label_id: &str, done: u64, total: u64) -> String {
    let labels = [tr!("MCopyFilesTotalInfo"), tr!("MCopyBytesTotalInfo")];
    let label_w = labels.iter().map(|l| chars(l)).max().unwrap_or(0) + 1;
    let value = format!("{} / {}", group_thousands(done), group_thousands(total));
    format!(
        "{:<label_w$}{value:>value_w$}",
        tr!(label_id),
        value_w = 61usize.saturating_sub(label_w)
    )
}

fn date_time(t: Option<SystemTime>) -> (String, String) {
    t.map(|t| {
        let t: chrono::DateTime<chrono::Local> = t.into();
        (
            t.format("%d.%m.%Y").to_string(),
            t.format("%H:%M:%S").to_string(),
        )
    })
    .unwrap_or_default()
}

/// A line of the "file exists" warning: label, size, date, time (66 wide).
fn file_line(label_id: &str, info: &FileInfo) -> String {
    let (date, time) = date_time(info.modified);
    let label = tr!(label_id);
    // Far pads the label including its `&` to 26.
    format!(
        "{label:<26} {:>20} {date} {time}",
        group_thousands(info.size)
    )
}

/// Far's Message() with a system error after a separator.
fn error_message(title: &str, lines: Vec<String>, error: &str, buttons: &[&str]) -> Dialog {
    let cols = crossterm::terminal::size().map_or(80, |(w, _)| w) as usize;
    let widest = lines.iter().map(|l| chars(l)).max().unwrap_or(0).max(40);
    let width = widest.min(cols.saturating_sub(11));
    let mut all = lines;
    if !error.is_empty() {
        all.push("\x01".into());
        all.extend(wrap(error, width));
    }
    Dialog::message(title, &all, buttons, true)
}

impl App {
    pub(super) fn has_overlay(&self) -> bool {
        !self.overlays.is_empty()
    }

    /// Far's Message(): lines, a separator, OK.
    pub(super) fn message(&mut self, title: &str, lines: &[String], warning: bool) {
        let lines: Vec<String> = lines
            .iter()
            .flat_map(|l| l.lines().map(str::to_string))
            .collect();
        let ok = tr!("MOk");
        let dialog = Dialog::message(title, &lines, &[&ok], warning);
        self.overlays.push(Overlay::Dialog {
            dialog,
            purpose: Purpose::Message,
        });
    }

    // ------------------------------------------------------------ input

    pub(super) fn overlay_key(&mut self, key: KeyEvent) {
        match self.overlays.last_mut() {
            Some(Overlay::Progress(p)) => {
                if key.code == KeyCode::Esc
                    && let Some(op) = self.ops.get(&p.op)
                {
                    if !self.config.confirm.esc {
                        op.control.cancel();
                        return;
                    }
                    // Far: the operation stops while it asks.
                    op.control.set_paused(true);
                    let id = p.op;
                    let dialog = Dialog::message(
                        &tr!("MKeyESCWasPressed"),
                        &[tr!("MDoYouWantToCancel")],
                        &[&tr!("MYes"), &tr!("MNo")],
                        true,
                    );
                    self.overlays.push(Overlay::Dialog {
                        dialog,
                        purpose: Purpose::AbortOp { op: id },
                    });
                }
            }
            Some(Overlay::Dialog { .. }) => {
                // The completion list first; an edit recomputes it.
                if self.dialog_completion_key(&key)
                    || self.ghost_key(super::autocomplete::Owner::Dialog, &key)
                {
                    return;
                }
                let before = self.dialog_field_text();
                let Some(Overlay::Dialog { dialog, .. }) = self.overlays.last_mut() else {
                    return;
                };
                match dialog.handle_key(&key) {
                    Outcome::Closed(button) => {
                        self.completion = None;
                        self.close_dialog(button);
                    }
                    Outcome::History(request) => self.dialog_history(request),
                    Outcome::Pending => self.dialog_edited(before),
                }
            }
            Some(Overlay::Menu { .. }) => self.menu_key(key),
            Some(Overlay::MenuBar(_)) => self.menubar_key(key),
            Some(Overlay::AgentMenu(_)) => self.agent_menu_key(key),
            Some(Overlay::Find(_)) => self.find_key(&key),
            None => {}
        }
    }

    /// Mouse over the overlay; clicks outside the top dialog are ignored.
    pub(super) fn overlay_mouse(&mut self, ev: &MouseEvent) {
        if let Some(Overlay::Menu { .. }) = self.overlays.last() {
            self.menu_mouse(ev);
            return;
        }
        if let Some(Overlay::MenuBar(_)) = self.overlays.last() {
            self.menubar_mouse(ev);
            return;
        }
        if let Some(Overlay::AgentMenu(_)) = self.overlays.last() {
            self.agent_menu_mouse(ev);
            return;
        }
        if let Some(Overlay::Find(_)) = self.overlays.last() {
            self.find_mouse(ev);
            return;
        }
        if let Some(Overlay::Dialog { dialog, .. }) = self.overlays.last_mut() {
            match dialog.handle_mouse(ev) {
                Some(Outcome::Closed(button)) => self.close_dialog(button),
                Some(Outcome::History(request)) => self.dialog_history(request),
                _ => {}
            }
        }
    }

    fn close_dialog(&mut self, button: Option<usize>) {
        let Some(Overlay::Dialog { dialog, purpose }) = self.overlays.pop() else {
            return;
        };
        // The fields' histories: on a button other than Cancel (not Esc).
        if let Some(b) = button {
            let actor = match &purpose {
                Purpose::MkDir { actor, .. }
                | Purpose::Delete { actor, .. }
                | Purpose::Copy { actor, .. } => *actor,
                _ => Actor::User,
            };
            self.record_dialog_history(&dialog, b, actor);
        }
        match purpose {
            Purpose::MkDir { side, actor, reply } => {
                if button == Some(0) {
                    self.mkdir_from_dialog(side, &dialog, actor, reply);
                } else if let Some(reply) = reply {
                    let _ = reply.send(Err("the user declined creating the folders".into()));
                }
            }
            Purpose::Delete {
                targets,
                mode,
                actor,
                reply,
            } => {
                if button == Some(0) {
                    self.start_delete(targets, mode, actor, reply);
                } else if let Some(reply) = reply {
                    let _ = reply.send(Err("the user declined the deletion".into()));
                }
            }
            Purpose::Copy {
                sources,
                moving,
                side,
                actor,
                reply,
            } => {
                if button != Some(0) {
                    if let Some(reply) = reply {
                        let _ = reply.send(Err("the user declined the operation".into()));
                    }
                    return;
                }
                let dest = dialog.input_value(0).trim().trim_matches('"').to_string();
                let overwrite = OVERWRITE_CHOICES
                    .get(dialog.combo(0))
                    .map_or(Overwrite::Ask, |(_, o)| *o);
                if let Err(e) =
                    self.start_copy(sources, &dest, moving, overwrite, side, actor, reply)
                {
                    self.message(&tr!("MError"), &[e], true);
                }
            }
            Purpose::OpError { reply } => {
                let answer = match button {
                    Some(0) => ErrorAnswer::Retry,
                    Some(1) => ErrorAnswer::Skip,
                    Some(2) => ErrorAnswer::SkipAll,
                    _ => ErrorAnswer::Cancel,
                };
                let _ = reply.send(answer);
            }
            Purpose::Conflict { target, reply } => {
                let all = dialog.checked(0);
                let action = match button {
                    Some(0) => ConflictAction::Replace,
                    Some(1) => ConflictAction::Skip,
                    Some(2) if !all => {
                        // Far asks for the new name of this one file.
                        self.rename_dialog(&target, reply);
                        return;
                    }
                    Some(2) => ConflictAction::Rename,
                    Some(3) => ConflictAction::Append,
                    _ => ConflictAction::Cancel,
                };
                let _ = reply.send(ConflictAnswer { action, all });
            }
            Purpose::RenameTo { reply } => {
                let name = dialog.input_value(0);
                let action = if button == Some(0) && !name.trim().is_empty() {
                    ConflictAction::RenameTo(name.trim().to_string())
                } else {
                    ConflictAction::Skip
                };
                let _ = reply.send(ConflictAnswer { action, all: false });
            }
            Purpose::Message => {}
            Purpose::OpConfirm { reply, answers } => {
                let answer = button
                    .and_then(|b| answers.get(b).copied())
                    .unwrap_or(ConfirmAnswer::Cancel);
                let _ = reply.send(answer);
            }
            Purpose::AbortOp { op } => {
                if let Some(op) = self.ops.get(&op) {
                    if button == Some(0) {
                        op.control.cancel();
                    }
                    op.control.set_paused(false);
                }
            }
            Purpose::Confirmations => {
                if button == Some(0) {
                    self.confirmations_from_dialog(&dialog);
                }
            }
            Purpose::AutocompleteSettings => {
                if button == Some(0) {
                    self.autocomplete_settings_from_dialog(&dialog);
                }
            }
            Purpose::ViewerSettings => {
                if button == Some(0) {
                    self.viewer_settings_from_dialog(&dialog);
                }
            }
            Purpose::AgentSettings => {
                if button == Some(0) {
                    self.agent_settings_from_dialog(&dialog);
                }
            }
            Purpose::Select { side, add } => {
                if button == Some(0) {
                    self.select_from_dialog(side, add, dialog);
                }
            }
            Purpose::ViewerSearch { id } => self.viewer_search_dialog_closed(id, button, &dialog),
            Purpose::IdeDiff {
                new_contents,
                reply,
                ..
            } => {
                let answer = if button == Some(0) {
                    crate::ide::DiffAnswer::Saved(new_contents)
                } else {
                    crate::ide::DiffAnswer::Rejected
                };
                if let Some(reply) = reply {
                    let _ = reply.send(answer);
                }
            }
            Purpose::FindAsk => self.find_ask_closed(&dialog, button),
            Purpose::FindAdvanced { mask, text } => {
                self.find_advanced_closed(&dialog, button, mask, text)
            }
            Purpose::Attributes(state) => self.attributes_button(state, dialog, button),
            Purpose::HistoryMenuClear { which } => {
                if button == Some(0) {
                    self.store.clear(which_kind(which), "");
                }
                self.history_menu(which, None);
            }
            Purpose::FarImport { path } => {
                if button == Some(0) {
                    self.far_import(path);
                }
            }
            Purpose::HistoryClear { list } => {
                if button == Some(0) {
                    self.store.clear(crate::history::Kind::Dialog, &list);
                }
            }
            Purpose::AgentCompact => {
                if button == Some(0) {
                    self.agent_compact(dialog.input_value(0));
                }
            }
            Purpose::AgentResume { dir, id } => {
                if button == Some(0) {
                    self.relaunch_agent(dir, super::agent::Launch::Resume(id));
                }
            }
            Purpose::AgentConfirm(action) => {
                if button == Some(0) {
                    self.agent_action_now(action);
                }
            }
            Purpose::AgentRename => {
                if button == Some(0) {
                    self.agent_renamed(dialog.input_value(0));
                }
            }
            Purpose::AgentModel => {
                if button == Some(0) {
                    self.agent_model(dialog.input_value(0));
                }
            }
            Purpose::ViewerGoto { id } => {
                if button == Some(0) {
                    self.viewer_goto_closed(id, &dialog);
                }
            }
            Purpose::ViewerSearchWrap {
                id,
                backward,
                origin,
            } => {
                if button == Some(0) {
                    self.viewer_search_wrap(id, backward, origin);
                }
            }
        }
    }

    /// `bar` is where the menu bar goes: from the top row of the panels
    /// (below the agent pane when it is on top).
    pub(super) fn draw_overlays(
        &mut self,
        area: Rect,
        bar: Rect,
        agent_bar: Rect,
        buf: &mut Buffer,
    ) -> Option<Position> {
        let mut cursor = None;
        for overlay in &mut self.overlays {
            cursor = match overlay {
                Overlay::Dialog { dialog, .. } => dialog.draw(area, buf),
                Overlay::Progress(p) => {
                    progress_dialog(p).draw(area, buf);
                    None
                }
                Overlay::Menu { menu, .. } => {
                    menu.draw(area, buf);
                    None
                }
                Overlay::MenuBar(menubar) => {
                    menubar.draw(bar, buf);
                    None
                }
                Overlay::AgentMenu(menubar) => {
                    menubar.draw(agent_bar, buf);
                    None
                }
                Overlay::Find(v) => {
                    super::findfiles::draw_find(v, area, buf);
                    None
                }
            };
        }
        cursor
    }

    // ------------------------------------------------------------- mkdir

    /// F7, as Far's make-folder dialog (mkdir.cpp).
    pub(super) fn mkdir_dialog(&mut self) {
        self.open_mkdir_dialog(self.active, &[], Actor::User, None);
    }

    /// The make-folder dialog; the agent's request comes filled in.
    pub(super) fn open_mkdir_dialog(
        &mut self,
        side: usize,
        names: &[String],
        actor: Actor,
        reply: Option<oneshot::Sender<Reply>>,
    ) {
        let link_types = vec![
            Some(tr!("MMakeFolderLinkNone")),
            Some(tr!("MMakeFolderLinkJunction")),
            Some(tr!("MMakeFolderLinkSymlink")),
        ];
        let mut d = Dialog::far(tr!("MMakeFolderTitle"), FAR_WIDTH)
            .text(tr!("MCreateFolder"))
            .row(vec![
                input_at(5, 66, names.join(";"), Some("NewFolder"))
                    .path()
                    .use_last(),
            ])
            .separator()
            .row(vec![
                text_at(5, tr!("MMakeFolderLinkType")),
                combo_at(20, 51, link_types, 0),
            ])
            .row(vec![
                text_at(5, tr!("MMakeFolderLinkTarget")),
                input_at(20, 51, "", Some("NewFolderLinkTarget")).path(),
            ])
            .row(vec![check_at(5, tr!("MMultiMakeDir"), names.len() > 1)]);
        if actor == Actor::Agent {
            d = d.row(vec![
                text_at(5, tr!("requested-by-agent")).literal().centered(),
            ]);
        }
        let dialog = d.separator().buttons(&[&tr!("MOk"), &tr!("MCancel")], 0);
        self.overlays.push(Overlay::Dialog {
            dialog,
            purpose: Purpose::MkDir { side, actor, reply },
        });
    }

    fn mkdir_from_dialog(
        &mut self,
        side: usize,
        dialog: &Dialog,
        actor: Actor,
        reply: Option<oneshot::Sender<Reply>>,
    ) {
        let text = dialog.input_value(0);
        let names: Vec<String> = if dialog.checked(0) {
            text.split([';', ',']).map(str::to_string).collect()
        } else {
            vec![text]
        };
        let link = match dialog.combo(0) {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        };
        let result = match link {
            None => self.mkdir(side, actor, &names).map(|created| {
                let list: Vec<String> = created.iter().map(|p| p.display().to_string()).collect();
                format!("created: {}", list.join(", "))
            }),
            Some(junction) => {
                let target = PathBuf::from(dialog.input_value(1).trim().trim_matches('"'));
                self.make_links(side, &names, &target, junction)
                    .map(|()| format!("links created to {}", target.display()))
            }
        };
        if let Some(reply) = reply {
            let _ = reply.send(result.clone());
        }
        if let Err(lines) = result {
            self.message(&tr!("MError"), &[lines], true);
        }
    }

    /// Junctions or symbolic links named `names` pointing to `target`.
    fn make_links(
        &mut self,
        side: usize,
        names: &[String],
        target: &Path,
        junction: bool,
    ) -> Result<(), String> {
        let base = self.panels[side].path.clone();
        let target = base.join(target);
        for name in names.iter().map(|n| n.trim()).filter(|n| !n.is_empty()) {
            let link = base.join(name);
            ops::make_link(&link, &target, junction)
                .map_err(|e| format!("{}\n{}\n{e}", tr!("MCannotCreateFolder"), link.display()))?;
        }
        for p in &mut self.panels {
            p.reload(None);
        }
        Ok(())
    }

    /// Creates directories in a panel; puts the cursor on the first new one.
    pub(super) fn mkdir(
        &mut self,
        side: usize,
        actor: Actor,
        names: &[String],
    ) -> Result<Vec<PathBuf>, String> {
        let base = self.panels[side].path.clone();
        let (created, report) = ops::make_dirs(&base, names);
        self.note_own_change();
        if created.is_empty() && report.failed.is_empty() {
            return Err(tr!("MIncorrectDirList"));
        }
        let id = self.next_op_id();
        self.journal.push(
            actor,
            Event::FileOpStarted {
                op_id: id,
                op: OpKind::MkDir,
                count: created.len() + report.failed.len(),
                sources: limited(&created),
                dest: None,
            },
        );
        self.journal_finished(id, OpKind::MkDir, actor, &report);
        let focus = created
            .first()
            .and_then(|p| p.strip_prefix(&base).ok())
            .and_then(|rel| rel.components().next())
            .map(|c| c.as_os_str().to_string_lossy().into_owned());
        for p in &mut self.panels {
            p.reload(None);
        }
        if let Some(name) = focus {
            self.panels[side].set_cursor_by_name(&name);
        }
        match report.failed.first() {
            Some((path, e)) => Err(format!(
                "{}\n{}\n{e}",
                tr!("MCannotCreateFolder"),
                path.display()
            )),
            None => Ok(created),
        }
    }

    // ------------------------------------------------------------ delete

    /// Items an operation applies to: the selection, or the item under the
    /// cursor.
    pub(super) fn op_sources(&self, current_only: bool) -> Vec<PathBuf> {
        let panel = &self.panels[self.active];
        let selected: Vec<PathBuf> = panel.selected().map(|e| panel.path.join(&e.name)).collect();
        if !current_only && !selected.is_empty() {
            return selected;
        }
        panel
            .current()
            .filter(|e| e.name != "..")
            .map(|e| vec![panel.path.join(&e.name)])
            .unwrap_or_default()
    }

    /// F8 / Shift+Del, as Far's delete confirmation (delete.cpp).
    pub(super) fn delete_dialog(
        &mut self,
        targets: Vec<PathBuf>,
        mode: DeleteMode,
        actor: Actor,
        reply: Option<oneshot::Sender<Reply>>,
    ) {
        if targets.is_empty() {
            return;
        }
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 25));
        let what = match targets.as_slice() {
            [one] if one.is_dir() => tr!("MAskDeleteFolder"),
            [_] => tr!("MAskDeleteFile"),
            _ => tr!("MAskDeleteObjects"),
        };
        let (question, button) = match mode {
            DeleteMode::Permanent => (tr!("MAskDelete", p0 = what), tr!("MDelete")),
            DeleteMode::Trash => (tr!("MAskDeleteRecycle", p0 = what), tr!("MDeleteRecycle")),
            DeleteMode::Wipe => (tr!("MAskWipe", p0 = what), tr!("MDeleteWipe")),
        };
        let title = if mode == DeleteMode::Wipe {
            tr!("MDeleteWipeTitle")
        } else {
            tr!("MDeleteTitle")
        };
        let cancel = tr!("MCancel");
        let max_line = usize::from(cols.saturating_sub(12));
        let mut lines: Vec<(String, bool)> = Vec::new(); // text, is a name
        if let [one] = targets.as_slice() {
            lines.push((question, false));
            lines.push((
                truncate_center(&quote_outer_space(&name_of(one)), max_line),
                false,
            ));
        } else {
            lines.push((question, false));
            let show = 10.min(targets.len()).min(usize::from(rows / 2));
            let show = if targets.len() - show == 1 {
                show + 1
            } else {
                show
            };
            for t in &targets[..show] {
                lines.push((truncate_center(&name_of(t), max_line), true));
            }
            if targets.len() > show {
                lines.push((tr!("MAskDeleteAndMore", p0 = targets.len() - show), false));
            }
        }
        let content = lines
            .iter()
            .map(|(l, _)| chars(l))
            .chain([chars(&button) + chars(&cancel) + 6])
            .max()
            .unwrap_or(0)
            .min(max_line) as u16;
        let mut d = Dialog::new(title, content);
        if mode != DeleteMode::Trash {
            d = d.warning();
        }
        let single = targets.len() == 1;
        for (i, (line, is_name)) in lines.into_iter().enumerate() {
            let elem = text_at(5, line).literal();
            let elem = if single { elem.centered() } else { elem };
            d = d.row(vec![if is_name { elem.highlighted() } else { elem }]);
            if i == 0 && !single {
                d = d.separator();
            }
        }
        if actor == Actor::Agent {
            d = d.row(vec![
                text_at(5, tr!("requested-by-agent")).literal().centered(),
            ]);
        }
        let dialog = d.separator().buttons(&[&button, &cancel], 0);
        self.overlays.push(Overlay::Dialog {
            dialog,
            purpose: Purpose::Delete {
                targets,
                mode,
                actor,
                reply,
            },
        });
    }

    fn start_delete(
        &mut self,
        targets: Vec<PathBuf>,
        mode: DeleteMode,
        actor: Actor,
        reply: Option<oneshot::Sender<Reply>>,
    ) {
        let id = self.next_op_id();
        let kind = match mode {
            DeleteMode::Trash => OpKind::Trash,
            DeleteMode::Permanent => OpKind::Delete,
            DeleteMode::Wipe => OpKind::Wipe,
        };
        self.journal.push(
            actor,
            Event::FileOpStarted {
                op_id: id,
                op: kind,
                count: targets.len(),
                sources: limited(&targets),
                dest: None,
            },
        );
        let control = OpControl::new();
        let tx = self.tx.clone();
        let confirm = ops::Confirmations {
            folders: self.config.confirm.delete_folder,
            read_only: self.config.confirm.read_only,
        };
        let started = ops::spawn_delete(id, targets, mode, confirm, control.clone(), move |m| {
            let _ = tx.send(AppMsg::Op(m));
        });
        self.track_op(id, kind, actor, reply, control, None, None, started);
    }

    // -------------------------------------------------------- copy / move

    /// F5 / F6 (with `current_only`: Shift+F5 / Shift+F6).
    pub(super) fn copy_dialog(&mut self, moving: bool, current_only: bool) {
        let sources = self.op_sources(current_only);
        if sources.is_empty() {
            return;
        }
        // Like Far: the other panel's directory, or the name itself for
        // Shift+F5 / Shift+F6 (copy or rename in place).
        let dest = if current_only {
            name_of(&sources[0])
        } else {
            let mut s = self.panels[1 - self.active].path.display().to_string();
            if !s.ends_with(std::path::MAIN_SEPARATOR) {
                s.push(std::path::MAIN_SEPARATOR);
            }
            s
        };
        self.open_copy_dialog(sources, dest, moving, self.active, Actor::User, None);
    }

    /// Far's copy / move dialog (copy.cpp:648-977), 76×17.
    fn open_copy_dialog(
        &mut self,
        sources: Vec<PathBuf>,
        dest: String,
        moving: bool,
        side: usize,
        actor: Actor,
        reply: Option<oneshot::Sender<Reply>>,
    ) {
        let to = tr!("MCMLTargetTO");
        let prompt = if let [one] = sources.as_slice() {
            let id = if moving { "MMoveFile" } else { "MCopyFile" };
            let empty = visible(&tr!(id, p0 = "", p1 = to.clone()));
            let room = 67usize.saturating_sub(chars(&empty));
            let name = truncate_right(&name_of(one), room).replace('&', "&&");
            tr!(id, p0 = name, p1 = to)
        } else {
            let n = sources.len();
            let id = if moving { "MMoveFiles" } else { "MCopyFiles" };
            tr!(id, p0 = n, p1 = crate::i18n::far_items_suffix(n), p2 = to)
        };
        let rights = tr!("MCopySecurity");
        let rights_options = [
            tr!("MCopySecurityDefault"),
            tr!("MCopySecurityCopy"),
            tr!("MCopySecurityInherit"),
        ];
        let mut d = Dialog::far(
            tr!(if moving {
                "MMoveDlgTitle"
            } else {
                "MCopyDlgTitle"
            }),
            FAR_WIDTH,
        );
        let group = d.new_group();
        let mut security = vec![text_at(5, rights.clone())];
        let mut x = 5 + chars(&visible(&rights)) as u16 + 1;
        for (i, label) in rights_options.iter().enumerate() {
            // Only the default is implemented: access rights are not copied.
            let radio = radio_at(x, label.clone(), i == 0, group);
            security.push(if i == 0 { radio } else { radio.disabled() });
            x += chars(&visible(label)) as u16 + 5;
        }
        let existing: Vec<Option<String>> = OVERWRITE_CHOICES
            .iter()
            .map(|(id, _)| Some(tr!(*id)))
            .collect();
        d = d
            .row(vec![text_at(5, prompt)])
            .row(vec![input_at(5, 66, dest, Some("Copy")).path()])
            .separator()
            .row(security)
            .separator()
            .row(vec![
                text_at(5, tr!("MCopyIfFileExist")),
                combo_at(29, 42, existing, 0),
            ])
            .row(vec![
                check_at(5, tr!("MCopyPreserveAllTimestamps"), false).disabled(),
            ])
            .row(vec![
                check_at(5, tr!("MCopySymLinkContents"), false).disabled(),
            ])
            .row(vec![
                check_at(5, tr!("MCopyMultiActions"), false).disabled(),
            ])
            .separator()
            .row(vec![check_at(5, tr!("MCopyUseFilter"), false).disabled()]);
        if actor == Actor::Agent {
            d = d.row(vec![
                text_at(5, tr!("requested-by-agent")).literal().centered(),
            ]);
        }
        let dialog = d.separator().button_row(vec![
            Button::new(tr!(if moving {
                "MCopyDlgRename"
            } else {
                "MCopyDlgCopy"
            }))
            .default(),
            // No folder tree yet: Far hides the button when the tree is off.
            Button::new(tr!("MCopyDlgTree")).hidden(),
            Button::new(tr!("MCopySetFilter")).disabled(),
            Button::new(tr!("MCopyDlgCancel")),
        ]);
        self.overlays.push(Overlay::Dialog {
            dialog,
            purpose: Purpose::Copy {
                sources,
                moving,
                side,
                actor,
                reply,
            },
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn start_copy(
        &mut self,
        sources: Vec<PathBuf>,
        dest: &str,
        moving: bool,
        overwrite: Overwrite,
        side: usize,
        actor: Actor,
        reply: Option<oneshot::Sender<Reply>>,
    ) -> Result<(), String> {
        let fail = |reply: Option<oneshot::Sender<Reply>>, e: String| {
            if let Some(reply) = reply {
                let _ = reply.send(Err(e.clone()));
            }
            Err(e)
        };
        if dest.is_empty() {
            return fail(reply, tr!("copy-nothing"));
        }
        let dest = self.panels[side].path.join(dest);
        let job = CopyJob {
            sources,
            dest: dest.clone(),
            moving,
            overwrite,
        };
        let pairs = match ops::plan_targets(&job) {
            Ok(p) => p,
            Err(e) => return fail(reply, e),
        };
        let id = self.next_op_id();
        let kind = if moving { OpKind::Move } else { OpKind::Copy };
        self.journal.push(
            actor,
            Event::FileOpStarted {
                op_id: id,
                op: kind,
                count: job.sources.len(),
                sources: limited(&job.sources),
                dest: Some(dest.clone()),
            },
        );
        // A single item landing in one of the panels: show it there.
        let focus = match pairs.as_slice() {
            [(_, target)] => {
                let parent = target.parent().map(Path::to_path_buf);
                (0..2)
                    .find(|&s| parent.as_ref() == Some(&self.panels[s].path))
                    .map(|s| (s, name_of(target)))
            }
            _ => None,
        };
        let control = OpControl::new();
        let tx = self.tx.clone();
        let started = ops::spawn_copy(id, pairs, moving, overwrite, control.clone(), move |m| {
            let _ = tx.send(AppMsg::Op(m));
        });
        self.track_op(id, kind, actor, reply, control, focus, Some(dest), started);
        Ok(())
    }

    /// Registers a started operation and shows its progress window.
    #[allow(clippy::too_many_arguments)]
    fn track_op(
        &mut self,
        id: OpId,
        kind: OpKind,
        actor: Actor,
        reply: Option<oneshot::Sender<Reply>>,
        control: Arc<OpControl>,
        focus: Option<(usize, String)>,
        dest: Option<PathBuf>,
        started: std::io::Result<()>,
    ) {
        match started {
            Ok(()) => {
                self.ops.insert(
                    id,
                    RunningOp {
                        kind,
                        actor,
                        reply,
                        control,
                        focus,
                        dest,
                    },
                );
                self.overlays.push(Overlay::Progress(Progress {
                    op: id,
                    kind,
                    started: Instant::now(),
                    done: 0,
                    total: 0,
                    bytes_done: 0,
                    bytes_total: 0,
                    file_done: 0,
                    file_total: 0,
                    current: String::new(),
                    target: String::new(),
                }));
            }
            Err(e) => {
                let report = OpReport {
                    failed: vec![(PathBuf::new(), e.to_string())],
                    ..Default::default()
                };
                self.journal_finished(id, kind, actor, &report);
                if let Some(reply) = reply {
                    let _ = reply.send(Err(e.to_string()));
                }
            }
        }
    }

    // ---------------------------------------------------- op messages

    pub(super) fn on_op(&mut self, msg: OpMsg) {
        match msg {
            OpMsg::Progress {
                id,
                done,
                total,
                bytes_done,
                bytes_total,
                file_done,
                file_total,
                current,
                target,
            } => {
                for o in &mut self.overlays {
                    if let Overlay::Progress(p) = o
                        && p.op == id
                    {
                        p.done = done;
                        p.total = total;
                        p.bytes_done = bytes_done;
                        p.bytes_total = bytes_total;
                        p.file_done = file_done;
                        p.file_total = file_total;
                        p.current = current.display().to_string();
                        p.target = target
                            .as_ref()
                            .map(|t| t.display().to_string())
                            .unwrap_or_default();
                    }
                }
            }
            OpMsg::Error {
                path,
                error,
                context,
                reply,
                ..
            } => {
                let copy_like = match &context {
                    ErrorContext::Copy { dest } => Some(("MCannotCopy", dest.clone())),
                    ErrorContext::Move { dest } => Some(("MCannotMove", dest.clone())),
                    ErrorContext::Delete => None,
                };
                let dialog = match copy_like {
                    Some((what, dest)) => error_message(
                        &tr!("MError"),
                        vec![
                            tr!(what),
                            format!("\"{}\"", path.display()),
                            tr!("MCannotCopyTo"),
                            format!("\"{}\"", dest.display()),
                        ],
                        &error,
                        &[
                            &tr!("MCopyRetry"),
                            &tr!("MCopySkip"),
                            &tr!("MCopySkipAll"),
                            &tr!("MCopyCancel"),
                        ],
                    ),
                    None => error_message(
                        &tr!("MError"),
                        vec![
                            tr!(if path.is_dir() {
                                "MCannotDeleteFolder"
                            } else {
                                "MCannotDeleteFile"
                            }),
                            quote_outer_space(&path.display().to_string()),
                        ],
                        &error,
                        &[
                            &tr!("MDeleteRetry"),
                            &tr!("MDeleteSkip"),
                            &tr!("MDeleteFileSkipAll"),
                            &tr!("MCancel"),
                        ],
                    ),
                };
                self.ask(dialog, Purpose::OpError { reply });
            }
            OpMsg::Confirm {
                question, reply, ..
            } => {
                use ConfirmAnswer::{All, Cancel, Skip, SkipAll, Yes};
                let (dialog, answers) = confirm_dialog(&question);
                let answers = answers.unwrap_or(vec![Yes, All, Skip, SkipAll, Cancel]);
                self.ask(dialog, Purpose::OpConfirm { reply, answers });
            }
            OpMsg::Conflict {
                target,
                new,
                existing,
                reply,
                ..
            } => {
                let dialog = Dialog::far(tr!("MWarning"), FAR_WIDTH)
                    .warning()
                    .center(tr!("MCopyFileExist"))
                    .row(vec![
                        input_at(
                            5,
                            66,
                            quote_outer_space(&target.display().to_string()),
                            None,
                        )
                        .readonly(),
                    ])
                    .separator()
                    .row(vec![text_at(5, file_line("MCopySource", &new))])
                    .row(vec![text_at(5, file_line("MCopyDest", &existing))])
                    .separator()
                    .row(vec![check_at(5, tr!("MCopyRememberChoice"), false)])
                    .separator()
                    .buttons(
                        &[
                            &tr!("MCopyOverwrite"),
                            &tr!("MCopySkip"),
                            &tr!("MCopyRename"),
                            &tr!("MCopyAppend"),
                            &tr!("MCopyCancel"),
                        ],
                        0,
                    );
                self.ask(dialog, Purpose::Conflict { target, reply });
            }
            OpMsg::Finished { id, report } => {
                self.overlays
                    .retain(|o| !matches!(o, Overlay::Progress(p) if p.op == id));
                let Some(op) = self.ops.remove(&id) else {
                    return;
                };
                self.note_own_change();
                self.journal_finished(id, op.kind, op.actor, &report);
                for p in &mut self.panels {
                    p.reload(None);
                }
                if let Some((side, name)) = &op.focus {
                    self.panels[*side].set_cursor_by_name(name);
                }
                if let Some(reply) = op.reply {
                    let _ = reply.send(Ok(describe_report(op.kind, op.dest.as_deref(), &report)));
                }
            }
        }
    }

    /// "&Имя" without "remember": Far asks for the new name.
    fn rename_dialog(&mut self, target: &Path, reply: mpsc::Sender<ConflictAnswer>) {
        let suggestion = ops::unique_name(target);
        let dialog = Dialog::far(tr!("MCopyRenameTitle"), FAR_WIDTH)
            .text(tr!("MCopyRenameText"))
            .row(vec![input_at(5, 66, name_of(&suggestion), None)])
            .separator()
            .buttons(&[&tr!("MOk"), &tr!("MCancel")], 0);
        self.overlays.push(Overlay::Dialog {
            dialog,
            purpose: Purpose::RenameTo { reply },
        });
    }

    /// Shows a question from a running operation.
    fn ask(&mut self, dialog: Dialog, purpose: Purpose) {
        self.overlays.push(Overlay::Dialog { dialog, purpose });
        // The question needs the user even if they were talking to the agent.
        if self.focus == Focus::Agent {
            self.say(tr!("op-waits-answer"));
        }
    }

    fn journal_finished(&mut self, id: OpId, kind: OpKind, actor: Actor, report: &OpReport) {
        self.journal.push(
            actor,
            Event::FileOpFinished {
                op_id: id,
                op: kind,
                done: report.done,
                skipped: report.skipped,
                failed_count: report.failed.len(),
                failed: report.failed.iter().take(LIST_LIMIT).cloned().collect(),
                cancelled: report.cancelled,
            },
        );
    }

    fn next_op_id(&mut self) -> OpId {
        self.next_op_id += 1;
        self.next_op_id
    }

    // ------------------------------------------------------------- agent

    /// `afar_mkdir`: creating is allowed without confirmation.
    pub(super) fn agent_mkdir(&mut self, side: usize, names: &[String]) -> Reply {
        self.set_panels_visible(true);
        let created = self.mkdir(side, Actor::Agent, names)?;
        let list: Vec<String> = created.iter().map(|p| p.display().to_string()).collect();
        Ok(format!("created: {}", list.join(", ")))
    }

    /// Paths of `names` in a panel; those that do not exist go to the error.
    fn agent_sources(&self, side: usize, names: &[String]) -> Result<Vec<PathBuf>, String> {
        let base = &self.panels[side].path;
        let (found, missing): (Vec<PathBuf>, Vec<PathBuf>) = names
            .iter()
            .map(|n| base.join(n.trim_matches('"')))
            .partition(|p| std::fs::symlink_metadata(p).is_ok());
        if found.is_empty() {
            return Err(format!("nothing found: {missing:?}"));
        }
        Ok(found)
    }

    /// Shows the agent's request: panels visible, items selected, the
    /// keyboard on the dialog.
    fn present_agent_request(&mut self, side: usize, names: &[String], what: String) {
        self.set_panels_visible(true);
        self.panels[side].select_names(names, false);
        self.focus = Focus::Panels;
        self.say(tr!("agent-asks", what = what));
    }

    /// `afar_delete`: asks the user in a dialog; the reply is sent when the
    /// deletion has finished or the user declined.
    pub(super) fn agent_delete(
        &mut self,
        side: usize,
        names: &[String],
        mode: DeleteMode,
        reply: oneshot::Sender<Reply>,
    ) {
        let action = if mode == DeleteMode::Trash {
            AgentAction::Delete
        } else {
            AgentAction::DeletePermanent
        };
        let level = self.permission(action);
        if level == Level::Deny {
            let _ = reply.send(Err(Self::denied(action)));
            return;
        }
        match self.agent_sources(side, names) {
            Ok(targets) if level == Level::Allow => {
                self.set_panels_visible(true);
                self.start_delete(targets, mode, Actor::Agent, Some(reply));
            }
            Ok(targets) => {
                let what = tr!("objects", count = targets.len());
                self.present_agent_request(side, names, format!("{}: {what}", tr!("MDeleteTitle")));
                self.delete_dialog(targets, mode, Actor::Agent, Some(reply));
            }
            Err(e) => {
                let _ = reply.send(Err(e));
            }
        }
    }

    /// `afar_copy` / `afar_move`: the copy dialog filled in by the agent;
    /// the user may change it before confirming.
    pub(super) fn agent_copy(
        &mut self,
        side: usize,
        names: &[String],
        dest: String,
        moving: bool,
        reply: oneshot::Sender<Reply>,
    ) {
        let action = if moving {
            AgentAction::Move
        } else {
            AgentAction::Copy
        };
        let level = self.permission(action);
        if level == Level::Deny {
            let _ = reply.send(Err(Self::denied(action)));
            return;
        }
        match self.agent_sources(side, names) {
            Ok(sources) if level == Level::Allow => {
                self.set_panels_visible(true);
                // Conflicts with existing files are still asked about.
                let _ = self.start_copy(
                    sources,
                    &dest,
                    moving,
                    Overwrite::Ask,
                    side,
                    Actor::Agent,
                    Some(reply),
                );
            }
            Ok(sources) => {
                let title = tr!(if moving {
                    "MMoveDlgTitle"
                } else {
                    "MCopyDlgTitle"
                });
                let what = tr!("objects", count = sources.len());
                self.present_agent_request(side, names, format!("{title}: {what}"));
                self.open_copy_dialog(sources, dest, moving, side, Actor::Agent, Some(reply));
            }
            Err(e) => {
                let _ = reply.send(Err(e));
            }
        }
    }
}

/// Far's progress windows (copy_progress.cpp, delete.cpp).
/// Far's questions while deleting (delete.cpp): a non-empty folder
/// (Delete / All / Skip / Cancel) and a read-only file (Delete / All /
/// Skip / Skip all / Cancel); the answers in button order.
fn confirm_dialog(question: &Question) -> (Dialog, Option<Vec<ConfirmAnswer>>) {
    use ConfirmAnswer::{All, Cancel, Skip, Yes};
    match question {
        Question::NonEmptyFolder { path, mode } => {
            let (title, text, button) = match mode {
                DeleteMode::Trash => (
                    "MDeleteFolderTitle",
                    "MRecycleFolderConfirm",
                    "MDeleteRecycle",
                ),
                DeleteMode::Permanent => (
                    "MDeleteFolderTitle",
                    "MDeleteFolderConfirm",
                    "MDeleteFileDelete",
                ),
                DeleteMode::Wipe => ("MWipeFolderTitle", "MWipeFolderConfirm", "MDeleteFileWipe"),
            };
            let lines = [tr!(text), path.display().to_string()];
            let buttons = [
                tr!(button),
                tr!("MDeleteFileAll"),
                tr!("MDeleteFileSkip"),
                tr!("MDeleteFileCancel"),
            ];
            let buttons: Vec<&str> = buttons.iter().map(String::as_str).collect();
            (
                Dialog::message(&tr!(title), &lines, &buttons, true),
                Some(vec![Yes, All, Skip, Cancel]),
            )
        }
        Question::ReadOnly { path, mode } => {
            let (ask, button) = if *mode == DeleteMode::Wipe {
                ("MAskWipeRO", "MDeleteFileWipe")
            } else {
                ("MAskDeleteRO", "MDeleteFileDelete")
            };
            let lines = [tr!("MDeleteRO"), path.display().to_string(), tr!(ask)];
            let buttons = [
                tr!(button),
                tr!("MDeleteFileAll"),
                tr!("MDeleteFileSkip"),
                tr!("MDeleteFileSkipAll"),
                tr!("MDeleteFileCancel"),
            ];
            let buttons: Vec<&str> = buttons.iter().map(String::as_str).collect();
            (
                Dialog::message(&tr!("MWarning"), &lines, &buttons, true),
                None,
            )
        }
    }
}

fn progress_dialog(p: &Progress) -> Dialog {
    match p.kind {
        OpKind::Copy | OpKind::Move => {
            let moving = p.kind == OpKind::Move;
            let elapsed = p.started.elapsed();
            let remaining = if p.bytes_done > 0 && p.bytes_total > p.bytes_done {
                elapsed.mul_f64((p.bytes_total - p.bytes_done) as f64 / p.bytes_done as f64)
            } else {
                Duration::ZERO
            };
            let speed = match elapsed.as_secs() {
                0 => String::new(),
                secs => {
                    let bps = p.bytes_done / secs;
                    let size = size_float(bps);
                    // "12,3 М" + "Б/с": Far glues the unit and "B/s".
                    let size = if bps < 1024 { format!("{bps} ") } else { size };
                    format!("{size}{}", tr!("MCopyTimeInfoSpeed"))
                }
            };
            let time = format!("{} {}", tr!("MCopyTimeInfoElapsed"), hms(elapsed));
            let left = format!("{} {}", tr!("MCopyTimeInfoRemaining"), hms(remaining));
            let used = chars(&time) + chars(&left) + chars(&speed);
            let gap = FAR_TEXT.saturating_sub(used) / 2;
            let time_line = format!("{time}{:gap$}{left}{:gap$}{speed}", "", "");
            Dialog::far(
                tr!(if moving {
                    "MMoveDlgTitle"
                } else {
                    "MCopyDlgTitle"
                }),
                FAR_WIDTH,
            )
            .row(vec![
                text_at(
                    5,
                    tr!(if moving {
                        "MCopyMoving"
                    } else {
                        "MCopyCopying"
                    }),
                )
                .literal(),
            ])
            .row(vec![
                text_at(5, truncate_path(&p.current, FAR_TEXT)).literal(),
            ])
            .row(vec![text_at(5, tr!("MCopyTo")).literal()])
            .row(vec![
                text_at(5, truncate_path(&p.target, FAR_TEXT)).literal(),
            ])
            .row(vec![text_at(5, bar(p.file_done, p.file_total)).literal()])
            .caption(tr!("MCopyDlgTotal"))
            .row(vec![
                text_at(
                    5,
                    counter("MCopyFilesTotalInfo", p.done as u64, p.total as u64),
                )
                .literal(),
            ])
            .row(vec![
                text_at(
                    5,
                    counter("MCopyBytesTotalInfo", p.bytes_done, p.bytes_total),
                )
                .literal(),
            ])
            .row(vec![text_at(5, bar(p.bytes_done, p.bytes_total)).literal()])
            .separator()
            .row(vec![
                text_at(5, truncate_right(&time_line, FAR_TEXT)).literal(),
            ])
        }
        _ => {
            let label = tr!("MCopyFilesTotalInfo");
            let count = group_thousands(p.done as u64);
            let line = format!(
                "{label} {count:>w$}",
                w = 61usize.saturating_sub(chars(&label) + 1)
            );
            let (title, doing) = if p.kind == OpKind::Wipe {
                (tr!("MDeleteWipeTitle"), tr!("MDeletingWiping"))
            } else {
                (tr!("MDeleteTitle"), tr!("MDeleting"))
            };
            Dialog::far(title, FAR_WIDTH)
                .row(vec![text_at(5, doing).literal()])
                .row(vec![
                    text_at(5, truncate_path(&p.current, FAR_TEXT)).literal(),
                ])
                .separator()
                .row(vec![text_at(5, line).literal()])
        }
    }
}

fn describe_report(kind: OpKind, dest: Option<&Path>, report: &OpReport) -> String {
    let mut out = format!("{}: {} item(s) done", kind.name(), report.done);
    if let Some(dest) = dest {
        out.push_str(&format!(" → {}", dest.display()));
    }
    if report.skipped > 0 {
        out.push_str(&format!(", {} existing file(s) skipped", report.skipped));
    }
    if !report.failed.is_empty() {
        out.push_str(&format!(", {} failed", report.failed.len()));
        for (p, e) in report.failed.iter().take(5) {
            out.push_str(&format!("\n  {}: {e}", p.display()));
        }
    }
    if report.cancelled {
        out.push_str(" (cancelled)");
    }
    out
}

impl App {
    /// A field asks for its history (docs/15): the list, the next match
    /// (Ctrl+End), lock, delete, clear.
    pub(super) fn dialog_history(&mut self, request: crate::dialog::HistoryRequest) {
        use crate::dialog::HistoryRequest as R;
        use crate::history::Kind;

        let refresh = match &request {
            R::Lock { list, text, locked } => {
                self.store.set_locked(Kind::Dialog, list, text, *locked);
                Some(list.clone())
            }
            R::Delete { list, text } => {
                self.store.delete(Kind::Dialog, list, text);
                Some(list.clone())
            }
            _ => None,
        };
        match request {
            R::Open { list } => {
                let items = self.history_lines(&list);
                if let Some(Overlay::Dialog { dialog, .. }) = self.overlays.last_mut() {
                    dialog.show_history(items);
                }
            }
            R::Next {
                list,
                prefix,
                after,
            } => {
                let next = self
                    .store
                    .next_matching(Kind::Dialog, &list, &prefix, &after);
                if let Some(text) = next
                    && let Some(Overlay::Dialog { dialog, .. }) = self.overlays.last_mut()
                {
                    dialog.set_focused_input(&text);
                }
            }
            R::Clear { list } => {
                let dialog = Dialog::message(
                    &tr!("MHistoryTitle"),
                    &[tr!("MHistoryClear")],
                    &[&tr!("MClear"), &tr!("MCancel")],
                    true,
                );
                self.overlays.push(Overlay::Dialog {
                    dialog,
                    purpose: Purpose::HistoryClear { list },
                });
            }
            R::Lock { .. } | R::Delete { .. } => {}
        }
        if let Some(list) = refresh {
            let items = self.history_lines(&list);
            if let Some(Overlay::Dialog { dialog, .. }) = self.overlays.last_mut() {
                dialog.refresh_history(items);
            }
        }
    }

    /// The focused field's history list (docs/15): its entries in the
    /// set order, with when / where, the agent's marked, missing paths
    /// grey; a path field also offers the passive panel and the folders the
    /// panels went to.
    fn history_lines(&mut self, list: &str) -> Vec<crate::dialog::HistLine> {
        use crate::dialog::HistLine;
        use crate::history::Kind;
        let path_field = match self.overlays.last_mut() {
            Some(Overlay::Dialog { dialog, .. }) => dialog.focused_field().is_some_and(|f| f.path),
            _ => false,
        };
        let order = self.history_order();
        let base = self.panels[self.active].path.clone();
        let line = |e: &crate::history::Entry, missing_check: bool| HistLine::Entry {
            text: e.text.clone(),
            locked: e.locked,
            agent: e.actor == "agent",
            missing: missing_check && path_missing(&e.text, &base),
            detail: entry_detail(e.last_used, &e.folder),
        };
        let mut lines: Vec<HistLine> = self
            .store
            .ordered(Kind::Dialog, list, &order)
            .iter()
            .map(|e| line(e, path_field))
            .collect();
        if path_field {
            let shown = |lines: &[HistLine], t: &str| {
                lines.iter().any(
                    |l| matches!(l, HistLine::Entry { text, .. } if text.eq_ignore_ascii_case(t)),
                )
            };
            let passive = self.panels[1 - self.active].path.display().to_string();
            if !shown(&lines, &passive) {
                lines.push(HistLine::Title(tr!("history-passive-panel")));
                lines.push(HistLine::Entry {
                    text: passive,
                    locked: false,
                    agent: false,
                    missing: false,
                    detail: String::new(),
                });
            }
            let folders: Vec<HistLine> = self
                .store
                .ordered(Kind::Folder, "", &order)
                .iter()
                .filter(|e| !shown(&lines, &e.text))
                .take(15)
                .map(|e| line(e, true))
                .collect();
            if !folders.is_empty() {
                lines.push(HistLine::Title(tr!("history-folders")));
                lines.extend(folders);
            }
        }
        lines
    }

    /// The order of history lists by the settings, for the active panel's
    /// folder.
    pub(super) fn history_order(&self) -> crate::history::Order {
        crate::history::Order {
            frecency: self.config.history.order == crate::config::HistoryOrder::Frecency,
            folder: self.panels[self.active].path.display().to_string(),
            agent_last: self.config.history.agent_entries == crate::config::AgentEntries::Marked,
        }
    }

    /// The text to keep in a history: secrets replaced (by the setting).
    pub(super) fn history_text(&self, text: &str) -> String {
        if self.config.history.redact_secrets {
            crate::history::redact(text)
        } else {
            text.to_string()
        }
    }

    /// Writes the fields' values into their histories, unless the dialog
    /// was cancelled (Far writes on any button; Cancel here does not).
    fn record_dialog_history(&mut self, dialog: &Dialog, button: usize, actor: Actor) {
        let cancel =
            [tr!("MCancel"), tr!("MSearchReplaceCancel")].map(|l| crate::dialog::visible(&l));
        if dialog
            .button_label(button)
            .is_some_and(|label| cancel.contains(&label))
        {
            return;
        }
        if !self.config.history.dialogs {
            return;
        }
        let folder = self.panels[self.active].path.display().to_string();
        let actor = match actor {
            Actor::Agent => "agent",
            _ => "user",
        };
        for (list, value) in dialog.history_values() {
            let kept = self.history_text(&value);
            self.store
                .add(crate::history::Kind::Dialog, &list, &kept, &folder, actor);
        }
    }

    /// New dialogs: empty fields marked `use_last` get their history's
    /// newest entry (once, when first drawn).
    pub(super) fn fill_dialogs_from_history(&mut self) {
        for overlay in &mut self.overlays {
            if let Overlay::Dialog { dialog, .. } = overlay
                && !dialog.history_filled()
            {
                let store = &self.store;
                // The agent's entries do not fill fields (unless mixed).
                let users_only =
                    self.config.history.agent_entries == crate::config::AgentEntries::Marked;
                dialog.fill_last(|list| {
                    if users_only {
                        store.last_by_user(crate::history::Kind::Dialog, list)
                    } else {
                        store.last(crate::history::Kind::Dialog, list)
                    }
                });
            }
        }
    }
}

/// "07.10 14:32 · far": when an entry was last used and in which folder.
fn which_kind(which: super::historymenu::HistoryMenu) -> crate::history::Kind {
    use super::historymenu::HistoryMenu;
    match which {
        HistoryMenu::Commands => crate::history::Kind::Command,
        HistoryMenu::Views => crate::history::Kind::View,
        HistoryMenu::Folders => crate::history::Kind::Folder,
    }
}

fn entry_detail(last_used_ms: i64, folder: &str) -> String {
    use chrono::TimeZone;
    let time = chrono::Local
        .timestamp_millis_opt(last_used_ms)
        .single()
        .map(|t| t.format("%d.%m %H:%M").to_string())
        .unwrap_or_default();
    let place: String = std::path::Path::new(folder)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| folder.to_string())
        .chars()
        .take(16)
        .collect();
    if place.is_empty() {
        time
    } else {
        format!("{time} · {place}")
    }
}

/// A path entry that does not exist now (relative to `base`); network
/// paths are not checked (they can hang).
fn path_missing(text: &str, base: &Path) -> bool {
    let t = text.trim().trim_matches('"');
    if t.is_empty() || t.starts_with("\\\\") || t.contains(';') {
        return false;
    }
    let p = Path::new(t);
    let full = if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    };
    !full.exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn far_text_helpers() {
        assert_eq!(truncate_right("abcdef", 4), "abc…");
        assert_eq!(truncate_center("abcdefgh", 5), "ab…gh");
        assert_eq!(truncate_path(r"C:\a\b\c\d\e", 8), r"C:\…\d\e");
        assert_eq!(quote_outer_space(" x"), "\" x\"");
        assert_eq!(quote_outer_space("x y"), "x y");
        assert_eq!(hms(Duration::from_secs(3725)), "01:02:05");
        assert_eq!(chars(&bar(1, 2)), 66);
        assert!(bar(1, 2).ends_with(" 50%"));
    }
}
