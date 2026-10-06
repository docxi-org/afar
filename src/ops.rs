//! File operations. Long ones run on their own thread and report through a
//! callback (`OpMsg`): progress, questions on errors (the thread waits for
//! the answer) and the final report. See docs/05-file-operations.md.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::time::{Duration, Instant};

use serde::Serialize;

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
}

impl OpKind {
    pub fn name(self) -> &'static str {
        match self {
            OpKind::MkDir => "mkdir",
            OpKind::Trash => "trash",
            OpKind::Delete => "delete",
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

pub enum OpMsg {
    Progress {
        id: OpId,
        done: usize,
        total: usize,
        current: PathBuf,
    },
    /// The operation waits for `reply`.
    Error {
        id: OpId,
        path: PathBuf,
        error: String,
        reply: Sender<ErrorAnswer>,
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

/// Deletes `targets` (to the recycle bin unless `permanent`) on a new
/// thread. Symbolic links and junctions are deleted themselves, never what
/// they point to.
pub fn spawn_delete(
    id: OpId,
    targets: Vec<PathBuf>,
    permanent: bool,
    cancel: Arc<AtomicBool>,
    send: impl Fn(OpMsg) + Send + 'static,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name(format!("op-{id}"))
        .spawn(move || {
            let mut w = Worker::new(id, cancel, &send);
            if permanent {
                w.total = targets.iter().map(|t| count_tree(t)).sum();
                for t in &targets {
                    if w.delete_tree(t) == Flow::Stop {
                        break;
                    }
                }
            } else {
                w.total = targets.len();
                for t in &targets {
                    if w.cancelled() {
                        break;
                    }
                    if w.attempt(t, |p| trash::delete(p).map_err(|e| e.to_string())) == Flow::Stop {
                        break;
                    }
                }
            }
            let report = std::mem::take(&mut w.report);
            send(OpMsg::Finished { id, report });
        })?;
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Flow {
    Continue,
    Stop,
}

struct Worker<'a, F: Fn(OpMsg)> {
    id: OpId,
    cancel: Arc<AtomicBool>,
    send: &'a F,
    skip_all: bool,
    total: usize,
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
            current: current.to_path_buf(),
        });
    }

    /// Runs `op` on `path`, asking the user what to do on errors.
    fn attempt(&mut self, path: &Path, mut op: impl FnMut(&Path) -> Result<(), String>) -> Flow {
        self.progress(path);
        loop {
            let error = match op(path) {
                Ok(()) => {
                    self.report.done += 1;
                    return Flow::Continue;
                }
                Err(e) => e,
            };
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
                ErrorAnswer::Retry => continue,
                ErrorAnswer::Skip | ErrorAnswer::SkipAll => {
                    self.skip_all |= answer == ErrorAnswer::SkipAll;
                    self.report.failed.push((path.to_path_buf(), error));
                    return Flow::Continue;
                }
                ErrorAnswer::Cancel => {
                    self.report.failed.push((path.to_path_buf(), error));
                    self.report.cancelled = true;
                    return Flow::Stop;
                }
            }
        }
    }

    fn delete_tree(&mut self, path: &Path) -> Flow {
        if self.cancelled() {
            return Flow::Stop;
        }
        let meta = match std::fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) => {
                let e = e.to_string();
                return self.attempt(path, |_| Err(e.clone()));
            }
        };
        let ft = meta.file_type();
        if ft.is_symlink() {
            // Links (and junctions) are removed themselves.
            return self.attempt(path, |p| {
                std::fs::remove_file(p)
                    .or_else(|_| std::fs::remove_dir(p))
                    .map_err(|e| e.to_string())
            });
        }
        if ft.is_dir() {
            let children: Vec<PathBuf> = match std::fs::read_dir(path) {
                Ok(rd) => rd.flatten().map(|e| e.path()).collect(),
                Err(e) => {
                    let e = e.to_string();
                    return self.attempt(path, |_| Err(e.clone()));
                }
            };
            for child in &children {
                if self.delete_tree(child) == Flow::Stop {
                    return Flow::Stop;
                }
            }
            return self.attempt(path, |p| std::fs::remove_dir(p).map_err(|e| e.to_string()));
        }
        self.attempt(path, remove_file_forced)
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

/// Number of items a permanent delete of `path` will process.
fn count_tree(path: &Path) -> usize {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {
            1 + std::fs::read_dir(path)
                .map(|rd| rd.flatten().map(|e| count_tree(&e.path())).sum())
                .unwrap_or(0)
        }
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("afar-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Runs a delete and answers errors with `answer`.
    fn run_delete(targets: Vec<PathBuf>, answer: ErrorAnswer) -> (OpReport, usize) {
        let (tx, rx) = mpsc::channel();
        spawn_delete(
            1,
            targets,
            true,
            Arc::new(AtomicBool::new(false)),
            move |m| {
                let _ = tx.send(m);
            },
        )
        .unwrap();
        let mut errors = 0;
        loop {
            match rx.recv().unwrap() {
                OpMsg::Finished { report, .. } => return (report, errors),
                OpMsg::Error { reply, .. } => {
                    errors += 1;
                    reply.send(answer).unwrap();
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

    #[test]
    fn deletes_tree_with_readonly_file() {
        let base = temp_dir("del");
        let tree = base.join("tree");
        std::fs::create_dir_all(tree.join("sub")).unwrap();
        std::fs::write(tree.join("a.txt"), "a").unwrap();
        let ro = tree.join("sub").join("ro.txt");
        std::fs::write(&ro, "ro").unwrap();
        let mut perms = std::fs::metadata(&ro).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&ro, perms).unwrap();
        let (report, errors) = run_delete(vec![tree.clone()], ErrorAnswer::Cancel);
        assert_eq!(errors, 0, "{:?}", report.failed);
        assert_eq!(report.done, 4); // tree, sub, a.txt, ro.txt
        assert!(!tree.exists());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn reports_missing_items_and_skips() {
        let base = temp_dir("missing");
        let (report, errors) = run_delete(
            vec![base.join("nope"), base.join("nope2")],
            ErrorAnswer::SkipAll,
        );
        assert_eq!(errors, 1, "SkipAll answers once");
        assert_eq!(report.failed.len(), 2);
        assert!(!report.cancelled);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn does_not_follow_junctions() {
        let base = temp_dir("junction");
        let target = base.join("target");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("keep.txt"), "keep").unwrap();
        let tree = base.join("tree");
        std::fs::create_dir_all(&tree).unwrap();
        let link = tree.join("link");
        let ok = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !ok {
            eprintln!("mklink /J unavailable, skipping");
            return;
        }
        let (report, errors) = run_delete(vec![tree.clone()], ErrorAnswer::Cancel);
        assert_eq!(errors, 0, "{:?}", report.failed);
        assert!(!tree.exists());
        assert!(
            target.join("keep.txt").exists(),
            "the junction's target must survive"
        );
        std::fs::remove_dir_all(&base).unwrap();
    }
}
