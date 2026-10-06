//! Deleting: to the recycle bin or permanently.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use super::{Flow, OpId, OpMsg, Worker, remove_file_forced};

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
            let report = w.finish();
            send(OpMsg::Finished { id, report });
        })?;
    Ok(())
}

impl<F: Fn(OpMsg)> Worker<'_, F> {
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
    use std::sync::mpsc;

    use super::super::test_util::{drive, temp_dir};
    use super::super::{ConflictAction, ConflictAnswer, ErrorAnswer, OpReport};
    use super::*;

    const NO_CONFLICT: ConflictAnswer = ConflictAnswer {
        action: ConflictAction::Cancel,
        all: false,
    };

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
        let (report, errors, _) = drive(rx, answer, NO_CONFLICT);
        (report, errors)
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
