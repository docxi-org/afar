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

pub use copy::{CopyJob, plan_targets, spawn_copy, unique_name};
pub use delete::{DeleteMode, spawn_delete};

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
    /// Overwrite the contents, then delete (Alt+Del).
    Wipe,
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
            OpKind::Wipe => "wipe",
            OpKind::Copy => "copy",
            OpKind::Move => "move",
        }
    }
}

/// Stops or pauses a running operation from the UI.
#[derive(Default, Debug)]
pub struct OpControl {
    cancel: AtomicBool,
    paused: AtomicBool,
}

impl OpControl {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// While paused the operation waits at its next step (Far asks
    /// "cancel?" with the operation stopped).
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::SeqCst);
    }

    fn cancelled(&self) -> bool {
        while self.paused.load(Ordering::SeqCst) && !self.cancel.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(50));
        }
        self.cancel.load(Ordering::SeqCst)
    }
}

/// A question an operation asks before touching an item.
#[derive(Clone, Debug)]
pub enum Question {
    /// Deleting a folder that has something in it.
    NonEmptyFolder { path: PathBuf, mode: DeleteMode },
    /// Deleting a read-only file.
    ReadOnly { path: PathBuf, mode: DeleteMode },
}

/// Answers to a `Question` (Far's buttons).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfirmAnswer {
    Yes,
    /// Yes to this and to the following questions of this kind.
    All,
    Skip,
    /// Skip this and the following items of this kind.
    SkipAll,
    Cancel,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConflictAction {
    Replace,
    Skip,
    /// Under a new name chosen automatically: `name (2).ext`.
    Rename,
    /// Under the name the user typed.
    RenameTo(String),
    Append,
    Cancel,
}

/// Answer to a conflict; `all` applies it to the following conflicts too.
#[derive(Clone, Debug, PartialEq, Eq)]
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

/// What an operation was doing when an error happened (for Far's wording).
#[derive(Clone, Debug, Default)]
pub enum ErrorContext {
    #[default]
    Delete,
    Copy {
        dest: PathBuf,
    },
    Move {
        dest: PathBuf,
    },
}

pub enum OpMsg {
    Progress {
        id: OpId,
        done: usize,
        total: usize,
        /// Bytes copied; zero for operations without data transfer.
        bytes_done: u64,
        bytes_total: u64,
        /// The file being copied: bytes done of its size.
        file_done: u64,
        file_total: u64,
        current: PathBuf,
        /// Where `current` goes (copy and move).
        target: Option<PathBuf>,
    },
    /// The operation waits for `reply`.
    Error {
        id: OpId,
        path: PathBuf,
        error: String,
        context: ErrorContext,
        reply: Sender<ErrorAnswer>,
    },
    /// A question before deleting; the operation waits for `reply`.
    Confirm {
        id: OpId,
        question: Question,
        reply: Sender<ConfirmAnswer>,
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

/// Creates a junction (`junction`) or a directory symbolic link `link`
/// pointing to `target` (Far's make-folder dialog, "link type").
pub fn make_link(link: &Path, target: &Path, junction: bool) -> Result<(), String> {
    if !target.is_dir() {
        return Err(format!("{}: not a folder", target.display()));
    }
    #[cfg(windows)]
    {
        if junction {
            let out = std::process::Command::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .map_err(|e| e.to_string())?;
            return if out.status.success() {
                Ok(())
            } else {
                Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
            };
        }
        std::os::windows::fs::symlink_dir(target, link).map_err(|e| e.to_string())
    }
    #[cfg(not(windows))]
    {
        let _ = junction;
        std::os::unix::fs::symlink(target, link).map_err(|e| e.to_string())
    }
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
    control: Arc<OpControl>,
    send: &'a F,
    skip_all: bool,
    /// Standing answers: delete non-empty folders without asking; delete
    /// (`Some(true)`) or skip (`Some(false)`) read-only files.
    delete_folders: bool,
    readonly: Option<bool>,
    total: usize,
    bytes_total: u64,
    bytes_done: u64,
    file_done: u64,
    file_total: u64,
    target: Option<PathBuf>,
    context: ErrorContext,
    report: OpReport,
    last_progress: Option<Instant>,
}

impl<'a, F: Fn(OpMsg)> Worker<'a, F> {
    fn new(id: OpId, control: Arc<OpControl>, send: &'a F) -> Self {
        Self {
            id,
            control,
            send,
            skip_all: false,
            delete_folders: false,
            readonly: None,
            total: 0,
            bytes_total: 0,
            bytes_done: 0,
            file_done: 0,
            file_total: 0,
            target: None,
            context: ErrorContext::Delete,
            report: OpReport::default(),
            last_progress: None,
        }
    }

    fn cancelled(&mut self) -> bool {
        if self.control.cancelled() {
            self.report.cancelled = true;
        }
        self.report.cancelled
    }

    /// Asks the user and waits for the answer.
    fn confirm(&mut self, question: Question) -> ConfirmAnswer {
        let (reply, rx) = mpsc::channel();
        (self.send)(OpMsg::Confirm {
            id: self.id,
            question,
            reply,
        });
        rx.recv().unwrap_or(ConfirmAnswer::Cancel)
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
            file_done: self.file_done,
            file_total: self.file_total,
            current: current.to_path_buf(),
            target: self.target.clone(),
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
                context: self.context.clone(),
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
                    reply.send(conflict.clone()).unwrap();
                }
                OpMsg::Confirm { reply, .. } => reply.send(ConfirmAnswer::All).unwrap(),
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
