//! Journal of what happens in afar: kept in memory and appended to
//! `journal.jsonl` in the session directory.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local};
use serde::Serialize;

use crate::ops::OpKind;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Actor {
    User,
    Agent,
    System,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    AppStarted {
        left: PathBuf,
        right: PathBuf,
    },
    DirChanged {
        panel: &'static str,
        from: PathBuf,
        to: PathBuf,
    },
    SelectionChanged {
        panel: &'static str,
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
        failed_count: usize,
        /// Up to 20 failures.
        failed: Vec<(PathBuf, String)>,
        cancelled: bool,
    },
}

#[derive(Clone, Debug, Serialize)]
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
                failed_count,
                failed,
                cancelled,
            } => {
                let mut s = format!("done   [op-{op_id}] {}: {done} ok", op.name());
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
        };
        out.push_str(&format!(
            "#{} {} {actor} {what}\n",
            e.seq,
            e.ts.format("%H:%M:%S")
        ));
    }
    out
}
