//! The links to Claude Code beyond MCP and hooks (docs/11-viewer-editor.md,
//! "Связь с Claude Code"): afar as the agent's IDE (`crate::ide`) — the
//! agent's edits to review (`openDiff`), the viewer's selection and
//! Ctrl+Enter reaching the agent (`selection_changed`, `at_mentioned`) —
//! and the channel that wakes the agent (`afar channel`).

use serde_json::json;

use super::fileops::{Overlay, Purpose};
use super::{App, Focus};
use crate::dialog::Dialog;
use crate::ide::{DiffAnswer, IdeMsg};
use crate::tr;

/// A viewer's file, found text and top, as last told to the agent.
pub(super) type SentSelection = (std::path::PathBuf, Option<(u64, u64)>, u64);

impl App {
    pub(super) fn on_ide(&mut self, msg: IdeMsg) {
        match msg {
            IdeMsg::Connected { .. } => {
                self.ide_connected = true;
                // Tell it what is shown now.
                self.ide_sent = None;
                self.say(tr!("ide-connected"));
            }
            IdeMsg::Disconnected => {
                if self.ide_connected {
                    self.say(tr!("ide-disconnected"));
                }
                self.ide_connected = false;
            }
            IdeMsg::OpenDiff {
                path,
                new_contents,
                tab_name,
                reply,
            } => {
                let old = std::fs::read_to_string(&path).unwrap_or_default();
                let lines = tr!(
                    "ide-diff-lines",
                    old = old.lines().count(),
                    new = new_contents.lines().count()
                );
                let dialog = Dialog::message(
                    &tr!("ide-diff-title"),
                    &[path.display().to_string(), lines],
                    &[&tr!("ide-diff-accept"), &tr!("ide-diff-reject")],
                    false,
                );
                self.overlays.push(Overlay::Dialog {
                    dialog,
                    purpose: Purpose::IdeDiff {
                        tab_name,
                        new_contents,
                        reply: Some(reply),
                    },
                });
            }
            // Answered in the agent's terminal: the dialog goes.
            IdeMsg::CloseTab { tab_name } => {
                if let Some(i) = self.overlays.iter().position(|o| {
                    matches!(o, Overlay::Dialog { purpose: Purpose::IdeDiff { tab_name: t, .. }, .. } if *t == tab_name)
                }) && let Overlay::Dialog {
                    purpose: Purpose::IdeDiff { reply, .. },
                    ..
                } = self.overlays.remove(i)
                    && let Some(reply) = reply
                {
                    let _ = reply.send(DiffAnswer::Closed);
                }
            }
        }
    }

    /// `selection_changed` when the shown viewer, its found text or its
    /// position changes (called from the main loop's tick).
    pub(super) fn ide_sync_selection(&mut self) {
        if !self.ide_connected {
            return;
        }
        let Some(i) = self.shown_viewer() else {
            return;
        };
        let v = &mut self.viewers[i];
        let now = (v.path().to_path_buf(), v.selection, v.top);
        if self.ide_sent.as_ref() == Some(&now) {
            return;
        }
        let (path, selection, top) = now.clone();
        self.ide_sent = Some(now);
        let (from, to) = selection.unwrap_or((top, top));
        let (l1, c1) = v.line_col(from);
        let (l2, c2) = v.line_col(to);
        let text = if selection.is_some() {
            v.text_between(from, to)
        } else {
            String::new()
        };
        if let Some(ide) = &self.ide {
            ide.notify(
                "selection_changed",
                json!({
                    "text": text,
                    "filePath": path.display().to_string(),
                    "fileUrl": format!("file:///{}", path.display().to_string().replace('\\', "/")),
                    "selection": {
                        "start": {"line": l1, "character": c1},
                        "end": {"line": l2, "character": c2},
                        "isEmpty": selection.is_none(),
                    },
                }),
            );
        }
    }

    /// Ctrl+Enter in a viewer: `@file#Lа-b` of the found text (or of the
    /// shown lines) into the agent's input, and the focus to the agent.
    pub(super) fn ide_mention(&mut self, i: usize) {
        let Some(ide) = &self.ide else {
            self.say(tr!("viewer-not-yet"));
            return;
        };
        let v = &mut self.viewers[i];
        let (from, to) = match v.selection {
            Some(s) => s,
            None => (v.top, v.visible_end().saturating_sub(1)),
        };
        let (l1, _) = v.line_col(from);
        let (l2, _) = v.line_col(to);
        ide.notify(
            "at_mentioned",
            json!({
                "filePath": v.path().display().to_string(),
                "lineStart": l1,
                "lineEnd": l2,
            }),
        );
        self.focus = Focus::Agent;
    }

    /// An event for the agent's channel: it starts the agent's turn.
    pub(super) fn channel_send(&mut self, content: String, meta: &[(&str, &str)]) {
        let meta: serde_json::Map<String, serde_json::Value> = meta
            .iter()
            .map(|(k, v)| (k.to_string(), json!(v)))
            .collect();
        self.channel_events
            .push(json!({"content": content, "meta": meta}));
        if let Some(waiter) = self.channel_waiter.take() {
            let events = std::mem::take(&mut self.channel_events);
            let _ = waiter.send(Ok(json!(events).to_string()));
        }
    }

    /// `afar channel` asks for events: now if there are some, else when
    /// one comes (the request times out empty in the bridge's loop).
    pub(super) fn channel_wait(&mut self, reply: tokio::sync::oneshot::Sender<crate::mcp::Reply>) {
        if self.channel_events.is_empty() {
            // A newer request replaces an older one (its bridge gave up).
            if let Some(old) = self.channel_waiter.replace(reply) {
                let _ = old.send(Ok("[]".into()));
            }
        } else {
            let events = std::mem::take(&mut self.channel_events);
            let _ = reply.send(Ok(json!(events).to_string()));
        }
    }

    /// Starts or stops afar's IDE server (`[agent] ide`); the agent finds it
    /// when it starts next time.
    pub(super) fn set_ide(&mut self, enabled: bool) {
        if enabled == self.ide.is_some() {
            return;
        }
        if !enabled {
            // Dropping it removes the lock file.
            self.ide = None;
            self.ide_connected = false;
            return;
        }
        let dir = self.journal.dir().to_path_buf();
        match crate::ide::start(self.tx.clone(), super::new_uuid(), &dir) {
            Ok(server) => self.ide = Some(server),
            Err(e) => self.say(tr!("ide-start-failed", error = format!("{e:#}"))),
        }
    }
}
