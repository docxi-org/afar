//! Far's position cache for the viewer: where each file was left, its code
//! page, mode and bookmarks. Kept in `%LOCALAPPDATA%\afar\history\viewer.json`
//! until the histories move to SQLite (M2).

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::Remembered;

/// Far keeps 1000 entries in its view history.
const LIMIT: usize = 1000;

#[derive(Default, Serialize, Deserialize)]
pub struct Positions {
    /// Most recent first.
    entries: Vec<(String, Remembered)>,
}

fn key(path: &Path) -> String {
    let s = path.display().to_string();
    if cfg!(windows) { s.to_lowercase() } else { s }
}

impl Positions {
    pub fn load(file: &Path) -> Self {
        std::fs::read(file)
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, file: &Path) {
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec(self) {
            let _ = std::fs::write(file, json);
        }
    }

    pub fn get(&self, path: &Path) -> Option<&Remembered> {
        let k = key(path);
        self.entries.iter().find(|(p, _)| *p == k).map(|(_, r)| r)
    }

    pub fn put(&mut self, path: &Path, r: Remembered) {
        let k = key(path);
        self.entries.retain(|(p, _)| *p != k);
        self.entries.insert(0, (k, r));
        self.entries.truncate(LIMIT);
    }
}
