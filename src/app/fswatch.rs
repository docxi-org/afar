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
/// Changes journaled together: folder key, folder, author, what ran
/// meanwhile, the names.
type Group = (
    String,
    PathBuf,
    Actor,
    Option<During>,
    Vec<(String, Change)>,
);

/// What was running when files changed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum During {
    /// The user's command.
    Command(u64),
    /// The agent's Bash.
    AgentBash,
}

struct Pending {
    path: PathBuf,
    change: Change,
    /// Who did it as known when it happened; `None`: afar's own operation.
    actor: Option<Actor>,
    /// What was running when it happened (it may or may not be the cause).
    during: Option<During>,
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
    /// The agent's Bash commands still running: tool use id, start.
    agent_bash: Vec<(String, Instant)>,
    agent_bash_done: Option<Instant>,
    /// afar's own operations finished recently.
    /// afar's own operations: their id, the paths they touch (keys of
    /// `path_key`), when they ended.
    own: Vec<(u64, Vec<String>, Option<Instant>)>,
    /// Files changed by the agent: key, time, what it was.
    marks: Vec<(String, Instant, crate::panel::AgentMark)>,
    /// The shell tool of the agent's last command (`Bash`, `PowerShell`).
    shell_tool: String,
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
            own: Vec::new(),
            marks: Vec::new(),
            shell_tool: "Bash".into(),
        }
    }

    fn agent_bash_running(&self, now: Instant) -> bool {
        self.agent_bash
            .iter()
            .any(|(_, t)| now.duration_since(*t) < BASH_LIMIT)
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
    /// afar's own operation `id` starts: changes of these paths (and of
    /// what is inside them) are its own.
    pub(super) fn own_paths(&mut self, id: u64, paths: &[PathBuf]) {
        let keys = paths.iter().map(|p| path_key(p)).collect();
        self.fs.own.push((id, keys, None));
    }

    /// afar's own operation `id` finished: its late changes are its own
    /// for a moment more.
    pub(super) fn note_own_change(&mut self, id: u64) {
        let now = Instant::now();
        for o in self.fs.own.iter_mut().filter(|o| o.0 == id) {
            o.2 = Some(now);
        }
    }

    /// A change of `path` made by one of afar's own operations: one of its
    /// paths, inside one, or the folder holding one (its entry changes).
    fn is_own(&self, path: &Path, change: Change, now: Instant) -> bool {
        let key = path_key(path);
        // `ReplaceFileW` keeps its own temporary file (`name~RF….TMP`) beside
        // the file it replaces.
        let name = key.rsplit('\\').next().unwrap_or("");
        let replace_tmp = name.contains("~rf") && name.ends_with(".tmp");
        let inside = |root: &str| {
            if replace_tmp && parent_key(root) == parent_key(&key) {
                return true;
            }
            key == root
                || key.strip_prefix(root).is_some_and(|r| r.starts_with('\\'))
                || change == Change::Modified
                    && root
                        .strip_prefix(key.as_str())
                        .is_some_and(|r| r.starts_with('\\'))
        };
        self.fs
            .own
            .iter()
            .filter(|(_, _, done)| done.is_none_or(|t| now.duration_since(t) < AFTERGLOW))
            .any(|(_, roots, _)| roots.iter().any(|r| inside(r)))
    }

    pub(super) fn on_fs(&mut self, ev: FsEvent) {
        let now = Instant::now();
        // afar's own operation first: its journal entry says who did it
        // (the user's F7 while the agent's command runs is still the user's);
        // only its own paths — others may write meanwhile.
        let actor = if self.is_own(&ev.path, ev.change, now) {
            None
        } else {
            // A running command — the user's or the agent's Bash — is not
            // proof: another process may write meanwhile; the journal says
            // both.
            Some(Actor::External)
        };
        let during = match actor {
            Some(_) if self.fs.agent_bash_running(now) => Some(During::AgentBash),
            Some(_) => self.running.as_ref().map(|r| During::Command(r.id)),
            None => None,
        };
        self.fs.pending.push(Pending {
            path: ev.path,
            change: ev.change,
            actor,
            during,
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
                during: p.during,
                at: p.at,
            })
            .collect();

        self.fs
            .marks
            .retain(|(_, t, _)| now.duration_since(*t) < MARK_TIME);
        self.fs
            .agent_paths
            .retain(|(_, t)| now.duration_since(*t) < Duration::from_secs(30));
        self.fs
            .agent_bash
            .retain(|(_, t)| now.duration_since(*t) < BASH_LIMIT);
        self.fs
            .own
            .retain(|(_, _, done)| done.is_none_or(|t| now.duration_since(t) < AFTERGLOW));
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
            // Changes while the agent's Bash runs are marked in the panels
            // as its own (most likely they are).
            if p.during == Some(During::AgentBash) && p.change != Change::Removed {
                let tool = self.fs.shell_tool.clone();
                self.fs.marks.push((key.clone(), now, mark(tool)));
            }
            let dir_key = parent_key(&key).to_string();
            let group = match groups
                .iter_mut()
                .find(|g| g.0 == dir_key && g.2 == actor && g.3 == p.during)
            {
                Some(g) => g,
                None => {
                    let dir = p.path.parent().map(Path::to_path_buf).unwrap_or_default();
                    groups.push((dir_key, dir, actor, p.during, Vec::new()));
                    groups.last_mut().unwrap()
                }
            };
            // One change per name: created wins over modified; created and
            // then removed is nothing to report but still a removal.
            match group.4.iter_mut().find(|(n, _)| *n == name) {
                Some((_, c)) => {
                    *c = match (*c, p.change) {
                        (Change::Created, Change::Modified) => Change::Created,
                        (Change::Removed, Change::Created) => Change::Modified,
                        (_, new) => new,
                    }
                }
                None => group.4.push((name, p.change)),
            }
        }
        for (_, dir, actor, during, changes) in groups {
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
                    during_cmd: match during {
                        Some(During::Command(id)) => Some(id),
                        _ => None,
                    },
                    during_agent_bash: during == Some(During::AgentBash),
                },
            );
        }
    }

    /// PreToolUse (Bash only): the agent's command starts.
    pub(super) fn on_pre_tool(&mut self, input: &str) -> String {
        let v: serde_json::Value = serde_json::from_str(input).unwrap_or_default();
        // A file open in the editor: its buffer is the truth.
        if let Some(tool) = v["tool_name"].as_str()
            && let Some(deny) = self.editor_guard(tool, &v["tool_input"])
        {
            return deny;
        }
        if let Some(shell @ ("Bash" | "PowerShell")) = v["tool_name"].as_str() {
            self.fs.shell_tool = shell.to_string();
            let id = v["tool_use_id"].as_str().unwrap_or_default().to_string();
            self.fs.agent_bash.push((id, Instant::now()));
        }
        String::new()
    }

    /// PostToolUse (`failed`: PostToolUseFailure): journal the agent's
    /// tool, remember and highlight the files it changed, reload the panels
    /// showing them.
    pub(super) fn on_post_tool(&mut self, input: &str, failed: bool) -> String {
        let v: serde_json::Value = serde_json::from_str(input).unwrap_or_default();
        let tool = v["tool_name"].as_str().unwrap_or("?").to_string();
        let now = Instant::now();
        let mut paths = Vec::new();
        let summary = match tool.as_str() {
            "Bash" | "PowerShell" => {
                // The command by its id; without one, the oldest.
                let id = v["tool_use_id"].as_str().unwrap_or_default();
                let at = self
                    .fs
                    .agent_bash
                    .iter()
                    .position(|(i, _)| i == id)
                    .unwrap_or(0);
                if at < self.fs.agent_bash.len() {
                    self.fs.agent_bash.remove(at);
                }
                self.fs.agent_bash_done = Some(now);
                let cmd = v["tool_input"]["command"].as_str().unwrap_or("");
                let first = cmd.lines().next().unwrap_or("");
                let mut s: String = first.chars().take(200).collect();
                if first.chars().count() > 200 || cmd.lines().count() > 1 {
                    s.push_str(" …");
                }
                if v["tool_response"]["interrupted"] == true || v["is_interrupt"] == true {
                    s.push_str(" (interrupted)");
                } else if failed {
                    s.push_str(" (failed)");
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
            self.fs.marks.push((key.clone(), now, mark(tool.clone())));
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

    /// A new prompt or the agent's turn over: whatever Bash we thought was
    /// running has ended (a denied command gets no PostToolUse).
    pub(super) fn fs_agent_idle(&mut self) {
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
                .filter(|(k, _, _)| parent_key(k) == dir)
                .map(|(k, _, m)| {
                    let name = k.rsplit_once('\\').map_or(k.as_str(), |(_, n)| n);
                    (name.to_string(), m.clone())
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

/// A mark for a file the agent changed now with `tool`.
fn mark(tool: String) -> crate::panel::AgentMark {
    crate::panel::AgentMark {
        time: chrono::Local::now(),
        tool,
    }
}
