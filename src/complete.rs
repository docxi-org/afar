//! Autocompletion candidates (Far's `EditControl::AutoComplete`,
//! docs/14 §8, docs/15): the field's history by the whole line, then — by
//! the line's last token — files and folders, `%VARIABLES%`, programs on
//! `PATH`. Candidates are whole new lines.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Far's WordDiv without `":\/%.?-`, plus blanks: what ends a token.
const TOKEN_DIV: &str = "~!^&*()+|{}<>`=[];',";

/// Results per source at most (a folder can be huge).
const MAX_PER_SOURCE: usize = 500;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// The whole new line.
    pub line: String,
    /// What the list shows.
    pub shown: String,
    /// From the history (Shift+Del can remove it).
    pub history: bool,
    pub locked: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    /// The source's title (Far's "История", "Файлы", "Окружение"), as a
    /// message id.
    pub title: &'static str,
    pub items: Vec<Candidate>,
}

/// Which sources to use for this request.
#[derive(Clone, Copy, Debug, Default)]
pub struct Sources {
    pub history: bool,
    pub files: bool,
    pub variables: bool,
    pub programs: bool,
}

/// The line split for completing its last token: what stays, the token,
/// and whether the user opened a quote for it.
pub fn split_token(line: &str) -> (&str, &str, bool) {
    let quotes = line.matches('"').count();
    if quotes % 2 == 1 {
        let q = line.rfind('"').unwrap_or(0);
        return (&line[..q], &line[q + 1..], true);
    }
    let start = line
        .char_indices()
        .rfind(|(_, c)| c.is_whitespace() || TOKEN_DIV.contains(*c))
        .map_or(0, |(i, c)| i + c.len_utf8());
    (&line[..start], &line[start..], false)
}

fn starts_with_ci(text: &str, prefix: &str) -> bool {
    text.to_lowercase().starts_with(&prefix.to_lowercase())
}

/// `prefix` + `name`, quoted when it has a blank or the user opened a
/// quote (which is then closed).
fn join(prefix: &str, name: &str, quoted: bool) -> String {
    if quoted || name.contains(' ') {
        format!("{prefix}\"{name}\"")
    } else {
        format!("{prefix}{name}")
    }
}

/// `%NAME%` replaced by the variable's value.
fn expand_env(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(a) = rest.find('%') {
        let Some(b) = rest[a + 1..].find('%') else {
            break;
        };
        let name = &rest[a + 1..a + 1 + b];
        out.push_str(&rest[..a]);
        match std::env::var(name) {
            Ok(v) if !name.is_empty() => out.push_str(&v),
            _ => out.push_str(&rest[a..a + 2 + b]),
        }
        rest = &rest[a + 2 + b..];
    }
    out.push_str(rest);
    out
}

/// The executables on `PATH` (by `PATHEXT`), read at most once a minute
/// (Far reads them on every key).
#[derive(Default)]
pub struct Programs {
    names: Vec<String>,
    read: Option<Instant>,
}

impl Programs {
    fn names(&mut self) -> &[String] {
        if self
            .read
            .is_none_or(|t| t.elapsed() > Duration::from_secs(60))
        {
            self.read = Some(Instant::now());
            let exts: Vec<String> = std::env::var("PATHEXT")
                .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
                .split(';')
                .filter(|e| !e.is_empty())
                .map(|e| e.to_lowercase())
                .collect();
            let mut names: Vec<String> = std::env::var_os("PATH")
                .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
                .unwrap_or_default()
                .into_iter()
                .filter_map(|dir| std::fs::read_dir(dir).ok())
                .flat_map(|rd| rd.flatten())
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    let lower = name.to_lowercase();
                    exts.iter()
                        .any(|x| lower.ends_with(x.as_str()))
                        .then_some(name)
                })
                .collect();
            names.sort_by_key(|n| n.to_lowercase());
            names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
            self.names = names;
        }
        &self.names
    }
}

/// Sorted by name (case-insensitive), without repeats, capped.
fn tidy(mut items: Vec<Candidate>) -> Vec<Candidate> {
    items.sort_by_key(|c| c.shown.to_lowercase());
    items.dedup_by(|a, b| a.line.eq_ignore_ascii_case(&b.line));
    items.truncate(MAX_PER_SOURCE);
    items
}

/// The candidates for `line`: the field's `history` (texts and locks, in
/// the list's order), and by the last token — files relative to `base`,
/// variables, programs.
pub fn complete(
    line: &str,
    history: &[(String, bool)],
    sources: Sources,
    base: &Path,
    programs: &mut Programs,
) -> Vec<Group> {
    let mut groups = Vec::new();
    if line.is_empty() {
        return groups;
    }
    if sources.history {
        let items: Vec<Candidate> = history
            .iter()
            .filter(|(t, _)| starts_with_ci(t, line))
            .map(|(t, locked)| Candidate {
                line: t.clone(),
                shown: t.clone(),
                history: true,
                locked: *locked,
            })
            .collect();
        if !items.is_empty() {
            groups.push(Group {
                title: "MCompletionHistoryTitle",
                items,
            });
        }
    }
    let (prefix, token, quoted) = split_token(line);
    let token = token.trim_matches('"');
    if token.is_empty() {
        return groups;
    }
    if sources.files {
        let items = files(prefix, token, quoted, base);
        if !items.is_empty() {
            groups.push(Group {
                title: "MCompletionFilesTitle",
                items,
            });
        }
    }
    if sources.variables && token.starts_with('%') {
        let items: Vec<Candidate> = std::env::vars()
            .map(|(k, _)| format!("%{k}%"))
            .filter(|v| starts_with_ci(v, token))
            .map(|v| Candidate {
                line: join(prefix, &v, quoted),
                shown: v,
                history: false,
                locked: false,
            })
            .collect();
        if !items.is_empty() {
            groups.push(Group {
                title: "MCompletionEnvironmentTitle",
                items: tidy(items),
            });
        }
    }
    if sources.programs && !line.contains(['\\', '/']) {
        let items: Vec<Candidate> = programs
            .names()
            .iter()
            .filter(|n| starts_with_ci(n, token))
            .map(|n| Candidate {
                line: join(prefix, n, quoted),
                shown: n.clone(),
                history: false,
                locked: false,
            })
            .collect();
        if !items.is_empty() {
            groups.push(Group {
                title: "MCompletionFilesTitle",
                items: tidy(items),
            });
        }
    }
    groups
}

/// Files and folders whose names start with the token's last part, in its
/// folder (relative to `base`).
fn files(prefix: &str, token: &str, quoted: bool, base: &Path) -> Vec<Candidate> {
    let expanded = expand_env(token);
    let cut = expanded.rfind(['\\', '/']).map_or(0, |i| i + 1);
    let (dir_part, name_part) = expanded.split_at(cut);
    let dir: PathBuf = if dir_part.is_empty() {
        base.to_path_buf()
    } else {
        let p = Path::new(dir_part);
        if p.is_absolute() || dir_part.starts_with(['\\', '/']) || dir_part.contains(':') {
            p.to_path_buf()
        } else {
            base.join(p)
        }
    };
    // The typed folder part stays as typed (not expanded).
    let typed_cut = token.rfind(['\\', '/']).map_or(0, |i| i + 1);
    let typed_dir = &token[..typed_cut];
    let Ok(read) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let items = read
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| starts_with_ci(n, name_part))
        .take(MAX_PER_SOURCE * 4)
        .map(|n| {
            let name = format!("{typed_dir}{n}");
            Candidate {
                line: join(prefix, &name, quoted),
                shown: name,
                history: false,
                locked: false,
            }
        })
        .collect();
    tidy(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_like_far() {
        assert_eq!(split_token("copy foo.t"), ("copy ", "foo.t", false));
        assert_eq!(split_token("dir c:\\win"), ("dir ", "c:\\win", false));
        assert_eq!(split_token("a.exe x=y"), ("a.exe x=", "y", false));
        assert_eq!(
            split_token("type \"Program F"),
            ("type ", "Program F", true)
        );
        assert_eq!(split_token("echo %PA"), ("echo ", "%PA", false));
    }

    #[test]
    fn completes_history_files_and_variables() {
        let dir = std::env::temp_dir().join(format!("afar-complete-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Sub Dir")).unwrap();
        std::fs::write(dir.join("readme.md"), "").unwrap();
        std::fs::write(dir.join("Rust.txt"), "").unwrap();
        let history = vec![("type readme.md".to_string(), true), ("tree".into(), false)];
        let sources = Sources {
            history: true,
            files: true,
            variables: true,
            programs: false,
        };
        let mut programs = Programs::default();
        let g = complete("type r", &history, sources, &dir, &mut programs);
        assert_eq!(g[0].title, "MCompletionHistoryTitle");
        assert_eq!(g[0].items[0].line, "type readme.md");
        assert!(g[0].items[0].locked);
        let files: Vec<&str> = g[1].items.iter().map(|c| c.line.as_str()).collect();
        assert_eq!(files, ["type readme.md", "type Rust.txt"]);
        // A name with a blank is quoted; an opened quote is closed.
        let g = complete("cd su", &[], sources, &dir, &mut programs);
        assert_eq!(g[0].items[0].line, "cd \"Sub Dir\"");
        let g = complete("cd \"Su", &[], sources, &dir, &mut programs);
        assert_eq!(g[0].items[0].line, "cd \"Sub Dir\"");
        // %VARIABLES%.
        unsafe { std::env::set_var("AFAR_TEST_VAR", "1") };
        let g = complete("echo %AFAR_TEST_V", &[], sources, &dir, &mut programs);
        assert_eq!(g[0].items[0].line, "echo %AFAR_TEST_VAR%");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
