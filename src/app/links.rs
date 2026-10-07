//! Hyperlinks (OSC 8, docs/16) in the agent's pane and on the user screen
//! (a running command's and ended ones' output): underlined and colored;
//! under the mouse their address shows in a tooltip; Ctrl+click opens a file
//! in the viewer, a folder in the active panel, a web address in the
//! browser.

use std::path::{Path, PathBuf};

use super::{App, Focus};
use crate::tr;

/// The link of the visible cell at `row`, `col` of a terminal screen.
pub(super) fn link_at(screen: &vt100::Screen, row: u16, col: u16) -> Option<String> {
    let id = screen.cell(row, col)?.hyperlink();
    screen.hyperlink(id).map(str::to_string)
}

impl App {
    /// The link at a screen position: in the agent's pane, the running
    /// command's live screen, the kept output on the user screen.
    fn link_under(&self, x: u16, y: u16, l: &super::Layout) -> Option<String> {
        use ratatui::layout::Position;
        let pos = Position::new(x, y);
        if l.agent.contains(pos) {
            let pty = self.agent.pty.as_ref()?;
            return link_at(pty.parser().screen(), y - l.agent.y, x - l.agent.x);
        }
        // A viewer covers the user screen (unless peeked under with Ctrl+O).
        if self.shown_viewer().is_some() && !self.viewer_peek {
            return None;
        }
        // The user screen is under the mouse (as in `on_mouse`).
        let user_screen = !self.panels_visible() || l.top.height < 5 || !l.top.contains(pos);
        if !l.user.contains(pos) || !user_screen {
            return None;
        }
        if let Some(run) = &self.running
            && self.last_live.contains(pos)
        {
            return link_at(
                run.pty.parser().screen(),
                y - self.last_live.y,
                x - self.last_live.x,
            );
        }
        let (rect, start) = self.user_lines;
        if !rect.contains(pos) {
            return None;
        }
        let captured = self
            .running
            .as_ref()
            .map(|r| r.captured.as_slice())
            .unwrap_or(&[]);
        let line = self
            .history
            .iter()
            .chain(captured)
            .nth(start + usize::from(y - rect.y))?;
        line.link_at_column(usize::from(x - rect.x))
            .map(str::to_string)
    }

    /// The mouse moved: over a link — its address goes next to the mouse.
    pub(super) fn hover_link(&mut self, ev: &crossterm::event::MouseEvent, l: &super::Layout) {
        self.hovered_link = self
            .link_under(ev.column, ev.row, l)
            .map(|uri| (uri, ev.column, ev.row));
    }

    /// The tooltip of the link under the mouse: a line below it (above at
    /// the bottom), from the mouse's column, shifted left to fit.
    pub(super) fn draw_link_tooltip(
        &mut self,
        area: ratatui::layout::Rect,
        buf: &mut ratatui::buffer::Buffer,
    ) {
        if self.has_overlay() {
            return;
        }
        let Some((uri, x, y)) = &self.hovered_link else {
            return;
        };
        let text = format!(" {} ", tr!("link-tooltip", uri = uri.as_str()));
        let w = (text.chars().count() as u16).min(area.width);
        let ty = if *y + 1 < area.bottom() {
            y + 1
        } else {
            y.saturating_sub(1)
        };
        let tx = (*x).min(area.right().saturating_sub(w));
        buf.set_stringn(tx, ty, &text, usize::from(w), crate::theme::LINK_TOOLTIP);
    }

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

    /// A Ctrl+left press on a link in the kept output of ended commands
    /// on the user screen: the link.
    pub(super) fn kept_link_click(&mut self, ev: &crossterm::event::MouseEvent) -> Option<String> {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};
        let (rect, start) = self.user_lines;
        if ev.kind != MouseEventKind::Down(MouseButton::Left)
            || !ev.modifiers.contains(KeyModifiers::CONTROL)
            || !rect.contains(ratatui::layout::Position::new(ev.column, ev.row))
        {
            return None;
        }
        let index = start + usize::from(ev.row - rect.y);
        let captured = self
            .running
            .as_ref()
            .map(|r| r.captured.as_slice())
            .unwrap_or(&[]);
        let line = self.history.iter().chain(captured).nth(index)?;
        let uri = line
            .link_at_column(usize::from(ev.column - rect.x))?
            .to_string();
        self.link_pressed = true;
        Some(uri)
    }

    /// Opens a link: a file or folder (`file:` URIs and plain absolute
    /// paths) in afar, web and mail addresses in their programs; other
    /// schemes are not opened.
    pub(super) fn open_link(&mut self, uri: &str) {
        if let Some(path) = file_path(uri) {
            let (path, line) = with_line(path, uri);
            if path.is_dir() {
                self.focus = Focus::Panels;
                let side = self.active;
                self.change_dir(side, &path);
            } else if path.is_file() {
                self.focus = Focus::Panels;
                self.record_view(&path);
                if let Some(id) = self.open_viewer(&path, vec![path.clone()])
                    && let Some(line) = line
                    && let Ok(n) = line.parse::<u64>()
                    && let Some(v) = self.viewers.iter_mut().find(|v| v.id == id)
                {
                    v.goto_line(n);
                }
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

/// The line a link points to: a fragment `#L10` / `#L10-20` / `#10`, or
/// a `:10` (`:10:5`) after a path that is not there as written.
fn with_line(path: PathBuf, uri: &str) -> (PathBuf, Option<String>) {
    if let Some(frag) = uri.rsplit_once('#').map(|(_, f)| f) {
        let digits: String = frag
            .trim_start_matches(['L', 'l'])
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if !digits.is_empty() {
            return (path, Some(digits));
        }
    }
    if !path.exists() {
        let s = path.to_string_lossy();
        // `name:10` or `name:10:5` (the drive's colon is the first one).
        let mut parts = s.rsplitn(3, ':');
        let (a, b) = (parts.next(), parts.next());
        let all_digits = |x: &str| !x.is_empty() && x.chars().all(|c| c.is_ascii_digit());
        if let (Some(a), Some(b)) = (a, b)
            && all_digits(a)
        {
            if all_digits(b)
                && let Some(rest) = parts.next()
                && rest.len() > 2
            {
                return (PathBuf::from(rest), Some(b.to_string()));
            }
            let base = &s[..s.len() - a.len() - 1];
            if base.len() > 2 {
                return (PathBuf::from(base), Some(a.to_string()));
            }
        }
    }
    (path, None)
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
    fn lines_of_links() {
        let p = PathBuf::from;
        assert_eq!(
            with_line(p(r"C:\x.rs"), "file:///C:/x.rs#L42"),
            (p(r"C:\x.rs"), Some("42".into()))
        );
        assert_eq!(
            with_line(p(r"C:\x.rs"), "file:///C:/x.rs#L42-50"),
            (p(r"C:\x.rs"), Some("42".into()))
        );
        assert_eq!(
            with_line(p(r"C:\no-such\x.rs:12"), r"C:\no-such\x.rs:12"),
            (p(r"C:\no-such\x.rs"), Some("12".into()))
        );
        assert_eq!(
            with_line(p(r"C:\no-such\x.rs:12:5"), r"C:\no-such\x.rs:12:5"),
            (p(r"C:\no-such\x.rs"), Some("12".into()))
        );
        assert_eq!(
            with_line(p(r"C:\no-such"), r"C:\no-such"),
            (p(r"C:\no-such"), None)
        );
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
