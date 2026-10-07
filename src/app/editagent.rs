//! The agent and the editor (docs/11 «Редактор и агент», stage 1): while
//! a file is open in afar's editor, its buffer is the truth — the agent
//! reads it with `afar_buffer_read` and changes it with
//! `afar_buffer_edit` / `afar_buffer_insert` (one undo step each, its
//! lines marked until the user accepts them); the `PreToolUse` hook
//! refuses its `Read` of a modified buffer's file and its `Edit` / `Write`
//! of an open one. `afar_edit` opens a file, `afar_editor_state` tells
//! what the user has open.

use std::path::{Path, PathBuf};

use super::App;
use super::policy::AgentAction;
use crate::config::Level;
use crate::editor::{AgentEdit, Eol, Pos};
use crate::journal::{Actor, Event};
use crate::tr;
use crate::wm::ScreenId;

/// Lines `afar_buffer_read` gives at most without a range (as `Read`).
const READ_LIMIT: usize = 2000;

fn eol_name(eol: Eol) -> &'static str {
    match eol {
        Eol::Lf => "LF",
        Eol::Cr => "CR",
        Eol::CrCrLf => "CR CR LF",
        _ => "CR LF",
    }
}

/// `2, 5, 7-9`.
fn line_list(lines: &[u64]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let mut j = i;
        while j + 1 < lines.len() && lines[j + 1] == lines[j] + 1 {
            j += 1;
        }
        parts.push(if i == j {
            lines[i].to_string()
        } else {
            format!("{}-{}", lines[i], lines[j])
        });
        i = j + 1;
    }
    parts.join(", ")
}

fn key(path: &Path) -> String {
    super::fswatch::path_key(path)
}

impl App {
    fn agent_path(&self, path: &str) -> PathBuf {
        let p = PathBuf::from(path);
        if p.is_absolute() {
            p
        } else {
            self.panels[self.active].path.join(p)
        }
    }

    /// The editor open on `path`.
    fn editor_of(&self, path: &Path) -> Option<usize> {
        let k = key(path);
        self.editors.iter().position(|e| key(e.path()) == k)
    }

    fn open_editor_for_agent(&self, path: &Path) -> Result<usize, String> {
        self.editor_of(path).ok_or_else(|| {
            format!(
                "{} is not open in afar's editor; use afar_edit to open it (or your own Read / Edit)",
                path.display()
            )
        })
    }

    /// `afar_edit`: the file in the editor at a line or a pattern; on the
    /// screen if the user is on the panels (or follows the agent),
    /// otherwise behind with a word to the user.
    pub(super) fn agent_edit(
        &mut self,
        path: &str,
        line: Option<u64>,
        pattern: Option<String>,
    ) -> Result<String, String> {
        let path = self.agent_path(path);
        if path.is_dir() {
            return Err(format!("{} is a directory", path.display()));
        }
        let shown = self.wm.current_screen();
        let fresh = self.editor_of(&path).is_none();
        let i = match self.editor_of(&path) {
            Some(i) => i,
            None => {
                let id = self
                    .open_editor(&path, None, None)
                    .ok_or_else(|| format!("cannot open {}", path.display()))?;
                self.journal
                    .push(Actor::Agent, Event::EditorOpened { path: path.clone() });
                // open_editor shows it; the etiquette may put the screen back.
                if !(self.follow_agent || shown == ScreenId::Panels) {
                    self.wm.switch_to(shown);
                }
                self.editors
                    .iter()
                    .position(|e| e.id == id)
                    .ok_or("editor gone")?
            }
        };
        let e = &mut self.editors[i];
        let total = e.line_count();
        let target = match (&pattern, line) {
            (Some(p), _) => {
                let re = regex::Regex::new(p).map_err(|e| format!("bad pattern: {e}"))?;
                let from = line.unwrap_or(1).max(1) as usize - 1;
                let found = e
                    .lines()
                    .iter()
                    .enumerate()
                    .skip(from)
                    .find(|(_, l)| re.is_match(&l.text))
                    .map(|(n, _)| n)
                    .ok_or_else(|| format!("{p:?} not found in {}", path.display()))?;
                Some(found)
            }
            (None, Some(0)) => return Err("lines count from 1".into()),
            (None, Some(l)) if l as usize > total => {
                return Err(format!(
                    "line {l} is past the end: {} has {total} lines",
                    path.display()
                ));
            }
            (None, Some(l)) => Some(l as usize - 1),
            (None, None) => None,
        };
        let id = e.id;
        let on_screen = !fresh && shown == ScreenId::Editor(id);
        if let Some(n) = target {
            let e = &mut self.editors[i];
            // The user's cursor is theirs while they work in this editor;
            // the line comes into view.
            if fresh || !on_screen {
                e.cursor = Pos::new(n, 0);
            }
            let h = usize::from(e.area.height.max(1));
            if n < e.top || n >= e.top + h {
                e.top = n.saturating_sub(3);
            }
        }
        let what = match target {
            Some(n) => format!("{}:{}", path.display(), n + 1),
            None => path.display().to_string(),
        };
        let shown = self.wm.current_screen();
        let behind = if shown == ScreenId::Editor(id) {
            ""
        } else if self.follow_agent || shown == ScreenId::Panels {
            self.wm.switch_to(ScreenId::Editor(id));
            ""
        } else {
            self.say(tr!("agent-opened-behind", what = what.as_str()));
            " (behind: the user is busy with another screen and was told where it is)"
        };
        let e = &self.editors[i];
        Ok(format!(
            "{what} is open in afar's editor ({total} lines, version {}{}){behind}. Its buffer is \
             the truth now: read it with afar_buffer_read, change it with afar_buffer_edit / \
             afar_buffer_insert.",
            e.version,
            if e.modified() { ", modified" } else { "" }
        ))
    }

    /// `afar_editor_state`.
    pub(super) fn agent_editor_state(&mut self) -> String {
        let shown = self.shown_editor();
        let items: Vec<serde_json::Value> = self
            .editors
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let h = usize::from(e.area.height.max(1));
                let mut item = serde_json::json!({
                    "path": e.path().display().to_string(),
                    "on_screen": shown == Some(i),
                    "version": e.version,
                    "modified": e.modified(),
                    "lines": e.line_count(),
                    "cursor": {"line": e.cursor.line + 1, "col": e.cursor.col + 1},
                    "visible_lines": [e.top + 1, (e.top + h).min(e.line_count())],
                    "you_last_read_version": e.agent_last_read(),
                    "codepage": crate::viewer::codepage::long_name(e.cp),
                    "bom": e.bom,
                    "line_endings": eol_name(e.default_eol),
                    "locked": e.locked,
                    "disk_changed": e.disk_changed,
                });
                if let Some((s, t)) = e.selection() {
                    let text: String = e
                        .selected_text()
                        .unwrap_or_default()
                        .chars()
                        .take(4000)
                        .collect();
                    item["selection"] = serde_json::json!({
                        "from_line": s.line + 1, "to_line": t.line + 1, "text": text,
                    });
                }
                item
            })
            .collect();
        serde_json::json!({ "editors": items }).to_string()
    }

    /// `afar_buffer_read`: lines with numbers, or the changes since a
    /// version the agent has read.
    pub(super) fn agent_buffer_read(
        &mut self,
        path: &str,
        from: Option<u64>,
        to: Option<u64>,
        since: Option<u64>,
    ) -> Result<String, String> {
        let path = self.agent_path(path);
        let i = self.open_editor_for_agent(&path)?;
        let e = &mut self.editors[i];
        let lines = e.plain_lines();
        let total = lines.len();
        let mut out = format!(
            "[afar editor: {} version {}{}, {total} lines, {}; the user's cursor at {}:{}]\n",
            path.display(),
            e.version,
            if e.modified() {
                ", modified (not saved)"
            } else {
                ""
            },
            crate::viewer::codepage::long_name(e.cp),
            e.cursor.line + 1,
            e.cursor.col + 1,
        );
        if e.disk_changed {
            out.push_str(
                "(the file on the disk changed after it was opened; the user kept this buffer, \
                 which will overwrite it on save)\n",
            );
        }
        if let Some(v) = since {
            match e.agent_version(v) {
                Some(old) => {
                    if v == e.version {
                        out.push_str("no changes since that version\n");
                    } else {
                        let a = old.join("\n");
                        let b = lines.join("\n");
                        let diff = similar::TextDiff::from_lines(&a, &b)
                            .unified_diff()
                            .context_radius(2)
                            .header(&format!("v{v}"), &format!("v{}", e.version))
                            .to_string();
                        out.push_str(&diff);
                    }
                    e.agent_read();
                    return Ok(out);
                }
                None => out.push_str(&format!(
                    "(version {v} is not one you read recently; the whole text follows)\n"
                )),
            }
        }
        let from = from.unwrap_or(1).max(1) as usize;
        if from > total {
            return Err(format!(
                "line {from} is past the end: the buffer has {total} lines"
            ));
        }
        if let Some(t) = to
            && (t as usize) < from
        {
            return Err(format!("to_line {t} is before from_line {from}"));
        }
        let to = to.map_or((from + READ_LIMIT - 1).min(total), |t| {
            (t as usize).min(total)
        });
        for (n, l) in lines.iter().enumerate().take(to).skip(from - 1) {
            out.push_str(&format!("{:>6}\t{l}\n", n + 1));
        }
        if to < total {
            out.push_str(&format!(
                "({} more lines; from_line={} to go on)\n",
                total - to,
                to + 1
            ));
        }
        e.agent_read();
        Ok(out)
    }

    fn may_edit_buffer(&self) -> Result<(), String> {
        if self.permission(AgentAction::EditBuffer) == Level::Deny {
            return Err(Self::denied(AgentAction::EditBuffer));
        }
        Ok(())
    }

    /// The agent's portion went in: journaled, the user told if they do
    /// not see it.
    fn agent_edited(&mut self, i: usize, edit: AgentEdit) -> String {
        let e = &mut self.editors[i];
        let (path, version) = (e.path().to_path_buf(), e.version);
        e.agent_read();
        let id = e.id;
        let changed: Vec<u64> = edit.changed.iter().map(|n| *n as u64 + 1).collect();
        self.journal.push(
            Actor::Agent,
            Event::BufferEdited {
                path: path.clone(),
                changed: changed.iter().take(20).copied().collect(),
                removed: edit.removed,
                version,
            },
        );
        let first = changed.first().copied().unwrap_or(1);
        if self.wm.current_screen() != ScreenId::Editor(id) {
            let what = format!("{}:{first}", path.display());
            self.say(tr!("agent-edited-behind", what = what.as_str()));
        }
        let mut what = match changed.as_slice() {
            [] => String::new(),
            [n] => format!("line {n} changed"),
            lines => format!("lines {} changed", line_list(lines)),
        };
        if edit.removed > 0 {
            if !what.is_empty() {
                what.push_str(", ");
            }
            what.push_str(&format!("{} line(s) removed", edit.removed));
        }
        if what.is_empty() {
            what = "nothing changed".into();
        }
        format!(
            "{}: {what} in the buffer (version {version}, not saved — the user saves with F2; your \
             lines are marked until they accept them)",
            path.display(),
        )
    }

    /// `afar_buffer_edit`.
    pub(super) fn agent_buffer_edit(
        &mut self,
        path: &str,
        old: &str,
        new: &str,
        all: bool,
    ) -> Result<String, String> {
        self.may_edit_buffer()?;
        let path = self.agent_path(path);
        let i = self.open_editor_for_agent(&path)?;
        if self.editors[i].locked {
            return Err("the user locked editing of this file (Ctrl+L)".into());
        }
        let edit = self.editors[i].agent_replace(old, new, all)?;
        Ok(self.agent_edited(i, edit))
    }

    /// `afar_buffer_insert`: after a line, or after the line holding a
    /// unique fragment.
    pub(super) fn agent_buffer_insert(
        &mut self,
        path: &str,
        after_line: Option<u64>,
        after_text: Option<String>,
        text: &str,
    ) -> Result<String, String> {
        self.may_edit_buffer()?;
        let path = self.agent_path(path);
        let i = self.open_editor_for_agent(&path)?;
        let e = &mut self.editors[i];
        if e.locked {
            return Err("the user locked editing of this file (Ctrl+L)".into());
        }
        let after = match (after_line, after_text) {
            (Some(n), None) => n as usize,
            (None, Some(t)) => {
                let found: Vec<usize> = e
                    .lines()
                    .iter()
                    .enumerate()
                    .filter(|(_, l)| l.text.contains(t.as_str()))
                    .map(|(n, _)| n)
                    .collect();
                match found.as_slice() {
                    [n] => n + 1,
                    [] => return Err(format!("{t:?} not found in the buffer")),
                    many => {
                        return Err(format!(
                            "{t:?} is on {} lines; give a unique fragment or after_line",
                            many.len()
                        ));
                    }
                }
            }
            _ => return Err("give after_line or after_text (one of them)".into()),
        };
        let edit = e.agent_insert(after, text)?;
        Ok(self.agent_edited(i, edit))
    }

    /// `PreToolUse` for `Read` / `Edit` / `MultiEdit` / `Write` /
    /// `NotebookEdit`: a file open in the editor is refused with the reason
    /// (its `Read` only while the buffer has unsaved changes). The hook's
    /// JSON output, or nothing to allow.
    pub(super) fn editor_guard(&self, tool: &str, input: &serde_json::Value) -> Option<String> {
        if self.editors.is_empty() {
            return None;
        }
        let path = input["file_path"]
            .as_str()
            .or_else(|| input["notebook_path"].as_str())?;
        let i = self.editor_of(Path::new(path))?;
        let e = &self.editors[i];
        let reason = match tool {
            "Read" if e.modified() => format!(
                "{path} is open in afar's editor with unsaved changes — the file on the disk is \
                 out of date; read the buffer with afar_buffer_read"
            ),
            "Read" => return None,
            "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => format!(
                "{path} is open in afar's editor — change its buffer with afar_buffer_edit or \
                 afar_buffer_insert (the user saves it); writing the file directly would go past \
                 the editor"
            ),
            _ => return None,
        };
        Some(
            serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": reason,
                }
            })
            .to_string(),
        )
    }
}
