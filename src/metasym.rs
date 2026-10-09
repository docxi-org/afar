//! Far's metasymbols in commands (`fnparse.cpp`): the user menu (F2) and
//! the file associations put the panels' files into a command line.
//!
//! `!!` `!`, `!.!` the name with its extension, `!` the name alone, `` !` ``
//! the extension, `!&` the selected names (quoted), `!@!` / `!$!` a file
//! listing them (modifiers between `@` and `!`: `F` full paths, `Q` quotes,
//! `U` UTF-8, `A` ANSI — else OEM), `!:` the drive, `!\` the folder with a
//! trailing `\`, `!=\` the same with links resolved, `!?!` the
//! description, `!?title?init!` asks the user. `!#` / `!^` / `![` / `!]`:
//! what follows is about the passive / active / left / right panel. Short
//! (8.3) names and paths (`!~`, `!-!`, `!+!`, `!/`, …) are the long ones.

use std::path::{Path, PathBuf};

/// One panel as the metasymbols see it.
#[derive(Clone, Debug, Default)]
pub struct PanelFiles {
    pub dir: PathBuf,
    /// The item under the cursor (none on `..`).
    pub current: Option<String>,
    /// The selected names, else the current one.
    pub selected: Vec<String>,
    pub description: Option<String>,
}

/// Both panels.
#[derive(Clone, Debug, Default)]
pub struct Panels {
    pub active: PanelFiles,
    pub passive: PanelFiles,
    pub left_active: bool,
}

/// A command with its metasymbols put in.
#[derive(Debug, Default, PartialEq)]
pub struct Expanded {
    pub text: String,
    /// List files made for `!@!` (to delete when done).
    pub lists: Vec<PathBuf>,
}

/// The questions a command asks (`!?title?init!`), in order.
pub fn prompts(cmd: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut s = cmd;
    while let Some(i) = s.find("!?") {
        let rest = &s[i + 2..];
        // `!?!` is the description.
        if let Some(after) = rest.strip_prefix('!') {
            s = after;
            continue;
        }
        match input_token(rest) {
            Some((title, init, used)) => {
                out.push((title.to_string(), init.to_string()));
                s = &rest[used..];
            }
            None => s = rest,
        }
    }
    out
}

/// `title?init!` after `!?`: the two parts and the bytes they take.
fn input_token(rest: &str) -> Option<(&str, &str, usize)> {
    let q = rest.find('?')?;
    let end = rest[q + 1..].find('!')? + q + 1;
    Some((&rest[..q], &rest[q + 1..end], end + 1))
}

/// `cmd` with the metasymbols replaced; `inputs` answer its `!?…!`
/// questions in order; list files go into `list_dir`.
pub fn expand(cmd: &str, panels: &Panels, inputs: &[String], list_dir: &Path) -> Expanded {
    let mut out = Expanded::default();
    let mut passive = false;
    let mut asked = 0;
    let mut s = cmd;
    while let Some(i) = s.find('!') {
        out.text.push_str(&s[..i]);
        let t = &s[i..];
        let p = if passive {
            &panels.passive
        } else {
            &panels.active
        };
        let cur = p.current.as_deref().unwrap_or("");
        let skip = |tok: &str| t.strip_prefix(tok);
        let rest = if let Some(r) = skip("!#") {
            passive = true;
            r
        } else if let Some(r) = skip("!^") {
            passive = false;
            r
        } else if let Some(r) = skip("![") {
            passive = !panels.left_active;
            r
        } else if let Some(r) = skip("!]") {
            passive = panels.left_active;
            r
        } else if let Some(r) = skip("!!") {
            out.text.push('!');
            r
        } else if let Some(r) = skip("!.!").or_else(|| skip("!-!")).or_else(|| skip("!+!")) {
            out.text.push_str(cur);
            r
        } else if let Some(r) = skip("!`~").or_else(|| skip("!`")) {
            out.text.push_str(extension(cur));
            r
        } else if let Some(r) = skip("!&~").or_else(|| skip("!&")) {
            // `Q` quotes every name, `q` only those with blanks (Far's
            // default quotes).
            let (always, r) = match r.as_bytes().first() {
                Some(b'q') => (false, &r[1..]),
                Some(b'Q') => (true, &r[1..]),
                _ => (true, r),
            };
            let names: Vec<String> = p
                .selected
                .iter()
                .map(|n| if always { quote(n) } else { quote_blank(n) })
                .collect();
            out.text.push_str(&names.join(" "));
            r
        } else if let Some(r) = skip("!@").or_else(|| skip("!$")) {
            match r.find('!') {
                Some(end) => {
                    let modifiers = &r[..end];
                    let file = list_file(p, modifiers, list_dir, out.lists.len());
                    out.text.push_str(&file.display().to_string());
                    out.lists.push(file);
                    &r[end + 1..]
                }
                None => {
                    out.text.push('!');
                    &t[1..]
                }
            }
        } else if let Some(r) = skip("!:") {
            out.text.push_str(&drive(&p.dir));
            r
        } else if let Some(r) = skip("!=\\").or_else(|| skip("!=/")) {
            let real = std::fs::canonicalize(&p.dir)
                .map(crate::panel::strip_verbatim)
                .unwrap_or_else(|_| p.dir.clone());
            out.text.push_str(&with_slash(&real));
            r
        } else if let Some(r) = skip("!\\").or_else(|| skip("!/")) {
            out.text.push_str(&with_slash(&p.dir));
            r
        } else if let Some(r) = skip("!?!") {
            out.text.push_str(p.description.as_deref().unwrap_or(""));
            r
        } else if let Some((_, _, used)) = skip("!?").and_then(input_token) {
            if let Some(answer) = inputs.get(asked) {
                out.text.push_str(answer);
            }
            asked += 1;
            &t[2 + used..]
        } else if let Some(r) = skip("!~") {
            out.text.push_str(stem(cur));
            r
        } else {
            out.text.push_str(stem(cur));
            &t[1..]
        };
        s = rest;
    }
    out.text.push_str(s);
    out
}

/// The name without its extension.
fn stem(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 => &name[..i],
        _ => name,
    }
}

fn extension(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 => &name[i + 1..],
        _ => "",
    }
}

fn quote(s: &str) -> String {
    format!("\"{s}\"")
}

fn quote_blank(s: &str) -> String {
    if s.contains([' ', '&', '(', ')', '^', ',', ';', '=']) {
        quote(s)
    } else {
        s.to_string()
    }
}

fn with_slash(p: &Path) -> String {
    let s = p.display().to_string();
    if s.ends_with(['\\', '/']) {
        s
    } else {
        format!("{s}\\")
    }
}

/// `C:` of a path (the share of a UNC path).
fn drive(p: &Path) -> String {
    match p.components().next() {
        Some(std::path::Component::Prefix(pre)) => pre.as_os_str().to_string_lossy().into_owned(),
        _ => String::new(),
    }
}

/// A file listing the panel's selected names, one a line (Far's
/// `MakeListFile`): `F` full paths, `Q` quoted, `S` `/` for `\`; `U` UTF-8,
/// `W` UTF-16, `A` ANSI, else the OEM code page.
fn list_file(p: &PanelFiles, modifiers: &str, dir: &Path, n: usize) -> PathBuf {
    let full = modifiers.contains('F');
    let quoted = modifiers.contains('Q');
    let slashes = modifiers.contains('S');
    let mut text = String::new();
    for name in &p.selected {
        let mut s = if full {
            p.dir.join(name).display().to_string()
        } else {
            name.clone()
        };
        if slashes {
            s = s.replace('\\', "/");
        }
        if quoted {
            s = quote(&s);
        }
        text.push_str(&s);
        text.push_str("\r\n");
    }
    use crate::viewer::codepage;
    let bytes = if modifiers.contains('U') {
        text.into_bytes()
    } else if modifiers.contains('W') {
        text.encode_utf16().flat_map(u16::to_le_bytes).collect()
    } else {
        let cp = if modifiers.contains('A') {
            codepage::ansi()
        } else {
            codepage::oem()
        };
        codepage::Codec::new(cp)
            .map_or_else(|| text.clone().into_bytes(), |c| c.encode_lossy(&text))
    };
    let _ = std::fs::create_dir_all(dir);
    let path = dir.join(format!("list-{}-{n}.txt", std::process::id()));
    let _ = std::fs::write(&path, bytes);
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panels() -> Panels {
        Panels {
            active: PanelFiles {
                dir: PathBuf::from(r"C:\work\src"),
                current: Some("main.rs".into()),
                selected: vec!["a b.txt".into(), "c.txt".into()],
                description: Some("entry".into()),
            },
            passive: PanelFiles {
                dir: PathBuf::from(r"D:\out"),
                current: Some("x.tar.gz".into()),
                selected: vec!["x.tar.gz".into()],
                description: None,
            },
            left_active: true,
        }
    }

    #[test]
    fn names_paths_lists_and_panels() {
        let p = panels();
        let dir = std::env::temp_dir();
        let e = |s: &str| expand(s, &p, &[], &dir).text;
        assert_eq!(e("rustc !.! -o !.exe"), "rustc main.rs -o main.exe");
        assert_eq!(e("ext=!` 100!!"), "ext=rs 100!");
        assert_eq!(e("zip !&"), "zip \"a b.txt\" \"c.txt\"");
        assert_eq!(e("zip !&q"), "zip \"a b.txt\" c.txt");
        assert_eq!(e("cd !\\ & !:"), r"cd C:\work\src\ & C:");
        assert_eq!(e("copy !.! !#!\\"), r"copy main.rs D:\out\");
        assert_eq!(e("!#!.! !^!.!"), "x.tar.gz main.rs");
        assert_eq!(e("![!.! !]!.!"), "main.rs x.tar.gz");
        assert_eq!(e("echo !?!"), "echo entry");
        let x = expand("tar !@FU!", &p, &[], &dir);
        assert_eq!(x.lists.len(), 1);
        let list = std::fs::read_to_string(&x.lists[0]).unwrap();
        assert_eq!(list, "C:\\work\\src\\a b.txt\r\nC:\\work\\src\\c.txt\r\n");
        std::fs::remove_file(&x.lists[0]).unwrap();
    }

    #[test]
    fn questions_are_asked_and_put_in() {
        let p = panels();
        let cmd = "grep !?Pattern?TODO! !?In files?*.rs! !.!";
        assert_eq!(
            prompts(cmd),
            vec![
                ("Pattern".to_string(), "TODO".to_string()),
                ("In files".to_string(), "*.rs".to_string()),
            ]
        );
        let x = expand(
            cmd,
            &p,
            &["fixme".into(), "*.md".into()],
            &std::env::temp_dir(),
        );
        assert_eq!(x.text, "grep fixme *.md main.rs");
        assert!(prompts("echo !?! !!").is_empty());
    }
}
