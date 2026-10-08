//! Markers in the text (docs/11 «Маркер в тексте», plan step 4): a line
//! that starts with a marker — or a code comment that does — is an
//! instruction for the agent. `!!` / `@ai`: the agent decides; `??`,
//! `@ai?`: it only answers; `@ai!`: it changes the text.

use std::path::Path;

/// What the marker asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnMode {
    /// The agent decides what to do.
    Auto,
    /// Only an answer: the text stays.
    Answer,
    /// Change the text.
    Edit,
}

/// The comment openers of a file, by its extension (none: markers only
/// at the line's start).
fn comment_openers(path: &Path) -> &'static [&'static str] {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "rs" | "c" | "h" | "cpp" | "hpp" | "cc" | "cs" | "java" | "js" | "mjs" | "ts" | "tsx"
        | "jsx" | "go" | "kt" | "swift" | "scala" | "dart" | "php" | "css" | "scss" | "less"
        | "json5" | "jsonc" => &["//", "/*", "*"],
        "py" | "sh" | "bash" | "zsh" | "ps1" | "psm1" | "rb" | "pl" | "r" | "toml" | "yaml"
        | "yml" | "conf" | "cfg" | "mk" | "cmake" | "dockerfile" | "nix" | "jl" | "ex" | "exs" => {
            &["#"]
        }
        "sql" | "lua" | "hs" | "ada" | "elm" => &["--"],
        "ini" | "asm" | "s" | "lisp" | "clj" | "el" | "scm" => &[";"],
        "tex" | "m" | "erl" | "pro" => &["%"],
        "bat" | "cmd" => &["REM", "rem", "::"],
        "html" | "htm" | "xml" | "xaml" | "svg" | "vue" | "md" | "markdown" => &["<!--"],
        _ => &[],
    }
}

/// The marker of `line`, if any: what it asks for and the instruction
/// after it. `markers` take a `?` / `!` after them (answer / edit);
/// `answer_markers` are answers as they are.
pub fn parse(
    line: &str,
    path: &Path,
    markers: &[String],
    answer_markers: &[String],
    in_comments: bool,
) -> Option<(TurnMode, String)> {
    let mut rest = line.trim_start();
    if in_comments {
        for opener in comment_openers(path) {
            if let Some(r) = rest.strip_prefix(opener) {
                rest = r.trim_start();
                break;
            }
        }
    }
    let (mode, after) = answer_markers
        .iter()
        .filter(|m| !m.is_empty())
        .find_map(|m| rest.strip_prefix(m.as_str()).map(|r| (TurnMode::Answer, r)))
        .or_else(|| {
            markers.iter().filter(|m| !m.is_empty()).find_map(|m| {
                let r = rest.strip_prefix(m.as_str())?;
                Some(if let Some(r) = r.strip_prefix('?') {
                    (TurnMode::Answer, r)
                } else if let Some(r) = r.strip_prefix('!') {
                    (TurnMode::Edit, r)
                } else {
                    (TurnMode::Auto, r)
                })
            })
        })?;
    // A marker is a word of its own: "!!x" or "@aim" is not one.
    if !(after.is_empty() || after.starts_with(char::is_whitespace)) {
        return None;
    }
    let text = after
        .trim()
        .trim_end_matches("-->")
        .trim_end_matches("*/")
        .trim()
        .to_string();
    Some((mode, text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(line: &str, file: &str) -> Option<(TurnMode, String)> {
        let markers = vec!["!!".to_string(), "@ai".to_string()];
        let answers = vec!["??".to_string()];
        parse(line, Path::new(file), &markers, &answers, true)
    }

    #[test]
    fn markers_and_their_modes() {
        assert_eq!(
            p("!! make a table", "a.md"),
            Some((TurnMode::Auto, "make a table".into()))
        );
        assert_eq!(
            p("  ?? why", "a.txt"),
            Some((TurnMode::Answer, "why".into()))
        );
        assert_eq!(
            p("@ai? what is it", "a.txt"),
            Some((TurnMode::Answer, "what is it".into()))
        );
        assert_eq!(
            p("@ai! fix it", "a.txt"),
            Some((TurnMode::Edit, "fix it".into()))
        );
        assert_eq!(p("@ai", "a.txt"), Some((TurnMode::Auto, String::new())));
        assert_eq!(p("!!x", "a.txt"), None);
        assert_eq!(p("@aim high", "a.txt"), None);
        assert_eq!(p("text !! later", "a.txt"), None);
    }

    #[test]
    fn markers_in_comments_by_the_file_type() {
        assert_eq!(
            p("    // !! rename this", "x.rs"),
            Some((TurnMode::Auto, "rename this".into()))
        );
        assert_eq!(
            p("# ?? why so slow", "x.py"),
            Some((TurnMode::Answer, "why so slow".into()))
        );
        assert_eq!(
            p("<!-- @ai! shorten -->", "x.md"),
            Some((TurnMode::Edit, "shorten".into()))
        );
        // Not a comment of a text file.
        assert_eq!(p("// !! rename", "x.txt"), None);
    }
}
