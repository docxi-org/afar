//! Autocompletion in the application (docs/15): after an edit of the
//! command line or a dialog's field the list of matches is recomputed
//! (`crate::complete`) and shown (`crate::completion`); while it is open,
//! keys go to it first.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::fileops::Overlay;
use super::{App, Focus};
use crate::completion::{Completion, Reply};
use crate::history::Kind;

/// Whose text the list completes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Owner {
    Cmdline,
    /// The focused field of the topmost dialog.
    Dialog,
}

pub(super) struct ActiveCompletion {
    owner: Owner,
    list: Completion,
    /// The history the candidates came from (for Shift+Del).
    kind: Kind,
    history: String,
}

/// Ctrl+Space (Far's manual completion).
fn is_ctrl_space(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char(' ') | KeyCode::Null)
}

impl App {
    /// The owner's text now.
    fn owner_text(&mut self, owner: Owner) -> Option<String> {
        match owner {
            Owner::Cmdline => Some(self.cmdline.clone()),
            Owner::Dialog => match self.overlays.last_mut() {
                Some(Overlay::Dialog { dialog, .. }) => dialog.focused_field().map(|f| f.value),
                _ => None,
            },
        }
    }

    fn set_owner_text(&mut self, owner: Owner, text: &str) {
        match owner {
            Owner::Cmdline => {
                self.cmdline = text.to_string();
                self.cmd_cursor = self.cmdline.chars().count();
            }
            Owner::Dialog => {
                if let Some(Overlay::Dialog { dialog, .. }) = self.overlays.last_mut() {
                    dialog.set_focused_input(text);
                }
            }
        }
    }

    /// Recomputes the list after an edit; `manual`: asked for (Ctrl+Space),
    /// so it shows even when automatic completion is off.
    pub(super) fn autocomplete(&mut self, owner: Owner, manual: bool) {
        let ac = self.config.autocomplete.clone();
        let enabled = match owner {
            Owner::Cmdline => ac.command_line,
            Owner::Dialog => ac.dialogs,
        };
        if !manual && (!enabled || !ac.show_list) {
            self.completion = None;
            return;
        }
        let (text, kind, history, path, exec) = match owner {
            Owner::Cmdline => (
                self.cmdline.clone(),
                Kind::Command,
                Some(String::new()),
                true,
                true,
            ),
            Owner::Dialog => {
                let Some(Overlay::Dialog { dialog, .. }) = self.overlays.last_mut() else {
                    self.completion = None;
                    return;
                };
                let Some(f) = dialog.focused_field() else {
                    self.completion = None;
                    return;
                };
                // Far: automatically only in fields with a history or paths.
                if !manual && f.history.is_none() && !f.path {
                    self.completion = None;
                    return;
                }
                (f.value, Kind::Dialog, f.history, f.path, f.exec)
            }
        };
        let sources = crate::complete::Sources {
            history: history.is_some() && ac.history.on(manual),
            files: path && ac.files.on(manual),
            variables: ac.variables.on(manual),
            programs: exec && ac.programs.on(manual),
        };
        let list = history.clone().unwrap_or_default();
        let entries: Vec<(String, bool)> = if sources.history {
            let order = self.history_order();
            self.store
                .ordered(kind, &list, &order)
                .into_iter()
                .map(|e| (e.text, e.locked))
                .collect()
        } else {
            Vec::new()
        };
        let base = self.panels[self.active].path.clone();
        let groups = crate::complete::complete(&text, &entries, sources, &base, &mut self.programs);
        // The command line's list opens upwards (Far's MenuUp).
        let up = owner == Owner::Cmdline
            || self
                .completion
                .as_ref()
                .is_some_and(|c| c.owner == owner && c.list.is_up());
        self.completion =
            Completion::new(&text, groups, ac.modal, up).map(|list| ActiveCompletion {
                owner,
                list,
                kind,
                history: list_name(&history),
            });
    }

    /// A key while the list is open; `true`: taken (also when it was passed
    /// on to the field and the list recomputed).
    pub(super) fn completion_key(&mut self, owner: Owner, key: &KeyEvent) -> bool {
        let Some(active) = &mut self.completion else {
            return false;
        };
        if active.owner != owner {
            return false;
        }
        match active.list.key(key) {
            Reply::SetText(text) => self.set_owner_text(owner, &text),
            Reply::Ignore => {}
            Reply::Close => self.completion = None,
            Reply::Accept(text) => {
                self.completion = None;
                self.set_owner_text(owner, &text);
            }
            Reply::DeleteHistory(text) => {
                let (kind, list) = (active.kind, active.history.clone());
                self.store.delete(kind, &list, &text);
                self.autocomplete(owner, true);
            }
            Reply::Pass => {
                let before = self.owner_text(owner);
                match owner {
                    Owner::Cmdline => {
                        self.cmdline_key(key);
                    }
                    Owner::Dialog => {
                        if let Some(Overlay::Dialog { dialog, .. }) = self.overlays.last_mut() {
                            dialog.handle_key(key);
                        }
                    }
                }
                if self.owner_text(owner) != before {
                    self.autocomplete(owner, false);
                }
            }
            Reply::CloseAndPass => {
                self.completion = None;
                return false;
            }
        }
        true
    }

    /// Keys of a dialog with completion: the open list first, Ctrl+Space,
    /// then the dialog — and an edit recomputes the list. `false`: the key
    /// is the dialog's to handle as usual.
    pub(super) fn dialog_completion_key(&mut self, key: &KeyEvent) -> bool {
        if self.completion_key(Owner::Dialog, key) {
            return true;
        }
        if is_ctrl_space(key) {
            self.autocomplete(Owner::Dialog, true);
            return true;
        }
        false
    }

    /// After a key the dialog handled: an edit of the field recomputes the
    /// list.
    pub(super) fn dialog_edited(&mut self, before: Option<String>) {
        if before.is_some() && self.owner_text(Owner::Dialog) != before {
            self.autocomplete(Owner::Dialog, false);
        }
    }

    pub(super) fn dialog_field_text(&mut self) -> Option<String> {
        self.owner_text(Owner::Dialog)
    }

    /// After a key of the panels: an edit of the command line recomputes
    /// the list.
    pub(super) fn cmdline_edited(&mut self, before: &str) {
        if self.cmdline != before {
            if self.cmdline.is_empty() {
                self.completion = None;
            } else {
                self.autocomplete(Owner::Cmdline, false);
            }
        }
    }

    /// The mouse while the list is open; `true`: taken.
    pub(super) fn completion_mouse(&mut self, ev: &MouseEvent) -> bool {
        let Some(active) = &mut self.completion else {
            return false;
        };
        let owner = active.owner;
        match active.list.mouse(ev) {
            None => false,
            Some(Reply::Close) => {
                self.completion = None;
                false
            }
            Some(Reply::Accept(text)) => {
                self.completion = None;
                self.set_owner_text(owner, &text);
                true
            }
            Some(Reply::SetText(text)) => {
                self.set_owner_text(owner, &text);
                true
            }
            Some(_) => true,
        }
    }

    /// Draws the open list over everything, at its field; drops it when
    /// its field is gone (a dialog closed, another screen, the focus).
    pub(super) fn draw_completion(
        &mut self,
        area: Rect,
        cmdline: Rect,
        prompt: u16,
        buf: &mut Buffer,
    ) {
        let Some(owner) = self.completion.as_ref().map(|c| c.owner) else {
            return;
        };
        let anchor = match owner {
            Owner::Cmdline => {
                let ok = !self.has_overlay()
                    && self.focus == Focus::Panels
                    && self.shown_viewer().is_none()
                    && cmdline.height > 0;
                ok.then(|| {
                    Rect::new(
                        cmdline.x + prompt.min(cmdline.width),
                        cmdline.y,
                        cmdline.width.saturating_sub(prompt),
                        1,
                    )
                })
            }
            Owner::Dialog => match self.overlays.last_mut() {
                Some(Overlay::Dialog { dialog, .. }) => dialog.focused_field().map(|f| f.rect),
                _ => None,
            },
        };
        match (anchor, &mut self.completion) {
            (Some(anchor), Some(active)) if anchor.width > 0 => active.list.draw(buf, area, anchor),
            _ => self.completion = None,
        }
    }
}

fn list_name(history: &Option<String>) -> String {
    history.clone().unwrap_or_default()
}
