//! Application state, event loop and drawing.
//!
//! All state is owned by the main loop; background threads (input, PTY
//! readers) only send `AppMsg`s.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{
    Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;

use crate::dev::{DevMsg, DevState, PanelState};
use crate::journal::{Actor, Event, Journal, format_entries};
use crate::mcp::{McpMsg, Reply, Request};
use crate::ops::{OpId, OpMsg};
use crate::panel::{FilePanel, put};
use crate::term::{PtySession, SpawnOptions};
use crate::wm::{self, Arrangement, Extent, ScreenId, SplitId, WinId, Wm};

mod fileops;
use crate::{keys, termview, theme, tr};
use fileops::{Overlay, RunningOp};

pub enum AppMsg {
    Input(TermEvent),
    AgentOutput,
    CommandOutput,
    Mcp(McpMsg),
    Op(OpMsg),
    Dev(DevMsg),
}

/// How the main loop ended.
pub enum Exit {
    Quit,
    /// Development mode: start the new build with this state.
    Restart(DevState),
}

/// Development mode (`afar --dev`): build status and restart conditions.
struct DevStatus {
    building: bool,
    /// A new build is ready: restart when nothing is in progress.
    ready: bool,
    restart_now: bool,
    last_agent_output: Instant,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Panels,
    Agent,
    /// Keys go to the running command.
    Command,
}

/// Environment variables of an enclosing Claude Code session that must not
/// leak into the agent we start.
const CLAUDE_SESSION_VARS: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_BRIDGE_SESSION_ID",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
];

const HISTORY_MAX: usize = 10_000;
/// Limits of the journal delta added to a prompt in live mode.
const LIVE_MAX_ENTRIES: usize = 60;
const LIVE_ERROR_TAIL: usize = 30;

const SYSTEM_PROMPT: &str = "You are running inside afar, a two-panel file manager in the style of \
Far Manager; the user sees its two file panels above your pane and works in them while talking to you. \
The afar_* MCP tools show you what the user did (afar_state, afar_journal, afar_commands, \
afar_command_output) and let you show things in the panels (afar_navigate, afar_select): when you \
refer to a file or directory, show it with afar_navigate. Blocks starting with [afar journal] in a \
user message are recent user actions added automatically.";

/// Record of a command run from the command line.
struct CmdRecord {
    id: u64,
    text: String,
    cwd: PathBuf,
    exit_code: Option<u32>,
    duration_ms: u64,
    lines: usize,
}

/// Where the agent finds afar's MCP server and hook endpoint.
pub struct AgentLink {
    pub port: u16,
    pub token: String,
}

struct RunningCommand {
    id: u64,
    pty: PtySession,
    started: Instant,
    /// Output lines that already scrolled off the command's screen.
    captured: Vec<String>,
    /// Panels were hidden automatically and come back when it ends.
    auto_switched: bool,
}

/// Geometry of a frame: the window manager's arrangement plus the fixed
/// bars at the bottom (command line, key bar).
#[derive(Clone, Default)]
struct Layout {
    /// The screen slot: panels or the user screen.
    top: Rect,
    /// File panels; empty rectangles when another screen is shown.
    panels: [Rect; 2],
    /// The agent pane with its frame, and the program area inside it.
    agent_frame: Rect,
    agent: Rect,
    cmdline: Rect,
    keybar: Rect,
    arrangement: Arrangement,
}

pub struct App {
    panels: [FilePanel; 2],
    active: usize,
    focus: Focus,
    wm: Wm,
    /// Splitter being dragged with the mouse, with the grab offset.
    drag: Option<(SplitId, u16)>,
    cmdline: String,
    /// Cursor in `cmdline`, in chars.
    cmd_cursor: usize,
    agent: Option<PtySession>,
    /// The "user screen": text of finished commands.
    history: Vec<String>,
    running: Option<RunningCommand>,
    next_cmd_id: u64,
    journal: Journal,
    commands: Vec<CmdRecord>,
    link: AgentLink,
    /// Journal entries are added to each agent prompt (live) or only
    /// announced (on-demand).
    live: bool,
    /// Last journal entry the agent has been told about.
    agent_seen_seq: u64,
    selection_changed: [Option<Instant>; 2],
    message: Option<(String, Instant)>,
    quit_armed: Option<Instant>,
    quit: bool,
    tx: Sender<AppMsg>,
    /// Geometry of the last frame, for mouse hit tests.
    last_layout: Option<Layout>,
    /// Where the running command's live screen was drawn.
    last_live: Rect,
    /// Last left click (time, panel, item) to detect double clicks.
    last_click: Option<(Instant, usize, usize)>,
    /// Dialogs and progress windows over the layout, topmost last.
    overlays: Vec<Overlay>,
    ops: std::collections::HashMap<OpId, RunningOp>,
    next_op_id: OpId,
    dev: Option<DevStatus>,
    /// Restored after a dev restart: keep the layout, continue the agent's
    /// conversation.
    restored: bool,
    /// Claude Code session id of the agent (`--session-id`), so that a dev
    /// restart resumes exactly this conversation.
    agent_session: Option<String>,
    /// The agent was started with `--resume` at that time: if it exits
    /// right away (nothing to resume yet), start it afresh.
    agent_resumed_at: Option<Instant>,
    agent_started: Instant,
    exit: Option<Exit>,
}

impl App {
    pub fn new(
        tx: Sender<AppMsg>,
        session_dir: PathBuf,
        link: AgentLink,
        dev: bool,
        restore: Option<DevState>,
    ) -> Self {
        let cwd = std::env::current_dir()
            .map(crate::panel::strip_verbatim)
            .unwrap_or_else(|_| PathBuf::from("."));
        let journal = Journal::open(session_dir);
        let mut app = Self {
            panels: [FilePanel::new(cwd.clone()), FilePanel::new(cwd)],
            active: 0,
            focus: Focus::Panels,
            wm: Wm::new(),
            drag: None,
            cmdline: String::new(),
            cmd_cursor: 0,
            agent: None,
            history: Vec::new(),
            running: None,
            next_cmd_id: 1,
            journal,
            commands: Vec::new(),
            link,
            live: false,
            agent_seen_seq: 0,
            selection_changed: [None, None],
            message: None,
            quit_armed: None,
            quit: false,
            tx,
            last_layout: None,
            last_live: Rect::default(),
            last_click: None,
            overlays: Vec::new(),
            ops: std::collections::HashMap::new(),
            next_op_id: 0,
            dev: dev.then(|| DevStatus {
                building: false,
                ready: false,
                restart_now: false,
                last_agent_output: Instant::now(),
            }),
            restored: false,
            agent_session: None,
            agent_resumed_at: None,
            agent_started: Instant::now(),
            exit: None,
        };
        if let Some(state) = restore {
            app.apply_state(state);
        }
        let (left, right) = (app.panels[0].path.clone(), app.panels[1].path.clone());
        app.journal
            .push(Actor::System, Event::AppStarted { left, right });
        app
    }

    fn apply_state(&mut self, state: DevState) {
        for (side, p) in state.panels.into_iter().enumerate().take(2) {
            if p.path.is_dir() {
                self.panels[side] = FilePanel::new(p.path);
            }
            if let Some(name) = p.cursor {
                self.panels[side].set_cursor_by_name(&name);
            }
        }
        self.active = state.active.min(1);
        self.live = state.live;
        for (id, extent) in state.splits {
            self.wm.set_extent(SplitId(id), extent);
        }
        self.set_panels_visible(state.panels_visible);
        self.agent_session = state.agent_session;
        self.restored = true;
    }

    fn state(&self) -> DevState {
        DevState {
            panels: self
                .panels
                .iter()
                .map(|p| PanelState {
                    path: p.path.clone(),
                    cursor: p.current().map(|e| e.name.clone()),
                })
                .collect(),
            active: self.active,
            panels_visible: self.panels_visible(),
            live: self.live,
            splits: self
                .wm
                .extents()
                .into_iter()
                .map(|(id, e)| (id.0, e))
                .collect(),
            agent_session: self.agent_session.clone(),
        }
    }

    /// Development mode: what keeps a restart waiting.
    fn restart_blocker(&self) -> Option<String> {
        let dev = self.dev.as_ref()?;
        if self.has_overlay() {
            Some(tr!("dev-blocker-dialog"))
        } else if !self.ops.is_empty() {
            Some(tr!("dev-blocker-operation"))
        } else if self.running.is_some() {
            Some(tr!("dev-blocker-command"))
        } else if self.agent_alive() && dev.last_agent_output.elapsed() < Duration::from_secs(3) {
            Some(tr!("dev-blocker-agent-busy"))
        } else if self.agent_alive() && self.agent_started.elapsed() < Duration::from_secs(10) {
            Some(tr!("dev-blocker-agent-starting"))
        } else {
            None
        }
    }

    fn on_dev(&mut self, msg: DevMsg) {
        let Some(dev) = &mut self.dev else { return };
        match msg {
            DevMsg::BuildStarted => {
                dev.building = true;
                self.say(tr!("dev-building"));
            }
            DevMsg::BuildFinished {
                ok,
                output,
                duration,
            } => {
                dev.building = false;
                if ok {
                    dev.ready = true;
                    self.say(tr!(
                        "dev-built",
                        secs = format!("{:.0}", duration.as_secs_f64())
                    ));
                } else {
                    self.record_build_failure(output);
                    self.say(tr!("dev-build-failed"));
                }
            }
        }
    }

    /// A failed build goes to the user screen and the command list, where
    /// the agent can read it too.
    fn record_build_failure(&mut self, output: Vec<String>) {
        let id = self.next_cmd_id;
        self.next_cmd_id += 1;
        let cwd = crate::dev::project_root();
        let text = "cargo build (afar --dev)".to_string();
        self.journal.push(
            Actor::System,
            Event::CommandStarted {
                cmd_id: id,
                text: text.clone(),
                cwd: cwd.clone(),
            },
        );
        let _ = std::fs::write(self.journal.output_path(id), output.join("\n"));
        self.journal.push(
            Actor::System,
            Event::CommandFinished {
                cmd_id: id,
                exit_code: Some(101),
                duration_ms: 0,
                lines: output.len(),
            },
        );
        self.commands.push(CmdRecord {
            id,
            text: text.clone(),
            cwd: cwd.clone(),
            exit_code: Some(101),
            duration_ms: 0,
            lines: output.len(),
        });
        self.push_history([format!("{}>{text}", cwd.display())]);
        self.push_history(output);
    }

    /// Restarts when a new build is ready (or on request) and nothing is
    /// in progress.
    fn maybe_restart(&mut self) {
        let Some(dev) = &self.dev else { return };
        if !(dev.ready || dev.restart_now) || dev.building {
            return;
        }
        if let Some(reason) = self.restart_blocker() {
            if dev.restart_now {
                self.say(tr!("dev-restart-waits", reason = reason));
            }
            return;
        }
        self.exit = Some(Exit::Restart(self.state()));
        self.quit = true;
    }

    pub fn run(mut self, terminal: &mut crate::tui::Tui, rx: Receiver<AppMsg>) -> Result<Exit> {
        let frame_log = std::env::var_os("AFAR_DEBUG_FRAMES").and_then(|_| {
            std::fs::File::options()
                .create(true)
                .append(true)
                .open(self.journal.dir().join("frames.log"))
                .ok()
        });
        let mut frame_log = frame_log;
        let size = terminal.size()?;
        if !self.restored {
            self.wm.set_extent(
                wm::MAIN_SPLIT,
                Extent::SecondFixed((size.height * 35 / 100).max(8)),
            );
        }
        self.last_layout = Some(self.layout(Rect::new(0, 0, size.width, size.height)));
        // AFAR_NO_AGENT: no agent until Enter in its pane (tests).
        if std::env::var_os("AFAR_NO_AGENT").is_none() {
            let (rows, cols) = self.last_agent_size();
            self.start_agent(cols, rows);
        }
        loop {
            let stats = terminal.draw(|frame| {
                let area = frame.area();
                let cursor = self.draw(area, frame.buffer_mut());
                if let Some(pos) = cursor {
                    frame.set_cursor_position(pos);
                }
            })?;
            if let Some(log) = &mut frame_log {
                use std::io::Write as _;
                let _ = writeln!(
                    log,
                    "{} bytes, {:.2} ms",
                    stats.bytes,
                    stats.elapsed.as_secs_f64() * 1000.0
                );
            }
            if self.quit {
                // Let Claude Code exit cleanly: killed while starting, it
                // falls back to its classic renderer next time.
                if let Some(agent) = &mut self.agent {
                    agent.shutdown(Duration::from_secs(3));
                }
                return Ok(self.exit.take().unwrap_or(Exit::Quit));
            }
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(msg) => {
                    self.handle(msg);
                    // Coalesce bursts (PTY output) into one redraw.
                    while let Ok(msg) = rx.try_recv() {
                        self.handle(msg);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Ok(Exit::Quit),
            }
            self.tick();
        }
    }

    fn handle(&mut self, msg: AppMsg) {
        match msg {
            AppMsg::Input(TermEvent::Key(key)) if key.kind != KeyEventKind::Release => {
                self.on_key(key)
            }
            AppMsg::Input(TermEvent::Mouse(mouse)) => self.on_mouse(mouse),
            AppMsg::AgentOutput => {
                if let Some(dev) = &mut self.dev {
                    dev.last_agent_output = Instant::now();
                }
                // `--resume` with nothing to resume exits at once.
                let quick_exit = self.agent.as_ref().is_some_and(|a| a.has_exited())
                    && self
                        .agent_resumed_at
                        .is_some_and(|t| t.elapsed() < Duration::from_secs(15));
                if quick_exit {
                    self.restored = false;
                    let (rows, cols) = self.last_agent_size();
                    self.start_agent(cols, rows);
                }
            }
            AppMsg::Input(_) => {}
            AppMsg::Dev(msg) => self.on_dev(msg),
            AppMsg::CommandOutput => self.on_command_output(),
            AppMsg::Mcp(McpMsg { request, reply }) => match request {
                // Answered later: after the user's confirmation.
                Request::Delete {
                    side,
                    names,
                    permanent,
                } => match self.resolve_side(&side) {
                    Ok(side) => self.agent_delete(side, &names, permanent, reply),
                    Err(e) => {
                        let _ = reply.send(Err(e));
                    }
                },
                Request::Copy {
                    side,
                    names,
                    dest,
                    moving,
                } => match self.resolve_side(&side) {
                    Ok(side) => self.agent_copy(side, &names, dest, moving, reply),
                    Err(e) => {
                        let _ = reply.send(Err(e));
                    }
                },
                request => {
                    let _ = reply.send(self.on_mcp(request));
                }
            },
            AppMsg::Op(msg) => self.on_op(msg),
        }
    }

    /// Periodic work: debounced journal entries, message expiry.
    fn tick(&mut self) {
        for side in 0..2 {
            if self.selection_changed[side]
                .is_some_and(|t| t.elapsed() > Duration::from_millis(500))
            {
                self.selection_changed[side] = None;
                let names: Vec<String> = self.panels[side]
                    .selected()
                    .map(|e| e.name.clone())
                    .collect();
                self.journal.push(
                    Actor::User,
                    Event::SelectionChanged {
                        panel: side_name(side),
                        count: names.len(),
                        sample: names.into_iter().take(5).collect(),
                    },
                );
            }
        }
        if self
            .message
            .as_ref()
            .is_some_and(|(_, t)| t.elapsed() > Duration::from_secs(5))
        {
            self.message = None;
        }
        self.maybe_restart();
    }

    fn say(&mut self, text: impl Into<String>) {
        self.message = Some((text.into(), Instant::now()));
    }

    // ---------------------------------------------------------------- agent

    /// Writes the MCP config and hook settings for the agent; returns the
    /// command-line arguments for `claude`.
    fn agent_args(&self) -> Result<Vec<String>> {
        let dir = self.journal.dir();
        let mcp = serde_json::json!({
            "mcpServers": { "afar": {
                "type": "http",
                "url": format!("http://127.0.0.1:{}/mcp", self.link.port),
                "headers": { "Authorization": format!("Bearer {}", self.link.token) },
            }}
        });
        let exe = std::env::current_exe()?
            .to_string_lossy()
            .replace('\\', "/");
        let hook = |event: &str| {
            serde_json::json!([{ "hooks": [
                { "type": "command", "command": format!("\"{exe}\" hook {event}") }
            ]}])
        };
        let settings = serde_json::json!({ "hooks": {
            "SessionStart": hook("session-start"),
            "UserPromptSubmit": hook("user-prompt"),
        }});
        let mcp_path = dir.join("mcp.json");
        let settings_path = dir.join("settings.json");
        std::fs::write(&mcp_path, serde_json::to_string_pretty(&mcp)?)?;
        std::fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;
        Ok(vec![
            "--settings".into(),
            settings_path.to_string_lossy().into_owned(),
            "--append-system-prompt".into(),
            SYSTEM_PROMPT.into(),
            // Variadic option: keep it last.
            "--mcp-config".into(),
            mcp_path.to_string_lossy().into_owned(),
        ])
    }

    fn start_agent(&mut self, cols: u16, rows: u16) {
        let tx = self.tx.clone();
        let cwd = self.panels[self.active].path.clone();
        let args = match self.agent_args() {
            Ok(args) => args,
            Err(e) => {
                self.say(tr!("agent-config-failed", error = format!("{e:#}")));
                vec![]
            }
        };
        let env = [
            (
                "AFAR_ENDPOINT".to_string(),
                format!("http://127.0.0.1:{}", self.link.port),
            ),
            ("AFAR_TOKEN".to_string(), self.link.token.clone()),
        ];
        // A new agent session has not seen anything yet.
        self.agent_seen_seq = 0;
        // Our own session id; after a dev restart: the same conversation.
        let mut args = args;
        self.agent_resumed_at = None;
        self.agent_started = Instant::now();
        match self.agent_session.clone().filter(|_| self.restored) {
            Some(id) => {
                args.splice(0..0, ["--resume".to_string(), id]);
                self.agent_resumed_at = Some(Instant::now());
            }
            None => {
                let id = new_uuid();
                args.splice(0..0, ["--session-id".to_string(), id.clone()]);
                self.agent_session = Some(id);
            }
        }
        match PtySession::spawn(
            SpawnOptions {
                program: "claude",
                args: &args,
                cwd: Some(&cwd),
                env: &env,
                env_remove: CLAUDE_SESSION_VARS,
                rows: rows.max(2),
                cols: cols.max(20),
                capture_lines: false,
            },
            move || {
                let _ = tx.send(AppMsg::AgentOutput);
            },
        ) {
            Ok(pty) => self.agent = Some(pty),
            Err(e) => self.say(tr!("agent-start-failed", error = format!("{e:#}"))),
        }
    }

    fn agent_alive(&self) -> bool {
        self.agent.as_ref().is_some_and(|a| !a.has_exited())
    }

    // ------------------------------------------------------------- commands

    fn execute(&mut self, text: String) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        // Built-ins handled by afar itself, like Far does.
        let lower = text.to_lowercase();
        if lower == "cd"
            || lower.starts_with("cd ")
            || lower.starts_with("cd\\")
            || lower.starts_with("cd..")
        {
            let arg = text[2..].trim().trim_matches('"');
            if arg.is_empty() {
                self.say(self.panels[self.active].path.display().to_string());
            } else {
                let target = self.panels[self.active].path.join(arg);
                self.change_dir(self.active, &target);
            }
            self.clear_cmdline();
            return;
        }
        if text.len() == 2 && text.ends_with(':') && text.as_bytes()[0].is_ascii_alphabetic() {
            self.change_dir(self.active, Path::new(&format!("{text}\\")));
            self.clear_cmdline();
            return;
        }
        if lower == "afar:restart" {
            self.request_restart();
            self.clear_cmdline();
            return;
        }
        if let Some(arg) = lower.strip_prefix("afar:") {
            match arg.trim() {
                "live" => self.live = !self.live,
                "live on" => self.live = true,
                "live off" => self.live = false,
                _ => {}
            }
            self.say(if self.live {
                tr!("observe-switched-live")
            } else {
                tr!("observe-switched-on-demand")
            });
            self.clear_cmdline();
            return;
        }
        if lower == "cls" {
            self.history.clear();
            self.clear_cmdline();
            return;
        }
        if self.running.is_some() {
            self.say(tr!("command-busy"));
            return;
        }

        let id = self.next_cmd_id;
        self.next_cmd_id += 1;
        let cwd = self.panels[self.active].path.clone();
        self.journal.push(
            Actor::User,
            Event::CommandStarted {
                cmd_id: id,
                text: text.clone(),
                cwd: cwd.clone(),
            },
        );
        self.commands.push(CmdRecord {
            id,
            text: text.clone(),
            cwd: cwd.clone(),
            exit_code: None,
            duration_ms: 0,
            lines: 0,
        });
        self.push_history([format!("{}>{}", cwd.display(), text)]);
        self.clear_cmdline();

        // The command text goes through an environment variable: cmd.exe
        // does not understand the MSVCRT quoting applied to arguments.
        let shell = std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".into());
        let tx = self.tx.clone();
        let (rows, cols) = self.last_top_size();
        match PtySession::spawn(
            SpawnOptions {
                program: &shell,
                args: &["/c".into(), "%AFAR_CMD%".into()],
                cwd: Some(&cwd),
                env: &[("AFAR_CMD".into(), text)],
                env_remove: &[],
                rows,
                cols,
                capture_lines: true,
            },
            move || {
                let _ = tx.send(AppMsg::CommandOutput);
            },
        ) {
            Ok(pty) => {
                self.running = Some(RunningCommand {
                    id,
                    pty,
                    started: Instant::now(),
                    captured: Vec::new(),
                    auto_switched: self.panels_visible(),
                });
                self.set_panels_visible(false);
                self.focus = Focus::Command;
            }
            Err(e) => {
                self.push_history([tr!("command-start-failed", error = format!("{e:#}"))]);
                self.journal.push(
                    Actor::System,
                    Event::CommandFinished {
                        cmd_id: id,
                        exit_code: None,
                        duration_ms: 0,
                        lines: 0,
                    },
                );
            }
        }
    }

    fn on_command_output(&mut self) {
        let Some(run) = &mut self.running else { return };
        termview::join_wrapped(run.pty.take_scrolled_lines(), &mut run.captured);
        if !run.pty.has_exited() {
            return;
        }
        let run = self.running.take().unwrap();
        let mut output = run.captured;
        output.extend(termview::screen_lines(run.pty.parser().screen()));
        let _ = std::fs::write(self.journal.output_path(run.id), output.join("\n"));
        let (exit_code, duration_ms) = (
            run.pty.exit_code(),
            run.started.elapsed().as_millis() as u64,
        );
        if let Some(rec) = self.commands.iter_mut().find(|c| c.id == run.id) {
            rec.exit_code = exit_code;
            rec.duration_ms = duration_ms;
            rec.lines = output.len();
        }
        self.journal.push(
            Actor::User,
            Event::CommandFinished {
                cmd_id: run.id,
                exit_code,
                duration_ms,
                lines: output.len(),
            },
        );
        self.push_history(output);
        for p in &mut self.panels {
            p.reload(None);
        }
        if self.focus == Focus::Command {
            self.focus = Focus::Panels;
        }
        if run.auto_switched {
            self.set_panels_visible(true);
        }
    }

    fn push_history(&mut self, lines: impl IntoIterator<Item = String>) {
        self.history.extend(lines);
        if self.history.len() > HISTORY_MAX {
            self.history.drain(..self.history.len() - HISTORY_MAX);
        }
    }

    // -------------------------------------------------------------- panels

    fn change_dir(&mut self, side: usize, path: &Path) {
        if let Err(e) = self.change_dir_as(Actor::User, side, path) {
            self.say(e);
        }
    }

    fn change_dir_as(&mut self, actor: Actor, side: usize, path: &Path) -> Result<(), String> {
        let from = self.panels[side].change_dir(path)?;
        let to = self.panels[side].path.clone();
        self.journal.push(
            actor,
            Event::DirChanged {
                panel: side_name(side),
                from,
                to,
            },
        );
        Ok(())
    }

    fn enter(&mut self) {
        let panel = &self.panels[self.active];
        let Some(e) = panel.current().cloned() else {
            return;
        };
        if e.name == ".." {
            if let Some(parent) = panel.path.parent().map(Path::to_path_buf) {
                self.change_dir(self.active, &parent);
            }
        } else if e.is_dir {
            let target = panel.path.join(&e.name);
            self.change_dir(self.active, &target);
        } else {
            self.execute(quote(&e.name));
        }
    }

    fn mark_selection_changed(&mut self) {
        self.selection_changed[self.active] = Some(Instant::now());
    }

    // ------------------------------------------------------------ cmdline

    fn clear_cmdline(&mut self) {
        self.cmdline.clear();
        self.cmd_cursor = 0;
    }

    fn cmdline_insert(&mut self, s: &str) {
        let byte = char_to_byte(&self.cmdline, self.cmd_cursor);
        self.cmdline.insert_str(byte, s);
        self.cmd_cursor += s.chars().count();
    }

    /// Editing keys of the command line; returns false if not handled.
    fn cmdline_key(&mut self, key: &KeyEvent) -> bool {
        let len = self.cmdline.chars().count();
        match key.code {
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    || key
                        .modifiers
                        .contains(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                // Ctrl+Alt is AltGr on many layouts.
                self.cmdline_insert(&c.to_string());
            }
            KeyCode::Backspace if self.cmd_cursor > 0 => {
                let byte = char_to_byte(&self.cmdline, self.cmd_cursor - 1);
                self.cmdline.remove(byte);
                self.cmd_cursor -= 1;
            }
            KeyCode::Delete if self.cmd_cursor < len => {
                let byte = char_to_byte(&self.cmdline, self.cmd_cursor);
                self.cmdline.remove(byte);
            }
            KeyCode::Left => self.cmd_cursor = self.cmd_cursor.saturating_sub(1),
            KeyCode::Right => self.cmd_cursor = (self.cmd_cursor + 1).min(len),
            KeyCode::Esc => self.clear_cmdline(),
            _ => return false,
        }
        true
    }

    // --------------------------------------------------------------- keys

    fn on_key(&mut self, key: KeyEvent) {
        if std::env::var_os("AFAR_DEBUG_KEYS").is_some() {
            use std::io::Write as _;
            if let Ok(mut f) = std::fs::File::options()
                .create(true)
                .append(true)
                .open(self.journal.dir().join("keys.log"))
            {
                let _ = writeln!(f, "{key:?}");
            }
        }
        let key = keys::normalize(key);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // Ctrl+Space; Ctrl+@ / Ctrl+2 is the same NUL byte on some paths.
        let is_focus_key =
            key.code == KeyCode::Null || ctrl && matches!(key.code, KeyCode::Char(' ' | '@' | '2'));
        if is_focus_key {
            self.focus = match self.focus {
                Focus::Agent => Focus::Panels,
                Focus::Panels | Focus::Command => Focus::Agent,
            };
            if self.focus == Focus::Panels && self.running.is_some() {
                // The command keeps running in the background.
                self.set_panels_visible(true);
            }
            return;
        }
        if self.focus != Focus::Panels {
            self.quit_armed = None;
        }
        // Dialogs take the keyboard, except while talking to the agent.
        if self.focus != Focus::Agent && self.has_overlay() {
            self.overlay_key(key);
            return;
        }
        match self.focus {
            Focus::Agent => self.agent_key(key),
            Focus::Command => {
                if let Some(run) = &self.running {
                    let app_cursor = run.pty.parser().screen().application_cursor();
                    if let Some(bytes) = keys::encode(&key, app_cursor) {
                        let _ = run.pty.write(&bytes);
                    }
                }
            }
            Focus::Panels => self.panels_key(key),
        }
    }

    fn agent_key(&mut self, key: KeyEvent) {
        if !self.agent_alive() {
            if key.code == KeyCode::Enter {
                let (rows, cols) = self.last_agent_size();
                self.start_agent(cols, rows);
            }
            return;
        }
        let agent = self.agent.as_ref().unwrap();
        // Typing returns from a scrolled-back view.
        agent.parser().screen_mut().set_scrollback(0);
        let app_cursor = agent.parser().screen().application_cursor();
        if let Some(bytes) = keys::encode(&key, app_cursor) {
            let _ = agent.write(&bytes);
        }
    }

    fn panels_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let a = self.active;
        let page = self.last_page();
        if key.code != KeyCode::F(10) {
            self.quit_armed = None;
        }
        match key.code {
            KeyCode::F(10) => {
                if !self.agent_alive() && self.running.is_none()
                    || self
                        .quit_armed
                        .is_some_and(|t| t.elapsed() < Duration::from_secs(3))
                {
                    self.quit = true;
                } else {
                    self.quit_armed = Some(Instant::now());
                    self.say(tr!("quit-confirm"));
                }
            }
            KeyCode::Char('o') if ctrl => {
                if self.running.is_some() && !self.panels_visible() {
                    self.focus = Focus::Command;
                } else {
                    self.set_panels_visible(!self.panels_visible());
                    if !self.panels_visible() && self.running.is_some() {
                        self.focus = Focus::Command;
                    }
                }
            }
            // Like Far: Ctrl+arrows move the boundaries between windows.
            KeyCode::Up if ctrl => self.move_splitter(wm::MAIN_SPLIT, -1),
            KeyCode::Down if ctrl => self.move_splitter(wm::MAIN_SPLIT, 1),
            KeyCode::Left if ctrl && self.cmdline.is_empty() => {
                self.move_splitter(wm::PANELS_SPLIT, -1)
            }
            KeyCode::Right if ctrl && self.cmdline.is_empty() => {
                self.move_splitter(wm::PANELS_SPLIT, 1)
            }
            KeyCode::Enter if ctrl => {
                if let Some(e) = self.panels[a].current() {
                    let name = if e.name == ".." {
                        "..".to_string()
                    } else {
                        quote(&e.name)
                    };
                    self.cmdline_insert(&format!("{name} "));
                }
            }
            KeyCode::Enter => {
                if self.cmdline.trim().is_empty() {
                    if self.panels_visible() {
                        self.enter();
                    }
                } else {
                    let text = self.cmdline.clone();
                    self.execute(text);
                }
            }
            _ if !self.panels_visible() => {
                self.cmdline_key(&key);
            }
            KeyCode::Char('r' | 'R') if ctrl && shift => self.request_restart(),
            KeyCode::Tab => self.active = 1 - a,
            KeyCode::F(5) if !alt && !ctrl => self.copy_dialog(false, shift),
            KeyCode::F(6) if !alt && !ctrl => self.copy_dialog(true, shift),
            KeyCode::F(7) if !alt && !ctrl && !shift => self.mkdir_dialog(),
            KeyCode::F(8) if !alt && !ctrl => {
                let targets = self.op_sources(shift);
                self.delete_dialog(targets, false, Actor::User, None);
            }
            KeyCode::Delete if shift => {
                let targets = self.op_sources(false);
                self.delete_dialog(targets, true, Actor::User, None);
            }
            KeyCode::Delete if !alt && !ctrl && self.cmdline.is_empty() => {
                let targets = self.op_sources(false);
                self.delete_dialog(targets, false, Actor::User, None);
            }
            KeyCode::Up if shift => {
                self.panels[a].toggle_selection();
                self.panels[a].move_cursor(-1);
                self.mark_selection_changed();
            }
            KeyCode::Down if shift => {
                self.panels[a].toggle_selection();
                self.panels[a].move_cursor(1);
                self.mark_selection_changed();
            }
            KeyCode::Insert => {
                self.panels[a].toggle_selection();
                self.panels[a].move_cursor(1);
                self.mark_selection_changed();
            }
            KeyCode::Up => self.panels[a].move_cursor(-1),
            KeyCode::Down => self.panels[a].move_cursor(1),
            KeyCode::PageUp if ctrl => {
                if let Some(parent) = self.panels[a].path.parent().map(Path::to_path_buf) {
                    self.change_dir(a, &parent);
                }
            }
            KeyCode::PageUp => self.panels[a].move_cursor(-(page as isize)),
            KeyCode::PageDown => self.panels[a].move_cursor(page as isize),
            KeyCode::Home => self.panels[a].move_cursor(isize::MIN / 2),
            KeyCode::End => self.panels[a].move_cursor(isize::MAX / 2),
            KeyCode::Char('\\') if ctrl => {
                if let Some(root) = self.panels[a]
                    .path
                    .ancestors()
                    .last()
                    .map(Path::to_path_buf)
                {
                    self.change_dir(a, &root);
                }
            }
            KeyCode::Char('r') if ctrl => self.panels[a].reload(None),
            KeyCode::F(n @ (1..=9 | 11 | 12)) if !alt && !ctrl => {
                self.say(tr!("not-implemented", n = n));
            }
            _ => {
                self.cmdline_key(&key);
            }
        }
    }

    /// Moves a splitter by `delta` cells (positive: right / down).
    fn move_splitter(&mut self, id: SplitId, delta: i32) {
        let Some(sp) = self
            .last_layout
            .as_ref()
            .and_then(|l| l.arrangement.splitter(id).copied())
        else {
            return;
        };
        self.wm
            .set_first(id, i32::from(sp.first()) + delta, sp.total());
    }

    /// Ctrl+Shift+R / `afar:restart`: restart in development mode.
    fn request_restart(&mut self) {
        match &mut self.dev {
            Some(dev) => {
                dev.restart_now = true;
                self.maybe_restart();
            }
            None => self.say(tr!("dev-only")),
        }
    }

    fn panels_visible(&self) -> bool {
        self.wm.current_screen() == ScreenId::Panels
    }

    fn set_panels_visible(&mut self, visible: bool) {
        self.wm.switch_to(if visible {
            ScreenId::Panels
        } else {
            ScreenId::UserScreen
        });
    }

    // ---------------------------------------------------------------- mcp

    fn on_mcp(&mut self, request: Request) -> Reply {
        match request {
            Request::State => Ok(self.state_json().to_string()),
            Request::Journal { since, limit } => {
                let entries = self.journal.since(since.unwrap_or(0));
                let start = entries.len().saturating_sub(limit);
                if let Some(last) = entries.last() {
                    self.agent_seen_seq = self.agent_seen_seq.max(last.seq);
                }
                Ok(format_entries(&entries[start..]))
            }
            Request::Commands { limit } => {
                let start = self.commands.len().saturating_sub(limit);
                let mut out = String::new();
                for c in &self.commands[start..] {
                    let status = match (
                        c.exit_code,
                        self.running.as_ref().is_some_and(|r| r.id == c.id),
                    ) {
                        (_, true) => "running".to_string(),
                        (Some(code), _) => format!("exit {}", code as i32),
                        (None, _) => "exit ?".to_string(),
                    };
                    out.push_str(&format!(
                        "cmd-{}  {}  (cwd {})  {status}, {:.1}s, {} lines\n",
                        c.id,
                        c.text,
                        c.cwd.display(),
                        c.duration_ms as f64 / 1000.0,
                        c.lines
                    ));
                }
                Ok(if out.is_empty() {
                    "no commands yet".into()
                } else {
                    out
                })
            }
            Request::CommandOutput {
                cmd_id,
                tail,
                head,
                grep,
            } => {
                let id = cmd_id
                    .or_else(|| self.commands.last().map(|c| c.id))
                    .ok_or("no commands yet")?;
                let lines = self.command_output(id)?;
                let mut numbered: Vec<(usize, &String)> =
                    lines.iter().enumerate().map(|(i, l)| (i + 1, l)).collect();
                if let Some(g) = &grep {
                    let g = g.to_lowercase();
                    numbered.retain(|(_, l)| l.to_lowercase().contains(&g));
                }
                if let Some(h) = head {
                    numbered.truncate(h);
                }
                let tail = tail.or((head.is_none() && grep.is_none()).then_some(200));
                if let Some(t) = tail {
                    numbered.drain(..numbered.len().saturating_sub(t));
                }
                let mut out = format!("[cmd-{id}: {} lines total]\n", lines.len());
                for (n, l) in numbered {
                    out.push_str(&format!("{n:>5}: {l}\n"));
                }
                Ok(out)
            }
            Request::TerminalScreen => match &self.running {
                Some(run) => Ok(format!(
                    "[cmd-{} is running]\n{}",
                    run.id,
                    termview::screen_lines(run.pty.parser().screen()).join("\n")
                )),
                None => {
                    let start = self.history.len().saturating_sub(40);
                    Ok(format!(
                        "[no running command; end of the user screen]\n{}",
                        self.history[start..].join("\n")
                    ))
                }
            },
            Request::Navigate { side, path, cursor } => {
                let side = self.resolve_side(&side)?;
                let target = self.panels[side].path.join(&path);
                self.change_dir_as(Actor::Agent, side, &target)?;
                self.active = side;
                self.set_panels_visible(true);
                let mut note = String::new();
                if let Some(name) = cursor
                    && !self.panels[side].set_cursor_by_name(&name)
                {
                    note = format!(" ({name} not found there)");
                }
                let panel = &self.panels[side];
                Ok(format!(
                    "{} panel: {}, cursor on {}{note}",
                    side_name(side),
                    panel.path.display(),
                    panel.current().map_or("-", |e| e.name.as_str())
                ))
            }
            Request::Select { side, names, add } => {
                let side = self.resolve_side(&side)?;
                let found = self.panels[side].select_names(&names, add);
                self.set_panels_visible(true);
                let selected: Vec<String> = self.panels[side]
                    .selected()
                    .map(|e| e.name.clone())
                    .collect();
                self.journal.push(
                    Actor::Agent,
                    Event::SelectionChanged {
                        panel: side_name(side),
                        count: selected.len(),
                        sample: selected.iter().take(5).cloned().collect(),
                    },
                );
                let missing: Vec<&String> = names
                    .iter()
                    .filter(|n| {
                        !self.panels[side]
                            .entries
                            .iter()
                            .any(|e| e.name.eq_ignore_ascii_case(n))
                    })
                    .collect();
                let mut out = format!(
                    "selected {found} of {} in the {} panel",
                    names.len(),
                    side_name(side)
                );
                if !missing.is_empty() {
                    out.push_str(&format!("; not found: {missing:?}"));
                }
                Ok(out)
            }
            Request::MkDir { side, names } => {
                let side = self.resolve_side(&side)?;
                self.agent_mkdir(side, &names)
            }
            Request::Delete { .. } | Request::Copy { .. } => Err("handled asynchronously".into()),
            Request::HookPrompt => Ok(self.prompt_context()),
            Request::HookSessionStart => Ok(format!(
                "[afar] left panel: {} | right panel: {} | active: {} | journal at #{} | mode: {}",
                self.panels[0].path.display(),
                self.panels[1].path.display(),
                side_name(self.active),
                self.journal.last_seq(),
                if self.live {
                    "live (actions are added to prompts)"
                } else {
                    "on-demand (use afar_journal)"
                }
            )),
        }
    }

    fn resolve_side(&self, side: &str) -> Result<usize, String> {
        match side {
            "left" => Ok(0),
            "right" => Ok(1),
            "active" | "" => Ok(self.active),
            "passive" => Ok(1 - self.active),
            other => Err(format!(
                "unknown side {other:?}: use left, right, active or passive"
            )),
        }
    }

    fn command_output(&self, id: u64) -> Result<Vec<String>, String> {
        if let Some(run) = self.running.as_ref().filter(|r| r.id == id) {
            let mut lines = run.captured.clone();
            lines.extend(termview::screen_lines(run.pty.parser().screen()));
            return Ok(lines);
        }
        std::fs::read_to_string(self.journal.output_path(id))
            .map(|s| s.lines().map(str::to_string).collect())
            .map_err(|_| format!("no output for cmd-{id}"))
    }

    fn state_json(&self) -> serde_json::Value {
        let panel = |side: usize| {
            let p = &self.panels[side];
            let selected: Vec<&str> = p.selected().map(|e| e.name.as_str()).collect();
            serde_json::json!({
                "path": p.path,
                "cursor": p.current().map(|e| e.name.as_str()),
                "items": p.entries.len(),
                "selected_count": selected.len(),
                "selected": selected.iter().take(20).collect::<Vec<_>>(),
            })
        };
        serde_json::json!({
            "left": panel(0),
            "right": panel(1),
            "active_panel": side_name(self.active),
            "focus": format!("{:?}", self.focus).to_lowercase(),
            "panels_visible": self.panels_visible(),
            "running_command": self.running.as_ref().map(|r| {
                let rec = self.commands.iter().find(|c| c.id == r.id);
                serde_json::json!({ "cmd_id": r.id, "text": rec.map(|c| c.text.as_str()) })
            }),
            "journal_last_seq": self.journal.last_seq(),
            "observe_mode": if self.live { "live" } else { "on-demand" },
        })
    }

    /// Text added to the user's prompt by the `UserPromptSubmit` hook.
    fn prompt_context(&mut self) -> String {
        let new = self.journal.since(self.agent_seen_seq);
        let (Some(first), Some(last)) = (new.first(), new.last()) else {
            return String::new();
        };
        let (first, last) = (first.seq, last.seq);
        let count = new.len();
        let since = self.agent_seen_seq;
        self.agent_seen_seq = last;
        if !self.live {
            return format!(
                "[afar: {count} new journal {} #{first}–#{last}; call afar_journal(since={since}) if relevant]",
                if count == 1 { "entry" } else { "entries" }
            );
        }
        let new = self.journal.since(since);
        let shown = &new[new.len().saturating_sub(LIVE_MAX_ENTRIES)..];
        let mut out = format!(
            "[afar journal #{first}–#{last} | left: {} | right: {} | active: {}]\n",
            self.panels[0].path.display(),
            self.panels[1].path.display(),
            side_name(self.active)
        );
        if shown.len() < new.len() {
            out.push_str(&format!(
                "… {} earlier entries: afar_journal(since={since})\n",
                new.len() - shown.len()
            ));
        }
        out.push_str(&format_entries(shown));
        // Failed commands: the end of their output, so that "why did it
        // fail?" needs no extra tool call.
        let failed: Vec<u64> = shown
            .iter()
            .filter_map(|e| match e.event {
                Event::CommandFinished {
                    cmd_id,
                    exit_code: Some(code),
                    ..
                } if code != 0 => Some(cmd_id),
                _ => None,
            })
            .collect();
        for id in failed {
            if let Ok(lines) = self.command_output(id) {
                let start = lines.len().saturating_sub(LIVE_ERROR_TAIL);
                out.push_str(&format!(
                    "--- last {} lines of cmd-{id} output ---\n",
                    lines.len() - start
                ));
                for l in &lines[start..] {
                    out.push_str(l);
                    out.push('\n');
                }
            }
        }
        out
    }

    // -------------------------------------------------------------- mouse

    fn on_mouse(&mut self, ev: MouseEvent) {
        let Some(l) = self.last_layout.clone() else {
            return;
        };
        let pos = Position::new(ev.column, ev.row);
        let pressed = matches!(ev.kind, MouseEventKind::Down(_));

        // Dialogs take the mouse, except over the agent pane.
        if self.has_overlay() && !l.agent_frame.contains(pos) {
            self.overlay_mouse(&ev);
            return;
        }

        // Dragging a boundary between windows.
        if let Some((id, offset)) = self.drag {
            match ev.kind {
                MouseEventKind::Drag(MouseButton::Left) => {
                    if let Some(sp) = l.arrangement.splitter(id) {
                        let first = sp.first_for(pos, offset);
                        self.wm.set_first(id, i32::from(first), sp.total());
                    }
                    return;
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    self.drag = None;
                    return;
                }
                _ => self.drag = None,
            }
        }
        if ev.kind == MouseEventKind::Down(MouseButton::Left)
            && let Some(grab) = l.arrangement.grab(pos)
        {
            self.drag = Some(grab);
            return;
        }

        if l.agent_frame.contains(pos) {
            if pressed {
                self.focus = Focus::Agent;
            }
            let Some(agent) = &self.agent else { return };
            let mut parser = agent.parser();
            let screen = parser.screen_mut();
            let mode = screen.mouse_protocol_mode();
            if mode != vt100::MouseProtocolMode::None {
                if l.agent.contains(pos) {
                    let encoding = screen.mouse_protocol_encoding();
                    let (col, row) = (ev.column - l.agent.x, ev.row - l.agent.y);
                    drop(parser);
                    if let Some(bytes) = keys::encode_mouse(&ev, col, row, mode, encoding) {
                        let _ = agent.write(&bytes);
                    }
                }
                return;
            }
            // The program does not want the mouse: the wheel scrolls back.
            let offset = screen.scrollback();
            match ev.kind {
                MouseEventKind::ScrollUp => screen.set_scrollback(offset + 3),
                MouseEventKind::ScrollDown => screen.set_scrollback(offset.saturating_sub(3)),
                _ => {}
            }
            return;
        }

        if l.keybar.contains(pos) {
            // Clicking a key label presses that key, like in Far.
            if ev.kind == MouseEventKind::Down(MouseButton::Left) && self.focus == Focus::Panels {
                let cell = (l.keybar.width / 12).max(4);
                let n = (ev.column - l.keybar.x) / cell + 1;
                if n <= 12 {
                    self.panels_key(KeyEvent::new(KeyCode::F(n as u8), KeyModifiers::NONE));
                }
            }
            return;
        }

        if l.cmdline.contains(pos) {
            if pressed {
                self.focus = Focus::Panels;
            }
            return;
        }

        if !l.top.contains(pos) {
            return;
        }
        if !self.panels_visible() || l.top.height < 5 {
            // User screen: the running command gets the mouse if it wants it.
            let Some(run) = &self.running else { return };
            if pressed {
                self.focus = Focus::Command;
            }
            if self.focus == Focus::Command && self.last_live.contains(pos) {
                let parser = run.pty.parser();
                let screen = parser.screen();
                let (mode, encoding) = (
                    screen.mouse_protocol_mode(),
                    screen.mouse_protocol_encoding(),
                );
                drop(parser);
                let (col, row) = (ev.column - self.last_live.x, ev.row - self.last_live.y);
                if let Some(bytes) = keys::encode_mouse(&ev, col, row, mode, encoding) {
                    let _ = run.pty.write(&bytes);
                }
            }
            return;
        }

        let Some(side) = (0..2).find(|&s| l.panels[s].contains(pos)) else {
            return;
        };
        let item = self.panels[side].item_at(ev.column, ev.row);
        match ev.kind {
            MouseEventKind::Down(button) => {
                self.focus = Focus::Panels;
                self.active = side;
                let Some(i) = item else { return };
                self.panels[side].cursor = i;
                match button {
                    MouseButton::Left => {
                        let double = self.last_click.is_some_and(|(t, s, it)| {
                            s == side && it == i && t.elapsed() < Duration::from_millis(400)
                        });
                        if double {
                            self.last_click = None;
                            self.enter();
                        } else {
                            self.last_click = Some((Instant::now(), side, i));
                        }
                    }
                    MouseButton::Right => {
                        self.panels[side].toggle_selection();
                        self.mark_selection_changed();
                    }
                    MouseButton::Middle => {}
                }
            }
            MouseEventKind::ScrollUp => self.panels[side].move_cursor(-3),
            MouseEventKind::ScrollDown => self.panels[side].move_cursor(3),
            _ => {}
        }
    }

    // ------------------------------------------------------------- layout

    fn layout(&self, area: Rect) -> Layout {
        let keybar = Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1);
        let cmdline = Rect::new(area.x, keybar.y.saturating_sub(1), area.width, 1);
        let desktop = Rect::new(area.x, area.y, area.width, cmdline.y.saturating_sub(area.y));
        let arrangement = self.wm.arrange(desktop);
        let rect = |w| arrangement.rect(w).unwrap_or_default();
        let agent_frame = rect(WinId::Agent);
        let agent = Rect::new(
            agent_frame.x + 1,
            agent_frame.y + 1,
            agent_frame.width.saturating_sub(2),
            agent_frame.height.saturating_sub(2),
        );
        Layout {
            top: arrangement.screen_area,
            panels: [rect(WinId::Panel(0)), rect(WinId::Panel(1))],
            agent_frame,
            agent,
            cmdline,
            keybar,
            arrangement,
        }
    }

    fn last_top_size(&self) -> (u16, u16) {
        let top = self.last_layout.as_ref().map(|l| l.top).unwrap_or_default();
        (top.height.max(2), top.width.max(20))
    }

    fn last_agent_size(&self) -> (u16, u16) {
        let agent = self
            .last_layout
            .as_ref()
            .map(|l| l.agent)
            .unwrap_or_default();
        (agent.height.max(2), agent.width.max(20))
    }

    fn last_page(&self) -> usize {
        let h = self
            .last_layout
            .as_ref()
            .map_or(0, |l| l.panels[self.active].height);
        h.saturating_sub(6).max(1) as usize
    }

    // --------------------------------------------------------------- draw

    fn draw(&mut self, area: Rect, buf: &mut Buffer) -> Option<Position> {
        let l = self.layout(area);
        self.last_layout = Some(l.clone());
        let mut cursor = None;

        if let Some(agent) = &self.agent {
            let _ = agent.resize(l.agent.height, l.agent.width);
        }
        if let Some(run) = &self.running {
            let _ = run.pty.resize(l.top.height, l.top.width);
        }

        // Top area: panels or the user screen.
        if self.panels_visible() && l.top.height >= 5 {
            let panels_active = self.focus == Focus::Panels;
            self.panels[0].draw(l.panels[0], buf, panels_active && self.active == 0);
            self.panels[1].draw(l.panels[1], buf, panels_active && self.active == 1);
        } else {
            let c = self.draw_user_screen(l.top, buf);
            if self.focus == Focus::Command {
                cursor = c;
            }
        }

        // Agent pane: a Far-style frame on the panel's blue background.
        let agent_focused = self.focus == Focus::Agent;
        let status = match &self.agent {
            None => tr!("agent-not-started"),
            Some(a) if a.has_exited() => tr!(
                "agent-exited",
                code = a.exit_code().map_or(-1, |c| c as i64)
            ),
            Some(_) => tr!("agent-running"),
        };
        let frame = l.agent_frame;
        crate::panel::draw_frame(buf, frame, theme::PANEL_BOX);
        let title_style = if agent_focused {
            theme::PANEL_TITLE_SELECTED
        } else {
            theme::PANEL_TITLE
        };
        let title = format!(" {} ", tr!("agent-title", status = status));
        crate::panel::put_title(buf, frame, frame.y, &title, title_style);
        let unseen = self.journal.last_seq().saturating_sub(self.agent_seen_seq);
        let mode = if self.live {
            tr!("observe-live")
        } else {
            tr!("observe-on-demand")
        };
        let scrolled = self
            .agent
            .as_ref()
            .map_or(0, |a| a.parser().screen().scrollback());
        let scroll_note = if scrolled > 0 {
            format!("↑{scrolled} · ")
        } else {
            String::new()
        };
        let hint = format!(
            " {} ",
            tr!(
                "agent-footer",
                scroll = scroll_note,
                mode = mode,
                unseen = unseen
            )
        );
        let hint_w = hint.chars().count() as u16;
        if frame.width > hint_w + 4 {
            let y = frame.bottom() - 1;
            put(
                buf,
                frame.right() - hint_w - 2,
                y,
                hint_w,
                &hint,
                theme::PANEL_BOX,
            );
        }
        // Only the frame is blue: inside, the program keeps the terminal's
        // own colors.
        buf.set_style(l.agent, Style::reset());
        if let Some(agent) = &self.agent {
            let c = termview::draw_rows(
                &crate::term::view(&agent.parser()),
                0,
                l.agent,
                buf,
                Style::reset(),
            );
            if agent_focused {
                cursor = c;
            }
        }

        // Command line.
        let prompt = format!("{}>", self.panels[self.active].path.display());
        let line = format!("{prompt}{}", self.cmdline);
        put(
            buf,
            l.cmdline.x,
            l.cmdline.y,
            l.cmdline.width,
            &line,
            theme::COMMAND_LINE,
        );
        if self.focus == Focus::Panels {
            let x = (prompt.chars().count() + self.cmd_cursor) as u16;
            if x < l.cmdline.width {
                cursor = Some(Position::new(l.cmdline.x + x, l.cmdline.y));
            }
        }
        if let Some((msg, _)) = &self.message {
            let text = format!(" {msg} ");
            let w = (text.chars().count() as u16).min(l.cmdline.width);
            put(
                buf,
                l.cmdline.right() - w,
                l.cmdline.y,
                w,
                &text,
                theme::MESSAGE,
            );
        }

        self.draw_keybar(l.keybar, buf);

        // Overlay: dialogs and progress windows over everything but the
        // bottom bars.
        if self.has_overlay() {
            let over = Rect::new(
                area.x,
                area.y,
                area.width,
                l.cmdline.y.saturating_sub(area.y),
            );
            let c = self.draw_overlays(over, buf);
            if self.focus != Focus::Agent {
                cursor = c;
            }
        }
        cursor
    }

    /// History of finished commands plus the live screen of the running one.
    fn draw_user_screen(&mut self, area: Rect, buf: &mut Buffer) -> Option<Position> {
        buf.set_style(area, theme::COMMAND_LINE);
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                buf[(x, y)].set_symbol(" ");
            }
        }
        let mut live_rows = 0u16;
        let mut cursor = None;
        let text: Vec<&String> = match &self.running {
            Some(run) => {
                let parser = run.pty.parser();
                let screen = parser.screen();
                live_rows = if screen.alternate_screen() {
                    area.height
                } else {
                    (termview::screen_lines(screen).len() as u16)
                        .max(screen.cursor_position().0 + 1)
                        .min(area.height)
                };
                let live = Rect::new(area.x, area.bottom() - live_rows, area.width, live_rows);
                self.last_live = live;
                cursor =
                    termview::draw_rows(&crate::term::view(&parser), 0, live, buf, Style::reset());
                self.history.iter().chain(run.captured.iter()).collect()
            }
            None => self.history.iter().collect(),
        };
        let rows = (area.height - live_rows) as usize;
        let start = text.len().saturating_sub(rows);
        let first_y = area.bottom() - live_rows - (text.len() - start) as u16;
        for (i, line) in text[start..].iter().enumerate() {
            buf.set_stringn(
                area.x,
                first_y + i as u16,
                line.as_str(),
                area.width as usize,
                theme::COMMAND_LINE,
            );
        }
        cursor
    }

    /// Far's key bar (keybar.cpp): number, label of at least 6 cells, a
    /// space; at 98 columns and more the labels widen, below that the bar
    /// is cut off at the right edge.
    fn draw_keybar(&self, area: Rect, buf: &mut Buffer) {
        const MIN_LABEL: u16 = 6;
        buf.set_style(area, theme::KEYBAR_TEXT);
        for x in area.left()..area.right() {
            buf[(x, area.y)].set_symbol(" ");
        }
        if self.focus != Focus::Panels {
            let text = match self.focus {
                Focus::Agent => tr!("hint-agent"),
                _ => tr!("hint-command"),
            };
            put(
                buf,
                area.x,
                area.y,
                area.width,
                &format!(" {text}"),
                theme::KEYBAR_TEXT,
            );
            return;
        }
        let width = area.width;
        let mut pos = 0u16;
        for i in 0..12u16 {
            if pos >= width {
                break;
            }
            let num = (i + 1).to_string();
            let num_w = num.len() as u16;
            let gap = u16::from(i < 11);
            let min_end = pos + num_w + MIN_LABEL + gap;
            let end = if width >= 98 {
                min_end.max((i + 1) * width / 12)
            } else {
                min_end
            }
            .min(width);
            let x = area.x + pos;
            put(
                buf,
                x,
                area.y,
                num_w.min(width - pos),
                &num,
                theme::KEYBAR_NUM,
            );
            let label_x = pos + num_w;
            if label_x < end {
                let label_w = end.saturating_sub(label_x + gap).min(width - label_x);
                let label = crate::i18n::plain(&tr!(&format!("MF{}", i + 1)));
                put(
                    buf,
                    area.x + label_x,
                    area.y,
                    label_w,
                    &label,
                    theme::KEYBAR_TEXT,
                );
                if gap == 1 && end - 1 < width {
                    buf[(area.x + end - 1, area.y)]
                        .set_symbol(" ")
                        .set_style(theme::KEYBAR_NUM);
                }
            }
            pos = end;
        }
    }
}

/// A random UUID (version 4) for a Claude Code session id.
fn new_uuid() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut bytes = [0u8; 16];
    for (i, chunk) in bytes.chunks_mut(8).enumerate() {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_usize(i);
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        );
        chunk.copy_from_slice(&h.finish().to_le_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn side_name(side: usize) -> &'static str {
    if side == 0 { "left" } else { "right" }
}

fn quote(name: &str) -> String {
    if name.contains([' ', '&', '(', ')', '^', ',', ';', '=']) {
        format!("\"{name}\"")
    } else {
        name.to_string()
    }
}

fn char_to_byte(s: &str, chars: usize) -> usize {
    s.char_indices().nth(chars).map_or(s.len(), |(i, _)| i)
}
