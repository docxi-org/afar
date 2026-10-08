//! The agent and the viewer (docs/11, "Агент и просмотр", stage 2):
//! `afar_view` opens a file at a line (or a pattern) and may mark it,
//! `afar_highlight` marks lines with labels (info / warning / error,
//! blinking, for a while), `afar_viewer_state` tells what the user sees
//! and has selected. Attention etiquette: the agent's file comes to the
//! screen only from the panels or in "follow the agent" mode, otherwise
//! it opens behind and the message line says so. Files changed while open
//! show their changed lines; the journal gets what the user views and
//! selects.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::App;
use crate::journal::{Actor, Event};
use crate::tr;
use crate::viewer::{Mark, MarkKind};
use crate::wm::ScreenId;

/// A mark asked for by the agent.
#[derive(Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct MarkSpec {
    /// First line (from 1).
    pub from_line: u64,
    /// Last line (inclusive); `from_line` if omitted.
    pub to_line: Option<u64>,
    /// Shown with the place: in the viewer's status line, as a note at the
    /// line's end in the editor.
    pub label: Option<String>,
    /// `info` (default), `warning` or `error`.
    pub kind: Option<String>,
}

fn kind_of(s: Option<&str>) -> MarkKind {
    match s.unwrap_or("info") {
        "warning" | "warn" => MarkKind::Warning,
        "error" => MarkKind::Error,
        _ => MarkKind::Info,
    }
}

/// How long a mark blinks when asked to.
const FLASH: Duration = Duration::from_millis(1500);

impl App {
    /// The viewer showing `path`, opened (in the background unless the
    /// user may be interrupted) when there is none; its index.
    fn viewer_for(&mut self, path: &Path, actor: Actor) -> Result<usize, String> {
        let key = path.to_string_lossy().to_lowercase();
        if let Some(i) = self
            .viewers
            .iter()
            .position(|v| v.path().to_string_lossy().to_lowercase() == key)
        {
            return Ok(i);
        }
        if path.is_dir() {
            return Err(format!(
                "{} is a directory; afar_navigate shows it in a panel",
                path.display()
            ));
        }
        if !path.is_file() {
            return Err(format!("no such file: {}", path.display()));
        }
        let shown = self.wm.current_screen();
        let _ = actor;
        let id = self
            .open_viewer(path, vec![path.to_path_buf()])
            .ok_or_else(|| format!("cannot open {}", path.display()))?;
        // open_viewer shows it; the etiquette may put the user's screen back.
        if !self.agent_may_show(shown) {
            self.wm.switch_to(shown);
        }
        self.viewers
            .iter()
            .position(|v| v.id == id)
            .ok_or_else(|| "viewer gone".to_string())
    }

    /// The agent may bring its file to the screen: following it, or the
    /// user is on the panels (not reading another file).
    fn agent_may_show(&self, shown: ScreenId) -> bool {
        self.follow_agent || shown == ScreenId::Panels
    }

    /// Brings viewer `i` to the screen if the etiquette allows; otherwise
    /// tells the user where it is. A note for the agent's answer when the
    /// file stays behind.
    fn agent_show(&mut self, i: usize, what: &str) -> &'static str {
        let shown = self.wm.current_screen();
        let id = self.viewers[i].id;
        if shown == ScreenId::Viewer(id) {
            return "";
        }
        if self.agent_may_show(shown) {
            self.wm.switch_to(ScreenId::Viewer(id));
            ""
        } else {
            self.say(tr!("agent-opened-behind", what = what));
            " (behind: the user is busy with another screen and was told where it is)"
        }
    }

    /// Lines `from..=to` must be in the file of viewer `i`: an error names
    /// the file's length.
    fn check_lines(&mut self, i: usize, from: u64, to: u64) -> Result<(), String> {
        let total = self.viewers[i].line_count().max(1);
        if from == 0 || to < from {
            return Err(format!("bad line range {from}-{to}: lines count from 1"));
        }
        if from > total {
            return Err(format!(
                "line {from} is past the end: {} has {total} lines",
                self.viewers[i].path().display()
            ));
        }
        Ok(())
    }

    fn resolve_path(&self, path: &str) -> PathBuf {
        let p = PathBuf::from(path);
        if p.is_absolute() {
            p
        } else {
            self.panels[self.active].path.join(p)
        }
    }

    /// `afar_view`.
    pub(super) fn agent_view(
        &mut self,
        path: &str,
        line: Option<u64>,
        pattern: Option<String>,
        highlight: Option<MarkSpec>,
    ) -> Result<String, String> {
        let path = self.resolve_path(path);
        let (i, opened) = self.agent_viewer(&path)?;
        let result = self.agent_view_at(i, &path, line, pattern, highlight);
        // A failed call leaves no viewer it opened; a good one is journaled.
        match (&result, opened) {
            (Err(_), true) => self.close_viewer(i),
            (Ok(_), true) => self.journal_viewed(&path),
            _ => {}
        }
        result
    }

    fn journal_viewed(&mut self, path: &Path) {
        self.journal.push(
            Actor::Agent,
            Event::FileViewed {
                path: path.to_path_buf(),
            },
        );
    }

    /// The viewer of `path` for the agent, and whether it was opened now.
    fn agent_viewer(&mut self, path: &Path) -> Result<(usize, bool), String> {
        let before = self.viewers.len();
        let i = self.viewer_for(path, Actor::Agent)?;
        Ok((i, self.viewers.len() > before))
    }

    fn agent_view_at(
        &mut self,
        i: usize,
        path: &Path,
        line: Option<u64>,
        pattern: Option<String>,
        highlight: Option<MarkSpec>,
    ) -> Result<String, String> {
        if let Some(l) = line {
            self.check_lines(i, l, l)?;
        }
        if let Some(spec) = &highlight {
            let from = spec.from_line;
            self.check_lines(i, from, spec.to_line.unwrap_or(from))?;
        }
        let v = &mut self.viewers[i];
        let target = match (&pattern, line) {
            (Some(p), _) => {
                let re = regex::Regex::new(p).map_err(|e| format!("bad pattern: {e}"))?;
                Some(
                    v.find_line(&re, line.unwrap_or(1))
                        .ok_or_else(|| format!("{p:?} not found in {}", path.display()))?,
                )
            }
            (None, Some(l)) => Some(l),
            (None, None) => None,
        };
        if let Some(l) = target {
            v.show_line(l);
        }
        if let Some(spec) = highlight {
            let mut mark = make_mark(&spec, true);
            mark.to = mark.to.min(v.line_count().max(1));
            v.add_marks(vec![mark], false);
        } else if let Some(l) = target {
            // The place itself, blinking for a moment.
            v.add_marks(
                vec![Mark {
                    from: l,
                    to: l,
                    label: String::new(),
                    kind: MarkKind::Info,
                    agent: true,
                    flash_until: Some(Instant::now() + FLASH),
                    expires: Some(Instant::now() + FLASH),
                    stale: false,
                }],
                false,
            );
        }
        let total = v.line_count();
        let what = match target {
            Some(l) => format!("{}:{l}", path.display()),
            None => path.display().to_string(),
        };
        let behind = self.agent_show(i, &what);
        Ok(format!(
            "{what} is open in the viewer ({total} lines){behind}"
        ))
    }

    /// `afar_highlight`.
    pub(super) fn agent_highlight(
        &mut self,
        path: &str,
        marks: Vec<MarkSpec>,
        flash: bool,
        ttl_s: Option<u64>,
        clear: bool,
    ) -> Result<String, String> {
        let path = self.resolve_path(path);
        // A file open in the editor: the marks go there (docs/11, plan
        // step 3) — lines and notes in the margin.
        if let Some(i) = self.editor_of(&path) {
            return self.agent_highlight_editor(i, &path, marks, flash, ttl_s, clear);
        }
        let (i, opened) = self.agent_viewer(&path)?;
        for spec in &marks {
            let from = spec.from_line;
            if let Err(e) = self.check_lines(i, from, spec.to_line.unwrap_or(from)) {
                if opened {
                    self.close_viewer(i);
                }
                return Err(e);
            }
        }
        if opened {
            self.journal_viewed(&path);
        }
        let total = self.viewers[i].line_count().max(1);
        let v = &mut self.viewers[i];
        if clear {
            v.marks.retain(|m| !m.agent);
        }
        let expires = ttl_s.map(|s| Instant::now() + Duration::from_secs(s));
        let new: Vec<Mark> = marks
            .iter()
            .map(|spec| {
                let mut m = make_mark(spec, flash);
                m.to = m.to.min(total);
                m.expires = expires;
                m
            })
            .collect();
        let first = new.iter().map(|m| m.from).min();
        let n = new.len();
        v.add_marks(new, false);
        if let Some(l) = first {
            v.show_line(l);
        }
        let what = match first {
            Some(l) => format!("{}:{l}", path.display()),
            None => path.display().to_string(),
        };
        let behind = if n > 0 { self.agent_show(i, &what) } else { "" };
        Ok(format!("{n} place(s) marked in {}{behind}", path.display()))
    }

    fn agent_highlight_editor(
        &mut self,
        i: usize,
        path: &Path,
        marks: Vec<MarkSpec>,
        flash: bool,
        ttl_s: Option<u64>,
        clear: bool,
    ) -> Result<String, String> {
        let total = self.editors[i].line_count() as u64;
        for spec in &marks {
            let to = spec.to_line.unwrap_or(spec.from_line);
            if spec.from_line < 1 || to > total || to < spec.from_line {
                return Err(format!(
                    "lines {}-{to} are outside the editor buffer (1-{total})",
                    spec.from_line
                ));
            }
        }
        let e = &mut self.editors[i];
        if clear {
            e.marks.retain(|m| !m.agent);
        }
        let expires = ttl_s.map(|s| Instant::now() + Duration::from_secs(s));
        let n = marks.len();
        let first = marks.iter().map(|m| m.from_line).min();
        for spec in &marks {
            let mut m = make_mark(spec, flash);
            m.to = m.to.min(total);
            m.expires = expires;
            e.marks.push(m);
        }
        // The first place on the screen when it is not; the cursor stays
        // where it is, the screen too until the user acts.
        if let Some(l) = first {
            let line = l as usize - 1;
            let h = usize::from(e.area.height.max(1));
            if line < e.top || line >= e.top + h {
                e.top = line.saturating_sub(h / 4);
                e.hold_view = true;
            }
        }
        Ok(format!(
            "{n} place(s) marked in the editor buffer of {} (the labels show at the lines' ends)",
            path.display()
        ))
    }

    /// `afar_viewer_state`: the open files, what the user sees and selected.
    pub(super) fn agent_viewer_state(&mut self) -> String {
        let shown = self.shown_viewer();
        let mut out = Vec::new();
        for i in 0..self.viewers.len() {
            let v = &mut self.viewers[i];
            v.marks_tick();
            // A viewer never drawn (opened behind) has no rows yet: where
            // it will start.
            let visible = match v.visible_lines() {
                Some((first, last)) => serde_json::json!([first, last]),
                None => serde_json::Value::Null,
            };
            let top_line = v.top_line();
            let mut item = serde_json::json!({
                "path": v.path().display().to_string(),
                "on_screen": shown == Some(i),
                "visible_lines": visible,
                "top_line": top_line,
                "marks": v.marks.iter().map(|m| serde_json::json!({
                    "from_line": m.from, "to_line": m.to, "label": m.label,
                    "kind": format!("{:?}", m.kind).to_lowercase(),
                    "by_agent": m.agent, "stale": m.stale,
                })).collect::<Vec<_>>(),
            });
            if let Some((a, b)) = v.selection_lines() {
                let text = v.selected_text().unwrap_or_default();
                let text: String = text.chars().take(4000).collect();
                item["selection"] = serde_json::json!({"from_line": a, "to_line": b, "text": text});
            }
            out.push(item);
        }
        let query = &self.viewer_query.text;
        serde_json::json!({
            "viewers": out,
            "follow_agent": self.follow_agent,
            "search": if query.is_empty() { None } else { Some(query.clone()) },
        })
        .to_string()
    }

    /// Once a second: the shown viewer's file changed (marks for the
    /// changed lines, the agent's edits followed when asked), marks that
    /// blink or expire, the selection for the journal.
    pub(super) fn viewers_tick(&mut self) {
        // Marks expire in the viewers behind too.
        for v in &mut self.viewers {
            v.marks_tick();
        }
        let Some(i) = self.shown_viewer() else { return };
        if let Some(changes) = self.viewers[i].check_changed() {
            self.mark_changes(i, changes);
        }
        // The selection, once it stays a second.
        let sel = self.viewers[i].selection_lines();
        let path = self.viewers[i].path().to_path_buf();
        let now = Instant::now();
        match (&self.view_selection, sel) {
            (Some((p, s, since, reported)), Some(cur)) if *p == path && *s == cur => {
                if !reported && now.duration_since(*since) > Duration::from_secs(1) {
                    self.journal.push(
                        Actor::User,
                        Event::ViewerSelection {
                            path: path.clone(),
                            from_line: cur.0,
                            to_line: cur.1,
                        },
                    );
                    self.view_selection = Some((path, cur, *since, true));
                }
            }
            (_, Some(cur)) => self.view_selection = Some((path, cur, now, false)),
            (_, None) => self.view_selection = None,
        }
    }

    /// Lines of viewer `i` changed on the disk: marked, by the agent's
    /// hand or not; followed when "follow the agent" is on.
    fn mark_changes(&mut self, i: usize, changes: Vec<(u64, u64)>) {
        if changes.is_empty() {
            return;
        }
        let key = super::fswatch::path_key(self.viewers[i].path());
        let by_agent = self
            .fs
            .agent_paths
            .iter()
            .any(|(k, t)| *k == key && t.elapsed() < Duration::from_secs(120));
        let label = if by_agent {
            tr!("view-changed-by-agent")
        } else {
            tr!("view-changed")
        };
        let v = &mut self.viewers[i];
        v.marks.retain(|m| m.kind != MarkKind::Changed);
        let first = changes[0].0;
        v.add_marks(
            changes
                .into_iter()
                .map(|(from, to)| Mark {
                    from,
                    to: to.max(from),
                    label: label.clone(),
                    kind: MarkKind::Changed,
                    agent: false,
                    flash_until: None,
                    expires: None,
                    stale: false,
                })
                .collect(),
            false,
        );
        if self.follow_agent && by_agent {
            self.viewers[i].show_line(first);
            let id = self.viewers[i].id;
            self.wm.switch_to(ScreenId::Viewer(id));
        } else if self.shown_viewer() != Some(i) || by_agent {
            let what = format!("{}:{first}", self.viewers[i].path().display());
            self.say(tr!("view-changed-at", what = what));
        }
    }

    /// The agent wrote files (PostToolUse): open viewers of them look
    /// now, not at the next second.
    pub(super) fn viewers_agent_wrote(&mut self, paths: &[PathBuf]) {
        for p in paths {
            let key = super::fswatch::path_key(p);
            for i in 0..self.viewers.len() {
                if super::fswatch::path_key(self.viewers[i].path()) == key
                    && let Some(changes) = self.viewers[i].check_changed()
                {
                    self.mark_changes(i, changes);
                }
            }
        }
    }

    /// "Follow the agent" on or off.
    pub(super) fn toggle_follow_agent(&mut self) {
        self.follow_agent = !self.follow_agent;
        for v in &mut self.viewers {
            v.follow = self.follow_agent;
        }
        self.say(if self.follow_agent {
            tr!("follow-on")
        } else {
            tr!("follow-off")
        });
    }
}

fn make_mark(spec: &MarkSpec, flash: bool) -> Mark {
    let from = spec.from_line.max(1);
    Mark {
        from,
        to: spec.to_line.unwrap_or(from).max(from),
        label: spec.label.clone().unwrap_or_default(),
        kind: kind_of(spec.kind.as_deref()),
        agent: true,
        flash_until: flash.then(|| Instant::now() + FLASH),
        expires: None,
        stale: false,
    }
}
