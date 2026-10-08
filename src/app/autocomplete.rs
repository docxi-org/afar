//! Autocompletion in the application (docs/15): after an edit of the
//! command line or a dialog's field the matches are recomputed
//! (`crate::complete`) and shown — as the ghost suggestion after the text,
//! or as the list (`crate::completion`); while the list is open, keys go
//! to it first.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::fileops::Overlay;
use super::{App, Focus};
use crate::completion::{Completion, Reply};
use crate::config::Suggest;
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
                self.cmd_anchor = None;
            }
            Owner::Dialog => {
                if let Some(Overlay::Dialog { dialog, .. }) = self.overlays.last_mut() {
                    dialog.set_focused_input(text);
                }
            }
        }
    }

    /// Drops the owner's ghost suggestion.
    fn clear_ghost(&mut self, owner: Owner) {
        match owner {
            Owner::Cmdline => self.cmd_ghost = None,
            Owner::Dialog => {
                if let Some(Overlay::Dialog { dialog, .. }) = self.overlays.last_mut() {
                    dialog.set_ghost(None);
                }
            }
        }
    }

    /// Recomputes the suggestion after an edit: the ghost or the list, as
    /// set; `manual`: the list is asked for (Ctrl+Space), so it shows even
    /// when automatic completion is off.
    pub(super) fn autocomplete(&mut self, owner: Owner, manual: bool) {
        let ac = self.config.autocomplete.clone();
        let enabled = match owner {
            Owner::Cmdline => ac.command_line,
            Owner::Dialog => ac.dialogs,
        };
        self.clear_ghost(owner);
        // A list asked for stays while the text is edited.
        let list_open = self.completion.as_ref().is_some_and(|c| c.owner == owner);
        let want_list = manual || list_open || (enabled && ac.suggest == Suggest::List);
        let want_ghost = !want_list && enabled && ac.suggest == Suggest::Ghost;
        if !want_list && !want_ghost {
            self.completion = None;
            return;
        }
        let (text, kind, history, path, exec, at_end) = match owner {
            Owner::Cmdline => (
                self.cmdline.clone(),
                Kind::Command,
                Some(String::new()),
                true,
                true,
                self.cmd_cursor >= self.cmdline.chars().count(),
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
                (f.value, Kind::Dialog, f.history, f.path, f.exec, f.at_end)
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
        let passive = &self.panels[1 - self.active];
        let passive_names: Vec<String> = passive.entries.iter().map(|e| e.name.clone()).collect();
        let ctx = crate::complete::Context {
            base: &base,
            // The same folder adds nothing to the files.
            passive: (passive.path != base).then_some((passive.path.as_path(), &passive_names[..])),
            fuzzy: ac.fuzzy,
        };
        if want_ghost {
            self.completion = None;
            if !at_end {
                return;
            }
            let rest =
                crate::complete::ghost(&text, &entries, sources, &ctx, &mut self.complete_cache);
            let ghost = rest.map(|r| (text, r));
            match owner {
                Owner::Cmdline => self.cmd_ghost = ghost,
                Owner::Dialog => {
                    if let Some(Overlay::Dialog { dialog, .. }) = self.overlays.last_mut() {
                        dialog.set_ghost(ghost);
                    }
                }
            }
            return;
        }
        let groups =
            crate::complete::complete(&text, &entries, sources, &ctx, &mut self.complete_cache);
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

    /// The command line's ghost, when it is to be seen: the line is the
    /// one it was made for, the cursor at its end, the panels have the keys.
    pub(super) fn shown_cmd_ghost(&self) -> Option<String> {
        let (base, rest) = self.cmd_ghost.as_ref()?;
        (*base == self.cmdline
            && self.cmd_cursor >= self.cmdline.chars().count()
            && self.focus == Focus::Panels
            && !self.has_overlay()
            && self.completion.is_none()
            && self.shown_viewer().is_none())
        .then(|| rest.clone())
    }

    /// Takes the ghost suggestion: `→` (and `End` in a field) at the end of
    /// the text — all of it, `Ctrl+→` — a word. `true`: taken.
    pub(super) fn ghost_key(&mut self, owner: Owner, key: &KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let plain = key.modifiers.difference(KeyModifiers::SHIFT).is_empty();
        let word = match key.code {
            KeyCode::Right if ctrl => true,
            KeyCode::Right if plain => false,
            KeyCode::End if plain && owner == Owner::Dialog => false,
            _ => return false,
        };
        let open_list = self.completion.is_some();
        let (base, rest) = match owner {
            Owner::Cmdline => match self.shown_cmd_ghost() {
                Some(rest) => (self.cmdline.clone(), rest),
                None => return false,
            },
            Owner::Dialog => {
                let Some(Overlay::Dialog { dialog, .. }) = self.overlays.last_mut() else {
                    return false;
                };
                let Some(f) = dialog.focused_field() else {
                    return false;
                };
                match dialog.ghost() {
                    Some((base, rest)) if base == f.value && f.at_end && !open_list => (base, rest),
                    _ => return false,
                }
            }
        };
        let taken = if word { first_word(&rest) } else { rest };
        self.set_owner_text(owner, &format!("{base}{taken}"));
        self.autocomplete(owner, false);
        true
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

/// The ghost's first word: separators before it, the word, and a path's
/// `\` after it.
fn first_word(rest: &str) -> String {
    let sep = |c: char| c.is_whitespace() || matches!(c, '\\' | '/' | '"');
    let lead = rest.chars().take_while(|c| sep(*c)).count();
    let word = rest.chars().skip(lead).take_while(|c| !sep(*c)).count();
    let mut n = lead + word;
    if rest.chars().nth(n).is_some_and(|c| matches!(c, '\\' | '/')) {
        n += 1;
    }
    rest.chars().take(n).collect()
}

#[cfg(test)]
mod tests {
    use super::first_word;

    #[test]
    fn ghost_words() {
        assert_eq!(first_word("ild --release"), "ild");
        assert_eq!(first_word(" --release"), " --release");
        assert_eq!(first_word("Files\\Far\\far.exe"), "Files\\");
        assert_eq!(first_word("\\Far"), "\\Far");
    }
}
