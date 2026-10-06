//! File operations. Long ones run on their own thread and report through a
//! callback (`OpMsg`): progress, questions (on errors and on existing
//! files — the thread waits for the answer) and the final report.
//! See docs/05-file-operations.md.

mod copy;
mod delete;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;

pub use copy::{CopyJob, plan_targets, spawn_copy};
pub use delete::spawn_delete;

pub type OpId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OpKind {
    #[serde(rename = "mkdir")]
    MkDir,
    /// Delete to the recycle bin.
    Trash,
    /// Delete permanently.
    Delete,
    Copy,
    /// Move or rename.
    Move,
}

impl OpKind {
    pub fn name(self) -> &'static str {
        match self {
            OpKind::MkDir => "mkdir",
            OpKind::Trash => "trash",
            OpKind::Delete => "delete",
            OpKind::Copy => "copy",
            OpKind::Move => "move",
        }
    }
}

/// The user's answer to an error on one item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorAnswer {
    Retry,
    Skip,
    SkipAll,
    Cancel,
}

/// What to do with a file that already exists at the destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overwrite {
    Ask,
    Replace,
    Skip,
    /// Replace only if the source is newer.
    ReplaceIfNewer,
    /// Copy under a new name: `name (2).ext`.
    Rename,
    Append,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictAction {
    Replace,
    Skip,
    Rename,
    Append,
    Cancel,
}

/// Answer to a conflict; `all` applies it to the following conflicts too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConflictAnswer {
    pub action: ConflictAction,
    pub all: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FileInfo {
    pub size: u64,
    pub modified: Option<SystemTime>,
}

impl FileInfo {
    fn of(path: &Path) -> Self {
        std::fs::metadata(path)
            .map(|m| FileInfo {
                size: m.len(),
                modified: m.modified().ok(),
            })
            .unwrap_or_default()
    }
}

pub enum OpMsg {
    Progress {
        id: OpId,
        done: usize,
        total: usize,
        /// Bytes copied; zero for operations without data transfer.
        bytes_done: u64,
        bytes_total: u64,
        current: PathBuf,
    },
    /// The operation waits for `reply`.
    Error {
        id: OpId,
        path: PathBuf,
        error: String,
        reply: Sender<ErrorAnswer>,
    },
    /// The destination file exists; the operation waits for `reply`.
    Conflict {
        id: OpId,
        source: PathBuf,
        target: PathBuf,
        new: FileInfo,
        existing: FileInfo,
        reply: Sender<ConflictAnswer>,
    },
    Finished {
        id: OpId,
        report: OpReport,
    },
}

#[derive(Debug, Default)]
pub struct OpReport {
    /// Items processed successfully (for a tree: every file and directory).
    pub done: usize,
    /// Existing files left alone.
    pub skipped: usize,
    pub failed: Vec<(PathBuf, String)>,
    pub cancelled: bool,
}

/// Creates directories `names` (relative to `base` or absolute, nested
/// paths allowed). Quick enough to run on the caller's thread.
pub fn make_dirs(base: &Path, names: &[String]) -> (Vec<PathBuf>, OpReport) {
    let mut created = Vec::new();
    let mut report = OpReport::default();
    for name in names
        .iter()
        .map(|n| n.trim().trim_matches('"'))
        .filter(|n| !n.is_empty())
    {
        // `a/b` from the agent: use the platform's separator.
        let path = base.join(name.replace('/', std::path::MAIN_SEPARATOR_STR));
        match std::fs::create_dir_all(&path) {
            Ok(()) => {
                report.done += 1;
                created.push(path);
            }
            Err(e) => report.failed.push((path, e.to_string())),
        }
    }
    (created, report)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Flow {
    Continue,
    Stop,
}

/// What to do after an error.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Next {
    Retry,
    Skip,
    Stop,
}

/// State of a running operation: counters, the user's standing answers,
/// and the channel to the UI.
struct Worker<'a, F: Fn(OpMsg)> {
    id: OpId,
    cancel: Arc<AtomicBool>,
    send: &'a F,
    skip_all: bool,
    total: usize,
    bytes_total: u64,
    bytes_done: u64,
    report: OpReport,
    last_progress: Option<Instant>,
}

impl<'a, F: Fn(OpMsg)> Worker<'a, F> {
    fn new(id: OpId, cancel: Arc<AtomicBool>, send: &'a F) -> Self {
        Self {
            id,
            cancel,
            send,
            skip_all: false,
            total: 0,
            bytes_total: 0,
            bytes_done: 0,
            report: OpReport::default(),
            last_progress: None,
        }
    }

    fn cancelled(&mut self) -> bool {
        if self.cancel.load(Ordering::SeqCst) {
            self.report.cancelled = true;
        }
        self.report.cancelled
    }

    fn progress(&mut self, current: &Path) {
        // At most ~10 updates per second.
        if self
            .last_progress
            .is_some_and(|t| t.elapsed() < Duration::from_millis(100))
        {
            return;
        }
        self.last_progress = Some(Instant::now());
        (self.send)(OpMsg::Progress {
            id: self.id,
            done: self.report.done,
            total: self.total,
            bytes_done: self.bytes_done,
            bytes_total: self.bytes_total,
            current: current.to_path_buf(),
        });
    }

    /// Asks the user about an error on `path` (unless "skip all" was
    /// chosen); records the failure unless retrying.
    fn on_error(&mut self, path: &Path, error: String) -> Next {
        let answer = if self.skip_all {
            ErrorAnswer::Skip
        } else {
            let (reply, rx) = mpsc::channel();
            (self.send)(OpMsg::Error {
                id: self.id,
                path: path.to_path_buf(),
                error: error.clone(),
                reply,
            });
            rx.recv().unwrap_or(ErrorAnswer::Cancel)
        };
        match answer {
            ErrorAnswer::Retry => Next::Retry,
            ErrorAnswer::Skip | ErrorAnswer::SkipAll => {
                self.skip_all |= answer == ErrorAnswer::SkipAll;
                self.report.failed.push((path.to_path_buf(), error));
                Next::Skip
            }
            ErrorAnswer::Cancel => {
                self.report.failed.push((path.to_path_buf(), error));
                self.report.cancelled = true;
                Next::Stop
            }
        }
    }

    /// Runs `op` on `path`, asking the user what to do on errors.
    fn attempt(&mut self, path: &Path, mut op: impl FnMut(&Path) -> Result<(), String>) -> Flow {
        self.progress(path);
        loop {
            match op(path) {
                Ok(()) => {
                    self.report.done += 1;
                    return Flow::Continue;
                }
                Err(e) => match self.on_error(path, e) {
                    Next::Retry => continue,
                    Next::Skip => return Flow::Continue,
                    Next::Stop => return Flow::Stop,
                },
            }
        }
    }

    fn finish(mut self) -> OpReport {
        std::mem::take(&mut self.report)
    }
}

/// Removes a file, clearing the read-only attribute if that is what stops it.
fn remove_file_forced(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            let readonly = std::fs::metadata(path)
                .map(|m| m.permissions().readonly())
                .unwrap_or(false);
            if !readonly {
                return Err(e.to_string());
            }
            let mut perms = std::fs::metadata(path)
                .map_err(|e| e.to_string())?
                .permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
            std::fs::set_permissions(path, perms).map_err(|e| e.to_string())?;
            std::fs::remove_file(path).map_err(|e| e.to_string())
        }
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
pub(crate) mod test_util {
    use super::*;

    pub fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("afar-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Collects messages of an operation, answering questions with the
    /// given answers; returns the report and the number of questions.
    pub fn drive(
        rx: mpsc::Receiver<OpMsg>,
        error: ErrorAnswer,
        conflict: ConflictAnswer,
    ) -> (OpReport, usize, usize) {
        let (mut errors, mut conflicts) = (0, 0);
        loop {
            match rx.recv().unwrap() {
                OpMsg::Finished { report, .. } => return (report, errors, conflicts),
                OpMsg::Error { reply, .. } => {
                    errors += 1;
                    reply.send(error).unwrap();
                }
                OpMsg::Conflict { reply, .. } => {
                    conflicts += 1;
                    reply.send(conflict).unwrap();
                }
                OpMsg::Progress { .. } => {}
            }
        }
    }

    #[test]
    fn makes_nested_dirs() {
        let base = temp_dir("mkdir");
        let (created, report) = make_dirs(&base, &["a\\b\\c".into(), " d ".into(), "".into()]);
        assert_eq!(report.done, 2);
        assert!(report.failed.is_empty());
        assert_eq!(created, vec![base.join("a\\b\\c"), base.join("d")]);
        assert!(base.join("a").join("b").join("c").is_dir());
        std::fs::remove_dir_all(&base).unwrap();
    }
}
