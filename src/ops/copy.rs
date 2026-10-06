//! Copying and moving (renaming).
//!
//! Files are written to a temporary `.<name>.afar-part` next to the target
//! and renamed at the end, so an interrupted copy never leaves a partial
//! file under the real name. Moving within a volume is a rename; across
//! volumes — a copy and then deleting the source, item by item.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;

use crate::tr;

use super::{
    ConflictAction, ConflictAnswer, ErrorContext, FileInfo, Flow, Next, OpControl, OpId, OpMsg,
    Overwrite, Worker, remove_file_forced,
};

const BUFFER: usize = 1 << 20;

pub struct CopyJob {
    pub sources: Vec<PathBuf>,
    /// As entered: an existing directory (or a path ending with a separator)
    /// to copy into, or the new name of a single source.
    pub dest: PathBuf,
    pub moving: bool,
    pub overwrite: Overwrite,
}

/// Source → target pairs for a job, after the checks that must pass before
/// anything is touched: not into itself, not into its own subdirectory.
pub fn plan_targets(job: &CopyJob) -> Result<Vec<(PathBuf, PathBuf)>, String> {
    if job.sources.is_empty() {
        return Err(tr!("copy-nothing"));
    }
    let dest_text = job.dest.to_string_lossy();
    let into = job.dest.is_dir() || dest_text.ends_with(['\\', '/']) || job.sources.len() > 1;
    let mut pairs = Vec::new();
    for src in &job.sources {
        let name = src
            .file_name()
            .ok_or_else(|| tr!("copy-root", path = src.display().to_string()))?;
        let target = if into {
            job.dest.join(name)
        } else {
            job.dest.clone()
        };
        let (s, t) = (normalize(src), normalize(&target));
        // Moving to the same name in another letter case is a real rename.
        let case_rename = job.moving && src != &target;
        // Far's message: three lines — what, the path, "onto itself".
        let onto_itself =
            |first: &str, last: &str| format!("{}\n{}\n{}", tr!(first), src.display(), tr!(last));
        if s == t && !case_rename {
            return Err(if src.is_dir() {
                onto_itself("MCannotCopyFolderToItself1", "MCannotCopyFolderToItself2")
            } else {
                onto_itself("MCannotCopyFileToItself1", "MCannotCopyFileToItself2")
            });
        }
        let sep = std::path::MAIN_SEPARATOR;
        if src.is_dir() && t.starts_with(&format!("{s}{sep}")) {
            return Err(onto_itself(
                "MCannotCopyFolderToItself1",
                "MCannotCopyFolderToItself2",
            ));
        }
        pairs.push((src.clone(), target));
    }
    Ok(pairs)
}

/// Comparable form of a path that may not exist yet: the existing part
/// canonicalized, case-folded on Windows.
fn normalize(p: &Path) -> String {
    let mut existing = p.to_path_buf();
    let mut rest = Vec::new();
    while !existing.exists() {
        match (existing.file_name(), existing.parent()) {
            (Some(n), Some(parent)) => {
                rest.push(n.to_owned());
                existing = parent.to_path_buf();
            }
            _ => break,
        }
    }
    let mut base = std::fs::canonicalize(&existing).unwrap_or(existing);
    for n in rest.iter().rev() {
        base.push(n);
    }
    let s = base.to_string_lossy().into_owned();
    if cfg!(windows) { s.to_lowercase() } else { s }
}

/// Copies or moves `pairs` on a new thread.
pub fn spawn_copy(
    id: OpId,
    pairs: Vec<(PathBuf, PathBuf)>,
    moving: bool,
    overwrite: Overwrite,
    control: Arc<OpControl>,
    send: impl Fn(OpMsg) + Send + 'static,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name(format!("op-{id}"))
        .spawn(move || {
            let mut c = Copier {
                w: Worker::new(id, control, &send),
                moving,
                policy: overwrite,
            };
            for (src, _) in &pairs {
                let (n, bytes) = scan(src);
                c.w.total += n;
                c.w.bytes_total += bytes;
            }
            for (src, target) in &pairs {
                if c.copy_item(src, target) == Flow::Stop {
                    break;
                }
            }
            let report = c.w.finish();
            send(OpMsg::Finished { id, report });
        })?;
    Ok(())
}

/// Items and bytes under `path` (links count as one item without data).
fn scan(path: &Path) -> (usize, u64) {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => (1, 0),
        Ok(m) if m.is_dir() => std::fs::read_dir(path)
            .map(|rd| {
                rd.flatten()
                    .map(|e| scan(&e.path()))
                    .fold((1, 0), |(n, b), (cn, cb)| (n + cn, b + cb))
            })
            .unwrap_or((1, 0)),
        Ok(m) => (1, m.len()),
        Err(_) => (1, 0),
    }
}

/// `name (2).ext`, `name (3).ext`, … — the first that does not exist.
pub fn unique_name(target: &Path) -> PathBuf {
    let stem = target
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = target
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    (2..)
        .map(|n| target.with_file_name(format!("{stem} ({n}){ext}")))
        .find(|p| std::fs::symlink_metadata(p).is_err())
        .unwrap()
}

enum CopyError {
    Cancelled,
    Io(String),
}

impl From<std::io::Error> for CopyError {
    fn from(e: std::io::Error) -> Self {
        CopyError::Io(e.to_string())
    }
}

struct Copier<'a, F: Fn(OpMsg)> {
    w: Worker<'a, F>,
    moving: bool,
    /// Standing answer for existing files ("for all" sets it).
    policy: Overwrite,
}

impl<F: Fn(OpMsg)> Copier<'_, F> {
    fn copy_item(&mut self, src: &Path, target: &Path) -> Flow {
        if self.w.cancelled() {
            return Flow::Stop;
        }
        let dest = target.to_path_buf();
        self.w.context = if self.moving {
            ErrorContext::Move { dest }
        } else {
            ErrorContext::Copy { dest }
        };
        self.w.target = Some(target.to_path_buf());
        self.w.file_done = 0;
        self.w.file_total = 0;
        let meta = match std::fs::symlink_metadata(src) {
            Ok(m) => m,
            Err(e) => {
                let e = e.to_string();
                return self.w.attempt(src, |_| Err(e.clone()));
            }
        };
        // Moving within a volume: one rename for the whole item (also when
        // only the letter case changes and the "target" is the source).
        let case_rename = self.moving && normalize(src) == normalize(target);
        if self.moving && (case_rename || std::fs::symlink_metadata(target).is_err()) {
            let (n, bytes) = scan(src);
            if std::fs::rename(src, target).is_ok() {
                self.w.report.done += n;
                self.w.bytes_done += bytes;
                self.w.progress(src);
                return Flow::Continue;
            }
            // Another volume (or another reason): copy and delete instead.
        }
        if let Some(parent) = target.parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
        {
            let parent = parent.to_path_buf();
            if self.w.attempt(&parent, |p| {
                std::fs::create_dir_all(p).map_err(|e| e.to_string())
            }) == Flow::Stop
            {
                return Flow::Stop;
            }
        }
        let ft = meta.file_type();
        if ft.is_symlink() {
            let flow = self.w.attempt(src, |s| copy_link(s, target));
            if flow == Flow::Continue && self.moving {
                let _ = std::fs::remove_file(src).or_else(|_| std::fs::remove_dir(src));
            }
            return flow;
        }
        if ft.is_dir() {
            return self.copy_dir(src, target);
        }
        self.copy_file_item(src, target, &meta)
    }

    fn copy_dir(&mut self, src: &Path, target: &Path) -> Flow {
        let exists = match std::fs::symlink_metadata(target) {
            Ok(m) if m.is_dir() => true,
            Ok(_) => {
                return self
                    .w
                    .attempt(target, |_| Err(tr!("copy-file-where-folder")));
            }
            Err(_) => false,
        };
        // Into an existing directory: merge.
        if !exists
            && self.w.attempt(target, |t| {
                std::fs::create_dir(t).map_err(|e| e.to_string())
            }) == Flow::Stop
        {
            return Flow::Stop;
        }
        let children: Vec<PathBuf> = match std::fs::read_dir(src) {
            Ok(rd) => rd.flatten().map(|e| e.path()).collect(),
            Err(e) => {
                let e = e.to_string();
                return self.w.attempt(src, |_| Err(e.clone()));
            }
        };
        for child in &children {
            let Some(name) = child.file_name() else {
                continue;
            };
            if self.copy_item(child, &target.join(name)) == Flow::Stop {
                return Flow::Stop;
            }
        }
        if exists {
            self.w.report.done += 1;
        }
        if self.moving {
            // Stays if something inside was skipped or failed.
            let _ = std::fs::remove_dir(src);
        }
        Flow::Continue
    }

    fn copy_file_item(&mut self, src: &Path, target: &Path, meta: &std::fs::Metadata) -> Flow {
        let mut target = target.to_path_buf();
        let mut append = false;
        if let Ok(existing) = std::fs::symlink_metadata(&target) {
            if existing.is_dir() {
                return self.w.attempt(src, |_| {
                    Err(tr!(
                        "copy-folder-where-file",
                        path = target.display().to_string()
                    ))
                });
            }
            match self.decide(src, &target, meta) {
                ConflictAction::Skip => {
                    self.w.report.skipped += 1;
                    self.w.bytes_done += meta.len();
                    return Flow::Continue;
                }
                ConflictAction::Cancel => {
                    self.w.report.cancelled = true;
                    return Flow::Stop;
                }
                ConflictAction::Rename => target = unique_name(&target),
                ConflictAction::RenameTo(name) => target = target.with_file_name(name),
                ConflictAction::Append => append = true,
                ConflictAction::Replace => {}
            }
        }
        // Moving within a volume, replacing an existing file.
        if self.moving && !append && std::fs::rename(src, &target).is_ok() {
            self.w.report.done += 1;
            self.w.bytes_done += meta.len();
            self.w.progress(src);
            return Flow::Continue;
        }
        loop {
            let before = self.w.bytes_done;
            match self.copy_file(src, &target, append) {
                Ok(()) => break,
                Err(CopyError::Cancelled) => {
                    self.w.report.cancelled = true;
                    return Flow::Stop;
                }
                Err(CopyError::Io(e)) => {
                    self.w.bytes_done = before;
                    match self.w.on_error(src, e) {
                        Next::Retry => continue,
                        Next::Skip => return Flow::Continue,
                        Next::Stop => return Flow::Stop,
                    }
                }
            }
        }
        self.w.report.done += 1;
        if self.moving {
            loop {
                match remove_file_forced(src) {
                    Ok(()) => break,
                    Err(e) => match self.w.on_error(src, e) {
                        Next::Retry => continue,
                        Next::Skip => break,
                        Next::Stop => return Flow::Stop,
                    },
                }
            }
        }
        Flow::Continue
    }

    /// What to do with an existing target, by policy or by asking.
    fn decide(&mut self, src: &Path, target: &Path, meta: &std::fs::Metadata) -> ConflictAction {
        match self.policy {
            Overwrite::Replace => ConflictAction::Replace,
            Overwrite::Skip => ConflictAction::Skip,
            Overwrite::Rename => ConflictAction::Rename,
            Overwrite::Append => ConflictAction::Append,
            Overwrite::ReplaceIfNewer => {
                let theirs = std::fs::metadata(target).and_then(|m| m.modified()).ok();
                match (meta.modified().ok(), theirs) {
                    (Some(ours), Some(theirs)) if ours > theirs => ConflictAction::Replace,
                    _ => ConflictAction::Skip,
                }
            }
            Overwrite::Ask => {
                let (reply, rx) = mpsc::channel();
                (self.w.send)(OpMsg::Conflict {
                    id: self.w.id,
                    source: src.to_path_buf(),
                    target: target.to_path_buf(),
                    new: FileInfo::of(src),
                    existing: FileInfo::of(target),
                    reply,
                });
                let answer = rx.recv().unwrap_or(ConflictAnswer {
                    action: ConflictAction::Cancel,
                    all: false,
                });
                if answer.all {
                    self.policy = match answer.action {
                        ConflictAction::Replace => Overwrite::Replace,
                        ConflictAction::Skip => Overwrite::Skip,
                        ConflictAction::Rename | ConflictAction::RenameTo(_) => Overwrite::Rename,
                        ConflictAction::Append => Overwrite::Append,
                        ConflictAction::Cancel => self.policy,
                    };
                }
                answer.action
            }
        }
    }

    /// Copies one file's data, attributes and modification time.
    fn copy_file(&mut self, src: &Path, target: &Path, append: bool) -> Result<(), CopyError> {
        let mut input = File::open(src)?;
        let meta = input.metadata()?;
        self.w.file_total = meta.len();
        self.w.file_done = 0;
        if append {
            let mut out = File::options().append(true).open(target)?;
            return self.pump(src, &mut input, &mut out);
        }
        let name = target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let temp = target.with_file_name(format!(".{name}.afar-part"));
        let result = (|| {
            let mut out = File::create(&temp)?;
            self.pump(src, &mut input, &mut out)?;
            if let Ok(modified) = meta.modified() {
                out.set_modified(modified)?;
            }
            drop(out);
            // A read-only file in the way cannot be replaced.
            if let Ok(m) = std::fs::metadata(target)
                && m.permissions().readonly()
            {
                let mut p = m.permissions();
                #[allow(clippy::permissions_set_readonly_false)]
                p.set_readonly(false);
                std::fs::set_permissions(target, p)?;
            }
            std::fs::rename(&temp, target)?;
            if meta.permissions().readonly() {
                std::fs::set_permissions(target, meta.permissions())?;
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result
    }

    fn pump(&mut self, src: &Path, input: &mut File, out: &mut File) -> Result<(), CopyError> {
        let mut buf = vec![0u8; BUFFER];
        loop {
            if self.w.cancelled() {
                return Err(CopyError::Cancelled);
            }
            let n = input.read(&mut buf)?;
            if n == 0 {
                return Ok(());
            }
            out.write_all(&buf[..n])?;
            self.w.bytes_done += n as u64;
            self.w.file_done += n as u64;
            self.w.progress(src);
        }
    }
}

/// Recreates a symbolic link (or junction) as a link to the same target.
fn copy_link(src: &Path, target: &Path) -> Result<(), String> {
    let to = std::fs::read_link(src).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    {
        let is_dir = std::fs::metadata(src).map(|m| m.is_dir()).unwrap_or(false);
        let r = if is_dir {
            std::os::windows::fs::symlink_dir(&to, target)
        } else {
            std::os::windows::fs::symlink_file(&to, target)
        };
        r.map_err(|e| tr!("link-create-failed", error = e.to_string()))
    }
    #[cfg(not(windows))]
    {
        std::os::unix::fs::symlink(&to, target).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_util::{drive, temp_dir};
    use super::super::{ErrorAnswer, OpReport};
    use super::*;

    fn run(
        sources: Vec<PathBuf>,
        dest: PathBuf,
        moving: bool,
        overwrite: Overwrite,
        conflict: ConflictAnswer,
    ) -> (OpReport, usize, usize) {
        let job = CopyJob {
            sources,
            dest,
            moving,
            overwrite,
        };
        let pairs = plan_targets(&job).unwrap();
        let (tx, rx) = mpsc::channel();
        spawn_copy(1, pairs, moving, overwrite, OpControl::new(), move |m| {
            let _ = tx.send(m);
        })
        .unwrap();
        drive(rx, ErrorAnswer::Cancel, conflict)
    }

    const CANCEL: ConflictAnswer = ConflictAnswer {
        action: ConflictAction::Cancel,
        all: false,
    };

    fn write(p: &Path, s: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    }

    fn read(p: &Path) -> String {
        std::fs::read_to_string(p).unwrap()
    }

    #[test]
    fn copies_tree_into_directory() {
        let base = temp_dir("copy-tree");
        write(&base.join("src/a.txt"), "a");
        write(&base.join("src/sub/b.txt"), "bb");
        std::fs::create_dir_all(base.join("dst")).unwrap();
        let (report, errors, _) = run(
            vec![base.join("src")],
            base.join("dst"),
            false,
            Overwrite::Ask,
            CANCEL,
        );
        assert_eq!((errors, report.failed.len()), (0, 0), "{:?}", report.failed);
        assert_eq!(read(&base.join("dst/src/sub/b.txt")), "bb");
        assert_eq!(report.done, 4); // src, a.txt, sub, b.txt
        assert!(base.join("src/a.txt").exists(), "copy keeps the source");
        // No temporary files left behind.
        assert!(!base.join("dst/src/.a.txt.afar-part").exists());
        let src_time = std::fs::metadata(base.join("src/a.txt"))
            .unwrap()
            .modified()
            .unwrap();
        let dst_time = std::fs::metadata(base.join("dst/src/a.txt"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(src_time, dst_time);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn copies_single_file_under_new_name() {
        let base = temp_dir("copy-rename");
        write(&base.join("a.txt"), "a");
        let (report, ..) = run(
            vec![base.join("a.txt")],
            base.join("b.txt"),
            false,
            Overwrite::Ask,
            CANCEL,
        );
        assert_eq!(report.done, 1);
        assert_eq!(read(&base.join("b.txt")), "a");
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn asks_on_conflicts_and_applies_to_all() {
        let base = temp_dir("copy-conflict");
        for n in ["x", "y"] {
            write(&base.join(format!("src/{n}.txt")), "new");
            write(&base.join(format!("dst/{n}.txt")), "old");
        }
        let skip_all = ConflictAnswer {
            action: ConflictAction::Skip,
            all: true,
        };
        let (report, _, conflicts) = run(
            vec![base.join("src/x.txt"), base.join("src/y.txt")],
            base.join("dst"),
            false,
            Overwrite::Ask,
            skip_all,
        );
        assert_eq!(conflicts, 1, "for all: asked once");
        assert_eq!(report.skipped, 2);
        assert_eq!(read(&base.join("dst/x.txt")), "old");
        let replace = ConflictAnswer {
            action: ConflictAction::Replace,
            all: false,
        };
        let (_, _, conflicts) = run(
            vec![base.join("src/x.txt"), base.join("src/y.txt")],
            base.join("dst"),
            false,
            Overwrite::Ask,
            replace,
        );
        assert_eq!(conflicts, 2);
        assert_eq!(read(&base.join("dst/y.txt")), "new");
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn renames_and_appends_by_policy() {
        let base = temp_dir("copy-policy");
        write(&base.join("src/a.txt"), "new");
        write(&base.join("dst/a.txt"), "old");
        run(
            vec![base.join("src/a.txt")],
            base.join("dst"),
            false,
            Overwrite::Rename,
            CANCEL,
        );
        assert_eq!(read(&base.join("dst/a (2).txt")), "new");
        run(
            vec![base.join("src/a.txt")],
            base.join("dst"),
            false,
            Overwrite::Append,
            CANCEL,
        );
        assert_eq!(read(&base.join("dst/a.txt")), "oldnew");
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn replaces_only_older_files_if_asked() {
        let base = temp_dir("copy-newer");
        write(&base.join("dst/a.txt"), "old");
        std::thread::sleep(std::time::Duration::from_millis(20));
        write(&base.join("src/a.txt"), "new");
        write(&base.join("src/b.txt"), "older");
        std::thread::sleep(std::time::Duration::from_millis(20));
        write(&base.join("dst/b.txt"), "newer");
        let (report, ..) = run(
            vec![base.join("src/a.txt"), base.join("src/b.txt")],
            base.join("dst"),
            false,
            Overwrite::ReplaceIfNewer,
            CANCEL,
        );
        assert_eq!((report.done, report.skipped), (1, 1));
        assert_eq!(read(&base.join("dst/a.txt")), "new");
        assert_eq!(read(&base.join("dst/b.txt")), "newer");
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn moves_and_merges_directories() {
        let base = temp_dir("move");
        write(&base.join("a/one.txt"), "1");
        write(&base.join("a/sub/two.txt"), "2");
        write(&base.join("dst/a/existing.txt"), "e");
        // Into a directory that already has `a`: merged by copy + delete.
        let (report, errors, _) = run(
            vec![base.join("a")],
            base.join("dst"),
            true,
            Overwrite::Ask,
            CANCEL,
        );
        assert_eq!(errors, 0, "{:?}", report.failed);
        assert!(!base.join("a").exists(), "the source is gone");
        assert_eq!(read(&base.join("dst/a/sub/two.txt")), "2");
        assert!(base.join("dst/a/existing.txt").exists());
        // A plain rename.
        let (report, ..) = run(
            vec![base.join("dst/a/one.txt")],
            base.join("dst/a/uno.txt"),
            true,
            Overwrite::Ask,
            CANCEL,
        );
        assert_eq!(report.done, 1);
        assert!(base.join("dst/a/uno.txt").exists() && !base.join("dst/a/one.txt").exists());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn refuses_copying_into_itself() {
        let base = temp_dir("copy-self");
        write(&base.join("dir/a.txt"), "a");
        let job = |sources: Vec<PathBuf>, dest: PathBuf, moving| CopyJob {
            sources,
            dest,
            moving,
            overwrite: Overwrite::Ask,
        };
        assert!(plan_targets(&job(vec![base.join("dir")], base.join("dir/sub"), false)).is_err());
        assert!(plan_targets(&job(vec![base.join("dir")], base.join("dir"), false)).is_err());
        assert!(plan_targets(&job(vec![base.join("dir/a.txt")], base.join("dir"), false)).is_err());
        // Sibling with a common prefix is fine.
        assert!(plan_targets(&job(vec![base.join("dir")], base.join("dir2"), false)).is_ok());
        // Changing only the letter case is a rename.
        assert!(
            plan_targets(&job(
                vec![base.join("dir/a.txt")],
                base.join("dir/A.txt"),
                true
            ))
            .is_ok()
        );
        std::fs::remove_dir_all(&base).unwrap();
    }
}
