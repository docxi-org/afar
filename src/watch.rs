//! Watching the folders shown in the panels (`notify`, not recursive):
//! changes reload the panels and go to the journal (docs/05-file-operations.md).

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use notify::event::{ModifyKind, RenameMode};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};

use crate::app::AppMsg;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Change {
    Created,
    Modified,
    Removed,
}

/// One change of one path.
#[derive(Clone, Debug)]
pub struct FsEvent {
    pub path: PathBuf,
    pub change: Change,
}

pub struct Watcher {
    inner: Option<RecommendedWatcher>,
    watched: Vec<PathBuf>,
}

impl Watcher {
    pub fn new(tx: Sender<AppMsg>) -> Self {
        let inner = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else { return };
            for e in convert(event) {
                let _ = tx.send(AppMsg::Fs(e));
            }
        })
        .ok();
        Self {
            inner,
            watched: Vec::new(),
        }
    }

    /// Watches exactly `dirs` (the panels' folders).
    pub fn sync(&mut self, dirs: &[&Path]) {
        let Some(inner) = &mut self.inner else { return };
        let mut wanted: Vec<PathBuf> = dirs.iter().map(|d| d.to_path_buf()).collect();
        wanted.dedup();
        for old in &self.watched {
            if !wanted.contains(old) {
                let _ = inner.unwatch(old);
            }
        }
        for dir in &wanted {
            if !self.watched.contains(dir) {
                let _ = inner.watch(dir, RecursiveMode::NonRecursive);
            }
        }
        self.watched = wanted;
    }
}

fn convert(event: notify::Event) -> Vec<FsEvent> {
    let one = |path: &PathBuf, change| FsEvent {
        path: path.clone(),
        change,
    };
    match event.kind {
        EventKind::Create(_) => event
            .paths
            .iter()
            .map(|p| one(p, Change::Created))
            .collect(),
        EventKind::Remove(_) => event
            .paths
            .iter()
            .map(|p| one(p, Change::Removed))
            .collect(),
        EventKind::Modify(ModifyKind::Name(mode)) => match (mode, event.paths.as_slice()) {
            (RenameMode::From, [p]) => vec![one(p, Change::Removed)],
            (RenameMode::To, [p]) => vec![one(p, Change::Created)],
            (RenameMode::Both, [from, to]) => {
                vec![one(from, Change::Removed), one(to, Change::Created)]
            }
            (_, paths) => paths.iter().map(|p| one(p, Change::Modified)).collect(),
        },
        EventKind::Modify(_) | EventKind::Any | EventKind::Other => event
            .paths
            .iter()
            .map(|p| one(p, Change::Modified))
            .collect(),
        EventKind::Access(_) => Vec::new(),
    }
}
