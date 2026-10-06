//! Deleting: to the recycle bin, permanently, or wiping (Alt+Del), with
//! Far's questions about non-empty folders and read-only files
//! (far/delete.cpp).

use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{ConfirmAnswer, Flow, OpControl, OpId, OpMsg, Question, Worker, remove_file_forced};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeleteMode {
    /// To the recycle bin (F8).
    Trash,
    /// For good (Shift+Del).
    Permanent,
    /// Contents overwritten first (Alt+Del).
    Wipe,
}

/// Deletes `targets` on a new thread. Symbolic links and junctions are
/// deleted themselves, never what they point to.
/// Which of Far's questions to ask (Options → Confirmations).
#[derive(Clone, Copy, Debug)]
pub struct Confirmations {
    pub folders: bool,
    pub read_only: bool,
}

impl Default for Confirmations {
    fn default() -> Self {
        Self {
            folders: true,
            read_only: true,
        }
    }
}

pub fn spawn_delete(
    id: OpId,
    targets: Vec<PathBuf>,
    mode: DeleteMode,
    confirm: Confirmations,
    control: Arc<OpControl>,
    send: impl Fn(OpMsg) + Send + 'static,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name(format!("op-{id}"))
        .spawn(move || {
            let mut w = Worker::new(id, control, &send);
            w.delete_folders = !confirm.folders;
            if !confirm.read_only {
                w.readonly = Some(true);
            }
            if mode == DeleteMode::Trash {
                w.total = targets.len();
                for t in &targets {
                    if w.cancelled() || w.trash(t) == Flow::Stop {
                        break;
                    }
                }
            } else {
                w.total = targets.iter().map(|t| count_tree(t)).sum();
                for t in &targets {
                    if w.delete_tree(t, mode) == Flow::Stop {
                        break;
                    }
                }
            }
            let report = w.finish();
            send(OpMsg::Finished { id, report });
        })?;
    Ok(())
}

/// What to do with an item after asking.
enum Decision {
    Go,
    Skip,
    Stop,
}

impl<F: Fn(OpMsg)> Worker<'_, F> {
    /// Far asks before deleting a folder with something in it (unless
    /// "All" was answered); links to folders are not asked about.
    fn ask_folder(&mut self, path: &Path, mode: DeleteMode) -> Decision {
        let not_empty = std::fs::read_dir(path).is_ok_and(|mut rd| rd.next().is_some());
        if self.delete_folders || !not_empty {
            return Decision::Go;
        }
        let question = Question::NonEmptyFolder {
            path: path.to_path_buf(),
            mode,
        };
        match self.confirm(question) {
            ConfirmAnswer::Yes => Decision::Go,
            ConfirmAnswer::All => {
                self.delete_folders = true;
                Decision::Go
            }
            ConfirmAnswer::Skip | ConfirmAnswer::SkipAll => {
                self.report.skipped += 1;
                Decision::Skip
            }
            ConfirmAnswer::Cancel => {
                self.report.cancelled = true;
                Decision::Stop
            }
        }
    }

    /// Far asks before deleting a read-only file; "yes" clears the
    /// attribute.
    fn ask_readonly(&mut self, path: &Path, mode: DeleteMode) -> Decision {
        let readonly = std::fs::symlink_metadata(path).is_ok_and(|m| m.permissions().readonly());
        if !readonly {
            return Decision::Go;
        }
        let answer = match self.readonly {
            Some(true) => ConfirmAnswer::Yes,
            Some(false) => ConfirmAnswer::Skip,
            None => self.confirm(Question::ReadOnly {
                path: path.to_path_buf(),
                mode,
            }),
        };
        match answer {
            ConfirmAnswer::Yes | ConfirmAnswer::All => {
                if answer == ConfirmAnswer::All {
                    self.readonly = Some(true);
                }
                if let Ok(m) = std::fs::metadata(path) {
                    let mut perms = m.permissions();
                    #[allow(clippy::permissions_set_readonly_false)]
                    perms.set_readonly(false);
                    let _ = std::fs::set_permissions(path, perms);
                }
                Decision::Go
            }
            ConfirmAnswer::Skip | ConfirmAnswer::SkipAll => {
                if answer == ConfirmAnswer::SkipAll {
                    self.readonly = Some(false);
                }
                self.report.skipped += 1;
                Decision::Skip
            }
            ConfirmAnswer::Cancel => {
                self.report.cancelled = true;
                Decision::Stop
            }
        }
    }

    /// One item to the recycle bin.
    fn trash(&mut self, path: &Path) -> Flow {
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            return self.attempt(path, |p| trash::delete(p).map_err(|e| e.to_string()));
        };
        let decision = if meta.file_type().is_symlink() {
            Decision::Go
        } else if meta.is_dir() {
            self.ask_folder(path, DeleteMode::Trash)
        } else {
            self.ask_readonly(path, DeleteMode::Trash)
        };
        match decision {
            Decision::Go => self.attempt(path, |p| trash::delete(p).map_err(|e| e.to_string())),
            Decision::Skip => Flow::Continue,
            Decision::Stop => Flow::Stop,
        }
    }

    fn delete_tree(&mut self, path: &Path, mode: DeleteMode) -> Flow {
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
            match self.ask_folder(path, mode) {
                Decision::Go => {}
                Decision::Skip => return Flow::Continue,
                Decision::Stop => return Flow::Stop,
            }
            let children: Vec<PathBuf> = match std::fs::read_dir(path) {
                Ok(rd) => rd.flatten().map(|e| e.path()).collect(),
                Err(e) => {
                    let e = e.to_string();
                    return self.attempt(path, |_| Err(e.clone()));
                }
            };
            for child in &children {
                if self.delete_tree(child, mode) == Flow::Stop {
                    return Flow::Stop;
                }
            }
            return if mode == DeleteMode::Wipe {
                self.attempt(path, wipe_dir)
            } else {
                self.attempt(path, |p| std::fs::remove_dir(p).map_err(|e| e.to_string()))
            };
        }
        match self.ask_readonly(path, mode) {
            Decision::Go => {}
            Decision::Skip => return Flow::Continue,
            Decision::Stop => return Flow::Stop,
        }
        if mode == DeleteMode::Wipe {
            self.attempt(path, wipe_file)
        } else {
            self.attempt(path, remove_file_forced)
        }
    }
}

/// Far's EraseFile: overwrite the contents with zeros, cut the file to
/// nothing, give it a meaningless name, delete it.
fn wipe_file(path: &Path) -> Result<(), String> {
    let err = |e: std::io::Error| e.to_string();
    let len = std::fs::metadata(path).map_err(err)?.len();
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(err)?;
        f.seek(SeekFrom::Start(0)).map_err(err)?;
        let zeros = vec![0u8; 1 << 16];
        let mut left = len;
        while left > 0 {
            let n = left.min(zeros.len() as u64) as usize;
            f.write_all(&zeros[..n]).map_err(err)?;
            left -= n as u64;
        }
        f.sync_all().map_err(err)?;
        f.set_len(0).map_err(err)?;
    }
    let renamed = anonymous_name(path);
    std::fs::rename(path, &renamed).map_err(err)?;
    std::fs::remove_file(&renamed).map_err(err)
}

/// Far's EraseDirectory: rename, then remove.
fn wipe_dir(path: &Path) -> Result<(), String> {
    let renamed = anonymous_name(path);
    let target = if std::fs::rename(path, &renamed).is_ok() {
        renamed
    } else {
        path.to_path_buf()
    };
    std::fs::remove_dir(&target).map_err(|e| e.to_string())
}

/// A name in the same folder that tells nothing about the original.
fn anonymous_name(path: &Path) -> PathBuf {
    let dir = path.parent().unwrap_or(Path::new("."));
    (0u32..)
        .map(|i| dir.join(format!("{:0>8}.tmp", i)))
        .find(|p| !p.exists())
        .unwrap_or_else(|| path.to_path_buf())
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
            DeleteMode::Permanent,
            Confirmations::default(),
            OpControl::new(),
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

    /// Runs a delete answering each question with the next of `answers`;
    /// returns the report and the questions asked.
    fn run_with(
        targets: Vec<PathBuf>,
        mode: DeleteMode,
        answers: &[ConfirmAnswer],
    ) -> (OpReport, Vec<Question>) {
        let (tx, rx) = mpsc::channel();
        spawn_delete(
            1,
            targets,
            mode,
            Confirmations::default(),
            OpControl::new(),
            move |m| {
                let _ = tx.send(m);
            },
        )
        .unwrap();
        let mut asked = Vec::new();
        loop {
            match rx.recv().unwrap() {
                OpMsg::Finished { report, .. } => return (report, asked),
                OpMsg::Confirm {
                    question, reply, ..
                } => {
                    let answer = answers
                        .get(asked.len())
                        .copied()
                        .unwrap_or(ConfirmAnswer::Cancel);
                    asked.push(question);
                    reply.send(answer).unwrap();
                }
                OpMsg::Error { reply, .. } => reply.send(ErrorAnswer::Cancel).unwrap(),
                _ => {}
            }
        }
    }

    #[test]
    fn asks_about_non_empty_folders() {
        let base = temp_dir("askdir");
        let full = base.join("full");
        std::fs::create_dir_all(full.join("inner")).unwrap();
        std::fs::write(full.join("inner").join("x.txt"), "x").unwrap();
        let empty = base.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        // Skip the full folder: nothing in it is touched; the empty one
        // goes without a question.
        let (report, asked) = run_with(
            vec![full.clone(), empty.clone()],
            DeleteMode::Permanent,
            &[ConfirmAnswer::Skip],
        );
        assert_eq!(asked.len(), 1);
        assert!(full.join("inner").join("x.txt").exists());
        assert!(!empty.exists());
        assert_eq!(report.skipped, 1);
        // "All": the nested non-empty folder is not asked about again.
        let (_, asked) = run_with(
            vec![full.clone()],
            DeleteMode::Permanent,
            &[ConfirmAnswer::All],
        );
        assert_eq!(asked.len(), 1);
        assert!(!full.exists());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn asks_about_read_only_files() {
        let base = temp_dir("askro");
        let mut files = Vec::new();
        for n in ["a.txt", "b.txt"] {
            let f = base.join(n);
            std::fs::write(&f, "ro").unwrap();
            let mut perms = std::fs::metadata(&f).unwrap().permissions();
            perms.set_readonly(true);
            std::fs::set_permissions(&f, perms).unwrap();
            files.push(f);
        }
        let (report, asked) = run_with(
            files.clone(),
            DeleteMode::Permanent,
            &[ConfirmAnswer::SkipAll],
        );
        assert_eq!(asked.len(), 1, "skip all answers once");
        assert_eq!(report.skipped, 2);
        assert!(files.iter().all(|f| f.exists()));
        let (report, _) = run_with(files.clone(), DeleteMode::Permanent, &[ConfirmAnswer::All]);
        assert_eq!(report.done, 2);
        assert!(files.iter().all(|f| !f.exists()));
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn wipes_contents_before_deleting() {
        let base = temp_dir("wipe");
        let dir = base.join("secret");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("key.txt"), "password").unwrap();
        let (report, _) = run_with(vec![dir.clone()], DeleteMode::Wipe, &[ConfirmAnswer::Yes]);
        assert_eq!(report.done, 2);
        assert!(!dir.exists());
        assert_eq!(
            std::fs::read_dir(&base).unwrap().count(),
            0,
            "no renamed leftovers"
        );
        // The overwrite itself.
        let f = base.join("f.bin");
        std::fs::write(&f, [7u8; 100_000]).unwrap();
        let renamed = anonymous_name(&f);
        assert_ne!(renamed, f);
        assert_eq!(renamed.parent(), f.parent());
        wipe_file(&f).unwrap();
        assert!(!f.exists());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn waits_while_paused_and_stops_when_cancelled() {
        let base = temp_dir("pause");
        let files: Vec<PathBuf> = (0..5)
            .map(|i| {
                let f = base.join(format!("{i}.txt"));
                std::fs::write(&f, "x").unwrap();
                f
            })
            .collect();
        let control = OpControl::new();
        control.set_paused(true);
        let (tx, rx) = mpsc::channel();
        spawn_delete(
            1,
            files.clone(),
            DeleteMode::Permanent,
            Confirmations::default(),
            control.clone(),
            move |m| {
                let _ = tx.send(m);
            },
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(
            files.iter().all(|f| f.exists()),
            "nothing happens while paused"
        );
        control.cancel();
        let (report, _, _) = drive(rx, ErrorAnswer::Cancel, NO_CONFLICT);
        assert!(report.cancelled);
        assert!(files.iter().all(|f| f.exists()));
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
