//! Hyperlinks (OSC 8, docs/16) in the agent's pane and on a running
//! command's screen: Ctrl+click opens a file in the viewer, a folder in
//! the active panel, a web address in the browser. While Ctrl is held the
//! links are underlined.

use std::path::{Path, PathBuf};

use super::{App, Focus};
use crate::tr;

/// The link of the visible cell at `row`, `col` of a terminal screen.
pub(super) fn link_at(screen: &vt100::Screen, row: u16, col: u16) -> Option<String> {
    let id = screen.cell(row, col)?.hyperlink();
    screen.hyperlink(id).map(str::to_string)
}

impl App {
    /// A Ctrl+left press on a link in `area` — the agent's screen
    /// (`agent`) or the running command's: the link.
    pub(super) fn link_click(
        &mut self,
        ev: &crossterm::event::MouseEvent,
        area: ratatui::layout::Rect,
        agent: bool,
    ) -> Option<String> {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};
        if ev.kind != MouseEventKind::Down(MouseButton::Left)
            || !ev.modifiers.contains(KeyModifiers::CONTROL)
            || !area.contains(ratatui::layout::Position::new(ev.column, ev.row))
        {
            return None;
        }
        let (row, col) = (ev.row - area.y, ev.column - area.x);
        let uri = if agent {
            link_at(self.agent.pty.as_ref()?.parser().screen(), row, col)
        } else {
            link_at(self.running.as_ref()?.pty.parser().screen(), row, col)
        }?;
        self.link_pressed = true;
        Some(uri)
    }

    /// Opens a link: a file or folder (`file:` URIs and plain absolute
    /// paths) in afar, web and mail addresses in their programs; other
    /// schemes are not opened.
    pub(super) fn open_link(&mut self, uri: &str) {
        if let Some(path) = file_path(uri) {
            if path.is_dir() {
                self.focus = Focus::Panels;
                let side = self.active;
                self.change_dir(side, &path);
            } else if path.is_file() {
                self.focus = Focus::Panels;
                self.open_viewer(&path, vec![path.clone()]);
            } else {
                self.say(tr!("link-not-found", path = path.display().to_string()));
            }
            return;
        }
        let scheme = uri.split(':').next().unwrap_or("").to_ascii_lowercase();
        if matches!(scheme.as_str(), "http" | "https" | "mailto") {
            // The shell's handler for the address (no command line to quote).
            let opened = std::process::Command::new("rundll32")
                .args(["url.dll,FileProtocolHandler", uri])
                .spawn();
            match opened {
                Ok(_) => self.say(tr!("link-opening", uri = uri)),
                Err(e) => self.say(tr!("link-failed", error = e.to_string())),
            }
        } else {
            self.say(tr!("link-unsupported", uri = uri));
        }
    }
}

/// The path of a `file:` URI (`file:///F:/x`, `file://host/share/x`,
/// percent-encoded; the query and fragment dropped), or of a plain
/// absolute path.
fn file_path(uri: &str) -> Option<PathBuf> {
    let lower = uri.to_ascii_lowercase();
    if !lower.starts_with("file:") {
        let p = Path::new(uri);
        return (p.is_absolute() && !uri.contains("://")).then(|| p.to_path_buf());
    }
    let rest = &uri[5..];
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let (host, path) = match rest.strip_prefix("//") {
        Some(r) => match r.find('/') {
            Some(i) => (&r[..i], &r[i..]),
            None => (r, ""),
        },
        None => ("", rest),
    };
    let path = percent_decode(path);
    let local = host.is_empty()
        || host.eq_ignore_ascii_case("localhost")
        || std::env::var("COMPUTERNAME").is_ok_and(|c| c.eq_ignore_ascii_case(host));
    let path = if local {
        // "/F:/x" → "F:/x"
        let b = path.as_bytes();
        if b.len() >= 3 && b[0] == b'/' && b[2] == b':' {
            path[1..].to_string()
        } else {
            path
        }
    } else {
        format!("//{host}{path}")
    };
    (!path.is_empty()).then(|| PathBuf::from(path.replace('/', "\\")))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |k: usize| b.get(k).and_then(|c| (*c as char).to_digit(16));
        if b[i] == b'%'
            && let (Some(h), Some(l)) = (hex(i + 1), hex(i + 2))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uris_to_paths() {
        let p = |u: &str| file_path(u).map(|p| p.display().to_string());
        assert_eq!(
            p("file:///F:/AGI/far/src/app.rs").as_deref(),
            Some(r"F:\AGI\far\src\app.rs")
        );
        assert_eq!(
            p("file:///C:/My%20Docs/a.txt#L10").as_deref(),
            Some(r"C:\My Docs\a.txt")
        );
        assert_eq!(p("file://localhost/C:/x").as_deref(), Some(r"C:\x"));
        assert_eq!(
            p("file://server/share/x").as_deref(),
            Some(r"\\server\share\x")
        );
        assert_eq!(p("file:///C:/%D0%BF.txt").as_deref(), Some(r"C:\п.txt"));
        assert_eq!(p(r"C:\plain\path").as_deref(), Some(r"C:\plain\path"));
        assert_eq!(p("https://example.com"), None);
        assert_eq!(p("relative/x"), None);
    }

    #[test]
    fn links_in_cells() {
        let mut parser = vt100::Parser::new(3, 40, 0);
        parser.process(b"a \x1b]8;;file:///C:/x;y.txt\x1b\\link\x1b[0m\x1b]8;;\x1b\\ b");
        assert_eq!(link_at(parser.screen(), 0, 0), None);
        assert_eq!(
            link_at(parser.screen(), 0, 2).as_deref(),
            Some("file:///C:/x;y.txt")
        );
        // SGR 0 inside the link does not end it; OSC 8 with no URI does.
        assert_eq!(
            link_at(parser.screen(), 0, 5).as_deref(),
            Some("file:///C:/x;y.txt")
        );
        assert_eq!(link_at(parser.screen(), 0, 7), None);
        // Erasing does not make links.
        parser.process(b"\x1b]8;;https://e.com\x1b\\\x1b[2K");
        assert_eq!(link_at(parser.screen(), 0, 2), None);
    }

    #[test]
    fn more_attributes() {
        let mut parser = vt100::Parser::new(2, 40, 0);
        parser.process(b"\x1b[9mS\x1b[29;5mB\x1b[25;8mH\x1b[28;4;58;2;1;2;3mU\x1b[24mN");
        let cell = |c| parser.screen().cell(0, c).unwrap().clone();
        assert!(cell(0).strikethrough());
        assert!(cell(1).blink() && !cell(1).strikethrough());
        assert!(cell(2).hidden() && !cell(2).blink());
        // The underline color is read and dropped (not taken for 2 = dim).
        assert!(cell(3).underline() && !cell(3).hidden() && !cell(3).dim());
        assert!(!cell(4).underline() && !cell(4).dim());
    }
}
