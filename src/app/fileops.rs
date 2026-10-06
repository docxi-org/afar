//! File operations in the UI: Far-style dialogs (F5–F8), operations running
//! in the background, their progress window and questions on errors and
//! existing files. Dialogs live in the overlay: modal for input, while the
//! agent pane and background work keep going.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use crossterm::event::{KeyCode, KeyEvent, MouseEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use tokio::sync::oneshot;

use super::{App, AppMsg, Focus};
use crate::dialog::{Dialog, Outcome};
use crate::journal::{Actor, Event};
use crate::mcp::Reply;
use crate::ops::{
    self, ConflictAction, ConflictAnswer, CopyJob, ErrorAnswer, FileInfo, OpId, OpKind, OpMsg,
    OpReport, Overwrite,
};
use crate::panel::group_thousands;

/// Longest path lists kept in journal entries.
const LIST_LIMIT: usize = 20;

/// Choices of "existing files" in the copy dialog, in order.
const OVERWRITE_CHOICES: [(&str, Overwrite); 6] = [
    ("Спрашивать", Overwrite::Ask),
    ("Заменять", Overwrite::Replace),
    ("Пропускать", Overwrite::Skip),
    ("Заменять, если новее", Overwrite::ReplaceIfNewer),
    ("Переименовывать", Overwrite::Rename),
    ("Дописывать", Overwrite::Append),
];

pub(super) enum Overlay {
    Dialog { dialog: Dialog, purpose: Purpose },
    Progress(Progress),
}

/// What a dialog was opened for, i.e. what to do when it closes.
pub(super) enum Purpose {
    MkDir {
        side: usize,
    },
    Delete {
        targets: Vec<PathBuf>,
        permanent: bool,
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
    Conflict {
        reply: mpsc::Sender<ConflictAnswer>,
    },
    Message,
}

pub(super) struct Progress {
    op: OpId,
    title: String,
    done: usize,
    total: usize,
    bytes_done: u64,
    bytes_total: u64,
    current: String,
}

pub(super) struct RunningOp {
    kind: OpKind,
    actor: Actor,
    reply: Option<oneshot::Sender<Reply>>,
    cancel: Arc<AtomicBool>,
    /// Item to put the cursor on when done: panel and name.
    focus: Option<(usize, String)>,
    /// Where things go (copy/move), for the agent's answer.
    dest: Option<PathBuf>,
}

/// "1 объект", "3 объекта", "5 объектов".
fn objects(n: usize) -> String {
    let word = match (n % 10, n % 100) {
        (1, r) if r != 11 => "объект",
        (2..=4, r) if !(12..=14).contains(&r) => "объекта",
        _ => "объектов",
    };
    format!("{n} {word}")
}

fn name_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

fn limited(paths: &[PathBuf]) -> Vec<PathBuf> {
    paths.iter().take(LIST_LIMIT).cloned().collect()
}

fn describe_file(info: &FileInfo) -> String {
    let time = info
        .modified
        .map(|t| {
            let t: chrono::DateTime<chrono::Local> = t.into();
            t.format("%d.%m.%Y %H:%M:%S").to_string()
        })
        .unwrap_or_default();
    format!("{:>16} байт   {time}", group_thousands(info.size))
}

/// A progress bar of `width` cells.
fn bar(done: u64, total: u64, width: u16) -> String {
    let width = u64::from(width);
    let filled = (width * done).checked_div(total).unwrap_or(0).min(width) as usize;
    format!(
        "{}{}",
        "█".repeat(filled),
        "░".repeat(width as usize - filled)
    )
}

impl App {
    pub(super) fn has_overlay(&self) -> bool {
        !self.overlays.is_empty()
    }

    pub(super) fn message(&mut self, title: &str, lines: &[String], warning: bool) {
        let width = lines
            .iter()
            .map(|l| l.chars().count())
            .max()
            .unwrap_or(20)
            .clamp(30, 70) as u16;
        let mut d = Dialog::new(title, width);
        if warning {
            d = d.warning();
        }
        for l in lines {
            for part in l.lines() {
                d = d.wrapped(part);
            }
        }
        let dialog = d.separator().buttons(&["OK"], 0);
        self.overlays.push(Overlay::Dialog {
            dialog,
            purpose: Purpose::Message,
        });
    }

    // ------------------------------------------------------------ input

    pub(super) fn overlay_key(&mut self, key: KeyEvent) {
        match self.overlays.last_mut() {
            Some(Overlay::Progress(p)) => {
                if key.code == KeyCode::Esc {
                    if let Some(op) = self.ops.get(&p.op) {
                        op.cancel.store(true, Ordering::SeqCst);
                    }
                    p.title = format!("{} — отмена…", p.title.trim_end_matches(" — отмена…"));
                }
            }
            Some(Overlay::Dialog { dialog, .. }) => {
                if let Outcome::Closed(button) = dialog.handle_key(&key) {
                    self.close_dialog(button);
                }
            }
            None => {}
        }
    }

    /// Mouse over the overlay; clicks outside the top dialog are ignored.
    pub(super) fn overlay_mouse(&mut self, ev: &MouseEvent) {
        if let Some(Overlay::Dialog { dialog, .. }) = self.overlays.last_mut()
            && let Some(Outcome::Closed(button)) = dialog.handle_mouse(ev)
        {
            self.close_dialog(button);
        }
    }

    fn close_dialog(&mut self, button: Option<usize>) {
        let Some(Overlay::Dialog { dialog, purpose }) = self.overlays.pop() else {
            return;
        };
        match purpose {
            Purpose::MkDir { side } => {
                if button == Some(0) {
                    let text = dialog.input_value(0).to_string();
                    let names: Vec<String> = if dialog.checked(0) {
                        text.split(';').map(str::to_string).collect()
                    } else {
                        vec![text]
                    };
                    if let Err(e) = self.mkdir(side, Actor::User, &names) {
                        self.message("Ошибка", &[e], true);
                    }
                }
            }
            Purpose::Delete {
                targets,
                permanent,
                actor,
                reply,
            } => {
                if button == Some(0) {
                    self.start_delete(targets, permanent, actor, reply);
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
                let overwrite = OVERWRITE_CHOICES[dialog.radio(0)].1;
                if let Err(e) =
                    self.start_copy(sources, &dest, moving, overwrite, side, actor, reply)
                {
                    self.message("Ошибка", &[e], true);
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
            Purpose::Conflict { reply } => {
                let action = match button {
                    Some(0) => ConflictAction::Replace,
                    Some(1) => ConflictAction::Skip,
                    Some(2) => ConflictAction::Rename,
                    Some(3) => ConflictAction::Append,
                    _ => ConflictAction::Cancel,
                };
                let _ = reply.send(ConflictAnswer {
                    action,
                    all: dialog.checked(0),
                });
            }
            Purpose::Message => {}
        }
    }

    pub(super) fn draw_overlays(&mut self, area: Rect, buf: &mut Buffer) -> Option<Position> {
        let mut cursor = None;
        for overlay in &mut self.overlays {
            cursor = match overlay {
                Overlay::Dialog { dialog, .. } => dialog.draw(area, buf),
                Overlay::Progress(p) => {
                    let width = 50u16;
                    let mut d = Dialog::new(&p.title, width).text(p.current.clone());
                    if p.bytes_total > 0 {
                        d = d
                            .text(bar(p.bytes_done, p.bytes_total, width))
                            .center(format!(
                                "{} из {} байт",
                                group_thousands(p.bytes_done),
                                group_thousands(p.bytes_total)
                            ))
                            .center(format!("объектов: {} из {}", p.done, p.total));
                    } else {
                        d = d
                            .text(bar(p.done as u64, p.total as u64, width))
                            .center(format!("{} из {}", p.done, p.total));
                    }
                    d.separator().center("Esc — отменить").draw(area, buf);
                    None
                }
            };
        }
        cursor
    }

    // ------------------------------------------------------------- mkdir

    pub(super) fn mkdir_dialog(&mut self) {
        let dialog = Dialog::new("Создание папки", 60)
            .text("Создать папку:")
            .input("")
            .check("Обработать несколько имён (через ;)", false)
            .separator()
            .buttons(&["OK", "Отмена"], 0);
        self.overlays.push(Overlay::Dialog {
            dialog,
            purpose: Purpose::MkDir { side: self.active },
        });
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
        if created.is_empty() && report.failed.is_empty() {
            return Err("не задано имя папки".into());
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
            Some((path, e)) => Err(format!("{}: {e}", path.display())),
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

    pub(super) fn delete_dialog(
        &mut self,
        targets: Vec<PathBuf>,
        permanent: bool,
        actor: Actor,
        reply: Option<oneshot::Sender<Reply>>,
    ) {
        if targets.is_empty() {
            return;
        }
        let what = if targets.len() == 1 {
            name_of(&targets[0])
        } else {
            objects(targets.len())
        };
        let question = if permanent {
            "Вы хотите безвозвратно удалить"
        } else {
            "Вы хотите удалить в Корзину"
        };
        let mut d = Dialog::new("Удаление", 50)
            .warning()
            .center(question)
            .center(what);
        if actor == Actor::Agent {
            d = d.center("— запрошено агентом —");
        }
        let dialog = d.separator().buttons(&["Удалить", "Отмена"], 0);
        self.overlays.push(Overlay::Dialog {
            dialog,
            purpose: Purpose::Delete {
                targets,
                permanent,
                actor,
                reply,
            },
        });
    }

    fn start_delete(
        &mut self,
        targets: Vec<PathBuf>,
        permanent: bool,
        actor: Actor,
        reply: Option<oneshot::Sender<Reply>>,
    ) {
        let id = self.next_op_id();
        let kind = if permanent {
            OpKind::Delete
        } else {
            OpKind::Trash
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
        let cancel = Arc::new(AtomicBool::new(false));
        let tx = self.tx.clone();
        let started = ops::spawn_delete(id, targets, permanent, cancel.clone(), move |m| {
            let _ = tx.send(AppMsg::Op(m));
        });
        let title = if permanent {
            "Удаление"
        } else {
            "Удаление в Корзину"
        };
        self.track_op(id, kind, actor, reply, cancel, None, None, title, started);
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
            let other = &self.panels[1 - self.active].path;
            let mut s = other.display().to_string();
            if !s.ends_with(std::path::MAIN_SEPARATOR) {
                s.push(std::path::MAIN_SEPARATOR);
            }
            s
        };
        self.open_copy_dialog(sources, dest, moving, self.active, Actor::User, None);
    }

    fn open_copy_dialog(
        &mut self,
        sources: Vec<PathBuf>,
        dest: String,
        moving: bool,
        side: usize,
        actor: Actor,
        reply: Option<oneshot::Sender<Reply>>,
    ) {
        let what = if sources.len() == 1 {
            format!("«{}»", name_of(&sources[0]))
        } else {
            objects(sources.len())
        };
        let (title, prompt, button) = if moving {
            (
                "Переименование/перенос",
                format!("Переименовать или перенести {what} в:"),
                "Перенести",
            )
        } else {
            ("Копирование", format!("Копировать {what} в:"), "Копировать")
        };
        let labels: Vec<&str> = OVERWRITE_CHOICES.iter().map(|(l, _)| *l).collect();
        let mut d = Dialog::new(title, 64)
            .wrapped(&prompt)
            .input(dest)
            .separator()
            .text("Уже существующие файлы:")
            .radios(&labels, 0);
        if actor == Actor::Agent {
            d = d.center("— запрошено агентом —");
        }
        let dialog = d.separator().buttons(&[button, "Отмена"], 0);
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
            return fail(reply, "не задано, куда".into());
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
        let cancel = Arc::new(AtomicBool::new(false));
        let tx = self.tx.clone();
        let started = ops::spawn_copy(id, pairs, moving, overwrite, cancel.clone(), move |m| {
            let _ = tx.send(AppMsg::Op(m));
        });
        let title = if moving {
            "Перенос"
        } else {
            "Копирование"
        };
        self.track_op(
            id,
            kind,
            actor,
            reply,
            cancel,
            focus,
            Some(dest),
            title,
            started,
        );
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
        cancel: Arc<AtomicBool>,
        focus: Option<(usize, String)>,
        dest: Option<PathBuf>,
        title: &str,
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
                        cancel,
                        focus,
                        dest,
                    },
                );
                self.overlays.push(Overlay::Progress(Progress {
                    op: id,
                    title: title.into(),
                    done: 0,
                    total: 0,
                    bytes_done: 0,
                    bytes_total: 0,
                    current: String::new(),
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
                current,
            } => {
                for o in &mut self.overlays {
                    if let Overlay::Progress(p) = o
                        && p.op == id
                    {
                        p.done = done;
                        p.total = total;
                        p.bytes_done = bytes_done;
                        p.bytes_total = bytes_total;
                        p.current = current.display().to_string();
                    }
                }
            }
            OpMsg::Error {
                path, error, reply, ..
            } => {
                let dialog = Dialog::new("Ошибка", 60)
                    .warning()
                    .text("Не удалось обработать")
                    .text(path.display().to_string())
                    .wrapped(&error)
                    .separator()
                    .buttons(&["Повторить", "Пропустить", "Пропустить все", "Отмена"], 0);
                self.ask(dialog, Purpose::OpError { reply });
            }
            OpMsg::Conflict {
                target,
                new,
                existing,
                reply,
                ..
            } => {
                let dialog = Dialog::new("Предупреждение", 64)
                    .warning()
                    .center("Файл уже существует")
                    .text(target.display().to_string())
                    .separator()
                    .text(format!("Новый:        {}", describe_file(&new)))
                    .text(format!("Существующий: {}", describe_file(&existing)))
                    .separator()
                    .check("Применить ко всем", false)
                    .buttons(
                        &[
                            "Заменить",
                            "Пропустить",
                            "Переименовать",
                            "Дописать",
                            "Отмена",
                        ],
                        0,
                    );
                self.ask(dialog, Purpose::Conflict { reply });
            }
            OpMsg::Finished { id, report } => {
                self.overlays
                    .retain(|o| !matches!(o, Overlay::Progress(p) if p.op == id));
                let Some(op) = self.ops.remove(&id) else {
                    return;
                };
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

    /// Shows a question from a running operation.
    fn ask(&mut self, dialog: Dialog, purpose: Purpose) {
        self.overlays.push(Overlay::Dialog { dialog, purpose });
        // The question needs the user even if they were talking to the agent.
        if self.focus == Focus::Agent {
            self.say("Файловая операция ждёт ответа — Ctrl+Space");
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
    fn present_agent_request(&mut self, side: usize, names: &[String], what: &str) {
        self.set_panels_visible(true);
        self.panels[side].select_names(names, false);
        self.focus = Focus::Panels;
        self.say(format!("Агент просит {what}"));
    }

    /// `afar_delete`: asks the user in a dialog; the reply is sent when the
    /// deletion has finished or the user declined.
    pub(super) fn agent_delete(
        &mut self,
        side: usize,
        names: &[String],
        permanent: bool,
        reply: oneshot::Sender<Reply>,
    ) {
        match self.agent_sources(side, names) {
            Ok(targets) => {
                self.present_agent_request(
                    side,
                    names,
                    &format!("удалить {}", objects(targets.len())),
                );
                self.delete_dialog(targets, permanent, Actor::Agent, Some(reply));
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
        match self.agent_sources(side, names) {
            Ok(sources) => {
                let what = if moving {
                    "перенести"
                } else {
                    "скопировать"
                };
                self.present_agent_request(
                    side,
                    names,
                    &format!("{what} {}", objects(sources.len())),
                );
                self.open_copy_dialog(sources, dest, moving, side, Actor::Agent, Some(reply));
            }
            Err(e) => {
                let _ = reply.send(Err(e));
            }
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

#[cfg(test)]
mod tests {
    use super::{bar, objects};

    #[test]
    fn plural_forms() {
        assert_eq!(objects(1), "1 объект");
        assert_eq!(objects(3), "3 объекта");
        assert_eq!(objects(5), "5 объектов");
        assert_eq!(objects(11), "11 объектов");
        assert_eq!(objects(22), "22 объекта");
        assert_eq!(objects(112), "112 объектов");
    }

    #[test]
    fn progress_bar() {
        assert_eq!(bar(1, 2, 4), "██░░");
        assert_eq!(bar(0, 0, 3), "░░░");
    }
}
