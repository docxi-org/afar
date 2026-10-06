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
use ratatui::DefaultTerminal;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;

use crate::journal::{Actor, Event, Journal, format_entries};
use crate::mcp::{McpMsg, Reply, Request};
use crate::panel::{FilePanel, put};
use crate::term::{PtySession, SpawnOptions};
use crate::{keys, termview, theme};

pub enum AppMsg {
    Input(TermEvent),
    AgentOutput,
    CommandOutput,
    Mcp(McpMsg),
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

#[derive(Clone, Copy)]
struct Layout {
    top: Rect,
    /// File panels (when shown) inside `top`.
    panels: [Rect; 2],
    /// The agent pane with its frame, and the program area inside it.
    agent_frame: Rect,
    agent: Rect,
    cmdline: Rect,
    keybar: Rect,
}

pub struct App {
    panels: [FilePanel; 2],
    active: usize,
    focus: Focus,
    show_panels: bool,
    cmdline: String,
    /// Cursor in `cmdline`, in chars.
    cmd_cursor: usize,
    agent: Option<PtySession>,
    agent_height: u16,
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
}

impl App {
    pub fn new(tx: Sender<AppMsg>, session_dir: PathBuf, link: AgentLink) -> Self {
        let cwd = std::env::current_dir()
            .map(crate::panel::strip_verbatim)
            .unwrap_or_else(|_| PathBuf::from("."));
        let mut journal = Journal::open(session_dir);
        journal.push(
            Actor::System,
            Event::AppStarted {
                left: cwd.clone(),
                right: cwd.clone(),
            },
        );
        Self {
            panels: [FilePanel::new(cwd.clone()), FilePanel::new(cwd)],
            active: 0,
            focus: Focus::Panels,
            show_panels: true,
            cmdline: String::new(),
            cmd_cursor: 0,
            agent: None,
            agent_height: 0,
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
        }
    }

    pub fn run(mut self, terminal: &mut DefaultTerminal, rx: Receiver<AppMsg>) -> Result<()> {
        let size = terminal.size()?;
        self.agent_height = (size.height * 35 / 100).max(8);
        let (rows, cols) = self.last_agent_size();
        self.start_agent(cols, rows);
        loop {
            terminal.draw(|frame| {
                let area = frame.area();
                let cursor = self.draw(area, frame.buffer_mut());
                if let Some(pos) = cursor {
                    frame.set_cursor_position(pos);
                }
            })?;
            if self.quit {
                return Ok(());
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
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
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
            AppMsg::Input(_) | AppMsg::AgentOutput => {}
            AppMsg::CommandOutput => self.on_command_output(),
            AppMsg::Mcp(msg) => {
                let reply = self.on_mcp(msg.request);
                let _ = msg.reply.send(reply);
            }
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
                self.say(format!("Не удалось подготовить конфигурацию агента: {e:#}"));
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
            Err(e) => self.say(format!("Не удалось запустить claude: {e:#}")),
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
        if let Some(arg) = lower.strip_prefix("afar:") {
            match arg.trim() {
                "live" => self.live = !self.live,
                "live on" => self.live = true,
                "live off" => self.live = false,
                _ => {}
            }
            self.say(if self.live {
                "Агент видит действия сразу (live). afar:live — переключить"
            } else {
                "Агент читает журнал по запросу. afar:live — переключить"
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
            self.say("Команда уже выполняется — дождитесь завершения");
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
                    auto_switched: self.show_panels,
                });
                self.show_panels = false;
                self.focus = Focus::Command;
            }
            Err(e) => {
                self.push_history([format!("Ошибка запуска: {e:#}")]);
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
            self.show_panels = true;
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
                self.show_panels = true;
            }
            return;
        }
        if self.focus != Focus::Panels {
            self.quit_armed = None;
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
                    self.say("Агент или команда ещё работают. F10 ещё раз — выход");
                }
            }
            KeyCode::Char('o') if ctrl => {
                if self.running.is_some() && !self.show_panels {
                    self.focus = Focus::Command;
                } else {
                    self.show_panels = !self.show_panels;
                    if !self.show_panels && self.running.is_some() {
                        self.focus = Focus::Command;
                    }
                }
            }
            KeyCode::Up if ctrl => self.resize_agent(1),
            KeyCode::Down if ctrl => self.resize_agent(-1),
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
                    if self.show_panels {
                        self.enter();
                    }
                } else {
                    let text = self.cmdline.clone();
                    self.execute(text);
                }
            }
            _ if !self.show_panels => {
                self.cmdline_key(&key);
            }
            KeyCode::Tab => self.active = 1 - a,
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
                self.say(format!("F{n} — ещё не реализовано в прототипе"));
            }
            _ => {
                self.cmdline_key(&key);
            }
        }
    }

    fn resize_agent(&mut self, delta: i32) {
        self.agent_height = (self.agent_height as i32 + delta).max(5) as u16;
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
                self.show_panels = true;
                let mut note = String::new();
                if let Some(name) = cursor {
                    if !self.panels[side].set_cursor_by_name(&name) {
                        note = format!(" ({name} not found there)");
                    }
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
                self.show_panels = true;
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
            "panels_visible": self.show_panels,
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
                "[afar: {count} new journal entries #{first}–#{last}; call afar_journal(since={since}) if relevant]"
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
        let Some(l) = self.last_layout else { return };
        let pos = Position::new(ev.column, ev.row);
        let pressed = matches!(ev.kind, MouseEventKind::Down(_));

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
        if !self.show_panels || l.top.height < 5 {
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

    fn layout(&mut self, area: Rect) -> Layout {
        let h = area.height;
        let max_agent = h.saturating_sub(10).max(5);
        self.agent_height = self.agent_height.clamp(5.min(max_agent), max_agent);
        let keybar = Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1);
        let cmdline = Rect::new(area.x, keybar.y.saturating_sub(1), area.width, 1);
        let agent_top = cmdline.y.saturating_sub(self.agent_height);
        let agent_frame = Rect::new(area.x, agent_top, area.width, self.agent_height);
        let agent = Rect::new(
            area.x + 1,
            agent_top + 1,
            area.width.saturating_sub(2),
            self.agent_height.saturating_sub(2),
        );
        let top = Rect::new(area.x, area.y, area.width, agent_top.saturating_sub(area.y));
        let half = top.width / 2;
        let panels = [
            Rect::new(top.x, top.y, half, top.height),
            Rect::new(top.x + half, top.y, top.width - half, top.height),
        ];
        Layout {
            top,
            panels,
            agent_frame,
            agent,
            cmdline,
            keybar,
        }
    }

    fn last_top_size(&self) -> (u16, u16) {
        let (w, h) = crossterm::terminal::size().unwrap_or((80, 25));
        let top = h.saturating_sub(2 + self.agent_height);
        (top.max(2), w.max(20))
    }

    fn last_agent_size(&self) -> (u16, u16) {
        let (w, _) = crossterm::terminal::size().unwrap_or((80, 25));
        (
            self.agent_height.saturating_sub(2).max(2),
            w.saturating_sub(2).max(20),
        )
    }

    fn last_page(&self) -> usize {
        let (rows, _) = self.last_top_size();
        rows.saturating_sub(6).max(1) as usize
    }

    // --------------------------------------------------------------- draw

    fn draw(&mut self, area: Rect, buf: &mut Buffer) -> Option<Position> {
        let l = self.layout(area);
        self.last_layout = Some(l);
        let mut cursor = None;

        if let Some(agent) = &self.agent {
            let _ = agent.resize(l.agent.height, l.agent.width);
        }
        if let Some(run) = &self.running {
            let _ = run.pty.resize(l.top.height, l.top.width);
        }

        // Top area: panels or the user screen.
        if self.show_panels && l.top.height >= 5 {
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
            None => "не запущен — Enter: запуск".to_string(),
            Some(a) if a.has_exited() => format!(
                "завершён (код {}) — Enter: перезапуск",
                a.exit_code().map_or(-1, |c| c as i64)
            ),
            Some(_) => "работает".to_string(),
        };
        let frame = l.agent_frame;
        crate::panel::draw_frame(buf, frame, theme::PANEL);
        let title_style = if agent_focused {
            theme::TITLE_ACTIVE
        } else {
            theme::PANEL
        };
        let title = format!(" Агент · claude · {status} ");
        crate::panel::put_title(buf, frame, frame.y, &title, title_style);
        let unseen = self.journal.last_seq().saturating_sub(self.agent_seen_seq);
        let mode = if self.live {
            "● live"
        } else {
            "○ по запросу"
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
        let hint = format!(" {scroll_note}{mode} · +{unseen} соб. · Ctrl+Space ");
        let hint_w = hint.chars().count() as u16;
        if frame.width > hint_w + 4 {
            let y = frame.bottom() - 1;
            put(
                buf,
                frame.right() - hint_w - 2,
                y,
                hint_w,
                &hint,
                theme::PANEL,
            );
        }
        // Only the frame is blue: inside, the program keeps the terminal's
        // own colors.
        buf.set_style(l.agent, Style::reset());
        if let Some(agent) = &self.agent {
            let c = termview::draw_rows(agent.parser().screen(), 0, l.agent, buf, Style::reset());
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
            theme::CMDLINE,
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
        cursor
    }

    /// History of finished commands plus the live screen of the running one.
    fn draw_user_screen(&mut self, area: Rect, buf: &mut Buffer) -> Option<Position> {
        buf.set_style(area, theme::CMDLINE);
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
                cursor = termview::draw_rows(screen, 0, live, buf, Style::reset());
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
                theme::CMDLINE,
            );
        }
        cursor
    }

    fn draw_keybar(&self, area: Rect, buf: &mut Buffer) {
        let labels: [&str; 12] = match self.focus {
            Focus::Panels => [
                "Помощь",
                "ПользМ",
                "Просм",
                "Редакт",
                "Копир",
                "Перен",
                "Папка",
                "Удален",
                "КонфМн",
                "Выход",
                "Модули",
                "Экраны",
            ],
            _ => ["", "", "", "", "", "", "", "", "", "", "", ""],
        };
        buf.set_style(area, theme::KEYBAR_NUM);
        if self.focus != Focus::Panels {
            let text = match self.focus {
                Focus::Agent => " Ввод идёт агенту · Ctrl+Space — к панелям",
                _ => " Ввод идёт команде · Ctrl+Space — к агенту · Ctrl+O — экран команды",
            };
            put(buf, area.x, area.y, area.width, text, theme::KEYBAR_LABEL);
            return;
        }
        let cell = (area.width / 12).max(4);
        for (i, label) in labels.iter().enumerate() {
            let x = area.x + i as u16 * cell;
            if x >= area.right() {
                break;
            }
            let num = (i + 1).to_string();
            let w = cell.min(area.right() - x);
            put(buf, x, area.y, num.len() as u16, &num, theme::KEYBAR_NUM);
            let lw = w.saturating_sub(num.len() as u16 + 1);
            put(
                buf,
                x + num.len() as u16,
                area.y,
                lw,
                label,
                theme::KEYBAR_LABEL,
            );
        }
    }
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
