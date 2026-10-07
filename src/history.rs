//! Histories (docs/15-history-autocomplete.md): the dialogs' input fields
//! (Far's DIF_HISTORY lists by name), commands, later folders and views —
//! one SQLite database `%LOCALAPPDATA%\afar\history.db`. Far's rules: a
//! repeated text (case-insensitive) moves to the top and keeps its lock;
//! old entries go on exit, but only those both older than the lifetime and
//! beyond the count of the newest, never locked ones.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

/// What a history is of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A dialog's input field; the list is the field's history name.
    Dialog,
    Command,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Dialog => "dialog",
            Kind::Command => "command",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub id: i64,
    pub text: String,
    pub locked: bool,
    /// Unix time, milliseconds.
    pub last_used: i64,
    pub folder: String,
    pub actor: String,
}

pub struct History {
    db: Connection,
}

/// Far's defaults (`History.*.Count` / `.Lifetime`).
pub const LIMIT: i64 = 1000;
pub const LIFETIME_DAYS: i64 = 90;
/// Uses kept per entry (for ordering by folder later).
const USES_KEPT: i64 = 20;

/// Unix time in milliseconds (uses close together must still differ).
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

impl History {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        Self::init(Connection::open(path)?)
    }

    /// A history that lives in memory only (tests, or when the file cannot
    /// be opened).
    pub fn in_memory() -> Self {
        Self::init(Connection::open_in_memory().expect("an in-memory database"))
            .expect("the history schema")
    }

    fn init(db: Connection) -> rusqlite::Result<Self> {
        // Several afar may run at once.
        let _ = db.pragma_update(None, "journal_mode", "WAL");
        let _ = db.busy_timeout(std::time::Duration::from_secs(2));
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS entry(
                 id INTEGER PRIMARY KEY,
                 kind TEXT NOT NULL,
                 list TEXT NOT NULL DEFAULT '',
                 text TEXT NOT NULL,
                 folder TEXT NOT NULL DEFAULT '',
                 actor TEXT NOT NULL DEFAULT 'user',
                 locked INTEGER NOT NULL DEFAULT 0,
                 first_used INTEGER NOT NULL,
                 last_used INTEGER NOT NULL,
                 uses INTEGER NOT NULL DEFAULT 1,
                 data TEXT);
             CREATE INDEX IF NOT EXISTS entry_list ON entry(kind, list, last_used);
             CREATE TABLE IF NOT EXISTS use(
                 entry_id INTEGER NOT NULL,
                 time INTEGER NOT NULL,
                 folder TEXT NOT NULL DEFAULT '');
             CREATE INDEX IF NOT EXISTS use_entry ON use(entry_id);",
        )?;
        Ok(Self { db })
    }

    /// Adds a use of `text`: a new entry, or the same text (any case)
    /// moved to the top with the new spelling.
    pub fn add(&self, kind: Kind, list: &str, text: &str, folder: &str, actor: &str) {
        let t = now();
        let found: Option<i64> = self
            .db
            .query_row(
                "SELECT id FROM entry WHERE kind = ?1 AND list = ?2 AND text = ?3 COLLATE NOCASE",
                params![kind.as_str(), list, text],
                |r| r.get(0),
            )
            .optional()
            .unwrap_or(None);
        let id = match found {
            Some(id) => {
                let _ = self.db.execute(
                    "UPDATE entry SET text = ?2, folder = ?3, actor = ?4, last_used = ?5,
                         uses = uses + 1 WHERE id = ?1",
                    params![id, text, folder, actor, t],
                );
                id
            }
            None => {
                let _ = self.db.execute(
                    "INSERT INTO entry(kind, list, text, folder, actor, first_used, last_used)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                    params![kind.as_str(), list, text, folder, actor, t],
                );
                self.db.last_insert_rowid()
            }
        };
        let _ = self.db.execute(
            "INSERT INTO use(entry_id, time, folder) VALUES (?1, ?2, ?3)",
            params![id, t, folder],
        );
        let _ = self.db.execute(
            "DELETE FROM use WHERE entry_id = ?1 AND rowid NOT IN
                 (SELECT rowid FROM use WHERE entry_id = ?1 ORDER BY time DESC LIMIT ?2)",
            params![id, USES_KEPT],
        );
    }

    /// A list: locked entries first, then the newest (Far's order).
    pub fn list(&self, kind: Kind, list: &str) -> Vec<Entry> {
        let Ok(mut stmt) = self.db.prepare(
            "SELECT id, text, locked, last_used, folder, actor FROM entry
             WHERE kind = ?1 AND list = ?2 ORDER BY locked DESC, last_used DESC, id DESC",
        ) else {
            return Vec::new();
        };
        stmt.query_map(params![kind.as_str(), list], |r| {
            Ok(Entry {
                id: r.get(0)?,
                text: r.get(1)?,
                locked: r.get::<_, i64>(2)? != 0,
                last_used: r.get(3)?,
                folder: r.get(4)?,
                actor: r.get(5)?,
            })
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    /// A list's texts by time, oldest first (the command line's Ctrl+E).
    pub fn recent(&self, kind: Kind, list: &str) -> Vec<String> {
        let Ok(mut stmt) = self
            .db
            .prepare("SELECT text FROM entry WHERE kind = ?1 AND list = ?2 ORDER BY last_used, id")
        else {
            return Vec::new();
        };
        stmt.query_map(params![kind.as_str(), list], |r| r.get(0))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    /// The newest entry of a list (Far's DIF_USELASTHISTORY).
    pub fn last(&self, kind: Kind, list: &str) -> Option<String> {
        self.db
            .query_row(
                "SELECT text FROM entry WHERE kind = ?1 AND list = ?2
                 ORDER BY last_used DESC, id DESC LIMIT 1",
                params![kind.as_str(), list],
                |r| r.get(0),
            )
            .optional()
            .unwrap_or(None)
    }

    /// Ctrl+End: the next entry (in the list's order, round) starting with
    /// `prefix`, after `after`.
    pub fn next_matching(
        &self,
        kind: Kind,
        list: &str,
        prefix: &str,
        after: &str,
    ) -> Option<String> {
        let lower = prefix.to_lowercase();
        let matches: Vec<String> = self
            .list(kind, list)
            .into_iter()
            .map(|e| e.text)
            .filter(|t| t.to_lowercase().starts_with(&lower))
            .collect();
        if matches.is_empty() {
            return None;
        }
        let after = after.to_lowercase();
        let at = matches.iter().position(|t| t.to_lowercase() == after);
        Some(match at {
            Some(i) => matches[(i + 1) % matches.len()].clone(),
            None => matches[0].clone(),
        })
    }

    pub fn set_locked(&self, kind: Kind, list: &str, text: &str, locked: bool) {
        let _ = self.db.execute(
            "UPDATE entry SET locked = ?4 WHERE kind = ?1 AND list = ?2 AND text = ?3 COLLATE NOCASE",
            params![kind.as_str(), list, text, locked as i64],
        );
    }

    /// Shift+Del: one entry, unless it is locked.
    pub fn delete(&self, kind: Kind, list: &str, text: &str) {
        let _ = self.db.execute(
            "DELETE FROM use WHERE entry_id IN (SELECT id FROM entry
                 WHERE kind = ?1 AND list = ?2 AND text = ?3 COLLATE NOCASE AND locked = 0)",
            params![kind.as_str(), list, text],
        );
        let _ = self.db.execute(
            "DELETE FROM entry WHERE kind = ?1 AND list = ?2 AND text = ?3 COLLATE NOCASE
                 AND locked = 0",
            params![kind.as_str(), list, text],
        );
    }

    /// Del: all entries of a list but the locked ones.
    pub fn clear(&self, kind: Kind, list: &str) {
        let _ = self.db.execute(
            "DELETE FROM use WHERE entry_id IN
                 (SELECT id FROM entry WHERE kind = ?1 AND list = ?2 AND locked = 0)",
            params![kind.as_str(), list],
        );
        let _ = self.db.execute(
            "DELETE FROM entry WHERE kind = ?1 AND list = ?2 AND locked = 0",
            params![kind.as_str(), list],
        );
    }

    /// On exit (Far's CompactHistory): unlocked entries both older than the
    /// lifetime and beyond the newest `LIMIT` of their list go.
    pub fn compact(&self) {
        let old = now() - LIFETIME_DAYS * 24 * 3600 * 1000;
        let _ = self.db.execute(
            "DELETE FROM entry WHERE locked = 0 AND last_used < ?1 AND id NOT IN (
                 SELECT id FROM (SELECT id, ROW_NUMBER() OVER (
                     PARTITION BY kind, list ORDER BY locked DESC, last_used DESC) AS n
                 FROM entry) WHERE n <= ?2)",
            params![old, LIMIT],
        );
        let _ = self.db.execute(
            "DELETE FROM use WHERE entry_id NOT IN (SELECT id FROM entry)",
            [],
        );
    }

    /// The commands of the old `commands.txt` (oldest first), once.
    pub fn import_commands(&self, lines: &[String]) {
        let count: i64 = self
            .db
            .query_row(
                "SELECT COUNT(*) FROM entry WHERE kind = 'command'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if count > 0 {
            return;
        }
        let base = now() - lines.len() as i64;
        let tx = self.db.unchecked_transaction();
        for (i, line) in lines.iter().enumerate() {
            let t = base + i as i64;
            let _ = self.db.execute(
                "INSERT INTO entry(kind, list, text, first_used, last_used)
                 VALUES ('command', '', ?1, ?2, ?2)",
                params![line, t],
            );
        }
        if let Ok(tx) = tx {
            let _ = tx.commit();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(h: &History, list: &str) -> Vec<String> {
        h.list(Kind::Dialog, list)
            .into_iter()
            .map(|e| e.text)
            .collect()
    }

    #[test]
    fn repeats_move_up_and_keep_the_lock() {
        let h = History::in_memory();
        for t in ["a", "b", "c"] {
            h.add(Kind::Dialog, "Copy", t, "", "user");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        h.add(Kind::Dialog, "Copy", "A", "", "user");
        assert_eq!(texts(&h, "Copy"), ["A", "c", "b"]);
        h.set_locked(Kind::Dialog, "Copy", "b", true);
        assert_eq!(texts(&h, "Copy"), ["b", "A", "c"]);
        h.add(Kind::Dialog, "Copy", "B", "", "user");
        assert!(h.list(Kind::Dialog, "Copy")[0].locked, "the lock stays");
        // Lists are separate; empty text is kept (as in Far).
        h.add(Kind::Dialog, "Masks", "", "", "user");
        assert_eq!(texts(&h, "Masks"), [""]);
    }

    #[test]
    fn delete_clear_and_cycle() {
        let h = History::in_memory();
        for t in ["src", "docs", "src/app", "target"] {
            h.add(Kind::Dialog, "Copy", t, "", "user");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        h.set_locked(Kind::Dialog, "Copy", "docs", true);
        h.delete(Kind::Dialog, "Copy", "docs");
        assert!(
            texts(&h, "Copy").contains(&"docs".to_string()),
            "locked stays"
        );
        // Ctrl+End: round the entries starting with "src".
        let first = h.next_matching(Kind::Dialog, "Copy", "src", "src").unwrap();
        let second = h
            .next_matching(Kind::Dialog, "Copy", "src", &first)
            .unwrap();
        assert_ne!(first, second);
        assert!(first.starts_with("src") && second.starts_with("src"));
        h.clear(Kind::Dialog, "Copy");
        assert_eq!(texts(&h, "Copy"), ["docs"]);
        assert_eq!(h.last(Kind::Dialog, "Copy").as_deref(), Some("docs"));
    }

    #[test]
    fn imports_commands_once() {
        let h = History::in_memory();
        h.import_commands(&["dir".into(), "cargo build".into()]);
        h.import_commands(&["other".into()]);
        let list: Vec<String> = h
            .list(Kind::Command, "")
            .into_iter()
            .map(|e| e.text)
            .collect();
        assert_eq!(list, ["cargo build", "dir"]);
    }
}
