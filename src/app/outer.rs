//! What afar tells the terminal it runs in (docs/16): the window title and
//! the active panel's folder (a new tab opens there), progress on the
//! taskbar button (file operations, else the running command's, else the
//! agent's), the bell of a program; programs' notifications go to the
//! message line.

use super::App;
use super::agent::AgentState;
use crate::term::Signals;
use crate::tr;

#[derive(Default)]
pub(super) struct Outer {
    /// Last sent: the title, the folder, the progress.
    title: String,
    progress: (u8, u8),
    /// The latest progress the agent and the running command reported.
    agent_progress: (u8, u8),
    command_progress: (u8, u8),
    /// Bytes to write after the next frame.
    pending: Vec<u8>,
}

impl App {
    /// The agent's state changed (hooks): a bell when it is done or asks
    /// while the window is in the background.
    pub(super) fn agent_state(&mut self, state: AgentState) {
        if state == self.agent.state {
            return;
        }
        let ends = matches!(state, AgentState::Ready | AgentState::Waiting(_))
            && self.agent.state == AgentState::Working;
        if ends && !self.window_focused {
            self.outer.pending.push(0x07);
        }
        self.agent.state = state;
    }

    /// `Notification`: a permission request makes the agent wait; other
    /// notices (waiting for input) go to the message line.
    pub(super) fn on_agent_notification(&mut self, input: &str) {
        let v: serde_json::Value = serde_json::from_str(input).unwrap_or_default();
        let message = v
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or_default()
            .to_string();
        let kind = v
            .get("notification_type")
            .and_then(|m| m.as_str())
            .unwrap_or_default();
        if kind == "permission_prompt" || message.to_lowercase().contains("permission") {
            self.agent_state(AgentState::Waiting(message));
        } else {
            if !self.window_focused {
                self.outer.pending.push(0x07);
            }
            if !message.is_empty() {
                self.say(format!("{}: {message}", tr!("note-from-agent")));
            }
        }
    }

    /// Takes what the agent and the running command signalled.
    pub(super) fn poll_signals(&mut self) {
        let agent = self.agent.pty.as_ref().map(|p| p.take_signals());
        let command = self.running.as_ref().map(|r| r.pty.take_signals());
        if self.running.is_none() {
            self.outer.command_progress = (0, 0);
        }
        if let Some(s) = agent {
            if let Some(p) = s.progress {
                self.outer.agent_progress = p;
            }
            self.take_common(s, true);
        }
        if let Some(s) = command {
            if let Some(p) = s.progress {
                self.outer.command_progress = p;
            }
            self.take_common(s, false);
        }
    }

    fn take_common(&mut self, s: Signals, agent: bool) {
        if s.bell {
            self.outer.pending.push(0x07);
        }
        for note in s.notes {
            let who = if agent {
                tr!("note-from-agent")
            } else {
                tr!("note-from-command")
            };
            self.say(format!("{who}: {note}"));
        }
    }

    /// The title, folder and progress for the terminal, when they changed;
    /// the bytes to write.
    pub(super) fn outer_update(&mut self) -> Vec<u8> {
        let folder = self.panels[self.active].path.display().to_string();
        let title = format!("{folder} - afar");
        if title != self.outer.title {
            // OSC 2: the title; OSC 9;9: the folder (Windows Terminal).
            self.outer
                .pending
                .extend_from_slice(format!("\x1b]2;{title}\x07\x1b]9;9;{folder}\x07").as_bytes());
            self.outer.title = title;
        }
        // The agent's own progress, else its state: working — the
        // indeterminate one, waiting — paused (yellow).
        let agent = match (&self.agent.state, self.outer.agent_progress) {
            (_, p) if p.0 != 0 => p,
            (AgentState::Working, _) if self.agent_alive() => (3, 0),
            (AgentState::Waiting(_), _) if self.agent_alive() => (4, 100),
            _ => (0, 0),
        };
        let progress = self
            .ops_progress()
            .or(Some(self.outer.command_progress).filter(|p| p.0 != 0))
            .unwrap_or(agent);
        if progress != self.outer.progress {
            let (state, percent) = progress;
            self.outer
                .pending
                .extend_from_slice(format!("\x1b]9;4;{state};{percent}\x07").as_bytes());
            self.outer.progress = progress;
        }
        std::mem::take(&mut self.outer.pending)
    }
}
