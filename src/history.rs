//! Histories (docs/15-history-autocomplete.md): the dialogs' input fields
//! (Far's DIF_HISTORY lists by name), commands, later folders and views —
//! one SQLite database `%LOCALAPPDATA%\afar\history.db`. Far's rules: a
//! repeated text (case-insensitive) moves to the top and keeps its lock;
//! old entries go on exit, but only those both older than the lifetime and
//! beyond the count of the newest, never locked ones.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

/// What a history is of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A dialog's input field; the list is the field's history name.
    Dialog,
    Command,
    /// Folders the panels went to (a source for path fields; later
    /// Alt+F12).
    Folder,
    /// Files viewed and edited (later Alt+F11); now only from Far.
    View,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Dialog => "dialog",
            Kind::Command => "command",
            Kind::Folder => "folder",
            Kind::View => "view",
        }
    }
}

/// How a list is ordered (docs/15, improvements 1 and 7).
#[derive(Clone, Debug, Default)]
pub struct Order {
    /// By how often and how lately, with more weight for uses in `folder`
    /// (or inside it); otherwise the newest first (Far).
    pub frecency: bool,
    pub folder: String,
    /// The agent's entries after the user's.
    pub agent_last: bool,
}

/// `inner` is `outer` or inside it (case-insensitive, `\` or `/`).
fn same_or_inside(inner: &str, outer: &str) -> bool {
    if outer.is_empty() {
        return false;
    }
    let norm = |s: &str| {
        s.to_lowercase()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_string()
    };
    let (i, o) = (norm(inner), norm(outer));
    i == o || i.starts_with(&format!("{o}\\"))
}

/// Secrets in a command or a field are replaced by `***` before it goes
/// into the history (docs/15, improvement 6).
pub fn redact(text: &str) -> String {
    use std::sync::OnceLock;
    static RULES: OnceLock<Vec<regex::Regex>> = OnceLock::new();
    let rules = RULES.get_or_init(|| {
        [
            // password=…, --token …, api_key: …
            r"(?i)((?:--?)?(?:password|passwd|pwd|token|secret|api[_-]?key)\s*[=:\s]\s*)(\S+)",
            // Authorization: Bearer …
            r"(?i)(authorization:\s*(?:bearer|basic)?\s*)(\S+)",
            // Keys by their look: sk-…, ghp_…, xoxb-…
            r"()\b((?:sk|pk|rk)-[A-Za-z0-9_-]{16,}|gh[pousr]_[A-Za-z0-9]{20,}|xox[abpr]-[A-Za-z0-9-]{10,})",
        ]
        .iter()
        .filter_map(|r| regex::Regex::new(r).ok())
        .collect()
    });
    let mut out = text.to_string();
    for re in rules {
        out = re.replace_all(&out, "${1}***").into_owned();
    }
    out
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
    /// More about the entry (JSON): a command's exit code and duration.
    pub data: Option<String>,
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
             CREATE INDEX IF NOT EXISTS use_entry ON use(entry_id);
             CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);",
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
            "SELECT id, text, locked, last_used, folder, actor, data FROM entry
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
                data: r.get(6)?,
            })
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    /// A list in the given order: locked first; then (if asked) the user's
    /// before the agent's; then by weight — each of the last uses counts
    /// `1 / (1 + days / 7)`, twice in the current folder or inside it — or
    /// the newest first.
    pub fn ordered(&self, kind: Kind, list: &str, order: &Order) -> Vec<Entry> {
        let mut entries = self.list(kind, list);
        let weight = |e: &Entry| -> f64 {
            if !order.frecency {
                return 0.0;
            }
            let Ok(mut stmt) = self
                .db
                .prepare_cached("SELECT time, folder FROM use WHERE entry_id = ?1")
            else {
                return 0.0;
            };
            let t = now();
            stmt.query_map(params![e.id], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })
            .map(|rows| {
                rows.flatten()
                    .map(|(time, folder)| {
                        let days = (t - time).max(0) as f64 / 86_400_000.0;
                        let here = if same_or_inside(&folder, &order.folder) {
                            2.0
                        } else {
                            1.0
                        };
                        here / (1.0 + days / 7.0)
                    })
                    .sum()
            })
            .unwrap_or(0.0)
        };
        let mut keyed: Vec<(f64, Entry)> = entries.drain(..).map(|e| (weight(&e), e)).collect();
        keyed.sort_by(|(wa, a), (wb, b)| {
            b.locked
                .cmp(&a.locked)
                .then_with(|| {
                    if order.agent_last {
                        (a.actor == "agent").cmp(&(b.actor == "agent"))
                    } else {
                        std::cmp::Ordering::Equal
                    }
                })
                .then_with(|| wb.partial_cmp(wa).unwrap_or(std::cmp::Ordering::Equal))
                .then_with(|| b.last_used.cmp(&a.last_used))
        });
        keyed.into_iter().map(|(_, e)| e).collect()
    }

    /// The newest entry the user made (not the agent).
    pub fn last_by_user(&self, kind: Kind, list: &str) -> Option<String> {
        self.db
            .query_row(
                "SELECT text FROM entry WHERE kind = ?1 AND list = ?2 AND actor = 'user'
                 ORDER BY last_used DESC, id DESC LIMIT 1",
                params![kind.as_str(), list],
                |r| r.get(0),
            )
            .optional()
            .unwrap_or(None)
    }

    /// More about an entry (a command's result): JSON in `data`.
    pub fn set_data(&self, kind: Kind, list: &str, text: &str, data: &str) {
        let _ = self.db.execute(
            "UPDATE entry SET data = ?4 WHERE kind = ?1 AND list = ?2 AND text = ?3 COLLATE NOCASE",
            params![kind.as_str(), list, text, data],
        );
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

    /// A note the application keeps in the database (e.g. that the import
    /// of Far's history has been offered).
    pub fn meta(&self, key: &str) -> Option<String> {
        self.db
            .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
                r.get(0)
            })
            .optional()
            .unwrap_or(None)
    }

    pub fn set_meta(&self, key: &str, value: &str) {
        let _ = self.db.execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        );
    }

    /// Takes Far's histories (docs/15, improvement 5): each Far record is a
    /// use at its time — a new entry, or one more use of the same text
    /// (any case; the lock is kept if either has it). A record already
    /// taken (a use at the same time) is skipped, so a second import adds
    /// only what is new. `redact`: secrets become `***`, as when recorded.
    pub fn import_far(&self, path: &Path, redact_secrets: bool) -> rusqlite::Result<FarCounts> {
        let rows = read_far(path)?;
        let mut counts = FarCounts::default();
        let tx = self.db.unchecked_transaction()?;
        for row in rows {
            let text = if redact_secrets && row.kind == Kind::Command {
                redact(&row.text)
            } else {
                row.text
            };
            let found: Option<(i64, i64)> = self
                .db
                .query_row(
                    "SELECT id, last_used FROM entry
                     WHERE kind = ?1 AND list = ?2 AND text = ?3 COLLATE NOCASE",
                    params![row.kind.as_str(), row.list, text],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let id = match found {
                Some((id, last_used)) => {
                    let taken: bool = self
                        .db
                        .query_row(
                            "SELECT EXISTS(SELECT 1 FROM use WHERE entry_id = ?1 AND time = ?2)",
                            params![id, row.time],
                            |r| r.get(0),
                        )
                        .unwrap_or(false);
                    if taken {
                        continue;
                    }
                    self.db.execute(
                        "UPDATE entry SET uses = uses + 1, locked = MAX(locked, ?2),
                             first_used = MIN(first_used, ?3), last_used = MAX(last_used, ?3)
                         WHERE id = ?1",
                        params![id, row.locked, row.time],
                    )?;
                    if row.time > last_used {
                        self.db.execute(
                            "UPDATE entry SET folder = ?2 WHERE id = ?1",
                            params![id, row.folder],
                        )?;
                    }
                    id
                }
                None => {
                    self.db.execute(
                        "INSERT INTO entry(kind, list, text, folder, locked, first_used,
                             last_used, data)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7)",
                        params![
                            row.kind.as_str(),
                            row.list,
                            text,
                            row.folder,
                            row.locked,
                            row.time,
                            row.data
                        ],
                    )?;
                    self.db.last_insert_rowid()
                }
            };
            self.db.execute(
                "INSERT INTO use(entry_id, time, folder) VALUES (?1, ?2, ?3)",
                params![id, row.time, row.folder],
            )?;
            counts.count(row.kind);
        }
        self.db.execute(
            "DELETE FROM use WHERE rowid IN (SELECT rowid FROM (SELECT rowid, ROW_NUMBER()
                 OVER (PARTITION BY entry_id ORDER BY time DESC) AS n FROM use) WHERE n > ?1)",
            params![USES_KEPT],
        )?;
        tx.commit()?;
        Ok(counts)
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

/// How many records of each kind (in Far's history, or taken from it).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FarCounts {
    pub commands: usize,
    pub folders: usize,
    pub views: usize,
    pub dialogs: usize,
}

impl FarCounts {
    fn count(&mut self, kind: Kind) {
        match kind {
            Kind::Command => self.commands += 1,
            Kind::Folder => self.folders += 1,
            Kind::View => self.views += 1,
            Kind::Dialog => self.dialogs += 1,
        }
    }

    pub fn total(&self) -> usize {
        self.commands + self.folders + self.views + self.dialogs
    }
}

/// Far's history file: in Far's local profile (`FARLOCALPROFILE` when
/// afar runs from Far, else `%LOCALAPPDATA%\Far Manager\Profile`), or in
/// the roaming one (`FARPROFILE`, `%APPDATA%\Far Manager\Profile`).
pub fn far_history_path() -> Option<PathBuf> {
    let var = |name: &str| std::env::var_os(name).map(PathBuf::from);
    let profile = |name: &str| var(name).map(|p| p.join("Far Manager").join("Profile"));
    [
        var("FARLOCALPROFILE"),
        profile("LOCALAPPDATA"),
        var("FARPROFILE"),
        profile("APPDATA"),
    ]
    .into_iter()
    .flatten()
    .map(|dir| dir.join("history.db"))
    .find(|p| p.is_file())
}

/// What Far's history holds, by kind.
pub fn far_counts(path: &Path) -> rusqlite::Result<FarCounts> {
    let mut counts = FarCounts::default();
    for row in read_far(path)? {
        counts.count(row.kind);
    }
    Ok(counts)
}

/// A record of Far's history as afar takes it.
struct FarRow {
    kind: Kind,
    list: String,
    text: String,
    locked: bool,
    /// Unix time, milliseconds.
    time: i64,
    folder: String,
    data: Option<String>,
}

/// Far's records, oldest first (docs/14 §2: `history(kind, key, type,
/// lock, name, time, guid, file, data)`; kinds: commands, folders,
/// view/edit, dialogs; time in 100 ns since 1601). Plugins' folders and
/// files (with a plugin's guid) cannot be opened by afar and are left out;
/// so are empty texts. Read only: Far may be running.
fn read_far(path: &Path) -> rusqlite::Result<Vec<FarRow>> {
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let _ = db.busy_timeout(std::time::Duration::from_secs(2));
    let mut stmt = db.prepare(
        "SELECT kind, key, type, lock, name, time, guid, data FROM history ORDER BY time, id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, i64>(5)?,
            r.get::<_, String>(6)?,
            r.get::<_, String>(7)?,
        ))
    })?;
    // FILETIME of the Unix epoch.
    const EPOCH: i64 = 116_444_736_000_000_000;
    let mut out = Vec::new();
    for row in rows {
        let (kind, key, ty, lock, name, time, guid, data) = row?;
        if name.is_empty() {
            continue;
        }
        let (kind, folder, data) = match kind {
            // A command's folder is in its data.
            0 => (Kind::Command, data, None),
            1 if guid.is_empty() => (Kind::Folder, String::new(), None),
            // Far's record type: 0 viewer, 1 editor, 2/3 external, 4 read-only editor.
            2 if guid.is_empty() => (
                Kind::View,
                String::new(),
                Some(format!("{{\"far_type\":{ty}}}")),
            ),
            3 => (Kind::Dialog, String::new(), None),
            _ => continue,
        };
        out.push(FarRow {
            kind,
            list: if kind == Kind::Dialog {
                key
            } else {
                String::new()
            },
            text: name,
            locked: lock != 0,
            time: (time - EPOCH) / 10_000,
            folder,
            data,
        });
    }
    Ok(out)
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
    fn orders_by_use_and_folder() {
        let h = History::in_memory();
        h.add(Kind::Dialog, "Copy", "d:\\a", "c:\\work", "user");
        h.add(Kind::Dialog, "Copy", "d:\\b", "c:\\other", "user");
        h.add(Kind::Dialog, "Copy", "d:\\b", "c:\\other", "user");
        h.add(Kind::Dialog, "Copy", "d:\\x", "c:\\work", "agent");
        let order = |folder: &str| Order {
            frecency: true,
            folder: folder.into(),
            agent_last: true,
        };
        let texts = |o: Order| -> Vec<String> {
            h.ordered(Kind::Dialog, "Copy", &o)
                .into_iter()
                .map(|e| e.text)
                .collect()
        };
        // Twice used wins elsewhere; the agent's entry is last.
        assert_eq!(texts(order("c:\\none")), ["d:\\b", "d:\\a", "d:\\x"]);
        // In c:\work (or inside it) its entry weighs twice: a tie with "b",
        // broken by the newest use.
        assert_eq!(texts(order("C:\\Work\\sub"))[2], "d:\\x");
        assert!(same_or_inside("c:\\work\\sub", "C:\\Work"));
        assert!(!same_or_inside("c:\\workshop", "c:\\work"));
        assert_eq!(
            h.last_by_user(Kind::Dialog, "Copy").as_deref(),
            Some("d:\\b")
        );
    }

    #[test]
    fn redacts_secrets() {
        assert_eq!(
            redact("curl -u x --password hunter2 y"),
            "curl -u x --password *** y"
        );
        assert_eq!(redact("set TOKEN=abc123"), "set TOKEN=***");
        assert_eq!(
            redact("curl -H \"Authorization: Bearer xyz\" u"),
            "curl -H \"Authorization: Bearer *** u"
        );
        assert_eq!(redact("use sk-abcdefghijklmnopqrstu now"), "use *** now");
        assert_eq!(redact("cargo build"), "cargo build");
    }

    #[test]
    fn imports_far_history() {
        let dir = std::env::temp_dir().join(format!("afar-far-import-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.db");
        {
            let far = Connection::open(&path).unwrap();
            far.execute_batch(
                "CREATE TABLE history(id INTEGER PRIMARY KEY, kind INTEGER NOT NULL,
                     key TEXT NOT NULL, type INTEGER NOT NULL, lock INTEGER NOT NULL,
                     name TEXT NOT NULL, time INTEGER NOT NULL, guid TEXT NOT NULL,
                     file TEXT NOT NULL, data TEXT NOT NULL);",
            )
            .unwrap();
            // 2024-01-01 00:00:00 UTC and a second later, as FILETIME.
            let t = 133_485_408_000_000_000_i64;
            // kind, key, type, lock, name, time, guid, data
            type Row<'a> = (i64, &'a str, i64, i64, &'a str, i64, &'a str, &'a str);
            let rows: [Row; 7] = [
                (0, "", 0, 0, "cargo build", t, "", "F:\\AGI\\far"),
                (0, "", 0, 1, "Cargo Build", t + 10_000_000, "", "F:\\AGI"),
                (1, "", 0, 0, "C:\\Windows", t, "", ""),
                (1, "", 0, 0, "arc:\\x.zip", t, "{plugin}", ""),
                (2, "", 1, 0, "C:\\a.txt", t, "", ""),
                (3, "Copy", 0, 0, "D:\\backup", t, "", ""),
                (3, "Copy", 0, 0, "", t, "", ""),
            ];
            for (kind, key, ty, lock, name, time, guid, data) in rows {
                far.execute(
                    "INSERT INTO history(kind, key, type, lock, name, time, guid, file, data)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, '', ?8)",
                    params![kind, key, ty, lock, name, time, guid, data],
                )
                .unwrap();
            }
        }
        let c = far_counts(&path).unwrap();
        assert_eq!((c.commands, c.folders, c.views, c.dialogs), (2, 1, 1, 1));
        let h = History::in_memory();
        h.add(Kind::Dialog, "Copy", "d:\\BACKUP", "", "user");
        let taken = h.import_far(&path, true).unwrap();
        assert_eq!(taken, c);
        // The two commands are one entry: two uses, locked, the newer folder.
        let cmds = h.list(Kind::Command, "");
        assert_eq!(cmds.len(), 1);
        assert!(cmds[0].locked);
        assert_eq!(cmds[0].folder, "F:\\AGI");
        assert_eq!(cmds[0].last_used, 1_704_067_201_000);
        // The existing field value got one more use and kept its spelling.
        assert_eq!(texts(&h, "Copy"), ["d:\\BACKUP"]);
        assert_eq!(
            h.list(Kind::View, "")[0].data.as_deref(),
            Some("{\"far_type\":1}")
        );
        // A second import takes nothing.
        assert_eq!(h.import_far(&path, true).unwrap().total(), 0);
        h.set_meta("far_import", "done");
        assert_eq!(h.meta("far_import").as_deref(), Some("done"));
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&dir).unwrap();
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
