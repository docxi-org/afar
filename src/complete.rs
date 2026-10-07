//! Autocompletion candidates (Far's `EditControl::AutoComplete`,
//! docs/14 §8, docs/15): the field's history by the whole line, then — by
//! the line's last token — files and folders, names on the passive panel,
//! `%VARIABLES%`, programs on `PATH`. Candidates are whole new lines.
//! afar's additions (docs/15, stage 4): fuzzy matches after the ones from
//! the start, folders read in the background, the ghost suggestion.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Far's WordDiv without `":\/%.?-`, plus blanks: what ends a token.
const TOKEN_DIV: &str = "~!^&*()+|{}<>`=[];',";

/// Results per source at most (a folder can be huge).
const MAX_PER_SOURCE: usize = 500;

/// How long a folder read waits before going on without it (a slow or
/// network folder comes on a later key).
const READ_WAIT: Duration = Duration::from_millis(150);
/// How long a folder's names stay cached.
const DIR_FRESH: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// The whole new line.
    pub line: String,
    /// What the list shows.
    pub shown: String,
    /// From the history (Shift+Del can remove it).
    pub history: bool,
    pub locked: bool,
    /// Characters of `shown` the typed text matched (highlighted).
    pub marks: Vec<usize>,
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

/// Where a request is made.
pub struct Context<'a> {
    /// Relative paths start here (the active panel's folder).
    pub base: &'a Path,
    /// The passive panel: its folder and names (offered as full paths).
    pub passive: Option<(&'a Path, &'a [String])>,
    /// Characters in order anywhere, after the matches from the start.
    pub fuzzy: bool,
}

/// `pattern`'s characters in `text` in order (case-insensitive): a score
/// (lower is closer: an early start, few gaps) and the matched positions.
pub fn fuzzy(text: &str, pattern: &str) -> Option<(usize, Vec<usize>)> {
    let t: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
    let mut marks = Vec::new();
    let mut from = 0;
    for pc in pattern.chars().flat_map(char::to_lowercase) {
        let i = (from..t.len()).find(|&i| t[i] == pc)?;
        marks.push(i);
        from = i + 1;
    }
    let first = *marks.first().unwrap_or(&0);
    let gaps: usize = marks.windows(2).map(|w| w[1] - w[0] - 1).sum();
    Some((first + gaps * 2, marks))
}

/// How `text` matches `pattern`: from the start (score 0, as Far), or —
/// when allowed — anywhere in order (after them).
fn matches(text: &str, pattern: &str, fuzzy_ok: bool) -> Option<(usize, Vec<usize>)> {
    if starts_with_ci(text, pattern) {
        return Some((0, (0..pattern.chars().count()).collect()));
    }
    if fuzzy_ok && !pattern.is_empty() {
        return fuzzy(text, pattern).map(|(s, m)| (s + 1000, m));
    }
    None
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

/// Folders' names and when they were read.
type Dirs = HashMap<PathBuf, (Vec<String>, Instant)>;

/// What completion keeps between keys: the programs on `PATH` (read at
/// most once a minute — Far reads them on every key) and folders' names
/// (read in the background, briefly cached).
#[derive(Default)]
pub struct Cache {
    programs: Vec<String>,
    programs_read: Option<Instant>,
    dirs: Arc<Mutex<Dirs>>,
}

impl Cache {
    fn programs(&mut self) -> &[String] {
        if self
            .programs_read
            .is_none_or(|t| t.elapsed() > Duration::from_secs(60))
        {
            self.programs_read = Some(Instant::now());
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
            self.programs = names;
        }
        &self.programs
    }

    /// A folder's names: from the cache, or read in a thread that is
    /// waited for a moment; a slow folder (a network one) comes into the
    /// cache later and is offered on a following key.
    fn dir(&self, dir: &Path) -> Vec<String> {
        if let Ok(dirs) = self.dirs.lock()
            && let Some((names, t)) = dirs.get(dir)
            && t.elapsed() < DIR_FRESH
        {
            return names.clone();
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let (path, dirs) = (dir.to_path_buf(), self.dirs.clone());
        std::thread::spawn(move || {
            let names: Vec<String> = std::fs::read_dir(&path)
                .map(|rd| {
                    rd.flatten()
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            if let Ok(mut d) = dirs.lock() {
                d.insert(path, (names.clone(), Instant::now()));
            }
            let _ = tx.send(names);
        });
        rx.recv_timeout(READ_WAIT).unwrap_or_default()
    }
}

/// Sorted by score, then by name, without repeats, capped.
fn tidy(mut items: Vec<(usize, Candidate)>) -> Vec<Candidate> {
    items.sort_by(|(sa, a), (sb, b)| {
        sa.cmp(sb)
            .then_with(|| a.shown.to_lowercase().cmp(&b.shown.to_lowercase()))
    });
    let mut out: Vec<Candidate> = items.into_iter().map(|(_, c)| c).collect();
    out.dedup_by(|a, b| a.line.eq_ignore_ascii_case(&b.line));
    out.truncate(MAX_PER_SOURCE);
    out
}

/// The candidates for `line`: the field's `history` (texts and locks, in
/// the list's order), and by the last token — files relative to the
/// context's folder, the passive panel's names, variables, programs.
pub fn complete(
    line: &str,
    history: &[(String, bool)],
    sources: Sources,
    ctx: &Context,
    cache: &mut Cache,
) -> Vec<Group> {
    let mut groups = Vec::new();
    if line.is_empty() {
        return groups;
    }
    if sources.history {
        let mut items: Vec<(usize, usize, Candidate)> = history
            .iter()
            .enumerate()
            .filter_map(|(order, (t, locked))| {
                matches(t, line, ctx.fuzzy).map(|(score, marks)| {
                    (
                        score,
                        order,
                        Candidate {
                            line: t.clone(),
                            shown: t.clone(),
                            history: true,
                            locked: *locked,
                            marks,
                        },
                    )
                })
            })
            .collect();
        // From the start first, in the history's order; then fuzzy ones.
        items.sort_by_key(|(score, order, _)| (*score > 0, *score, *order));
        let items: Vec<Candidate> = items.into_iter().map(|(_, _, c)| c).collect();
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
        let items = files(prefix, token, quoted, ctx, cache);
        if !items.is_empty() {
            groups.push(Group {
                title: "MCompletionFilesTitle",
                items,
            });
        }
        // Names on the passive panel, as full paths (`copy x <there>`).
        if let Some((dir, names)) = ctx.passive
            && !token.contains(['\\', '/', ':'])
        {
            let items: Vec<(usize, Candidate)> = names
                .iter()
                .filter(|n| n.as_str() != "..")
                .filter_map(|n| {
                    matches(n, token, ctx.fuzzy).map(|(score, marks)| {
                        let full = dir.join(n).display().to_string();
                        let offset = full.chars().count() - n.chars().count();
                        (
                            score,
                            Candidate {
                                line: join(prefix, &full, quoted),
                                shown: full,
                                history: false,
                                locked: false,
                                marks: marks.into_iter().map(|m| m + offset).collect(),
                            },
                        )
                    })
                })
                .collect();
            let items = tidy(items);
            if !items.is_empty() {
                groups.push(Group {
                    title: "completion-passive-panel",
                    items,
                });
            }
        }
    }
    if sources.variables && token.starts_with('%') {
        let items: Vec<(usize, Candidate)> = std::env::vars()
            .map(|(k, _)| format!("%{k}%"))
            .filter_map(|v| {
                matches(&v, token, false).map(|(score, marks)| {
                    (
                        score,
                        Candidate {
                            line: join(prefix, &v, quoted),
                            shown: v,
                            history: false,
                            locked: false,
                            marks,
                        },
                    )
                })
            })
            .collect();
        let items = tidy(items);
        if !items.is_empty() {
            groups.push(Group {
                title: "MCompletionEnvironmentTitle",
                items,
            });
        }
    }
    if sources.programs && !line.contains(['\\', '/']) {
        let items: Vec<(usize, Candidate)> = cache
            .programs()
            .iter()
            .filter_map(|n| {
                matches(n, token, ctx.fuzzy).map(|(score, marks)| {
                    (
                        score,
                        Candidate {
                            line: join(prefix, n, quoted),
                            shown: n.clone(),
                            history: false,
                            locked: false,
                            marks,
                        },
                    )
                })
            })
            .collect();
        let items = tidy(items);
        if !items.is_empty() {
            groups.push(Group {
                title: "MCompletionFilesTitle",
                items,
            });
        }
    }
    groups
}

/// The ghost suggestion (docs/15, improvement 8): what to show after the
/// typed `line` — the rest of the first history entry starting with it,
/// else of the first file or program starting with it.
pub fn ghost(
    line: &str,
    history: &[(String, bool)],
    sources: Sources,
    ctx: &Context,
    cache: &mut Cache,
) -> Option<String> {
    if line.is_empty() {
        return None;
    }
    let n = line.chars().count();
    let rest = |t: &str| -> Option<String> {
        (starts_with_ci(t, line) && t.chars().count() > n).then(|| t.chars().skip(n).collect())
    };
    if sources.history
        && let Some(s) = history.iter().find_map(|(t, _)| rest(t))
    {
        return Some(s);
    }
    let strict = Context {
        base: ctx.base,
        passive: None,
        fuzzy: false,
    };
    let others = Sources {
        history: false,
        ..sources
    };
    complete(line, &[], others, &strict, cache)
        .into_iter()
        .flat_map(|g| g.items)
        .find_map(|c| rest(&c.line))
}

/// Files and folders matching the token's last part, in its folder
/// (relative to the context's folder).
fn files(
    prefix: &str,
    token: &str,
    quoted: bool,
    ctx: &Context,
    cache: &mut Cache,
) -> Vec<Candidate> {
    let expanded = expand_env(token);
    let cut = expanded.rfind(['\\', '/']).map_or(0, |i| i + 1);
    let (dir_part, name_part) = expanded.split_at(cut);
    let dir: PathBuf = if dir_part.is_empty() {
        ctx.base.to_path_buf()
    } else {
        let p = Path::new(dir_part);
        if p.is_absolute() || dir_part.starts_with(['\\', '/']) || dir_part.contains(':') {
            p.to_path_buf()
        } else {
            ctx.base.join(p)
        }
    };
    // The typed folder part stays as typed (not expanded).
    let typed_cut = token.rfind(['\\', '/']).map_or(0, |i| i + 1);
    let typed_dir = &token[..typed_cut];
    let offset = typed_dir.chars().count();
    let items: Vec<(usize, Candidate)> = cache
        .dir(&dir)
        .into_iter()
        .filter_map(|n| {
            matches(&n, name_part, ctx.fuzzy).map(|(score, marks)| {
                let name = format!("{typed_dir}{n}");
                (
                    score,
                    Candidate {
                        line: join(prefix, &name, quoted),
                        shown: name,
                        history: false,
                        locked: false,
                        marks: marks.into_iter().map(|m| m + offset).collect(),
                    },
                )
            })
        })
        .take(MAX_PER_SOURCE * 4)
        .collect();
    tidy(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(base: &Path) -> Context<'_> {
        Context {
            base,
            passive: None,
            fuzzy: false,
        }
    }

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
        let mut cache = Cache::default();
        let g = complete("type r", &history, sources, &ctx(&dir), &mut cache);
        assert_eq!(g[0].title, "MCompletionHistoryTitle");
        assert_eq!(g[0].items[0].line, "type readme.md");
        assert!(g[0].items[0].locked);
        let files: Vec<&str> = g[1].items.iter().map(|c| c.line.as_str()).collect();
        assert_eq!(files, ["type readme.md", "type Rust.txt"]);
        // A name with a blank is quoted; an opened quote is closed.
        let g = complete("cd su", &[], sources, &ctx(&dir), &mut cache);
        assert_eq!(g[0].items[0].line, "cd \"Sub Dir\"");
        let g = complete("cd \"Su", &[], sources, &ctx(&dir), &mut cache);
        assert_eq!(g[0].items[0].line, "cd \"Sub Dir\"");
        // %VARIABLES%.
        unsafe { std::env::set_var("AFAR_TEST_VAR", "1") };
        let g = complete("echo %AFAR_TEST_V", &[], sources, &ctx(&dir), &mut cache);
        assert_eq!(g[0].items[0].line, "echo %AFAR_TEST_VAR%");
        // Fuzzy: after the matches from the start, marks for highlighting.
        let fuzzy_ctx = Context {
            base: &dir,
            passive: None,
            fuzzy: true,
        };
        let g = complete("type rmd", &[], sources, &fuzzy_ctx, &mut cache);
        assert_eq!(g[0].items[0].shown, "readme.md");
        assert_eq!(g[0].items[0].marks, [0, 4, 8]);
        // The ghost: the rest of a history entry, else of a file.
        let ghost_of =
            |line: &str| ghost(line, &history, sources, &ctx(&dir), &mut Cache::default());
        assert_eq!(ghost_of("type re").as_deref(), Some("adme.md"));
        // A name to quote: only after the opened quote.
        assert_eq!(ghost_of("cd \"Sub").as_deref(), Some(" Dir\""));
        assert_eq!(ghost_of("cd Sub"), None);
        assert_eq!(ghost_of("tree"), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn passive_panel_names_as_full_paths() {
        let base = std::env::temp_dir();
        let names = vec!["..".to_string(), "Docs".to_string(), "data.csv".to_string()];
        let other = Path::new(r"D:\work");
        let c = Context {
            base: &base,
            passive: Some((other, &names)),
            fuzzy: false,
        };
        let sources = Sources {
            files: true,
            ..Default::default()
        };
        let g = complete("copy x d", &[], sources, &c, &mut Cache::default());
        let passive = g
            .iter()
            .find(|g| g.title == "completion-passive-panel")
            .unwrap();
        let lines: Vec<&str> = passive.items.iter().map(|c| c.line.as_str()).collect();
        assert_eq!(lines, [r"copy x D:\work\data.csv", r"copy x D:\work\Docs"]);
    }
}
