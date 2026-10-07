//! Claude Code's saved conversations of a folder (docs/13-agent-sessions.md,
//! "Сессии каталога"): `~/.claude/projects/<folder>/<id>.jsonl`, where
//! `<folder>` is the path with every character but ASCII letters and digits
//! replaced by `-`. A session's name: the last `custom-title` (`/rename`),
//! else the last `ai-title`, else the last prompt.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// How much of a conversation file is read for its name: its start and
/// its end (files grow to tens of megabytes).
const HEAD: u64 = 256 * 1024;
const TAIL: u64 = 1024 * 1024;

#[derive(Clone, Debug)]
pub struct SessionInfo {
    pub id: String,
    pub title: String,
    pub modified: SystemTime,
    pub size: u64,
}

/// The folder Claude Code keeps a project's conversations in.
pub fn project_dir(cwd: &Path) -> PathBuf {
    let name: String = cwd
        .display()
        .to_string()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    crate::ide::claude_dir().join("projects").join(name)
}

/// The conversations of `cwd`, newest first.
pub fn list(cwd: &Path) -> Vec<SessionInfo> {
    let Ok(read) = std::fs::read_dir(project_dir(cwd)) else {
        return Vec::new();
    };
    let mut out: Vec<SessionInfo> = read
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension()? != "jsonl" {
                return None;
            }
            let id = path.file_stem()?.to_string_lossy().into_owned();
            let meta = e.metadata().ok()?;
            let title = title_of(&path).unwrap_or_else(|| id.clone());
            Some(SessionInfo {
                id,
                title,
                modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                size: meta.len(),
            })
        })
        .collect();
    out.sort_by_key(|s| std::cmp::Reverse(s.modified));
    out
}

/// The text of the start and the end of a file (lines cut at the edges
/// are dropped by the JSON parsing).
fn head_and_tail(path: &Path) -> Option<String> {
    let mut f = std::fs::File::open(path).ok()?;
    let size = f.metadata().ok()?.len();
    let mut bytes = Vec::new();
    if size <= HEAD + TAIL {
        f.read_to_end(&mut bytes).ok()?;
    } else {
        let mut head = vec![0u8; HEAD as usize];
        f.read_exact(&mut head).ok()?;
        f.seek(SeekFrom::Start(size - TAIL)).ok()?;
        let mut tail = Vec::new();
        f.read_to_end(&mut tail).ok()?;
        bytes = head;
        bytes.push(b'\n');
        bytes.extend(tail);
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// A conversation's name (see the module's comment).
pub fn title_of(path: &Path) -> Option<String> {
    let text = head_and_tail(path)?;
    let (mut custom, mut ai, mut prompt) = (None, None, None);
    for line in text.lines() {
        // Only the lines that can matter are parsed.
        if !(line.contains("\"custom-title\"")
            || line.contains("\"ai-title\"")
            || line.contains("\"last-prompt\""))
        {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let get = |k: &str| v.get(k).and_then(|t| t.as_str()).map(str::to_string);
        match v.get("type").and_then(|t| t.as_str()) {
            Some("custom-title") => custom = get("customTitle").or(custom),
            Some("ai-title") => ai = get("aiTitle").or(ai),
            Some("last-prompt") => prompt = get("lastPrompt").or(prompt),
            _ => {}
        }
    }
    custom.or(ai).or(prompt).map(|t| {
        let t: String = t.split_whitespace().collect::<Vec<_>>().join(" ");
        t.chars().take(80).collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_names_like_claude_code() {
        let dir = project_dir(Path::new(r"F:\AGI\far"));
        assert!(dir.ends_with("F--AGI-far"));
    }

    #[test]
    fn titles_by_priority() {
        let dir = std::env::temp_dir().join(format!("afar-sessions-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.jsonl");
        std::fs::write(
            &f,
            concat!(
                "{\"type\":\"last-prompt\",\"lastPrompt\":\"fix the build\"}\n",
                "{\"type\":\"ai-title\",\"aiTitle\":\"Build fix\"}\n",
                "{\"type\":\"user\",\"message\":{}}\n",
            ),
        )
        .unwrap();
        assert_eq!(title_of(&f).as_deref(), Some("Build fix"));
        std::fs::write(
            &f,
            concat!(
                "{\"type\":\"custom-title\",\"customTitle\":\"viewer\"}\n",
                "{\"type\":\"ai-title\",\"aiTitle\":\"Build fix\"}\n",
            ),
        )
        .unwrap();
        assert_eq!(title_of(&f).as_deref(), Some("viewer"));
        std::fs::write(&f, "{\"type\":\"last-prompt\",\"lastPrompt\":\"a\\nb\"}\n").unwrap();
        assert_eq!(title_of(&f).as_deref(), Some("a b"));
    }
}
