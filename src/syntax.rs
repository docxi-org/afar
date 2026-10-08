//! Syntax highlighting (docs/11 «Лучше, чем в Far»): `syntect` with the
//! syntaxes of `bat` (`two-face`), Oniguruma regexes (with `fancy-regex`
//! two-face leaves out PowerShell and JavaScript). Far itself
//! has none (its Colorer plugin does); the colors are Far's console ones
//! on the editor's blue, by the kind of the token (`theme::SYNTAX`).

use std::path::Path;
use std::sync::OnceLock;

use ratatui::style::Color;
use syntect::highlighting::{
    HighlightIterator, HighlightState, Highlighter, ScopeSelectors, StyleModifier, Theme,
    ThemeItem, ThemeSettings,
};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};

use crate::theme;

/// Lines longer than this are not highlighted (minified code: slow and of
/// no use).
pub const LINE_LIMIT: usize = 4000;

/// The syntaxes, loaded once.
pub fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(two_face::syntax::extra_newlines)
}

/// A token's kind carried through the theme: syntect knows RGB only, so
/// the kind rides in the red channel (green = 1 marks it); its color is
/// `theme::SYNTAX[kind]`.
fn mark(kind: usize) -> syntect::highlighting::Color {
    syntect::highlighting::Color {
        r: kind as u8,
        g: 1,
        b: 0,
        a: 0xFF,
    }
}

/// The color of a style the theme gave, if it is one of ours.
fn console(c: syntect::highlighting::Color) -> Option<Color> {
    if c.g == 1 && c.b == 0 {
        theme::SYNTAX.get(usize::from(c.r)).copied()
    } else {
        None
    }
}

/// The theme: scopes → kinds (indexes of `theme::SYNTAX`).
fn theme() -> &'static Theme {
    static THEME: OnceLock<Theme> = OnceLock::new();
    THEME.get_or_init(|| {
        let item = |scopes: &str, kind: usize| ThemeItem {
            scope: scopes.parse::<ScopeSelectors>().expect("valid scope selectors"),
            style: StyleModifier {
                foreground: Some(mark(kind)),
                background: None,
                font_style: None,
            },
        };
        Theme {
            name: Some("afar".into()),
            author: None,
            settings: ThemeSettings::default(),
            scopes: vec![
                item("comment, punctuation.definition.comment", 0),
                item("string, punctuation.definition.string, constant.character", 1),
                item("constant.numeric, constant.language, constant.other", 2),
                item("keyword, storage, keyword.operator.word, variable.language", 3),
                item(
                    "entity.name.function, support.function, entity.name.type, support.type, entity.name.class, entity.name.struct, entity.name.enum",
                    4,
                ),
                item("entity.name.tag, markup.heading, markup.heading entity.name.section", 5),
                // Markdown's inline code and emphasis, TOML's tables.
                item("markup.raw, markup.inline.raw", 1),
                item("markup.italic, markup.bold", 3),
                item("entity.name.section, meta.tag.table, entity.name.table", 4),
                item("invalid", 6),
            ],
        }
    })
}

/// The syntax of a file: by its name or extension, else by its first line
/// (`#!/bin/sh`, `<?xml`); plain text gets none.
pub fn syntax_for(path: &Path, first_line: &str) -> Option<&'static SyntaxReference> {
    let set = syntaxes();
    let name = path.file_name()?.to_string_lossy();
    let ext = path.extension().map(|e| e.to_string_lossy().into_owned());
    let found = set
        .find_syntax_by_extension(&name)
        .or_else(|| ext.as_deref().and_then(|e| set.find_syntax_by_extension(e)))
        .or_else(|| set.find_syntax_by_first_line(first_line))?;
    (found.name != "Plain Text").then_some(found)
}

/// The parse state before a line: what highlighting the next line needs.
#[derive(Clone)]
pub struct LineState {
    parse: ParseState,
    highlight: HighlightState,
}

/// A line's colored pieces: (start in chars, length in chars, color); the
/// default color is left out.
pub type Pieces = Vec<(usize, usize, Color)>;

impl LineState {
    pub fn start(syntax: &SyntaxReference) -> Self {
        let highlighter = Highlighter::new(theme());
        Self {
            parse: ParseState::new(syntax),
            highlight: HighlightState::new(&highlighter, ScopeStack::new()),
        }
    }

    /// Goes over `line` (no ending): the state moves to the next line;
    /// returns the line's colored pieces.
    pub fn line(&mut self, line: &str) -> Pieces {
        if line.len() > LINE_LIMIT {
            // Still a line for the state, not colored.
            let _ = self.parse.parse_line("\n", syntaxes());
            return Vec::new();
        }
        let text = format!("{line}\n");
        let Ok(ops) = self.parse.parse_line(&text, syntaxes()) else {
            return Vec::new();
        };
        let highlighter = Highlighter::new(theme());
        let mut out = Vec::new();
        let mut col = 0;
        for (style, piece) in HighlightIterator::new(&mut self.highlight, &ops, &text, &highlighter)
        {
            let piece = piece.strip_suffix('\n').unwrap_or(piece);
            let n = piece.chars().count();
            if n > 0
                && let Some(c) = console(style.foreground)
            {
                out.push((col, n, c));
            }
            col += n;
        }
        out
    }
}

/// The color of character `col` from a line's pieces.
pub fn color_at(pieces: &[(usize, usize, Color)], col: usize) -> Option<Color> {
    pieces
        .iter()
        .find(|(s, n, _)| col >= *s && col < s + n)
        .map(|(_, _, c)| *c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_is_colored_by_kind() {
        let syntax = syntax_for(Path::new("x.rs"), "").expect("Rust is known");
        let mut st = LineState::start(syntax);
        let pieces = st.line("fn main() { let s = \"hi\"; } // done");
        let kw = color_at(&pieces, 0);
        let string = color_at(&pieces, 21);
        let comment = color_at(&pieces, 32);
        assert_eq!(kw, Some(theme::SYNTAX[3]));
        assert_eq!(string, Some(theme::SYNTAX[1]));
        assert_eq!(comment, Some(theme::SYNTAX[0]));
        assert!(syntax_for(Path::new("notes.txt"), "hello").is_none());
        assert!(syntax_for(Path::new("run"), "#!/bin/bash").is_some());
        // Left out of two-face's fancy-regex build; here with Oniguruma.
        assert!(syntax_for(Path::new("x.ps1"), "").is_some());
        assert!(syntax_for(Path::new("x.js"), "").is_some());
    }

    #[test]
    fn a_markdown_heading_is_one_color() {
        let syntax = syntax_for(Path::new("x.md"), "").unwrap();
        let mut st = LineState::start(syntax);
        let pieces = st.line("# Title");
        assert_eq!(color_at(&pieces, 0), Some(theme::SYNTAX[5]));
        assert_eq!(color_at(&pieces, 3), Some(theme::SYNTAX[5]));
    }

    #[test]
    fn a_comment_goes_on_over_lines() {
        let syntax = syntax_for(Path::new("x.c"), "").unwrap();
        let mut st = LineState::start(syntax);
        st.line("/* a long");
        let pieces = st.line("comment */ int x;");
        assert_eq!(color_at(&pieces, 0), Some(theme::SYNTAX[0]));
        assert_eq!(color_at(&pieces, 11), Some(theme::SYNTAX[3]));
    }
}
