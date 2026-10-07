//! Journal of what happens in afar: kept in memory and appended to
//! `journal.jsonl` in the session directory.

use std::borrow::Cow;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

use crate::ops::OpKind;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Actor {
    User,
    Agent,
    System,
    /// Something outside afar changed files (seen by the folder watcher).
    External,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    AppStarted {
        left: PathBuf,
        right: PathBuf,
    },
    DirChanged {
        panel: Cow<'static, str>,
        from: PathBuf,
        to: PathBuf,
    },
    SelectionChanged {
        panel: Cow<'static, str>,
        count: usize,
        sample: Vec<String>,
    },
    CommandStarted {
        cmd_id: u64,
        text: String,
        cwd: PathBuf,
    },
    CommandFinished {
        cmd_id: u64,
        exit_code: Option<u32>,
        duration_ms: u64,
        lines: usize,
    },
    FileOpStarted {
        op_id: u64,
        op: OpKind,
        count: usize,
        /// Up to 20 paths.
        sources: Vec<PathBuf>,
        dest: Option<PathBuf>,
    },
    FileOpFinished {
        op_id: u64,
        op: OpKind,
        done: usize,
        /// Existing files left alone (copy/move).
        skipped: usize,
        /// Existing files overwritten or appended to, and files written
        /// under a new name beside an existing one (copy/move).
        #[serde(default)]
        replaced: usize,
        #[serde(default)]
        renamed: usize,
        failed_count: usize,
        /// Up to 20 failures.
        failed: Vec<(PathBuf, String)>,
        cancelled: bool,
    },
    /// Files in a panel's folder changed, not by afar's own operations.
    FsChanged {
        dir: PathBuf,
        /// Up to 10 names each; `count` is the total.
        created: Vec<String>,
        modified: Vec<String>,
        removed: Vec<String>,
        count: usize,
        /// The user's command running meanwhile (not necessarily the cause).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        during_cmd: Option<u64>,
        /// The agent's Bash was running meanwhile (not necessarily the cause).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        during_agent_bash: bool,
    },
    /// The agent used one of its own tools (PostToolUse hook).
    AgentToolUsed {
        tool: String,
        summary: String,
        paths: Vec<PathBuf>,
    },
    /// The agent's request that waited for the user ended without being
    /// done: declined, or the agent stopped waiting.
    AgentRequestClosed {
        op: String,
        /// Up to 20 of the paths it was about.
        #[serde(default)]
        paths: Vec<PathBuf>,
        outcome: String,
    },
    /// A file opened in the editor.
    EditorOpened {
        path: PathBuf,
    },
    /// The agent changed lines of an editor's buffer (not the file).
    BufferEdited {
        path: PathBuf,
        /// Lines changed or added (up to 20), and how many were removed.
        changed: Vec<u64>,
        removed: usize,
        version: u64,
    },
    /// The agent played input in afar (its test tools).
    TestInput {
        actions: Vec<String>,
    },
    /// A file saved from the editor.
    FileSaved {
        path: PathBuf,
        codepage: String,
    },
    /// The file of an editor with unsaved changes changed on the disk:
    /// what happened.
    EditorDiskChanged {
        path: PathBuf,
        outcome: String,
    },
    /// A file opened in the viewer.
    FileViewed {
        path: PathBuf,
    },
    /// Lines selected in the viewer (once the selection stays).
    ViewerSelection {
        path: PathBuf,
        from_line: u64,
        to_line: u64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub seq: u64,
    pub ts: DateTime<Local>,
    pub actor: Actor,
    #[serde(flatten)]
    pub event: Event,
}

pub struct Journal {
    entries: Vec<Entry>,
    file: Option<File>,
    dir: PathBuf,
}

impl Journal {
    pub fn open(dir: PathBuf) -> Self {
        let file = std::fs::create_dir_all(dir.join("output"))
            .and_then(|()| {
                File::options()
                    .create(true)
                    .append(true)
                    .open(dir.join("journal.jsonl"))
            })
            .ok();
        Self {
            entries: Vec::new(),
            file,
            dir,
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// A development-mode restart: the previous instance's entries come
    /// first, with their numbers, so the resumed agent's `since` still
    /// holds; they are copied into this session's file for the next
    /// restart. Entries already here are numbered after them.
    pub fn carry_over(&mut self, old_dir: &Path) {
        let Ok(text) = std::fs::read_to_string(old_dir.join("journal.jsonl")) else {
            return;
        };
        let mut old: Vec<Entry> = text
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        old.sort_by_key(|e| e.seq);
        old.dedup_by_key(|e| e.seq);
        let Some(last) = old.last().map(|e| e.seq) else {
            return;
        };
        // The commands' output goes along (afar_command_output).
        if let Ok(files) = std::fs::read_dir(old_dir.join("output")) {
            for f in files.flatten() {
                let to = self.dir.join("output").join(f.file_name());
                if !to.exists() {
                    let _ = std::fs::copy(f.path(), to);
                }
            }
        }
        let mine = std::mem::take(&mut self.entries);
        if let Some(f) = &mut self.file {
            // This session's file starts over with the carried entries.
            let _ = f.set_len(0);
            for e in &old {
                if let Ok(line) = serde_json::to_string(e) {
                    let _ = writeln!(f, "{line}");
                }
            }
        }
        self.entries = old;
        for (i, mut e) in mine.into_iter().enumerate() {
            e.seq = last + 1 + i as u64;
            if let Some(f) = &mut self.file
                && let Ok(line) = serde_json::to_string(&e)
            {
                let _ = writeln!(f, "{line}");
            }
            self.entries.push(e);
        }
    }

    pub fn push(&mut self, actor: Actor, event: Event) -> u64 {
        let seq = self.entries.last().map_or(1, |e| e.seq + 1);
        let entry = Entry {
            seq,
            ts: Local::now(),
            actor,
            event,
        };
        if let Some(f) = &mut self.file
            && let Ok(line) = serde_json::to_string(&entry)
        {
            let _ = writeln!(f, "{line}");
        }
        self.entries.push(entry);
        seq
    }

    pub fn last_seq(&self) -> u64 {
        self.entries.last().map_or(0, |e| e.seq)
    }

    pub fn since(&self, seq: u64) -> &[Entry] {
        let start = self.entries.partition_point(|e| e.seq <= seq);
        &self.entries[start..]
    }

    pub fn output_path(&self, cmd_id: u64) -> PathBuf {
        self.dir.join("output").join(format!("cmd-{cmd_id}.log"))
    }
}

/// Compact text form of entries for the agent.
pub fn format_entries(entries: &[Entry]) -> String {
    let mut out = String::new();
    for e in entries {
        let actor = match e.actor {
            Actor::User => "user ",
            Actor::Agent => "agent",
            Actor::System => "sys  ",
            Actor::External => "ext  ",
        };
        let what = match &e.event {
            Event::AppStarted { left, right } => {
                format!(
                    "start  left: {} | right: {}",
                    left.display(),
                    right.display()
                )
            }
            Event::DirChanged { panel, from, to } => {
                format!("cd     {panel:<5} {} → {}", from.display(), to.display())
            }
            Event::SelectionChanged {
                panel,
                count,
                sample,
            } => {
                if *count == 0 {
                    format!("select {panel:<5} selection cleared")
                } else {
                    let more = if *count > sample.len() { ", …" } else { "" };
                    format!(
                        "select {panel:<5} {count} selected: {}{more}",
                        sample.join(", ")
                    )
                }
            }
            Event::CommandStarted { cmd_id, text, cwd } => {
                format!("cmd    {text}  (cwd {})  [cmd-{cmd_id}]", cwd.display())
            }
            Event::CommandFinished {
                cmd_id,
                exit_code,
                duration_ms,
                lines,
            } => {
                let code = exit_code.map_or("?".to_string(), |c| (c as i32).to_string());
                format!(
                    "done   [cmd-{cmd_id}] exit {code}, {:.1}s, {lines} lines of output",
                    *duration_ms as f64 / 1000.0
                )
            }
            Event::FileOpStarted {
                op_id,
                op,
                count,
                sources,
                dest,
            } => {
                let names: Vec<String> = sources.iter().map(|p| p.display().to_string()).collect();
                let more = if *count > sources.len() { ", …" } else { "" };
                let dest = dest
                    .as_ref()
                    .map(|d| format!(" → {}", d.display()))
                    .unwrap_or_default();
                format!(
                    "{:<6} {count} item(s): {}{more}{dest}  [op-{op_id}]",
                    op.name(),
                    names.join(", ")
                )
            }
            Event::FileOpFinished {
                op_id,
                op,
                done,
                skipped,
                replaced,
                renamed,
                failed_count,
                failed,
                cancelled,
            } => {
                let mut s = format!("done   [op-{op_id}] {}: {done} ok", op.name());
                if *replaced > 0 {
                    s.push_str(&format!(", {replaced} overwritten"));
                }
                if *renamed > 0 {
                    s.push_str(&format!(", {renamed} renamed"));
                }
                if *skipped > 0 {
                    s.push_str(&format!(", {skipped} skipped"));
                }
                if *failed_count > 0 {
                    s.push_str(&format!(", {failed_count} failed"));
                    if let Some((p, e)) = failed.first() {
                        s.push_str(&format!(" (first: {}: {e})", p.display()));
                    }
                }
                if *cancelled {
                    s.push_str(", cancelled");
                }
                s
            }
            Event::FsChanged {
                dir,
                created,
                modified,
                removed,
                count,
                during_cmd,
                during_agent_bash,
            } => {
                let shown = created.len() + modified.len() + removed.len();
                let mut parts = Vec::new();
                for (label, names) in [("+", created), ("~", modified), ("-", removed)] {
                    if !names.is_empty() {
                        parts.push(format!("{label}{}", names.join(&format!(", {label}"))));
                    }
                }
                let more = if *count > shown { ", …" } else { "" };
                let during = match (during_cmd, during_agent_bash) {
                    (_, true) => "  (while your Bash ran)".to_string(),
                    (Some(id), _) => format!("  (while [cmd-{id}] ran)"),
                    _ => String::new(),
                };
                format!(
                    "fs     {}: {}{more}{during}",
                    dir.display(),
                    parts.join(", ")
                )
            }
            Event::AgentRequestClosed { op, paths, outcome } => {
                let names: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
                if names.is_empty() {
                    format!("ask    {op}: {outcome}")
                } else {
                    format!("ask    {op} {}: {outcome}", names.join(", "))
                }
            }
            Event::FileViewed { path } => format!("view   {}", path.display()),
            Event::EditorOpened { path } => format!("edit   {}", path.display()),
            Event::BufferEdited {
                path,
                changed,
                removed,
                version,
            } => {
                let lines: Vec<String> = changed.iter().map(u64::to_string).collect();
                let removed = if *removed > 0 {
                    format!(", {removed} removed")
                } else {
                    String::new()
                };
                format!(
                    "buffer {} lines {}{removed} (editor buffer v{version}, not saved)",
                    path.display(),
                    lines.join(",")
                )
            }
            Event::TestInput { actions } => format!("test   {}", actions.join(", ")),
            Event::FileSaved { path, codepage } => {
                format!("save   {} ({codepage})", path.display())
            }
            Event::EditorDiskChanged { path, outcome } => {
                format!(
                    "disk   {} changed on the disk while edited: {outcome}",
                    path.display()
                )
            }
            Event::ViewerSelection {
                path,
                from_line,
                to_line,
            } => format!("vsel   {}:{from_line}-{to_line}", path.display()),
            Event::AgentToolUsed { tool, summary, .. } => format!("tool   {tool}: {summary}"),
        };
        out.push_str(&format!(
            "#{} {} {actor} {what}\n",
            e.seq,
            e.ts.format("%H:%M:%S")
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carry_over_keeps_the_numbers() {
        let base = std::env::temp_dir().join(format!("afar-test-journal-{}", std::process::id()));
        let (a, b) = (base.join("a"), base.join("b"));
        let mut old = Journal::open(a.clone());
        for side in ["left", "right", "left"] {
            old.push(
                Actor::User,
                Event::DirChanged {
                    panel: side.into(),
                    from: "x".into(),
                    to: "y".into(),
                },
            );
        }
        drop(old);
        let mut new = Journal::open(b.clone());
        new.push(Actor::System, Event::FileViewed { path: "z".into() });
        new.carry_over(&a);
        assert_eq!(new.last_seq(), 4);
        assert_eq!(new.since(2).len(), 2);
        // The new file holds everything, for the next restart.
        let mut next = Journal::open(base.join("c"));
        next.carry_over(&b);
        assert_eq!(next.last_seq(), 4);
        assert!(
            matches!(&next.since(0)[1].event, Event::DirChanged { panel, .. } if panel == "right")
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
