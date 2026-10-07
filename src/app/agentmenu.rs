//! The agent pane's menu (docs/13-agent-sessions.md): F9 while the agent
//! has the focus, or a click on the pane's title. A Far-style menu bar on
//! the pane's top row: Session, Mode, Links, View.

use std::path::PathBuf;
use std::time::Duration;

use crossterm::event::{KeyEvent, MouseEvent};

use super::agent::Launch;
use super::fileops::{Overlay, Purpose};
use super::panelcmds::MenuPurpose;
use super::{App, Focus};
use crate::command::Command;
use crate::config::AgentPosition;
use crate::dialog::{Dialog, input_at};
use crate::keymap::Chord;
use crate::menu::{Item, Menu};
use crate::menubar::{MenuBar, Outcome, Title};
use crate::tr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AgentAction {
    NewSession,
    ResumeList,
    MoveToPanel,
    Restart,
    Rename,
    Compact,
    Clear,
    Interrupt,
    PermissionMode(&'static str),
    Model(&'static str),
    ModelOther,
    Effort(&'static str),
    Observe(bool),
    ToggleIde,
    ToggleChannels,
    ShowIdeLog,
    ShowJournal,
    AgentSettings,
    Position(bool),
    Taller,
    Shorter,
    Hide,
}

/// Claude Code's permission modes: the value and the label id.
const MODES: [(&str, &str); 6] = [
    ("default", "agent-mode-default"),
    ("acceptEdits", "agent-mode-accept-edits"),
    ("plan", "agent-mode-plan"),
    ("auto", "agent-mode-auto"),
    ("dontAsk", "agent-mode-dont-ask"),
    ("bypassPermissions", "agent-mode-bypass"),
];

/// How long after typed text its Enter goes.
const ENTER_DELAY: Duration = Duration::from_millis(300);

const MODELS: [(&str, &str); 3] = [("opus", "Opus"), ("sonnet", "Sonnet"), ("haiku", "Haiku")];

const EFFORTS: [(&str, &str); 5] = [
    ("low", "agent-effort-low"),
    ("medium", "agent-effort-medium"),
    ("high", "agent-effort-high"),
    ("xhigh", "agent-effort-xhigh"),
    ("max", "agent-effort-max"),
];

/// An item: text, key shown, action, check mark.
type Entry = (String, Option<&'static str>, Option<AgentAction>, bool);

fn sep() -> Entry {
    (String::new(), None, None, false)
}

fn item(id: &str, action: AgentAction) -> Entry {
    (tr!(id), None, Some(action), false)
}

fn title(text: String, entries: Vec<Entry>) -> Title<AgentAction> {
    let mut items = Vec::new();
    let mut actions = Vec::new();
    for (text, key, action, check) in entries {
        if text.is_empty() {
            items.push(Item::separator());
            actions.push(None);
            continue;
        }
        let mut it = Item::new(text)
            .disabled(action.is_none())
            .checked(check.then_some('√'));
        if let Some(chord) = key.and_then(Chord::parse) {
            it = it.accel_chord(chord);
        }
        items.push(it);
        actions.push(action);
    }
    Title {
        text,
        items,
        actions,
        selected: 0,
    }
}

impl App {
    /// F9 on the agent pane, or a click on its title.
    pub(super) fn agent_menu(&mut self) {
        use AgentAction::*;
        let a = &self.agent;
        let alive = self.agent_alive();
        let on = |b: bool, e: Entry| (e.0, e.1, if b { e.2 } else { None }, e.3);
        let session = vec![
            item("agent-menu-new", NewSession),
            item("agent-menu-resume", ResumeList),
            on(alive, item("agent-menu-move", MoveToPanel)),
            on(a.session_id.is_some(), item("agent-menu-restart", Restart)),
            on(alive, item("agent-menu-rename", Rename)),
            sep(),
            on(alive, item("agent-menu-compact", Compact)),
            on(alive, item("agent-menu-clear", Clear)),
            on(
                alive,
                (
                    tr!("agent-menu-interrupt"),
                    Some("Esc"),
                    Some(Interrupt),
                    false,
                ),
            ),
        ];
        let current = a.permission_mode.as_deref().unwrap_or("default");
        let mut mode: Vec<Entry> = MODES
            .iter()
            .map(|(m, id)| (tr!(id), None, Some(PermissionMode(m)), *m == current))
            .collect();
        mode.push(sep());
        for (m, label) in MODELS {
            mode.push(on(
                alive,
                (
                    tr!("agent-menu-model", model = label),
                    None,
                    Some(Model(m)),
                    false,
                ),
            ));
        }
        mode.push(on(alive, item("agent-menu-model-other", ModelOther)));
        mode.push(sep());
        for (e, id) in EFFORTS {
            mode.push(on(alive, item(id, Effort(e))));
        }
        mode.push(sep());
        mode.push((
            tr!("agent-menu-on-demand"),
            None,
            Some(Observe(false)),
            !a.live,
        ));
        mode.push((tr!("agent-menu-live"), None, Some(Observe(true)), a.live));
        let links = vec![
            (
                tr!("agent-menu-ide"),
                None,
                Some(ToggleIde),
                self.config.agent.ide,
            ),
            (
                tr!("agent-menu-channels"),
                None,
                Some(ToggleChannels),
                self.config.agent.channels,
            ),
            sep(),
            item("agent-menu-ide-log", ShowIdeLog),
            item("agent-menu-journal", ShowJournal),
            sep(),
            item("agent-menu-settings", AgentSettings),
        ];
        let top = self.wm.agent_on_top();
        let view = vec![
            (tr!("agent-menu-top"), None, Some(Position(true)), top),
            (tr!("agent-menu-bottom"), None, Some(Position(false)), !top),
            sep(),
            (
                tr!("agent-menu-taller"),
                Some("Ctrl+Up"),
                Some(Taller),
                false,
            ),
            (
                tr!("agent-menu-shorter"),
                Some("Ctrl+Down"),
                Some(Shorter),
                false,
            ),
            (tr!("agent-menu-hide"), Some("Ctrl+O"), Some(Hide), false),
        ];
        let titles = vec![
            title(tr!("agent-menu-session"), session),
            title(tr!("agent-menu-mode"), mode),
            title(tr!("agent-menu-links"), links),
            title(tr!("agent-menu-view"), view),
        ];
        // At the bottom of the screen the submenus may open upwards.
        let above = self
            .last_layout
            .as_ref()
            .map(|l| {
                ratatui::layout::Rect::new(
                    l.top.x,
                    l.top.y.min(l.agent_frame.y),
                    l.top.width,
                    l.agent_frame.y.saturating_sub(l.top.y.min(l.agent_frame.y)),
                )
            })
            .unwrap_or_default();
        let bar = MenuBar::new(titles, 0).with_room_above(above);
        self.overlays.push(Overlay::AgentMenu(bar));
    }

    /// The menu opened by a click on the pane's top frame: the bar on that
    /// row, the title under the mouse open (as Far's top row click).
    pub(super) fn agent_menu_at(&mut self, pos: ratatui::layout::Position) {
        self.agent_menu();
        if let Some(Overlay::AgentMenu(bar)) = self.overlays.last_mut() {
            bar.set_row(pos.y);
            if bar.has_title_at(pos) {
                let press = MouseEvent {
                    kind: crossterm::event::MouseEventKind::Down(
                        crossterm::event::MouseButton::Left,
                    ),
                    column: pos.x,
                    row: pos.y,
                    modifiers: crossterm::event::KeyModifiers::NONE,
                };
                bar.handle_mouse(&press);
            }
        }
    }

    pub(super) fn agent_menu_key(&mut self, key: KeyEvent) {
        if let Some(Overlay::AgentMenu(bar)) = self.overlays.last_mut() {
            let outcome = bar.handle_key(&key);
            self.agent_menu_outcome(outcome);
        }
    }

    pub(super) fn agent_menu_mouse(&mut self, ev: &MouseEvent) {
        if let Some(Overlay::AgentMenu(bar)) = self.overlays.last_mut() {
            let outcome = bar.handle_mouse(ev);
            self.agent_menu_outcome(outcome);
        }
    }

    fn agent_menu_outcome(&mut self, outcome: Outcome<AgentAction>) {
        let action = match outcome {
            Outcome::Pending => return,
            Outcome::Closed => None,
            Outcome::Chosen(a) => Some(a),
        };
        self.overlays.pop();
        if let Some(action) = action {
            self.agent_action(action);
        }
    }

    /// Types a command into the agent's input and presses Enter.
    fn agent_type(&mut self, text: &str) {
        if let Some(pty) = &self.agent.pty
            && !pty.has_exited()
        {
            // Enter comes a moment later (`AgentSession::send_enter`):
            // Claude Code takes a long text arriving at once for a paste,
            // and an Enter within a paste does not send it.
            let _ = pty.write(text.as_bytes());
            self.agent.enter_at = Some(std::time::Instant::now() + ENTER_DELAY);
            self.focus = Focus::Agent;
        }
    }

    /// Ends the running agent (as on quit) and starts it again.
    pub(super) fn relaunch_agent(&mut self, cwd: PathBuf, launch: Launch) {
        if let Some(mut pty) = self.agent.pty.take() {
            pty.shutdown(Duration::from_secs(3));
        }
        let (rows, cols) = self.last_agent_size();
        self.launch_agent(cwd, launch, &[], cols, rows);
        self.focus = Focus::Agent;
    }

    fn panel_dir(&self) -> PathBuf {
        self.panels[self.active].path.clone()
    }

    /// What to ask before an action (`[confirm] agent`): the question's
    /// lines, or `None` to go ahead.
    fn agent_question(&self, action: AgentAction) -> Option<Vec<String>> {
        use AgentAction::*;
        if !self.config.confirm.agent {
            return None;
        }
        let alive = self.agent_alive();
        let ends_agent = vec![tr!("agent-confirm-end"), tr!("agent-confirm-end-note")];
        match action {
            Clear => Some(vec![
                tr!("agent-confirm-clear"),
                tr!("agent-confirm-clear-note"),
            ]),
            NewSession | Restart if alive => Some(ends_agent),
            PermissionMode("bypassPermissions") => {
                let mut lines = vec![tr!("agent-confirm-bypass")];
                if alive {
                    lines.extend(ends_agent);
                }
                Some(lines)
            }
            PermissionMode(_) if alive => Some(ends_agent),
            _ => None,
        }
    }

    /// Runs an action of the agent menu, asking first when it loses
    /// context or ends the running agent.
    fn agent_action(&mut self, action: AgentAction) {
        match self.agent_question(action) {
            Some(lines) => {
                let dialog = Dialog::message(
                    &tr!("agent-confirm-title"),
                    &lines,
                    &[&tr!("MYes"), &tr!("MCancel")],
                    false,
                );
                self.overlays.push(Overlay::Dialog {
                    dialog,
                    purpose: Purpose::AgentConfirm(action),
                });
            }
            None => self.agent_action_now(action),
        }
    }

    pub(super) fn agent_action_now(&mut self, action: AgentAction) {
        use AgentAction::*;
        match action {
            NewSession => self.relaunch_agent(self.panel_dir(), Launch::Fresh),
            ResumeList => self.sessions_menu(self.panel_dir()),
            MoveToPanel => {
                let dir = self.panel_dir();
                self.agent_type(&format!("/cd {}", dir.display()));
                self.agent.cwd = Some(dir);
            }
            Restart => {
                if let Some(id) = self.agent.session_id.clone() {
                    let cwd = self.agent.cwd.clone().unwrap_or_else(|| self.panel_dir());
                    self.relaunch_agent(cwd, Launch::Resume(id));
                }
            }
            Rename => {
                let name = self.agent.name.clone().unwrap_or_default();
                let dialog = Dialog::new(tr!("agent-rename-title"), 50)
                    .text(tr!("agent-rename-prompt"))
                    .row(vec![input_at(5, 49, name, Some("AgentName"))])
                    .separator()
                    .buttons(&[&tr!("MOk"), &tr!("MCancel")], 0);
                self.overlays.push(Overlay::Dialog {
                    dialog,
                    purpose: Purpose::AgentRename,
                });
            }
            Compact => {
                // The instructions dialog asks as well.
                let dialog = Dialog::new(tr!("agent-compact-title"), 66)
                    .wrapped(&tr!("agent-confirm-compact-note"))
                    .text(tr!("agent-compact-prompt"))
                    .row(vec![input_at(5, 65, "", Some("AgentCompact"))])
                    .separator()
                    .buttons(&[&tr!("agent-compact-button"), &tr!("MCancel")], 0);
                self.overlays.push(Overlay::Dialog {
                    dialog,
                    purpose: Purpose::AgentCompact,
                });
            }
            Clear => self.agent_type("/clear"),
            Interrupt => {
                if let Some(pty) = &self.agent.pty {
                    let _ = pty.write(b"\x1b");
                }
            }
            PermissionMode(mode) => {
                self.agent.permission_mode = (mode != "default").then(|| mode.to_string());
                if self.agent_alive()
                    && let Some(id) = self.agent.session_id.clone()
                {
                    let cwd = self.agent.cwd.clone().unwrap_or_else(|| self.panel_dir());
                    self.relaunch_agent(cwd, Launch::Resume(id));
                }
            }
            Model(m) => self.agent_type(&format!("/model {m}")),
            ModelOther => {
                let dialog = Dialog::new(tr!("agent-model-title"), 50)
                    .text(tr!("agent-model-prompt"))
                    .row(vec![input_at(5, 49, "", Some("AgentModel"))])
                    .separator()
                    .buttons(&[&tr!("MOk"), &tr!("MCancel")], 0);
                self.overlays.push(Overlay::Dialog {
                    dialog,
                    purpose: Purpose::AgentModel,
                });
            }
            Effort(e) => self.agent_type(&format!("/effort {e}")),
            Observe(live) => {
                self.agent.live = live;
                self.say(if live {
                    tr!("observe-switched-live")
                } else {
                    tr!("observe-switched-on-demand")
                });
            }
            ToggleIde => {
                self.config.agent.ide = !self.config.agent.ide;
                let on = self.config.agent.ide;
                self.set_ide(on);
                self.save_settings_and_say_restart();
            }
            ToggleChannels => {
                self.config.agent.channels = !self.config.agent.channels;
                self.save_settings_and_say_restart();
            }
            ShowIdeLog => {
                let path = self.journal.dir().join("ide.log");
                self.open_viewer(&path, Vec::new());
            }
            ShowJournal => {
                let path = self.journal.dir().join("journal.jsonl");
                self.open_viewer(&path, Vec::new());
            }
            AgentSettings => self.agent_settings_dialog(),
            Position(top) => {
                self.wm.set_agent_on_top(top);
                self.config.agent.position = if top {
                    AgentPosition::Top
                } else {
                    AgentPosition::Bottom
                };
                let path = crate::config::config_path();
                if let Err(e) = self.config.save(&path) {
                    self.say(tr!("settings-save-failed", error = e));
                }
            }
            Taller => {
                self.run_command(Command::AgentTaller);
            }
            Shorter => {
                self.run_command(Command::AgentShorter);
            }
            Hide => self.hide_agent(),
        }
    }

    /// "Continue a session of this folder…": Claude Code's conversations of
    /// `dir`, newest first; the open one is checked.
    pub(super) fn sessions_menu(&mut self, dir: PathBuf) {
        let sessions = crate::claude_sessions::list(&dir);
        if sessions.is_empty() {
            self.say(tr!("agent-no-sessions", dir = dir.display().to_string()));
            return;
        }
        let width = sessions
            .iter()
            .map(|s| s.title.chars().count())
            .max()
            .unwrap_or(10)
            .min(60);
        let current = self.agent.session_id.clone();
        let items = sessions
            .iter()
            .map(|s| {
                let time: chrono::DateTime<chrono::Local> = s.modified.into();
                let title: String = s.title.chars().take(width).collect();
                Item::new(format!(
                    "{:<width$}  {}  {:>7}",
                    title.replace('&', "&&"),
                    time.format("%d.%m.%y %H:%M"),
                    crate::panel::size_float(s.size),
                ))
                .checked((current.as_deref() == Some(s.id.as_str())).then_some('√'))
            })
            .collect();
        let menu = Menu::new(
            tr!("agent-sessions-title", dir = dir.display().to_string()),
            items,
        );
        let ids = sessions.into_iter().map(|s| s.id).collect();
        self.overlays.push(Overlay::Menu {
            menu,
            purpose: MenuPurpose::Sessions { dir, ids },
        });
    }

    /// A session chosen from the folder's list: the running agent ends
    /// (asked first, unless it is that very session's agent stopped).
    pub(super) fn resume_session(&mut self, dir: PathBuf, id: String) {
        let action_needed = self.config.confirm.agent && self.agent_alive();
        if action_needed {
            let dialog = Dialog::message(
                &tr!("agent-confirm-title"),
                &[tr!("agent-confirm-end"), tr!("agent-confirm-end-note")],
                &[&tr!("MYes"), &tr!("MCancel")],
                false,
            );
            self.overlays.push(Overlay::Dialog {
                dialog,
                purpose: Purpose::AgentResume { dir, id },
            });
        } else {
            self.relaunch_agent(dir, Launch::Resume(id));
        }
    }

    pub(super) fn agent_renamed(&mut self, name: String) {
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        self.agent_type(&format!("/rename {name}"));
        self.agent.name = Some(name);
    }

    /// `/compact` with the optional instructions on what to keep.
    pub(super) fn agent_compact(&mut self, instructions: String) {
        let instructions = instructions
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if instructions.is_empty() {
            self.agent_type("/compact");
        } else {
            self.agent_type(&format!("/compact {instructions}"));
        }
    }

    pub(super) fn agent_model(&mut self, model: String) {
        let model = model.trim().to_string();
        if !model.is_empty() {
            self.agent_type(&format!("/model {model}"));
        }
    }

    fn save_settings_and_say_restart(&mut self) {
        let path = crate::config::config_path();
        match self.config.save(&path) {
            Ok(()) => self.say(tr!("agent-settings-restart")),
            Err(e) => self.say(tr!("settings-save-failed", error = e)),
        }
    }
}
