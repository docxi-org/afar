//! Hints: the mouse rests on something, after a short delay a block next
//! to it tells about it (a line is a block of one line). Each kind can be
//! turned off (`[hints]`): files in the panels, the key bar, the agent's
//! frame.

use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use super::{App, Layout, agent};
use crate::theme;
use crate::tr;

/// What the hint says: lines, each with its style (the first is the title).
pub(super) struct Hint {
    lines: Vec<(String, Style)>,
}

impl Hint {
    fn new(title: impl Into<String>) -> Self {
        Self {
            lines: vec![(title.into(), theme::HINT_TITLE)],
        }
    }

    fn line(mut self, text: impl Into<String>) -> Self {
        self.lines.push((text.into(), theme::HINT));
        self
    }

    fn styled(mut self, text: impl Into<String>, style: Style) -> Self {
        self.lines.push((text.into(), style));
        self
    }
}

/// Where the mouse rests and since when; the hint once shown.
pub(super) struct Hover {
    x: u16,
    y: u16,
    since: Instant,
    shown: Option<Hint>,
    /// Looked for and nothing to say here.
    nothing: bool,
}

/// The widest a hint gets (longer lines wrap).
const MAX_WIDTH: usize = 60;
const MAX_LINES: usize = 12;

impl App {
    /// The mouse moved to (x, y): a new place to wait at.
    pub(super) fn hover_moved(&mut self, x: u16, y: u16) {
        if self.hover.as_ref().is_some_and(|h| h.x == x && h.y == y) {
            return;
        }
        self.hover = self.config.hints.enabled.then(|| Hover {
            x,
            y,
            since: Instant::now(),
            shown: None,
            nothing: false,
        });
    }

    /// A key, a click or the wheel: the hint goes.
    pub(super) fn hover_reset(&mut self) {
        self.hover = None;
    }

    /// Waiting for a hint to show (the main loop then wakes sooner).
    pub(super) fn hint_pending(&self) -> bool {
        self.hover
            .as_ref()
            .is_some_and(|h| h.shown.is_none() && !h.nothing)
    }

    /// The delay is over: the hint of the place, if there is one.
    pub(super) fn hint_tick(&mut self) {
        let delay = std::time::Duration::from_millis(self.config.hints.delay_ms);
        let Some((x, y)) = self
            .hover
            .as_ref()
            .filter(|h| h.shown.is_none() && !h.nothing && h.since.elapsed() >= delay)
            .map(|h| (h.x, h.y))
        else {
            return;
        };
        let hint = if self.has_overlay() || self.hovered_link.is_some() {
            None
        } else {
            self.hint_at(x, y)
        };
        if let Some(h) = &mut self.hover {
            h.nothing = hint.is_none();
            h.shown = hint;
        }
    }

    /// What there is to say about the cell (x, y).
    fn hint_at(&mut self, x: u16, y: u16) -> Option<Hint> {
        let l = self.last_layout.clone()?;
        let pos = ratatui::layout::Position::new(x, y);
        let h = &self.config.hints;
        if h.keybar && l.keybar.contains(pos) {
            return self.keybar_hint(&l, x);
        }
        if h.agent && y == l.agent_frame.y && l.agent_frame.contains(pos) {
            return Some(self.agent_hint());
        }
        // The status line of a viewer or an editor (its top row).
        if h.status && y == l.top.y && l.top.contains(pos) {
            let idx = usize::from(x - l.top.x);
            if let Some(i) = self.shown_editor() {
                if self.editor_status {
                    // With room for the clock, as drawn.
                    let clock = l.top.y == 0 && l.top.right() == l.keybar.right();
                    let text = self.editor_status_line(i, l.top.width, clock);
                    return self.editor_status_hint(i, &text, idx);
                }
            } else if let Some(i) = self.shown_viewer()
                && self.viewers[i].status_line
            {
                let text = self.viewers[i].last_status.clone();
                return self.viewer_status_hint(i, &text, idx);
            }
        }
        // The agent's marks and proposals in a viewer or an editor.
        if h.marks {
            if let Some(i) = self.shown_editor() {
                match self.editors[i].agent_thing_at(x, y) {
                    Some(crate::editor::AgentThing::Proposal) => {
                        return Some(Hint::new(tr!("tip-proposal")).line(tr!("tip-proposal-keys")));
                    }
                    Some(crate::editor::AgentThing::Mark(m)) => return Some(mark_hint(&m)),
                    None => {}
                }
            } else if let Some(i) = self.shown_viewer()
                && let Some(m) = self.viewers[i].mark_at(x, y)
            {
                return Some(mark_hint(&m));
            }
        }
        if h.panels
            && self.shown_viewer().is_none()
            && self.shown_editor().is_none()
            && let Some(side) = (0..2).find(|&s| l.panels[s].contains(pos))
            && let Some(t) = self.panels[side].title_at(x, y)
        {
            return Some(self.title_hint(side, t));
        }
        if h.files
            && self.shown_viewer().is_none()
            && self.shown_editor().is_none()
            && let Some(side) = (0..2).find(|&s| l.panels[s].contains(pos))
            && let Some(i) = self.panels[side].item_at(x, y)
        {
            return self.file_hint(side, i);
        }
        None
    }

    /// A file in a panel: its full name, size, times, attributes, its
    /// description, and what the agent did to it.
    fn file_hint(&mut self, side: usize, i: usize) -> Option<Hint> {
        let panel = &self.panels[side];
        let e = panel.entries.get(i)?;
        if e.name == ".." {
            return None;
        }
        let mut hint = Hint::new(e.name.clone());
        let path = panel.entry_path(i);
        if e.link
            && let Ok(target) = std::fs::read_link(&path)
        {
            hint = hint.line(tr!("tip-link", target = target.display().to_string()));
        }
        if !e.is_dir {
            hint = hint.line(tr!(
                "tip-size",
                size = crate::panel::group_thousands(e.size),
                n = e.size as i64
            ));
        }
        let l = crate::locale::get();
        for (id, t) in [
            ("MColumnWrited", e.modified),
            ("MColumnCreated", e.created),
            ("MColumnAccessed", e.accessed),
        ] {
            if let Some(t) = t {
                let t: chrono::DateTime<chrono::Local> = t.into();
                hint = hint.line(format!(
                    "{:<9} {} {}",
                    tr!(id),
                    l.date(&t, true),
                    l.time(&t, true)
                ));
            }
        }
        let attrs = crate::app::attributes::attribute_names(e.attrs);
        if !attrs.is_empty() {
            hint = hint.line(tr!("tip-attrs", list = attrs.join(", ")));
        }
        if let Some(d) = panel.description_of(i) {
            hint = hint.line(d);
        }
        if let Some(m) = panel.agent_mark(i) {
            hint = hint.styled(tr!("tip-agent-mark", label = m.label()), theme::HINT_AGENT);
        }
        Some(hint)
    }

    /// A key of the key bar: what it does and its keys.
    fn keybar_hint(&self, l: &Layout, x: u16) -> Option<Hint> {
        let n = super::keybar_keys(l.keybar.width)
            .iter()
            .position(|(s, e)| x >= l.keybar.x + s && x < l.keybar.x + e)? as u8
            + 1;
        let group = super::modifier_group(self.held);
        let key = if group.is_empty() {
            format!("F{n}")
        } else {
            format!("{group}+F{n}")
        };
        let ctx = match (self.shown_editor(), self.shown_viewer()) {
            (Some(_), _) => crate::command::Ctx::Editor,
            (None, Some(_)) => crate::command::Ctx::Viewer,
            (None, None) => crate::command::Ctx::Panels,
        };
        let chord = crate::keymap::Chord::parse(&key)?;
        let Some(cmd) = self.keymap.get(ctx, &chord) else {
            return Some(Hint::new(key).line(tr!("tip-key-free")));
        };
        // The F9 menu's text, else afar's own words for the command.
        let id = format!("cmd-{}", cmd.def().name.replace(['.', '_'], "-"));
        let what = match super::mainmenu::menu_label(cmd) {
            Some(label) => Some(crate::i18n::plain(&tr!(label))),
            None => Some(tr!(&id)).filter(|t| *t != id),
        };
        let title = match what {
            Some(what) => format!("{key} — {what}"),
            None => key,
        };
        let keys: Vec<String> = self
            .keymap
            .keys_of(ctx, cmd)
            .iter()
            .map(|k| k.label())
            .collect();
        Some(
            Hint::new(title)
                .line(tr!("tip-keys", keys = keys.join(", ")))
                .line(tr!("tip-command", name = cmd.def().name)),
        )
    }

    /// The titles' row of a panel: the sort, a column.
    fn title_hint(&self, side: usize, t: crate::panel::TitleAt) -> Hint {
        use crate::panel::TitleAt;
        let sort = &self.panels[side].sort;
        match t {
            TitleAt::Sort | TitleAt::SelectedFirst => {
                let mode = crate::i18n::plain(&tr!(sort.mode.info().label));
                let mut hint = Hint::new(tr!("tip-sort", mode = mode));
                if sort.reverse {
                    hint = hint.line(tr!("tip-sort-reverse"));
                }
                if sort.selected_first {
                    hint = hint.line(tr!("tip-sort-selected-first"));
                }
                hint.line(tr!("tip-sort-keys"))
            }
            TitleAt::Column(title, tip) => {
                Hint::new(crate::i18n::plain(&tr!(title))).line(tr!(tip))
            }
        }
    }

    /// A field of the viewer's status line (`name│n/N│t│cp│size│Col c│p%`).
    fn viewer_status_hint(&self, i: usize, text: &str, idx: usize) -> Option<Hint> {
        let v = &self.viewers[i];
        let (mut fields, at) = fields_at(text, idx)?;
        // The clock's place after the last bar.
        if fields.last().is_some_and(|f| f.trim().is_empty()) {
            fields.pop();
        }
        let from_end = fields.len().checked_sub(at + 1)?;
        let hint = match from_end {
            0 => {
                let pct = fields[at].trim().to_string();
                Hint::new(tr!("tip-view-position", pct = pct))
                    .line(tr!(
                        "tip-view-offset",
                        offset = crate::panel::group_thousands(v.top)
                    ))
                    .line(tr!("tip-view-goto"))
            }
            1 => Hint::new(tr!("tip-view-column", col = v.left + 1)).line(tr!("tip-view-shift")),
            2 => Hint::new(tr!(
                "tip-size",
                size = crate::panel::group_thousands(v.size()),
                n = v.size() as i64
            )),
            3 => Hint::new(crate::viewer::codepage::long_name(v.codepage()))
                .line(tr!("tip-codepage-keys")),
            4 => {
                let mode = match v.mode_letter() {
                    'h' => tr!("tip-view-hex"),
                    'd' => tr!("tip-view-dump"),
                    _ => tr!("tip-view-text"),
                };
                Hint::new(mode).line(tr!("tip-view-mode-keys"))
            }
            _ if at == 0 => Hint::new(v.path().display().to_string()),
            _ => {
                let (k, total) = v.found_numbers()?;
                let title = match k {
                    Some(k) => tr!("tip-found", k = k, total = total),
                    None => tr!("tip-found-none", total = total),
                };
                Hint::new(title).line(tr!("tip-found-keys"))
            }
        };
        Some(hint)
    }

    /// A field of the editor's status line
    /// (`name│*-│cp│Стр n/N│Кол c│С k│RHS│code`).
    fn editor_status_hint(&self, i: usize, text: &str, idx: usize) -> Option<Hint> {
        let e = &self.editors[i];
        let (fields, at) = fields_at(text, idx)?;
        let attrs = fields.len() == 8;
        let from_end = fields.len().checked_sub(at + 1)?;
        let hint = match (from_end, attrs) {
            (0, _) => {
                let c = e
                    .lines()
                    .get(e.cursor.line)
                    .and_then(|l| l.text.chars().nth(e.cursor.col))?;
                Hint::new(tr!("tip-edit-char", code = format!("U+{:04X}", c as u32)))
                    .line(tr!("tip-edit-char-dec", dec = c as u32))
            }
            (1, true) => {
                let a = std::fs::metadata(e.path()).ok().map_or(0, |m| {
                    #[cfg(windows)]
                    {
                        use std::os::windows::fs::MetadataExt as _;
                        m.file_attributes()
                    }
                    #[cfg(not(windows))]
                    {
                        u32::from(m.permissions().readonly())
                    }
                });
                Hint::new(tr!(
                    "tip-attrs",
                    list = super::attributes::attribute_names(a).join(", ")
                ))
            }
            (1, false) | (2, true) => Hint::new(tr!("tip-edit-char-col", col = e.cursor.col + 1)),
            (2, false) | (3, true) => Hint::new(tr!("tip-edit-col")).line(tr!("tip-edit-col-why")),
            (3, false) | (4, true) => Hint::new(tr!(
                "tip-edit-line",
                line = e.cursor.line + 1,
                total = e.line_count()
            ))
            .line(tr!("tip-edit-goto")),
            (4, false) | (5, true) => {
                Hint::new(crate::viewer::codepage::long_name(e.cp)).line(tr!("tip-codepage-keys"))
            }
            (5, false) | (6, true) => {
                let mut hint = Hint::new(if e.modified() {
                    tr!("tip-edit-modified")
                } else {
                    tr!("tip-edit-unmodified")
                });
                if e.locked {
                    hint = hint.line(tr!("tip-edit-locked"));
                }
                hint
            }
            _ => Hint::new(e.path().display().to_string()),
        };
        Some(hint)
    }

    /// The agent's frame: its session and state.
    fn agent_hint(&self) -> Hint {
        let a = &self.agent;
        let mut hint = Hint::new(a.name.clone().unwrap_or_else(|| "claude".into()));
        let state = match &a.pty {
            None => tr!("agent-not-started"),
            Some(p) if p.has_exited() => tr!(
                "agent-exited",
                code = p.exit_code().map_or(-1, |c| c as i64)
            ),
            Some(_) => match &a.state {
                agent::AgentState::Ready => tr!("agent-ready"),
                agent::AgentState::Working => tr!("agent-working"),
                agent::AgentState::Waiting(m) => format!("{} — {m}", tr!("agent-waiting")),
            },
        };
        hint = hint.line(tr!("tip-agent-state", state = state));
        if let Some(cwd) = &a.cwd {
            hint = hint.line(tr!("tip-agent-folder", folder = cwd.display().to_string()));
        }
        if let Some(id) = &a.session_id {
            hint = hint.line(tr!("tip-agent-session", id = id.as_str()));
        }
        hint = hint.line(tr!(
            "tip-agent-mode",
            mode = a
                .permission_mode
                .clone()
                .unwrap_or_else(|| "default".into())
        ));
        let link = match (a.ide.is_some(), a.ide_connected) {
            (true, true) => tr!("tip-agent-ide-on"),
            (true, false) => tr!("tip-agent-ide-waiting"),
            (false, _) => tr!("tip-agent-ide-off"),
        };
        hint.line(link).line(if a.live {
            tr!("observe-live")
        } else {
            tr!("observe-on-demand")
        })
    }

    /// The hint shown: a block below and right of the mouse (above or to
    /// the left near the edges), lines wrapped at `MAX_WIDTH`.
    pub(super) fn draw_hint(&self, area: Rect, buf: &mut Buffer) {
        if self.has_overlay() {
            return;
        }
        let Some(Hover {
            x,
            y,
            shown: Some(hint),
            ..
        }) = &self.hover
        else {
            return;
        };
        let max = MAX_WIDTH
            .min(usize::from(area.width).saturating_sub(2))
            .max(10);
        let mut lines: Vec<(String, Style)> = Vec::new();
        for (text, style) in &hint.lines {
            for piece in wrap(text, max) {
                lines.push((piece, *style));
            }
        }
        lines.truncate(MAX_LINES);
        let w = lines
            .iter()
            .map(|(t, _)| t.chars().count())
            .max()
            .unwrap_or(0) as u16
            + 2;
        let h = lines.len() as u16;
        let w = w.min(area.width);
        let bx = if x + 1 + w <= area.right() {
            x + 1
        } else {
            area.right().saturating_sub(w)
        };
        let by = if y + 1 + h <= area.bottom() {
            y + 1
        } else {
            y.saturating_sub(h).max(area.y)
        };
        for (k, (text, style)) in lines.iter().enumerate() {
            let row = by + k as u16;
            for cx in bx..bx + w {
                buf[(cx, row)].set_symbol(" ").set_style(theme::HINT);
            }
            buf.set_stringn(bx + 1, row, text, usize::from(w - 2), *style);
        }
    }
}

/// A mark in a viewer or an editor: whose, what kind, its whole label.
fn mark_hint(m: &crate::viewer::Mark) -> Hint {
    use crate::viewer::MarkKind;
    let kind = match m.kind {
        MarkKind::Info => tr!("tip-mark-info"),
        MarkKind::Warning => tr!("tip-mark-warning"),
        MarkKind::Error => tr!("tip-mark-error"),
        MarkKind::Changed => tr!("tip-mark-changed"),
    };
    let title = if m.agent {
        format!("AI · {kind}")
    } else {
        kind
    };
    let lines = if m.from == m.to {
        tr!("tip-mark-line", line = m.from)
    } else {
        tr!("tip-mark-lines", from = m.from, to = m.to)
    };
    let mut hint = Hint::new(title);
    if !m.label.is_empty() {
        hint = hint.line(m.label.clone());
    }
    hint = hint.line(lines);
    if m.stale {
        hint = hint.line(tr!("tip-mark-stale"));
    }
    hint.line(tr!("tip-mark-keys"))
}

/// The `│`-separated fields of a status line and the one holding
/// character `idx`.
fn fields_at(text: &str, idx: usize) -> Option<(Vec<String>, usize)> {
    let chars: Vec<char> = text.chars().collect();
    if idx >= chars.len() || chars[idx] == '│' {
        return None;
    }
    let at = chars[..idx].iter().filter(|c| **c == '│').count();
    let fields = text.split('│').map(str::to_string).collect();
    Some((fields, at))
}

/// `text` cut into lines of at most `width` characters, at spaces when it
/// can.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split(' ') {
        let n = line.chars().count();
        if n > 0 && n + 1 + word.chars().count() > width {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
        while line.chars().count() > width {
            let head: String = line.chars().take(width).collect();
            line = line.chars().skip(width).collect();
            out.push(head);
        }
    }
    out.push(line);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_at_spaces_and_cuts_long_words() {
        assert_eq!(wrap("one two three", 7), vec!["one two", "three"]);
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap("", 5), vec![""]);
    }
}
