//! File operations in the UI: Far-style dialogs (F7, F8), operations running
//! in the background, their progress window and questions on errors.
//! Dialogs live in the overlay: modal for input, while the agent pane and
//! background work keep going.

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
use crate::ops::{self, ErrorAnswer, OpId, OpKind, OpMsg, OpReport};

/// Longest path lists kept in journal entries.
const LIST_LIMIT: usize = 20;

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
    OpError {
        reply: mpsc::Sender<ErrorAnswer>,
    },
    Message,
}

pub(super) struct Progress {
    op: OpId,
    title: String,
    done: usize,
    total: usize,
    current: String,
}

pub(super) struct RunningOp {
    kind: OpKind,
    actor: Actor,
    reply: Option<oneshot::Sender<Reply>>,
    cancel: Arc<AtomicBool>,
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
            d = d.text(l.clone());
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
            Purpose::OpError { reply } => {
                let answer = match button {
                    Some(0) => ErrorAnswer::Retry,
                    Some(1) => ErrorAnswer::Skip,
                    Some(2) => ErrorAnswer::SkipAll,
                    _ => ErrorAnswer::Cancel,
                };
                let _ = reply.send(answer);
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
                    let filled = (usize::from(width) * p.done)
                        .checked_div(p.total)
                        .unwrap_or(0)
                        .min(usize::from(width));
                    let bar = format!(
                        "{}{}",
                        "█".repeat(filled),
                        "░".repeat(usize::from(width) - filled)
                    );
                    let mut d = Dialog::new(&p.title, width)
                        .text(p.current.clone())
                        .text(bar)
                        .center(format!("{} из {}", p.done, p.total))
                        .separator()
                        .center("Esc — отменить");
                    d.draw(area, buf);
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

    /// Items to delete: the selection, or the item under the cursor.
    pub(super) fn delete_targets(&self, current_only: bool) -> Vec<PathBuf> {
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
        let total = targets.len();
        match ops::spawn_delete(id, targets, permanent, cancel.clone(), move |m| {
            let _ = tx.send(AppMsg::Op(m));
        }) {
            Ok(()) => {
                self.ops.insert(
                    id,
                    RunningOp {
                        kind,
                        actor,
                        reply,
                        cancel,
                    },
                );
                self.overlays.push(Overlay::Progress(Progress {
                    op: id,
                    title: if permanent {
                        "Удаление".into()
                    } else {
                        "Удаление в Корзину".into()
                    },
                    done: 0,
                    total,
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
                current,
            } => {
                for o in &mut self.overlays {
                    if let Overlay::Progress(p) = o
                        && p.op == id
                    {
                        p.done = done;
                        p.total = total;
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
                self.overlays.push(Overlay::Dialog {
                    dialog,
                    purpose: Purpose::OpError { reply },
                });
                // An error needs the user even if they were talking to the agent.
                if self.focus == Focus::Agent {
                    self.say("Ошибка файловой операции ждёт ответа — Ctrl+Space");
                }
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
                if let Some(reply) = op.reply {
                    let _ = reply.send(Ok(describe_report(op.kind, &report)));
                }
            }
        }
    }

    fn journal_finished(&mut self, id: OpId, kind: OpKind, actor: Actor, report: &OpReport) {
        self.journal.push(
            actor,
            Event::FileOpFinished {
                op_id: id,
                op: kind,
                done: report.done,
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

    /// `afar_delete`: asks the user in a dialog; the reply is sent when the
    /// deletion has finished or the user declined.
    pub(super) fn agent_delete(
        &mut self,
        side: usize,
        names: &[String],
        permanent: bool,
        reply: oneshot::Sender<Reply>,
    ) {
        let base = self.panels[side].path.clone();
        let (targets, missing): (Vec<PathBuf>, Vec<PathBuf>) = names
            .iter()
            .map(|n| base.join(n.trim_matches('"')))
            .partition(|p| std::fs::symlink_metadata(p).is_ok());
        if targets.is_empty() {
            let _ = reply.send(Err(format!("nothing to delete: not found {missing:?}")));
            return;
        }
        self.set_panels_visible(true);
        self.panels[side].select_names(names, false);
        self.focus = Focus::Panels;
        self.say(format!("Агент просит удалить {}", objects(targets.len())));
        self.delete_dialog(targets, permanent, Actor::Agent, Some(reply));
    }
}

fn describe_report(kind: OpKind, report: &OpReport) -> String {
    let mut out = format!("{}: {} item(s) done", kind.name(), report.done);
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
    use super::objects;

    #[test]
    fn plural_forms() {
        assert_eq!(objects(1), "1 объект");
        assert_eq!(objects(3), "3 объекта");
        assert_eq!(objects(5), "5 объектов");
        assert_eq!(objects(11), "11 объектов");
        assert_eq!(objects(22), "22 объекта");
        assert_eq!(objects(112), "112 объектов");
    }
}
