//! The agent's session (docs/13-agent-sessions.md): everything afar keeps
//! about one running `claude`. There is one session now; several sessions
//! (tabs, side by side, linked) will be a list of these.

use std::path::PathBuf;
use std::time::Instant;

use tokio::sync::oneshot;

use crate::mcp::Reply;
use crate::term::PtySession;

/// What the agent is doing, by Claude Code's hooks (shown on the pane's
/// frame, docs/16).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) enum AgentState {
    /// Started, or finished its turn (`Stop`).
    #[default]
    Ready,
    /// A prompt was sent or a tool ran (`UserPromptSubmit`, tool hooks).
    Working,
    /// Asks for permission (`Notification`): the message.
    Waiting(String),
}

pub(super) struct AgentSession {
    pub pty: Option<PtySession>,
    /// Claude Code's session id (`--session-id`, `--resume`), so that a
    /// dev restart resumes exactly this conversation.
    pub session_id: Option<String>,
    /// The session's name (`/rename`), shown in the pane's title.
    pub name: Option<String>,
    /// The folder the agent runs in.
    pub cwd: Option<PathBuf>,
    /// Claude Code's permission mode chosen in the agent menu
    /// (`--permission-mode`); `None`: its own default.
    pub permission_mode: Option<String>,
    /// Last journal entry the agent has been told about.
    pub seen_seq: u64,
    /// Journal entries are added to each agent prompt (live) or only
    /// announced (on-demand).
    pub live: bool,
    pub started: Instant,
    /// The agent was started with `--resume` at that time: if it exits
    /// right away (nothing to resume yet), start it afresh.
    pub resumed_at: Option<Instant>,
    /// `--resume` failed once: tried again at that time (the previous
    /// afar's `claude` may still hold the conversation).
    pub resume_retry_at: Option<Instant>,
    pub resume_retried: bool,
    /// afar as the agent's IDE (`[agent] ide`).
    pub ide: Option<crate::ide::IdeServer>,
    pub ide_connected: bool,
    /// What `selection_changed` last told the agent.
    pub ide_sent: Option<super::ide::SentSelection>,
    /// Events for the agent's channel, and the waiting `afar channel`.
    pub channel_events: Vec<serde_json::Value>,
    pub channel_waiter: Option<oneshot::Sender<Reply>>,
    /// The name was last read from the conversation file.
    pub name_checked: Instant,
    /// Enter for text typed into the agent's input, when it is due.
    pub enter_at: Option<Instant>,
    /// The development channels question has been answered by afar.
    pub channels_confirmed: bool,
    pub state: AgentState,
}

impl AgentSession {
    pub fn new(live: bool) -> Self {
        Self {
            pty: None,
            session_id: None,
            name: None,
            cwd: None,
            permission_mode: None,
            seen_seq: 0,
            live,
            started: Instant::now(),
            resumed_at: None,
            resume_retry_at: None,
            resume_retried: false,
            ide: None,
            ide_connected: false,
            ide_sent: None,
            channel_events: Vec::new(),
            channel_waiter: None,
            name_checked: Instant::now(),
            enter_at: None,
            channels_confirmed: false,
            state: AgentState::Ready,
        }
    }

    /// Presses Enter for typed text once its time has come.
    pub fn send_enter(&mut self) {
        if self.enter_at.is_some_and(|t| Instant::now() >= t) {
            self.enter_at = None;
            if let Some(pty) = &self.pty {
                let _ = pty.write(b"\r");
            }
        }
    }

    /// The conversation file of the session.
    pub fn transcript(&self) -> Option<PathBuf> {
        let (id, cwd) = (self.session_id.as_ref()?, self.cwd.as_ref()?);
        Some(crate::claude_sessions::project_dir(cwd).join(format!("{id}.jsonl")))
    }

    /// Every 15 s: the name from the conversation file (`/rename` typed in
    /// the agent, the name Claude Code gives).
    pub fn refresh_name(&mut self) {
        if self.name_checked.elapsed() < std::time::Duration::from_secs(15) {
            return;
        }
        self.name_checked = Instant::now();
        if let Some(name) = self
            .transcript()
            .and_then(|t| crate::claude_sessions::title_of(&t))
        {
            self.name = Some(name);
        }
    }
}

/// How the agent starts.
pub(super) enum Launch {
    /// A new conversation (`--session-id`, our own id).
    Fresh,
    /// A conversation of the folder to continue (`--resume <id>`).
    Resume(String),
}
