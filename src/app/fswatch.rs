//! Changes of the panels' folders and the agent's own tools
//! (docs/04-agent.md, "Действия агента своими средствами"):
//!
//! - the folder watcher reloads the panels and journals what changed, with
//!   who did it: the agent (a path it just edited, or while its Bash runs),
//!   the user (while their command runs), nobody (afar's own operations
//!   are journaled already) or something external;
//! - the PostToolUse hook journals the agent's Edit/Write/Bash;
//! - files the agent changed are highlighted for a while.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::App;
use crate::journal::{Actor, Event};
use crate::watch::{Change, FsEvent, Watcher};

/// Panels reload this long after the last change of a burst.
const RELOAD_DELAY: Duration = Duration::from_millis(100);
/// Changes go to the journal when their folder is quiet this long (the
/// agent's PostToolUse may come after the change itself).
const JOURNAL_DELAY: Duration = Duration::from_secs(1);
/// A folder that keeps changing (a growing log) still reloads and goes to
/// the journal this often.
const RELOAD_MAX_DELAY: Duration = Duration::from_millis(500);
const JOURNAL_MAX_DELAY: Duration = Duration::from_secs(5);
/// After an operation or a command, its late changes are still its own.
const AFTERGLOW: Duration = Duration::from_secs(2);
/// How long files changed by the agent stay highlighted.
const MARK_TIME: Duration = Duration::from_secs(10 * 60);
/// The agent's Bash counts as running at most this long without its
/// PostToolUse (a failed tool may not report).
const BASH_LIMIT: Duration = Duration::from_secs(10 * 60);

/// Changes of one folder by one author: folder key, folder, author,
/// names with their change.
type Group = (String, PathBuf, Actor, Vec<(String, Change)>);

struct Pending {
    path: PathBuf,
    change: Change,
    /// Who did it as known when it happened; `None`: afar's own operation.
    actor: Option<Actor>,
    at: Instant,
}

pub(super) struct FsState {
    watcher: Watcher,
    pending: Vec<Pending>,
    reload_at: Option<Instant>,
    /// The first change since the last reload.
    reload_first: Option<Instant>,
    /// Paths the agent's tools touched recently (keys of `path_key`).
    pub(super) agent_paths: Vec<(String, Instant)>,
    /// Starts of the agent's Bash commands still running.
    agent_bash: Vec<Instant>,
    agent_bash_done: Option<Instant>,
    /// afar's own operations finished recently.
    own_done: Option<Instant>,
    /// Files changed by the agent: key, time.
    marks: Vec<(String, Instant)>,
}

impl FsState {
    pub(super) fn new(tx: std::sync::mpsc::Sender<super::AppMsg>) -> Self {
        Self {
            watcher: Watcher::new(tx),
            pending: Vec::new(),
            reload_at: None,
            reload_first: None,
            agent_paths: Vec::new(),
            agent_bash: Vec::new(),
            agent_bash_done: None,
            own_done: None,
            marks: Vec::new(),
        }
    }

    fn agent_bash_running(&self, now: Instant) -> bool {
        self.agent_bash
            .iter()
            .any(|t| now.duration_since(*t) < BASH_LIMIT)
            || self
                .agent_bash_done
                .is_some_and(|t| now.duration_since(t) < AFTERGLOW)
    }
}

/// Comparable form of a path: Windows paths ignore case and slashes.
pub(super) fn path_key(p: &Path) -> String {
    let s = crate::panel::strip_verbatim(p.to_path_buf())
        .to_string_lossy()
        .replace('/', "\\");
    let s = s.trim_end_matches('\\').to_string();
    if cfg!(windows) { s.to_lowercase() } else { s }
}

fn parent_key(key: &str) -> &str {
    key.rsplit_once('\\').map_or("", |(dir, _)| dir)
}

impl App {
    /// afar's own file operation finished: its late changes are its own.
    pub(super) fn note_own_change(&mut self) {
        self.fs.own_done = Some(Instant::now());
    }

    pub(super) fn on_fs(&mut self, ev: FsEvent) {
        let now = Instant::now();
        let actor = if self.fs.agent_bash_running(now) {
            Some(Actor::Agent)
        } else if !self.ops.is_empty()
            || self
                .fs
                .own_done
                .is_some_and(|t| now.duration_since(t) < AFTERGLOW)
        {
            None
        } else if self.running.is_some() {
            Some(Actor::User)
        } else {
            Some(Actor::External)
        };
        self.fs.pending.push(Pending {
            path: ev.path,
            change: ev.change,
            actor,
            at: now,
        });
        self.fs.reload_at = Some(now + RELOAD_DELAY);
        self.fs.reload_first.get_or_insert(now);
    }

    /// Periodic part: watch the panels' folders, reload them after a
    /// burst of changes, journal quiet folders, expire marks.
    pub(super) fn fs_tick(&mut self) {
        let dirs = [self.panels[0].path.clone(), self.panels[1].path.clone()];
        self.fs.watcher.sync(&[&dirs[0], &dirs[1]]);
        let now = Instant::now();

        let due = self.fs.reload_at.is_some_and(|t| now >= t)
            || self
                .fs
                .reload_first
                .is_some_and(|t| now.duration_since(t) >= RELOAD_MAX_DELAY);
        if due {
            self.fs.reload_at = None;
            self.fs.reload_first = None;
            for side in 0..2 {
                let dir = path_key(&self.panels[side].path);
                if self
                    .fs
                    .pending
                    .iter()
                    .any(|p| parent_key(&path_key(&p.path)) == dir)
                {
                    self.panels[side].reload(None);
                }
            }
        }

        // Journal the folders quiet for JOURNAL_DELAY (or changing for
        // longer than JOURNAL_MAX_DELAY).
        let quiet = |dir: &str, pending: &[Pending]| {
            let mut changes = pending
                .iter()
                .filter(|p| parent_key(&path_key(&p.path)) == dir);
            changes
                .clone()
                .all(|p| now.duration_since(p.at) >= JOURNAL_DELAY)
                || changes.any(|p| now.duration_since(p.at) >= JOURNAL_MAX_DELAY)
        };
        let mut ready = Vec::new();
        let mut keep = Vec::new();
        let pending = std::mem::take(&mut self.fs.pending);
        for p in &pending {
            let key = path_key(&p.path);
            if quiet(parent_key(&key), &pending) {
                ready.push(p);
            } else {
                keep.push(p);
            }
        }
        self.journal_fs(&ready, now);
        self.fs.pending = keep
            .into_iter()
            .map(|p| Pending {
                path: p.path.clone(),
                change: p.change,
                actor: p.actor,
                at: p.at,
            })
            .collect();

        self.fs
            .marks
            .retain(|(_, t)| now.duration_since(*t) < MARK_TIME);
        self.fs
            .agent_paths
            .retain(|(_, t)| now.duration_since(*t) < Duration::from_secs(30));
        self.fs
            .agent_bash
            .retain(|t| now.duration_since(*t) < BASH_LIMIT);
    }

    /// One journal entry per folder and author.
    fn journal_fs(&mut self, ready: &[&Pending], now: Instant) {
        let mut groups: Vec<Group> = Vec::new();
        for p in ready {
            let key = path_key(&p.path);
            // The agent's own edits are journaled by its hook already.
            if self.fs.agent_paths.iter().any(|(k, _)| *k == key) {
                continue;
            }
            let Some(actor) = p.actor else { continue };
            let Some(name) = p.path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
                continue;
            };
            if actor == Actor::Agent && p.change != Change::Removed {
                self.fs.marks.push((key.clone(), now));
            }
            let dir_key = parent_key(&key).to_string();
            let group = match groups.iter_mut().find(|g| g.0 == dir_key && g.2 == actor) {
                Some(g) => g,
                None => {
                    let dir = p.path.parent().map(Path::to_path_buf).unwrap_or_default();
                    groups.push((dir_key, dir, actor, Vec::new()));
                    groups.last_mut().unwrap()
                }
            };
            // One change per name: created wins over modified; created and
            // then removed is nothing to report but still a removal.
            match group.3.iter_mut().find(|(n, _)| *n == name) {
                Some((_, c)) => {
                    *c = match (*c, p.change) {
                        (Change::Created, Change::Modified) => Change::Created,
                        (Change::Removed, Change::Created) => Change::Modified,
                        (_, new) => new,
                    }
                }
                None => group.3.push((name, p.change)),
            }
        }
        for (_, dir, actor, changes) in groups {
            let names = |want: Change| -> Vec<String> {
                changes
                    .iter()
                    .filter(|(_, c)| *c == want)
                    .map(|(n, _)| n.clone())
                    .take(10)
                    .collect()
            };
            self.journal.push(
                actor,
                Event::FsChanged {
                    dir,
                    created: names(Change::Created),
                    modified: names(Change::Modified),
                    removed: names(Change::Removed),
                    count: changes.len(),
                },
            );
        }
    }

    /// PreToolUse (Bash only): the agent's command starts.
    pub(super) fn on_pre_tool(&mut self, input: &str) -> String {
        let v: serde_json::Value = serde_json::from_str(input).unwrap_or_default();
        if v["tool_name"] == "Bash" {
            self.fs.agent_bash.push(Instant::now());
        }
        String::new()
    }

    /// PostToolUse: journal the agent's tool, remember and highlight the
    /// files it changed, reload the panels showing them.
    pub(super) fn on_post_tool(&mut self, input: &str) -> String {
        let v: serde_json::Value = serde_json::from_str(input).unwrap_or_default();
        let tool = v["tool_name"].as_str().unwrap_or("?").to_string();
        let now = Instant::now();
        let mut paths = Vec::new();
        let summary = match tool.as_str() {
            "Bash" => {
                if !self.fs.agent_bash.is_empty() {
                    self.fs.agent_bash.remove(0);
                }
                self.fs.agent_bash_done = Some(now);
                let cmd = v["tool_input"]["command"].as_str().unwrap_or("");
                let first = cmd.lines().next().unwrap_or("");
                let mut s: String = first.chars().take(200).collect();
                if first.chars().count() > 200 || cmd.lines().count() > 1 {
                    s.push_str(" …");
                }
                if v["tool_response"]["interrupted"] == true {
                    s.push_str(" (interrupted)");
                }
                s
            }
            _ => {
                let input = &v["tool_input"];
                let path = input["file_path"]
                    .as_str()
                    .or_else(|| input["notebook_path"].as_str())
                    .unwrap_or("");
                if !path.is_empty() {
                    paths.push(PathBuf::from(path));
                }
                path.to_string()
            }
        };
        // Open viewers of these files show what changed now.
        self.viewers_agent_wrote(&paths);
        for p in &paths {
            let key = path_key(p);
            self.fs.agent_paths.push((key.clone(), now));
            self.fs.marks.push((key.clone(), now));
            for panel in &mut self.panels {
                if path_key(&panel.path) == parent_key(&key) {
                    panel.reload(None);
                }
            }
        }
        self.journal.push(
            Actor::Agent,
            Event::AgentToolUsed {
                tool,
                summary,
                paths,
            },
        );
        String::new()
    }

    /// A new prompt: whatever Bash we thought was running has ended.
    pub(super) fn fs_new_prompt(&mut self) {
        self.fs.agent_bash.clear();
    }

    /// Names in each panel to highlight as changed by the agent.
    pub(super) fn apply_agent_marks(&mut self) {
        for panel in &mut self.panels {
            let dir = path_key(&panel.path);
            panel.agent_marked = self
                .fs
                .marks
                .iter()
                .filter(|(k, _)| parent_key(k) == dir)
                .map(|(k, _)| {
                    k.rsplit_once('\\')
                        .map_or(k.as_str(), |(_, n)| n)
                        .to_string()
                })
                .collect();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_ignore_case_and_slashes() {
        let a = path_key(Path::new("F:/AGI/Far/src/"));
        let b = path_key(Path::new(r"\\?\F:\AGI\far\src"));
        if cfg!(windows) {
            assert_eq!(a, b);
        }
        assert_eq!(
            parent_key(&path_key(Path::new(r"C:\x\y.txt"))),
            path_key(Path::new(r"C:\x"))
        );
    }
}
