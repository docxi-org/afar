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
use crate::keymap::Chord;
use crate::mcp::{McpMsg, Reply, Request};
use crate::ops::DeleteMode;
use crate::ops::{OpId, OpMsg};
use crate::panel::{FilePanel, put};
use crate::term::{PtySession, SpawnOptions};
use crate::wm::{self, Arrangement, Extent, ScreenId, SplitId, WinId, Wm};

mod agent;
mod agentmenu;
mod attributes;
mod autocomplete;
mod cmdline;
mod commands;
mod editagent;
mod testtools;
pub use testtools::PNG_MARK;
mod editcp;
mod editors;
mod editsearch;
mod editturn;
mod farimport;
mod fileops;
mod findfiles;
mod foldertree;
mod fswatch;
mod historymenu;
mod ide;
mod infopanel;
mod links;
mod mainmenu;
mod outer;
mod panelcmds;
mod paste;
mod policy;
mod quicksearch;
mod quickview;
mod settings;
mod viewagent;
pub use viewagent::MarkSpec;
mod viewers;
use crate::{keys, termview, theme, tr};
use fileops::{Overlay, RunningOp};

pub enum AppMsg {
    Input(TermEvent),
    AgentOutput,
    CommandOutput,
    Mcp(McpMsg),
    Op(OpMsg),
    Dev(DevMsg),
    /// A change in a folder shown in a panel.
    Fs(crate::watch::FsEvent),
    /// A viewer's search finished.
    ViewerFound(viewers::SearchDone),
    /// The agent through the IDE protocol.
    Ide(crate::ide::IdeMsg),
    /// Folder sizes counted in the background (F3 on a folder): the
    /// panel's folder and (name, size) pairs.
    DirSizes(PathBuf, Vec<(String, u64)>),
    /// A part of a drive's folder tree (Alt+F10): the root, the folders,
    /// whether it is all.
    TreeRead(PathBuf, Vec<foldertree::Node>, bool),
    /// Ctrl+A's "Set" finished: how many, what failed.
    AttributesDone(usize, Vec<String>),
    /// What the file search (Alt+F7) found or where it is.
    Find(crate::find::Event),
    /// The quick view's count of a folder (partial, then final).
    QuickViewStats(PathBuf, quickview::DirStats),
}

/// The state to start from.
pub enum Restore {
    /// A development rebuild: everything as it was, the agent resumed.
    Restart(DevState),
    /// The last run (`state.json`): the active panel stays in the current
    /// folder; layout, modes and the other panel come back.
    LastRun(DevState),
}

/// How the main loop ended.
pub enum Exit {
    Quit,
    /// Development mode: start the new build with this state.
    Restart(Box<DevState>),
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

/// Ctrl+O presses closer than this go round the hiding states.
const HIDING_CYCLE: Duration = Duration::from_secs(1);

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
    // An IDE of the outer terminal (VS Code): afar is the agent's IDE.
    "CLAUDE_CODE_SSE_PORT",
];

const HISTORY_MAX: usize = 10_000;
/// Limits of the journal delta added to a prompt in live mode.
const LIVE_MAX_ENTRIES: usize = 60;
const LIVE_ERROR_TAIL: usize = 30;

const SYSTEM_PROMPT: &str = "You are running inside afar, a two-panel file manager in the style of \
Far Manager; the user sees its two file panels above your pane and works in them while talking to you. \
The afar_* MCP tools show you what the user did (afar_state, afar_journal, afar_commands, \
afar_command_output) and let you show things in the panels (afar_navigate, afar_select): when you \
refer to a file or directory, show it with afar_navigate. To point at a place inside a file, open it \
in afar's viewer with afar_view (a line or a pattern) and mark lines with afar_highlight (a label; \
info / warning / error); afar_viewer_state tells which file and lines the user looks at and what they \
selected. A file open in afar's editor (afar_edit, afar_editor_state) is edited in its buffer, not on the \
disk: read it with afar_buffer_read, change it with afar_buffer_edit / afar_buffer_insert; the user saves. \
An event starting with [afar editor #N … | the user passes you the turn] comes from the editor: the user's \
instruction (if any), the cursor, the selection and the changes since the version you last saw; mode auto — do \
what it asks in that buffer (\"after this line\" means the line the instruction was typed at — the event says \
which, 0 meaning before the first line: afar_buffer_insert after_line 0); mode answer — \
do not change the text: answer in your pane, or mark the lines you speak about with afar_highlight (its label \
shows as a note at the line's end in the editor). Blocks starting with [afar journal] in a \
user message are recent user actions added automatically. In the journal, `fs` entries are file changes afar saw in the panels' folders; afar cannot tell who wrote them: \
`(while your Bash ran)` means they happened during your shell command (Bash or PowerShell) and most likely are its own \
writes, `(while [cmd-N] ran)` — during the user's command. `tool` entries are your own tool uses; files you change \
are highlighted in the panels.";

/// The agent's edit waiting while its difference is viewed: the viewer,
/// the file, the new text, the tab, the answer.
type ParkedDiff = (
    u32,
    PathBuf,
    String,
    String,
    Option<tokio::sync::oneshot::Sender<crate::ide::DiffAnswer>>,
);

/// Record of a command run from the command line.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CmdRecord {
    id: u64,
    text: String,
    cwd: PathBuf,
    exit_code: Option<u32>,
    duration_ms: u64,
    lines: usize,
    /// afar's own (a failed development build), not the user's.
    #[serde(default)]
    by_afar: bool,
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
    /// Output lines that already scrolled off the command's screen; the
    /// last one is still being wrapped when `continues`.
    captured: Vec<termview::Line>,
    continues: bool,
    /// Panels were hidden automatically and come back when it ends.
    auto_switched: bool,
}

/// Geometry of a frame: the window manager's arrangement plus the fixed
/// bars at the bottom (command line, key bar).
#[derive(Clone, Default)]
struct Layout {
    /// The screen slot: panels or the user screen.
    top: Rect,
    /// Where the user screen is shown: the screen slot, and the agent
    /// pane's place too when Ctrl+O has hidden it.
    user: Rect,
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
    /// The agent's session (docs/13-agent-sessions.md).
    agent: agent::AgentSession,
    /// The "user screen": text of finished commands.
    history: Vec<termview::Line>,
    running: Option<RunningCommand>,
    next_cmd_id: u64,
    journal: Journal,
    commands: Vec<CmdRecord>,
    link: AgentLink,
    selection_changed: [Option<Instant>; 2],
    message: Option<(String, Instant)>,
    quit_armed: Option<Instant>,
    quit: bool,
    tx: Sender<AppMsg>,
    /// Folder watching, the agent's tools, highlighting of its changes.
    fs: fswatch::FsState,
    /// Geometry of the last frame, for mouse hit tests.
    last_layout: Option<Layout>,
    /// Where the running command's live screen was drawn.
    last_live: Rect,
    /// A Ctrl+click opened a link: its release is not passed on.
    link_pressed: bool,
    /// The link the mouse is over and where: its address is shown next to
    /// the mouse.
    hovered_link: Option<(String, u16, u16)>,
    /// Where the user screen last drew the kept lines, and the first one's
    /// index in history + captured output.
    user_rows: (Rect, Vec<termview::Line>),
    /// The user screen scrolled back by this many lines (0: following the
    /// output), the lines it had then, and its height in the last frame.
    user_scroll: usize,
    user_total: usize,
    user_height: u16,
    /// What afar last told the outer terminal (title, progress).
    outer: outer::Outer,
    /// Ctrl+Q: the quick view in place of a panel.
    quick_view: Option<quickview::QuickView>,
    /// The agent's edit waiting while its difference is viewed: the
    /// viewer, the file, the new text, the tab, the answer.
    diff_parked: Option<ParkedDiff>,
    /// "Follow the agent": its files and edits come to the screen.
    follow_agent: bool,
    /// The viewer's selection: the file, the lines, since when, whether it
    /// went to the journal.
    view_selection: Option<(PathBuf, (u64, u64), Instant, bool)>,
    /// Ctrl+L: the information panel in place of a panel.
    info_panel: Option<infopanel::InfoPanel>,
    /// Alt+F10: the drives' folder trees read in this session.
    trees: std::collections::HashMap<PathBuf, foldertree::Tree>,
    /// Alt+F7: the dialog's options; the results window while a viewer
    /// opened from it is shown; the last click in it.
    find_options: findfiles::FindOptions,
    find_parked: Option<Box<findfiles::FindView>>,
    find_last_click: Option<Instant>,
    /// The terminal window has the focus (focus events, `?1004`).
    window_focused: bool,
    /// Last left click (time, panel, item) to detect double clicks.
    last_click: Option<(Instant, usize, usize)>,
    /// F-key pressed with the mouse on the key bar (acts on release).
    keybar_pressed: Option<u8>,
    /// The history database: dialogs' fields, commands (docs/15).
    store: crate::history::History,
    /// The open autocompletion list; what completion keeps between keys
    /// (programs on PATH, folders' names).
    completion: Option<autocomplete::ActiveCompletion>,
    complete_cache: crate::complete::Cache,
    /// The command line's ghost suggestion: (the line, the rest).
    cmd_ghost: Option<(String, String)>,
    /// Commands run from the command line (Ctrl+E / Ctrl+X).
    cmd_history: cmdline::History,
    /// The last mask of Gray + / Gray - (Far's strPrevMask).
    select_mask: String,
    /// Alt+letter search in a panel.
    quick_search: Option<quicksearch::QuickSearch>,
    /// The last folder on each drive, for the drive menu.
    drive_paths: std::collections::HashMap<char, PathBuf>,
    /// Open viewers (F3), each on its own screen.
    viewers: Vec<crate::viewer::Viewer>,
    next_viewer_id: u32,
    /// Editor screens (F4), and the next one's id.
    editors: Vec<crate::editor::Editor>,
    next_editor_id: u32,
    /// The editors' status line (Ctrl+Shift+B) and key bar (Ctrl+B).
    editor_status: bool,
    editor_keybar: bool,
    /// Where files were left in the editor.
    editor_places: editors::EditorPlaces,
    /// The test tools: the input being played, and the last frame (kept
    /// only while the tools are on).
    test_run: Option<testtools::TestRun>,
    last_frame: Option<(Buffer, Option<Position>)>,
    /// The cells the overlays covered in the last frame (the test tools).
    overlay_area: Option<Rect>,
    /// The clipboard of a test run (the user's is left alone).
    test_clipboard: Option<(String, bool)>,
    /// The last left click in an editor (double click: a word).
    editor_last_click: Option<(Instant, u16, u16)>,
    /// Wrapping and bars carried to the next viewer (Far's
    /// `KeepInitParameters`).
    viewer_defaults: crate::viewer::Defaults,
    /// Where files were left in the viewer.
    viewer_positions: crate::viewer::positions::Positions,
    /// Ctrl+O in a viewer: the user screen until a key.
    viewer_peek: bool,
    /// Ctrl+B in a viewer: its key bar.
    viewer_keybar: bool,
    /// The shown viewer last checked its file for changes.
    viewer_checked: Instant,
    /// A press on the agent pane's top frame that is also a boundary: a
    /// click there (released without moving) opens the pane's menu, a drag
    /// moves the boundary.
    agent_frame_click: Option<Position>,
    /// Modifier keys held now (the key bar shows their labels).
    held: KeyModifiers,
    /// The last Ctrl+O (quick presses go round the hiding states).
    hiding_pressed: Option<Instant>,
    /// The hiding a slow Ctrl+O returns to (see `cycle_hiding`).
    hiding_mode: u8,
    /// The last search of the viewers (Far shares it).
    viewer_query: crate::viewer::search::Query,
    /// The last editor search was a replace with this text (Shift+F7
    /// repeats it).
    editor_replace: Option<String>,
    /// A search running in the background.
    viewer_search: Option<viewers::RunningSearch>,
    /// The last input of Alt+F8 and its Hex box (Far keeps them).
    viewer_goto: (String, Option<bool>),
    /// Dialogs and progress windows over the layout, topmost last.
    overlays: Vec<Overlay>,
    ops: std::collections::HashMap<OpId, RunningOp>,
    next_op_id: OpId,
    dev: Option<DevStatus>,
    /// Restored after a dev restart: keep the layout, continue the agent's
    /// conversation.
    restored: bool,
    /// Window sizes came from a restart or the last run.
    layout_restored: bool,
    /// afar's data folder (`%LOCALAPPDATA%\afar`): sessions, history, state.
    data_dir: PathBuf,
    /// Settings (`config.toml`).
    config: crate::config::Config,
    /// Keys → commands: Far's, changed by `keymaps/far.toml`.
    keymap: crate::keymap::Keymap,
    exit: Option<Exit>,
}

impl App {
    pub fn new(
        tx: Sender<AppMsg>,
        session_dir: PathBuf,
        link: AgentLink,
        dev: bool,
        config: crate::config::Config,
        config_problem: Option<String>,
        restore: Option<Restore>,
    ) -> Self {
        let cwd = std::env::current_dir()
            .map(crate::panel::strip_verbatim)
            .unwrap_or_else(|_| PathBuf::from("."));
        // Sessions live in <data>/sessions/<time>; the history beside them.
        let data_dir = session_dir
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_default();
        // Histories: dialogs' fields, commands (docs/15).
        let (store, store_problem) =
            match crate::history::History::open(&data_dir.join("history.db")) {
                Ok(h) => (h, None),
                Err(e) => (crate::history::History::in_memory(), Some(e.to_string())),
            };
        let cmd_history =
            cmdline::History::load(&store, &data_dir.join("history").join("commands.txt"));
        let journal = Journal::open(session_dir);
        let viewer_positions = crate::viewer::positions::Positions::load(
            &data_dir.join("history").join("viewer.json"),
        );
        let (keymap, keymap_problems) = crate::keymap::Keymap::load(
            &crate::config::config_dir().join("keymaps").join("far.toml"),
        );
        let mut app = Self {
            panels: [FilePanel::new(cwd.clone()), FilePanel::new(cwd)],
            active: 0,
            focus: Focus::Panels,
            wm: {
                let mut wm = Wm::new();
                wm.set_agent_on_top(config.agent.position == crate::config::AgentPosition::Top);
                wm
            },
            drag: None,
            cmdline: String::new(),
            cmd_cursor: 0,
            agent: agent::AgentSession::new(config.agent.live),
            history: Vec::new(),
            running: None,
            next_cmd_id: 1,
            journal,
            commands: Vec::new(),
            link,
            selection_changed: [None, None],
            message: None,
            quit_armed: None,
            quit: false,
            fs: fswatch::FsState::new(tx.clone()),
            tx,
            last_layout: None,
            last_live: Rect::default(),
            link_pressed: false,
            hovered_link: None,
            user_rows: (Rect::default(), Vec::new()),
            user_scroll: 0,
            user_total: 0,
            user_height: 0,
            outer: Default::default(),
            quick_view: None,
            info_panel: None,
            follow_agent: false,
            diff_parked: None,
            view_selection: None,
            trees: Default::default(),
            find_options: Default::default(),
            find_parked: None,
            find_last_click: None,
            window_focused: true,
            last_click: None,
            keybar_pressed: None,
            select_mask: "*.*".into(),
            cmd_history,
            store,
            completion: None,
            complete_cache: Default::default(),
            cmd_ghost: None,
            quick_search: None,
            drive_paths: Default::default(),
            viewers: Vec::new(),
            next_viewer_id: 1,
            editors: Vec::new(),
            next_editor_id: 1,
            editor_status: true,
            editor_keybar: true,
            editor_places: editors::EditorPlaces::load(
                &data_dir.join("history").join("editor.json"),
            ),
            editor_last_click: None,
            test_run: None,
            last_frame: None,
            test_clipboard: None,
            overlay_area: None,
            viewer_defaults: crate::viewer::Defaults {
                scrollbar: config.viewer.scrollbar,
                ..Default::default()
            },
            viewer_positions,
            viewer_peek: false,
            viewer_keybar: true,
            viewer_checked: Instant::now(),
            agent_frame_click: None,
            held: KeyModifiers::NONE,
            hiding_pressed: None,
            hiding_mode: 1,
            viewer_query: Default::default(),
            editor_replace: None,
            viewer_search: None,
            viewer_goto: (String::new(), None),
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
            layout_restored: false,
            data_dir,
            config,
            keymap,
            exit: None,
        };
        match restore {
            Some(Restore::Restart(state)) => app.apply_state(state),
            Some(Restore::LastRun(state)) => app.apply_last_run(state),
            None => {}
        }
        for problem in keymap_problems {
            app.say(tr!("config-problem", problem = problem));
        }
        if let Some(problem) = config_problem {
            app.say(tr!("config-problem", problem = problem));
        }
        if let Some(problem) = store_problem {
            app.say(tr!("history-open-failed", error = problem));
        }
        app.set_ide(app.config.agent.ide);
        if app.config.agent.test_tools {
            app.say(tr!("test-tools-on"));
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
            self.panels[side].view = p.view;
            self.panels[side].sort = p.sort;
            self.panels[side].resort();
            if let Some(name) = p.cursor {
                self.panels[side].set_cursor_by_name(&name);
            }
        }
        self.active = state.active.min(1);
        self.agent.live = state.live;
        for (id, extent) in state.splits {
            self.wm.set_extent(SplitId(id), extent);
        }
        self.set_panels_visible(state.panels_visible);
        self.restore_viewers(state.viewers, state.viewer_shown);
        self.restore_editors(state.editors, state.editor_shown);
        self.wm.set_hidden(WinId::Agent(0), state.agent_hidden);
        self.agent.session_id = state.agent_session;
        self.agent.cwd = state.agent_cwd;
        self.agent.name = state.agent_name;
        self.agent.permission_mode = state.agent_permission_mode;
        // The journal goes on where it was: the resumed agent's numbers hold.
        if let Some(dir) = &state.journal_dir {
            self.journal.carry_over(dir);
        }
        self.agent.seen_seq = state.seen_seq;
        // The user screen and the commands with their output go on too.
        self.history = state.user_screen;
        self.commands = state.commands;
        self.next_op_id = self.next_op_id.max(state.next_op_id);
        self.next_cmd_id = state
            .next_cmd_id
            .max(self.commands.iter().map(|c| c.id + 1).max().unwrap_or(1));
        self.restored = true;
        self.layout_restored = true;
    }

    /// The last run: the active panel stays in the current folder (where
    /// afar was started), the other one, the modes and the layout return.
    fn apply_last_run(&mut self, state: DevState) {
        let active = state.active.min(1);
        for (side, p) in state.panels.into_iter().enumerate().take(2) {
            if side != active && p.path.is_dir() {
                self.panels[side] = FilePanel::new(p.path);
                if let Some(name) = &p.cursor {
                    self.panels[side].set_cursor_by_name(name);
                }
            }
            self.panels[side].view = p.view;
            self.panels[side].sort = p.sort;
            self.panels[side].resort();
        }
        self.active = active;
        for (id, extent) in state.splits {
            self.wm.set_extent(SplitId(id), extent);
        }
        self.layout_restored = true;
    }

    /// `state.json` of the last run, if any.
    pub fn load_state(data_dir: &Path) -> Option<DevState> {
        let data = std::fs::read(data_dir.join("state.json")).ok()?;
        serde_json::from_slice(&data).ok()
    }

    /// Saves the state for the next run (not the agent's conversation).
    fn save_state(&self) {
        let mut state = self.state();
        state.agent_session = None;
        state.agent_cwd = None;
        state.agent_name = None;
        state.agent_permission_mode = None;
        state.journal_dir = None;
        state.seen_seq = 0;
        state.user_screen = Vec::new();
        state.commands = Vec::new();
        state.next_cmd_id = 0;
        state.next_op_id = 0;
        state.editors = Vec::new();
        state.editor_shown = None;
        if let Ok(json) = serde_json::to_vec_pretty(&state) {
            let _ = std::fs::create_dir_all(&self.data_dir);
            let _ = std::fs::write(self.data_dir.join("state.json"), json);
        }
    }

    fn state(&self) -> DevState {
        let (viewers, viewer_shown) = self.viewer_states();
        let (editors, editor_shown) = self.editor_states();
        DevState {
            panels: self
                .panels
                .iter()
                .map(|p| PanelState {
                    path: p.path.clone(),
                    cursor: p.current().map(|e| e.name.clone()),
                    view: p.view,
                    sort: p.sort,
                })
                .collect(),
            active: self.active,
            panels_visible: self.panels_visible(),
            live: self.agent.live,
            splits: self
                .wm
                .extents()
                .into_iter()
                .map(|(id, e)| (id.0, e))
                .collect(),
            agent_session: self.agent.session_id.clone(),
            agent_cwd: self.agent.cwd.clone(),
            agent_name: self.agent.name.clone(),
            agent_permission_mode: self.agent.permission_mode.clone(),
            viewers,
            viewer_shown,
            editors,
            editor_shown,
            agent_hidden: self.wm.is_hidden(WinId::Agent(0)),
            journal_dir: Some(self.journal.dir().to_path_buf()),
            next_op_id: self.next_op_id,
            seen_seq: self.agent.seen_seq,
            user_screen: self.history.clone(),
            commands: self.commands.clone(),
            next_cmd_id: self.next_cmd_id,
        }
    }

    /// Development mode: what keeps a restart waiting.
    fn restart_blocker(&self) -> Option<String> {
        let dev = self.dev.as_ref()?;
        if self.editors_modified() {
            Some(tr!("dev-blocker-editor"))
        } else if self.has_overlay() {
            Some(tr!("dev-blocker-dialog"))
        } else if !self.ops.is_empty() {
            Some(tr!("dev-blocker-operation"))
        } else if self.running.is_some() {
            Some(tr!("dev-blocker-command"))
        } else if self.agent_alive() && dev.last_agent_output.elapsed() < Duration::from_secs(3) {
            Some(tr!("dev-blocker-agent-busy"))
        } else if self.agent_alive() && self.agent.started.elapsed() < Duration::from_secs(10) {
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
                    self.record_build_failure(output, duration);
                    self.say(tr!("dev-build-failed"));
                }
            }
        }
    }

    /// A failed build goes to the user screen and the command list, where
    /// the agent can read it too.
    fn record_build_failure(&mut self, output: Vec<String>, duration: Duration) {
        let duration_ms = duration.as_millis() as u64;
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
                duration_ms,
                lines: output.len(),
            },
        );
        self.commands.push(CmdRecord {
            id,
            text: text.clone(),
            cwd: cwd.clone(),
            exit_code: Some(101),
            duration_ms,
            lines: output.len(),
            by_afar: true,
        });
        self.push_history([format!("{}>{text}", cwd.display()).into()]);
        self.push_history(output.into_iter().map(termview::Line::from));
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
        self.exit = Some(Exit::Restart(Box::new(self.state())));
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
        if !self.layout_restored {
            self.wm.set_extent(
                wm::MAIN_SPLIT,
                Extent::SecondFixed((size.height * 35 / 100).max(8)),
            );
        }
        self.last_layout = Some(self.layout(Rect::new(0, 0, size.width, size.height)));
        // AFAR_NO_AGENT: no agent until Enter in its pane (tests); its
        // MCP config and hook settings are written anyway.
        if std::env::var_os("AFAR_NO_AGENT").is_some() {
            let _ = self.agent_args();
        } else {
            let (rows, cols) = self.last_agent_size();
            self.start_agent(cols, rows);
        }
        self.offer_far_import();
        loop {
            let stats = terminal.draw(|frame| {
                let area = frame.area();
                let cursor = self.draw(area, frame.buffer_mut());
                if let Some(pos) = cursor {
                    frame.set_cursor_position(pos);
                }
                if self.config.agent.test_tools {
                    self.last_frame = Some((frame.buffer_mut().clone(), cursor));
                }
            })?;
            self.poll_signals();
            let outer = self.outer_update();
            if !outer.is_empty() {
                let _ = terminal.send(&outer);
            }
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
                if let Some(agent) = &mut self.agent.pty {
                    agent.shutdown(Duration::from_secs(3));
                }
                let exit = self.exit.take().unwrap_or(Exit::Quit);
                // Far's CompactHistory, on exit.
                self.store.compact();
                if matches!(exit, Exit::Quit) {
                    self.remember_viewers();
                    self.remember_editors();
                    self.save_state();
                }
                return Ok(exit);
            }
            // The test tools play their input a frame at a time.
            let testing = self.test_step();
            let wait = if testing { 15 } else { 250 };
            match rx.recv_timeout(Duration::from_millis(wait)) {
                Ok(msg) => {
                    // Coalesce bursts (PTY output) into one redraw; a paste
                    // into the agent's pane goes as one piece.
                    let batch = self.take_messages(msg, &rx);
                    self.handle_batch(batch);
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Ok(Exit::Quit),
            }
            self.tick();
        }
    }

    fn handle(&mut self, msg: AppMsg) {
        match msg {
            // Shift, Ctrl, Alt alone: only the key bar follows them.
            AppMsg::Input(TermEvent::Key(key)) if matches!(key.code, KeyCode::Modifier(_)) => {
                self.held = key.modifiers;
            }
            AppMsg::Input(TermEvent::Key(key)) if key.kind != KeyEventKind::Release => {
                self.held = key.modifiers;
                self.on_key(key)
            }
            AppMsg::Input(TermEvent::Mouse(mouse)) => self.on_mouse(mouse),
            AppMsg::Input(TermEvent::FocusGained) => self.window_focused = true,
            AppMsg::Input(TermEvent::FocusLost) => self.window_focused = false,
            AppMsg::AgentOutput => {
                if let Some(dev) = &mut self.dev {
                    dev.last_agent_output = Instant::now();
                }
                self.confirm_dev_channels();
                // `--resume` with nothing to resume exits at once.
                let quick_exit = self.agent.pty.as_ref().is_some_and(|a| a.has_exited())
                    && self
                        .agent
                        .resumed_at
                        .is_some_and(|t| t.elapsed() < Duration::from_secs(15));
                if quick_exit {
                    self.resume_failed();
                }
            }
            AppMsg::Input(_) => {}
            AppMsg::Dev(msg) => self.on_dev(msg),
            AppMsg::ViewerFound(done) => self.viewer_found(done),
            AppMsg::Ide(msg) => self.on_ide(msg),
            AppMsg::DirSizes(dir, sizes) => self.dir_sizes(&dir, sizes),
            AppMsg::QuickViewStats(dir, stats) => self.quick_view_stats(dir, stats),
            AppMsg::Find(e) => self.find_event(e),
            AppMsg::TreeRead(root, nodes, done) => self.tree_read(root, nodes, done),
            AppMsg::AttributesDone(n, failed) => self.attributes_done(n, failed),
            AppMsg::CommandOutput => self.on_command_output(),
            AppMsg::Mcp(McpMsg {
                request: Request::ChannelWait,
                reply,
            }) => self.channel_wait(reply),
            AppMsg::Mcp(McpMsg {
                request:
                    Request::TestInput {
                        actions,
                        screen,
                        region,
                    },
                reply,
            }) => self.test_input(actions, screen, region, reply),
            AppMsg::Mcp(McpMsg { request, reply }) => match request {
                // Answered later: after the user's confirmation.
                Request::Delete {
                    side,
                    names,
                    permanent,
                } => match self.resolve_side(&side) {
                    Ok(side) => {
                        let mode = if permanent {
                            DeleteMode::Permanent
                        } else {
                            DeleteMode::Trash
                        };
                        self.agent_delete(side, &names, mode, reply)
                    }
                    Err(e) => {
                        let _ = reply.send(Err(e));
                    }
                },
                Request::MkDir { side, names } => match self.resolve_side(&side) {
                    Ok(side) => self.agent_mkdir_request(side, names, reply),
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
            AppMsg::Fs(ev) => self.on_fs(ev),
        }
    }

    /// Periodic work: debounced journal entries, message expiry.
    fn tick(&mut self) {
        self.fs_tick();
        self.drop_abandoned_requests();
        if self
            .agent
            .resume_retry_at
            .is_some_and(|t| Instant::now() >= t)
        {
            self.agent.resume_retry_at = None;
            let (rows, cols) = self.last_agent_size();
            self.start_agent(cols, rows);
        }
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
                        panel: side_name(side).into(),
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
        self.viewer_tick();
        self.editor_tick();
        self.channel_tick();
        self.ide_sync_selection();
        if self.agent_alive() {
            self.agent.send_enter();
            self.agent.refresh_name();
        }
        self.maybe_restart();
    }

    /// Lines a wheel notch moves: the setting, else Windows' (Far's
    /// get_wheel_scroll_lines); a "page" setting counts as 3.
    pub(super) fn wheel_lines(&self) -> i32 {
        match self.config.panels.wheel_lines {
            0 => windows_wheel_lines(),
            n => n.min(100) as i32,
        }
    }

    fn say(&mut self, text: impl Into<String>) {
        self.message = Some((text.into(), Instant::now()));
    }

    // ---------------------------------------------------------------- agent

    /// Writes the MCP config and hook settings for the agent (also without
    /// an agent: tests call the MCP server and hooks with them); returns
    /// the command-line arguments for `claude`.
    fn agent_args(&self) -> Result<Vec<String>> {
        let dir = self.journal.dir();
        let exe = std::env::current_exe()?
            .to_string_lossy()
            .replace('\\', "/");
        let mut mcp = serde_json::json!({
            "mcpServers": { "afar": {
                "type": "http",
                "url": format!("http://127.0.0.1:{}/mcp", self.link.port),
                "headers": { "Authorization": format!("Bearer {}", self.link.token) },
                // A tool waits for the user's answer to a dialog: Claude Code
                // would give up after a minute; afar's own limit (600 s)
                // comes first.
                "timeout": 660_000,
            }}
        });
        // The channel server; it reaches afar by AFAR_ENDPOINT/AFAR_TOKEN.
        if self.config.agent.channels {
            mcp["mcpServers"]["afar-channel"] = serde_json::json!({
                "type": "stdio",
                "command": exe,
                "args": ["channel"],
            });
        }
        let hook = |event: &str| {
            serde_json::json!([{ "hooks": [
                { "type": "command", "command": format!("\"{exe}\" hook {event}") }
            ]}])
        };
        let tool_hook = |event: &str, matcher: &str| {
            serde_json::json!([{ "matcher": matcher, "hooks": [
                { "type": "command", "command": format!("\"{exe}\" hook {event}") }
            ]}])
        };
        let settings = serde_json::json!({ "hooks": {
            "SessionStart": hook("session-start"),
            "UserPromptSubmit": hook("user-prompt"),
            // The agent's own edits and commands (docs/04-agent.md).
            // PowerShell is the agent's shell on Windows, as Bash elsewhere.
            // Read / Edit / Write: the editor's guard (a file open there).
            "PreToolUse": tool_hook(
                "pre-tool",
                "Bash|PowerShell|Read|Edit|MultiEdit|Write|NotebookEdit"
            ),
            "PostToolUse": tool_hook(
                "post-tool",
                "Edit|MultiEdit|Write|NotebookEdit|Bash|PowerShell"
            ),
            // A failed command gets this instead of PostToolUse.
            "PostToolUseFailure": tool_hook("post-tool-failure", "Bash|PowerShell"),
            // What the agent is doing, for the pane's frame (docs/16).
            "Stop": hook("stop"),
            "Notification": hook("notification"),
        }});
        let mcp_path = dir.join("mcp.json");
        let settings_path = dir.join("settings.json");
        std::fs::write(&mcp_path, serde_json::to_string_pretty(&mcp)?)?;
        std::fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;
        let mut args: Vec<String> = vec![
            "--settings".into(),
            settings_path.to_string_lossy().into_owned(),
            "--append-system-prompt".into(),
            SYSTEM_PROMPT.into(),
        ];
        if self.config.agent.channels {
            args.extend([
                "--dangerously-load-development-channels".into(),
                "server:afar-channel".into(),
            ]);
        }
        // Variadic option: keep it last.
        args.extend([
            "--mcp-config".into(),
            mcp_path.to_string_lossy().into_owned(),
        ]);
        Ok(args)
    }

    /// `[agent] confirm_channels`: answers Claude Code's question about
    /// development channels on the agent's start — once, in its first
    /// minute, and only when afar's own channel is the only one listed.
    fn confirm_dev_channels(&mut self) {
        let a = &self.config.agent;
        if !a.channels
            || !a.confirm_channels
            || self.agent.channels_confirmed
            || self.agent.started.elapsed() > Duration::from_secs(60)
        {
            return;
        }
        let Some(pty) = &self.agent.pty else { return };
        let lines = crate::termview::screen_lines(pty.parser().screen());
        let ours = lines
            .iter()
            .any(|l| l.trim() == "Channels: server:afar-channel");
        let selected = lines.iter().any(|l| {
            let t = l.trim();
            t.starts_with('❯') && t.ends_with("I am using this for local development")
        });
        if ours && selected {
            self.agent.channels_confirmed = true;
            let _ = pty.write(b"\r");
            self.say(tr!("agent-channels-confirmed"));
        }
    }

    /// Starts the agent: after a dev restart the same conversation in its
    /// folder, otherwise a new one in the active panel's folder.
    /// `claude --resume` exited at once. What it said goes to
    /// `agent-resume.log` in the session folder; the first time it is
    /// tried again in two seconds, then a new conversation starts and
    /// the user is told.
    fn resume_failed(&mut self) {
        self.agent.resumed_at = None;
        let id = self.agent.session_id.clone().unwrap_or_default();
        if let Some(pty) = &self.agent.pty {
            let screen = termview::screen_lines(pty.parser().screen());
            let text = format!(
                "{} claude --resume {id} exited with {:?}:\n{}\n\n",
                chrono::Local::now().format("%H:%M:%S"),
                pty.exit_code(),
                screen.join("\n").trim_end()
            );
            use std::io::Write as _;
            if let Ok(mut f) = std::fs::File::options()
                .create(true)
                .append(true)
                .open(self.journal.dir().join("agent-resume.log"))
            {
                let _ = f.write_all(text.as_bytes());
            }
        }
        if !self.agent.resume_retried {
            self.agent.resume_retried = true;
            self.agent.resume_retry_at = Some(Instant::now() + Duration::from_secs(2));
            return;
        }
        self.agent.resume_retried = false;
        self.restored = false;
        self.say(tr!("agent-resume-failed", id = id));
        let (rows, cols) = self.last_agent_size();
        self.start_agent(cols, rows);
    }

    fn start_agent(&mut self, cols: u16, rows: u16) {
        let mut resume = self.agent.session_id.clone().filter(|_| self.restored);
        let cwd = match (&resume, &self.agent.cwd) {
            (Some(_), Some(cwd)) if cwd.is_dir() => cwd.clone(),
            _ => self.panels[self.active].path.clone(),
        };
        // A conversation without a message has no file yet: `--resume`
        // would fail; it starts anew instead.
        if let Some(id) = &resume
            && !crate::claude_sessions::has_messages(
                &crate::claude_sessions::project_dir(&cwd).join(format!("{id}.jsonl")),
            )
        {
            resume = None;
        }
        let launch = match resume {
            Some(id) => agent::Launch::Resume(id),
            None => agent::Launch::Fresh,
        };
        self.launch_agent(cwd, launch, &[], cols, rows);
    }

    /// Starts `claude` in `cwd`: a new conversation or one to continue;
    /// `extra` arguments go before afar's own.
    fn launch_agent(
        &mut self,
        cwd: PathBuf,
        launch: agent::Launch,
        extra: &[String],
        cols: u16,
        rows: u16,
    ) {
        let tx = self.tx.clone();
        let program = self.config.agent.command.clone();
        let args = match self.agent_args() {
            Ok(args) => args,
            Err(e) => {
                self.say(tr!("agent-config-failed", error = format!("{e:#}")));
                vec![]
            }
        };
        let mut env = vec![
            (
                "AFAR_ENDPOINT".to_string(),
                format!("http://127.0.0.1:{}", self.link.port),
            ),
            ("AFAR_TOKEN".to_string(), self.link.token.clone()),
            // Links to files in the agent's output: afar opens them on
            // Ctrl+click (docs/16), whatever terminal afar runs in.
            ("FORCE_HYPERLINK".to_string(), "1".to_string()),
        ];
        // The agent connects to afar's IDE server by this port.
        if let Some(ide) = &self.agent.ide {
            env.push(("CLAUDE_CODE_SSE_PORT".to_string(), ide.port.to_string()));
        }
        // A new agent session has not seen anything yet; the same one
        // resumed (a development-mode restart) has.
        let same = matches!(&launch, agent::Launch::Resume(id) if self.agent.session_id.as_deref() == Some(id.as_str()));
        if !same {
            self.agent.seen_seq = 0;
        }
        // Our own session id; after a dev restart: the same conversation.
        // The configured extra arguments go first.
        let mut args: Vec<String> = self
            .config
            .agent
            .args
            .iter()
            .chain(extra)
            .cloned()
            .chain(args)
            .collect();
        if let Some(mode) = &self.agent.permission_mode {
            args.splice(0..0, ["--permission-mode".to_string(), mode.clone()]);
        }
        self.agent.resumed_at = None;
        self.agent.started = Instant::now();
        self.agent.channels_confirmed = false;
        self.agent.state = agent::AgentState::Ready;
        self.agent.cwd = Some(cwd.clone());
        self.agent.ide_sent = None;
        match launch {
            agent::Launch::Resume(id) => {
                self.agent.name = crate::claude_sessions::title_of(
                    &crate::claude_sessions::project_dir(&cwd).join(format!("{id}.jsonl")),
                );
                args.splice(0..0, ["--resume".to_string(), id.clone()]);
                self.agent.session_id = Some(id);
                self.agent.resumed_at = Some(Instant::now());
            }
            agent::Launch::Fresh => {
                let id = new_uuid();
                args.splice(0..0, ["--session-id".to_string(), id.clone()]);
                self.agent.session_id = Some(id);
                self.agent.name = None;
            }
        }
        match PtySession::spawn(
            SpawnOptions {
                program: &program,
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
            Ok(pty) => self.agent.pty = Some(pty),
            Err(e) => self.say(tr!("agent-start-failed", error = format!("{e:#}"))),
        }
    }

    fn agent_alive(&self) -> bool {
        self.agent.pty.as_ref().is_some_and(|a| !a.has_exited())
    }

    // ------------------------------------------------------------- commands

    fn execute(&mut self, text: String) {
        self.execute_as(text, true);
    }

    /// An external viewer or editor (Far's `ProcessExternal`): run like a
    /// command, but not kept in the command history.
    fn execute_external(&mut self, text: String) {
        self.execute_as(text, false);
    }

    fn execute_as(&mut self, text: String, history: bool) {
        // A blank in front: not kept in the history (bash's ignorespace).
        let private = !history || text.starts_with(' ') && self.config.history.skip_leading_space;
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        if !private && self.config.history.commands {
            // This session recalls the command as typed; the database keeps
            // it without secrets.
            self.cmd_history.add(&text);
            let folder = self.panels[self.active].path.display().to_string();
            let kept = self.history_text(&text);
            self.store
                .add(crate::history::Kind::Command, "", &kept, &folder, "user");
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
        // afar:channel <text>: an event for the agent (Channels test).
        if lower.starts_with("afar:channel ") {
            let event = text["afar:channel ".len()..].trim().to_string();
            self.channel_send(event, &[("kind", "command_line")]);
            self.clear_cmdline();
            return;
        }
        if let Some(arg) = lower.strip_prefix("afar:") {
            match arg.trim() {
                "live" => self.agent.live = !self.agent.live,
                "live on" => self.agent.live = true,
                "live off" => self.agent.live = false,
                _ => {}
            }
            self.say(if self.agent.live {
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
            by_afar: false,
        });
        self.push_history([format!("{}>{}", cwd.display(), text).into()]);
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
                self.user_scroll = 0;
                self.running = Some(RunningCommand {
                    id,
                    pty,
                    started: Instant::now(),
                    captured: Vec::new(),
                    continues: false,
                    auto_switched: self.panels_visible(),
                });
                self.set_panels_visible(false);
                self.focus = Focus::Command;
            }
            Err(e) => {
                self.push_history([tr!("command-start-failed", error = format!("{e:#}")).into()]);
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
        termview::join_wrapped(
            run.pty.take_scrolled_lines(),
            &mut run.captured,
            &mut run.continues,
        );
        if !run.pty.has_exited() {
            return;
        }
        let run = self.running.take().unwrap();
        let output =
            termview::output_lines(&run.captured, run.continues, run.pty.parser().screen());
        // What the agent reads: links with their addresses.
        let text: Vec<String> = output.iter().map(termview::Line::for_agent).collect();
        let _ = std::fs::write(self.journal.output_path(run.id), text.join("\n"));
        let (exit_code, duration_ms) = (
            run.pty.exit_code(),
            run.started.elapsed().as_millis() as u64,
        );
        if let Some(rec) = self.commands.iter_mut().find(|c| c.id == run.id) {
            rec.exit_code = exit_code;
            rec.duration_ms = duration_ms;
            rec.lines = output.len();
        }
        // The command's result in its history entry (docs/15, improvement 3).
        if let Some(text) = self
            .commands
            .iter()
            .find(|c| c.id == run.id)
            .map(|c| c.text.clone())
        {
            let kept = self.history_text(&text);
            let data = serde_json::json!({ "exit": exit_code, "ms": duration_ms }).to_string();
            self.store
                .set_data(crate::history::Kind::Command, "", &kept, &data);
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

    fn push_history(&mut self, lines: impl IntoIterator<Item = termview::Line>) {
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
        if let Some(letter) = crate::drives::letter_of(&from) {
            self.drive_paths.insert(letter, from.clone());
        }
        let to = self.panels[side].path.clone();
        // The folders history: a source for path fields (later Alt+F12).
        let who = match actor {
            Actor::Agent => "agent",
            _ => "user",
        };
        let folder = to.display().to_string();
        self.store
            .add(crate::history::Kind::Folder, "", &folder, &folder, who);
        self.journal.push(
            actor,
            Event::DirChanged {
                panel: side_name(side).into(),
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
        if e.name == ".." && panel.list.is_some() {
            // Out of the found files: back to the panel's folder.
            let side = self.active;
            self.panels[side].leave_list();
        } else if e.name == ".." {
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
        if self.panels[self.active].sort.selected_first {
            self.panels[self.active].resort();
        }
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

    // --------------------------------------------------------------- keys

    fn on_key(&mut self, key: KeyEvent) {
        // A key takes the link's tooltip away (the text may move).
        self.hovered_link = None;
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
        // Quick search sees the key as typed (Alt+ф searches for "ф").
        if self.focus == Focus::Panels
            && !self.has_overlay()
            && self.panels_visible()
            && self.quick_search_key(&key)
        {
            return;
        }
        let key = keys::normalize(key);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // Ctrl+Space (Ctrl+@ is the same NUL byte on some paths; Ctrl+2 is a
        // key of its own: view modes, bookmarks).
        let is_focus_key =
            key.code == KeyCode::Null || ctrl && matches!(key.code, KeyCode::Char(' ' | '@'));
        if is_focus_key {
            // While a menu or dialog is open, the agent pane is out of reach
            // (a dialog takes Ctrl+Space: Far's manual completion).
            if self.has_overlay() {
                self.overlay_key(key);
                return;
            }
            // Hidden by Ctrl+O: it comes back to take the input.
            if self.wm.is_hidden(WinId::Agent(0)) {
                self.wm.set_hidden(WinId::Agent(0), false);
                self.focus = Focus::Agent;
                return;
            }
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
        // Menus and dialogs take the keyboard (the agent pane too waits).
        if self.has_overlay() {
            self.overlay_key(key);
            return;
        }
        if self.focus != Focus::Agent && self.user_screen_shown() {
            match Chord::from_event(&key).and_then(|c| self.keymap.panels(&c)) {
                Some(command) if Self::is_screen_scroll(command) => {
                    self.scroll_user_screen_by(command);
                    return;
                }
                // Typing returns to the output's end.
                _ => self.user_scroll = 0,
            }
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
            Focus::Panels => self.screen_key(key),
        }
    }

    /// Keys of the current screen: a viewer or the panels.
    fn screen_key(&mut self, key: KeyEvent) {
        if let Some(i) = self.shown_editor() {
            return self.editor_key(i, key);
        }
        match self.shown_viewer() {
            Some(i) => self.viewer_key(i, key),
            None => self.panels_key(key),
        }
    }

    fn agent_key(&mut self, key: KeyEvent) {
        // F9: the agent pane's menu (Claude Code does not use the key).
        if key.code == KeyCode::F(9) && key.modifiers.is_empty() {
            self.agent_menu();
            return;
        }
        if !self.agent_alive() {
            if key.code == KeyCode::Enter {
                let (rows, cols) = self.last_agent_size();
                self.start_agent(cols, rows);
            }
            return;
        }
        // Esc interrupts the agent (no hook tells): it is ready again; a
        // tool it goes on with makes it working.
        if key.code == KeyCode::Esc && self.agent.state != agent::AgentState::Ready {
            self.agent_state(agent::AgentState::Ready);
        }
        let agent = self.agent.pty.as_ref().unwrap();
        // Typing returns from a scrolled-back view.
        agent.parser().screen_mut().set_scrollback(0);
        let app_cursor = agent.parser().screen().application_cursor();
        if let Some(bytes) = keys::encode(&key, app_cursor) {
            let _ = agent.write(&bytes);
        }
    }

    /// Keys of the panels and the command line: the key map's command, or
    /// the command line's own editing.
    fn panels_key(&mut self, key: KeyEvent) {
        // The command line's completion list takes keys first; an edit of
        // the line recomputes it.
        if self.quick_view_key(&key)
            || self.completion_key(autocomplete::Owner::Cmdline, &key)
            || self.ghost_key(autocomplete::Owner::Cmdline, &key)
        {
            return;
        }
        let before = self.cmdline.clone();
        self.panels_key_inner(key);
        self.cmdline_edited(&before);
    }

    fn panels_key_inner(&mut self, key: KeyEvent) {
        if key.code != KeyCode::F(10) {
            self.quit_armed = None;
        }
        let command = Chord::from_event(&key).and_then(|c| self.keymap.panels(&c));
        // With the panels hidden only some commands work (as in Far).
        if let Some(command) = command
            && (self.panels_visible() || command.def().with_panels_hidden)
            && self.run_command(command)
        {
            return;
        }
        if !self.cmdline_key(&key)
            && let KeyCode::F(n) = key.code
            && key.modifiers.is_empty()
        {
            self.say(tr!("not-implemented", n = n));
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

    fn panel_hidden(&self, side: usize) -> bool {
        self.wm.is_hidden(WinId::Panel(side))
    }

    /// Far's Ctrl+F1 / Ctrl+F2 / Ctrl+P: hides a panel in place or shows
    /// it again. A hidden active panel passes the focus to the other one;
    /// with both hidden the panels give way to the user screen (Ctrl+O
    /// brings both back).
    fn toggle_panel(&mut self, side: usize) {
        let hide = !self.panel_hidden(side);
        self.wm.set_hidden(WinId::Panel(side), hide);
        if !hide {
            if self.panel_hidden(self.active) {
                self.active = side;
            }
            return;
        }
        if self.panel_hidden(1 - side) {
            for side in 0..2 {
                self.wm.set_hidden(WinId::Panel(side), false);
            }
            self.set_panels_visible(false);
        } else if self.active == side {
            self.active = 1 - side;
        }
    }

    /// Far's Ctrl+U: the panels change places; the active one stays active.
    fn swap_panels(&mut self) {
        self.panels.swap(0, 1);
        self.selection_changed.swap(0, 1);
        let (left, right) = (self.panel_hidden(0), self.panel_hidden(1));
        self.wm.set_hidden(WinId::Panel(0), right);
        self.wm.set_hidden(WinId::Panel(1), left);
        self.active = 1 - self.active;
    }

    /// What Ctrl+O has hidden: 0 nothing, 1 the panels, 2 the panels and
    /// the agent pane, 3 the agent pane.
    fn hiding(&self) -> u8 {
        match (self.panels_visible(), self.wm.is_hidden(WinId::Agent(0))) {
            (true, false) => 0,
            (false, false) => 1,
            (false, true) => 2,
            (true, true) => 3,
        }
    }

    /// Hides the agent pane (the agent menu's "hide"): Ctrl+O's states with
    /// the agent hidden.
    fn hide_agent(&mut self) {
        let state = if self.panels_visible() { 3 } else { 2 };
        self.set_hiding(state);
    }

    fn set_hiding(&mut self, state: u8) {
        self.set_panels_visible(matches!(state, 0 | 3));
        self.wm.set_hidden(WinId::Agent(0), matches!(state, 2 | 3));
        if matches!(state, 2 | 3) && self.focus == Focus::Agent {
            self.focus = Focus::Panels;
        }
    }

    /// Ctrl+O. Quick presses go round: the panels hide, then the agent
    /// pane too, then the panels return, then the agent pane. A press
    /// after a pause toggles between showing everything and the hiding
    /// it was left in last (at first: the panels).
    fn cycle_hiding(&mut self) {
        let quick = self
            .hiding_pressed
            .is_some_and(|t| t.elapsed() < HIDING_CYCLE);
        self.hiding_pressed = Some(Instant::now());
        let state = self.hiding();
        let next = if quick {
            (state + 1) % 4
        } else if state != 0 {
            self.hiding_mode = state;
            0
        } else {
            self.hiding_mode
        };
        self.set_hiding(next);
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
            Request::Journal { since, limit } => Ok(self.journal_page(since, limit)),
            Request::Commands { limit } => {
                let start = self.commands.len().saturating_sub(limit);
                let mut out = String::new();
                for c in &self.commands[start..] {
                    let running = self.running.as_ref().filter(|r| r.id == c.id);
                    // The running one: its time and lines so far.
                    let (duration_ms, lines) = match running {
                        Some(r) => (
                            r.started.elapsed().as_millis() as u64,
                            termview::output_lines(
                                &r.captured,
                                r.continues,
                                r.pty.parser().screen(),
                            )
                            .len(),
                        ),
                        None => (c.duration_ms, c.lines),
                    };
                    let status = match (c.exit_code, running.is_some()) {
                        (_, true) => "running".to_string(),
                        (Some(code), _) => format!("exit {}", code as i32),
                        (None, _) => "exit ?".to_string(),
                    };
                    let by = if c.by_afar { "[afar] " } else { "" };
                    out.push_str(&format!(
                        "cmd-{}  {by}{}  (cwd {})  {status}, {:.1}s, {lines} lines\n",
                        c.id,
                        c.text,
                        c.cwd.display(),
                        duration_ms as f64 / 1000.0,
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
                // Both: the beginning and the end with a gap between.
                let tail = tail.or((head.is_none() && grep.is_none()).then_some(200));
                let n = numbered.len();
                let mut out = format!("[cmd-{id}: {} lines total]\n", lines.len());
                let put = |part: &[(usize, &String)], out: &mut String| {
                    for (n, l) in part {
                        out.push_str(&format!("{n:>5}: {l}\n"));
                    }
                };
                match (head, tail) {
                    (Some(h), Some(t)) if h + t < n => {
                        put(&numbered[..h], &mut out);
                        out.push_str(&format!("      … {} lines …\n", n - h - t));
                        put(&numbered[n - t..], &mut out);
                    }
                    (Some(_), Some(_)) | (None, None) => put(&numbered, &mut out),
                    (Some(h), None) => put(&numbered[..h.min(n)], &mut out),
                    (None, Some(t)) => put(&numbered[n.saturating_sub(t)..], &mut out),
                }
                Ok(out)
            }
            Request::TerminalScreen => match &self.running {
                Some(run) => Ok(format!(
                    "[cmd-{} is running]\n{}",
                    run.id,
                    termview::output_lines(&[], false, run.pty.parser().screen())
                        .iter()
                        .map(termview::Line::for_agent)
                        .collect::<Vec<_>>()
                        .join("\n")
                )),
                None => {
                    let start = self.history.len().saturating_sub(40);
                    let text: Vec<String> = self.history[start..]
                        .iter()
                        .map(termview::Line::for_agent)
                        .collect();
                    Ok(format!(
                        "[no running command; end of the user screen]\n{}",
                        text.join("\n")
                    ))
                }
            },
            Request::Navigate { .. } | Request::Select { .. }
                if self.permission(policy::AgentAction::Navigate) == crate::config::Level::Deny =>
            {
                Err(Self::denied(policy::AgentAction::Navigate))
            }
            Request::Navigate { side, path, cursor } => {
                let side = self.resolve_side(&side)?;
                let target = self.panels[side].path.join(&path);
                // The model reads the error: not translated, with the path.
                if target.is_file() {
                    return Err(format!(
                        "{} is a file; afar_navigate opens its folder with cursor={:?}, afar_view shows it",
                        target.display(),
                        target.file_name().unwrap_or_default().to_string_lossy()
                    ));
                }
                if !target.is_dir() {
                    return Err(format!("no such directory: {}", target.display()));
                }
                self.change_dir_as(Actor::Agent, side, &target)
                    .map_err(|e| format!("cannot open {}: {e}", target.display()))?;
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
                    "{} panel (now the active one): {}, cursor on {}{note}",
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
                        panel: side_name(side).into(),
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
            Request::TestInput { .. } => Err("answered in handle".into()),
            Request::TestScreen { format, region } if self.config.agent.test_tools => {
                self.screen_reply(&format, region.as_deref())
            }
            Request::TestScreen { .. } => Err("the test tools are off ([agent] test_tools)".into()),
            Request::Delete { .. } | Request::Copy { .. } | Request::MkDir { .. } => {
                Err("handled asynchronously".into())
            }
            Request::HookStop => {
                self.agent_state(agent::AgentState::Ready);
                self.fs_agent_idle();
                Ok(String::new())
            }
            Request::HookNotification(input) => {
                self.on_agent_notification(&input);
                Ok(String::new())
            }
            Request::HookPrompt => {
                self.agent_state(agent::AgentState::Working);
                self.fs_agent_idle();
                Ok(self.prompt_context())
            }
            Request::HookPreTool(input) => {
                self.agent_state(agent::AgentState::Working);
                Ok(self.on_pre_tool(&input))
            }
            Request::HookPostTool(input) => {
                self.agent_state(agent::AgentState::Working);
                Ok(self.on_post_tool(&input, false))
            }
            Request::HookPostToolFailure(input) => {
                self.agent_state(agent::AgentState::Working);
                Ok(self.on_post_tool(&input, true))
            }
            Request::ChannelWait => unreachable!("answered in handle"),
            Request::View {
                path,
                line,
                pattern,
                highlight,
            } => self.agent_view(&path, line, pattern, highlight),
            Request::Highlight {
                path,
                marks,
                flash,
                ttl_s,
                clear,
            } => self.agent_highlight(&path, marks, flash, ttl_s, clear),
            Request::ViewerState => Ok(self.agent_viewer_state()),
            Request::Edit {
                path,
                line,
                pattern,
            } => self.agent_edit(&path, line, pattern),
            Request::EditorState => Ok(self.agent_editor_state()),
            Request::BufferRead {
                path,
                from_line,
                to_line,
                since_version,
            } => self.agent_buffer_read(&path, from_line, to_line, since_version),
            Request::BufferEdit {
                path,
                old,
                new,
                all,
            } => self.agent_buffer_edit(&path, &old, &new, all),
            Request::BufferInsert {
                path,
                after_line,
                after_text,
                text,
            } => self.agent_buffer_insert(&path, after_line, after_text, &text),
            Request::HookSessionStart(input) => {
                let resumed = self.on_session_start(&input);
                let mut context = self.session_start_context();
                // Claude Code keeps the system prompt a conversation began
                // with (its `prompt_snapshot`), even when resumed with a new
                // --append-system-prompt: afar's current text comes here.
                if resumed {
                    context.push_str(
                        "\n[afar] afar's current instructions (newer than this conversation's \
                         system prompt where they differ):\n",
                    );
                    context.push_str(SYSTEM_PROMPT);
                }
                Ok(context)
            }
        }
    }

    /// SessionStart: a new conversation id after `/clear` (and on resume).
    /// `true`: a conversation resumed.
    fn on_session_start(&mut self, input: &str) -> bool {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(input) else {
            return false;
        };
        let resumed = v.get("source").and_then(|s| s.as_str()) == Some("resume");
        if let Some(id) = v.get("session_id").and_then(|i| i.as_str())
            && self.agent.session_id.as_deref() != Some(id)
        {
            self.agent.session_id = Some(id.to_string());
            // A new conversation has no name yet.
            if v.get("source").and_then(|s| s.as_str()) == Some("clear") {
                self.agent.name = None;
            }
        }
        resumed
    }

    /// What SessionStart tells the agent about afar.
    fn session_start_context(&self) -> String {
        format!(
            "[afar] left panel: {} | right panel: {} | active: {} | journal at #{} | mode: {}",
            self.panels[0].path.display(),
            self.panels[1].path.display(),
            side_name(self.active),
            self.journal.last_seq(),
            if self.agent.live {
                "live (actions are added to prompts)"
            } else {
                "on-demand (use afar_journal)"
            }
        )
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
            let lines =
                termview::output_lines(&run.captured, run.continues, run.pty.parser().screen());
            return Ok(lines.iter().map(termview::Line::for_agent).collect());
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
            "observe_mode": if self.agent.live { "live" } else { "on-demand" },
        })
    }

    /// `afar_journal`: with `since`, the first `limit` entries after it
    /// (pages); without, the last `limit`. What was left out is said, and
    /// only what was given counts as seen.
    fn journal_page(&mut self, since: Option<u64>, limit: usize) -> String {
        let last_seq = self.journal.last_seq();
        let entries = self.journal.since(since.unwrap_or(0));
        if entries.is_empty() {
            return match since {
                Some(s) => format!("no entries after #{s} (the last is #{last_seq})"),
                None => "the journal is empty".to_string(),
            };
        }
        let limit = limit.max(1);
        let (shown, before, after) = if since.is_some() {
            let n = entries.len().min(limit);
            (&entries[..n], 0, entries.len() - n)
        } else {
            let start = entries.len().saturating_sub(limit);
            (&entries[start..], start, 0)
        };
        let mut out = String::new();
        if before > 0 {
            out.push_str(&format!(
                "({before} earlier entries not shown; afar_journal(since=N) pages from #N)\n"
            ));
        }
        out.push_str(&format_entries(shown));
        let shown_last = shown.last().map_or(0, |e| e.seq);
        if after > 0 {
            out.push_str(&format!(
                "({after} more entries; call afar_journal(since={shown_last}))\n"
            ));
        }
        // Only what was given counts as seen (pages read in order).
        if before == 0 && self.agent.seen_seq >= since.unwrap_or(0) {
            self.agent.seen_seq = self.agent.seen_seq.max(shown_last);
        }
        out
    }

    /// Text added to the user's prompt by the `UserPromptSubmit` hook.
    fn prompt_context(&mut self) -> String {
        let new = self.journal.since(self.agent.seen_seq);
        let (Some(first), Some(last)) = (new.first(), new.last()) else {
            return String::new();
        };
        let (first, last) = (first.seq, last.seq);
        let count = new.len();
        let since = self.agent.seen_seq;
        self.agent.seen_seq = last;
        if !self.agent.live {
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
        let wheel = self.wheel_lines();
        // The completion list is over everything.
        if self.completion_mouse(&ev) {
            return;
        }
        let pressed = matches!(ev.kind, MouseEventKind::Down(_));
        // A click closes the quick search and does nothing else.
        if pressed && self.quick_search.is_some() {
            self.quick_search = None;
            return;
        }

        // A link under the mouse: its address in the message line.
        if ev.kind == MouseEventKind::Moved && !self.has_overlay() {
            self.hover_link(&ev, &l);
        }

        // Menus and dialogs take the mouse everywhere, the agent pane
        // included: a click over it must not move the focus there.
        if self.has_overlay() {
            self.overlay_mouse(&ev);
            return;
        }

        // Dragging a boundary between windows.
        if let Some((id, offset)) = self.drag {
            match ev.kind {
                MouseEventKind::Drag(MouseButton::Left) => {
                    self.agent_frame_click = None;
                    if let Some(sp) = l.arrangement.splitter(id) {
                        let first = sp.first_for(pos, offset);
                        self.wm.set_first(id, i32::from(first), sp.total());
                    }
                    return;
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    self.drag = None;
                    // Released where pressed: a click on the agent's frame.
                    if let Some(at) = self.agent_frame_click.take() {
                        self.agent_menu_at(at);
                    }
                    return;
                }
                _ => {
                    self.drag = None;
                    self.agent_frame_click = None;
                }
            }
        }
        // The agent pane's top frame opens its menu at the title under the
        // mouse; where it is also a boundary, only a click (not a drag).
        let f = l.agent_frame;
        if ev.kind == MouseEventKind::Down(MouseButton::Left)
            && pos.y == f.y
            && (f.x..f.right()).contains(&pos.x)
        {
            match l.arrangement.grab(pos) {
                Some(grab) => {
                    self.drag = Some(grab);
                    self.agent_frame_click = Some(pos);
                }
                None => self.agent_menu_at(pos),
            }
            return;
        }
        // The top row of the screen opens Far's menu bar (not over a
        // viewer, which has no menu).
        if matches!(
            ev.kind,
            MouseEventKind::Down(MouseButton::Left | MouseButton::Right)
        ) && ev.row == area_top(&l)
            && self.shown_viewer().is_none()
            && self.shown_editor().is_none()
        {
            self.top_row_click(&ev);
            return;
        }
        // Ctrl+click is for links (the row next to a boundary is a link's
        // too, e.g. the last output line above the agent pane).
        if ev.kind == MouseEventKind::Down(MouseButton::Left)
            && !ev.modifiers.contains(KeyModifiers::CONTROL)
            && let Some(grab) = l.arrangement.grab(pos)
        {
            self.drag = Some(grab);
            return;
        }

        if l.agent_frame.contains(pos) {
            // Ctrl+click on a link opens it (the program does not get it).
            if let Some(uri) = self.link_click(&ev, l.agent, true) {
                self.open_link(&uri);
                return;
            }
            if self.link_pressed && matches!(ev.kind, MouseEventKind::Up(_)) {
                self.link_pressed = false;
                return;
            }
            if pressed {
                self.focus = Focus::Agent;
            }
            let Some(agent) = &self.agent.pty else { return };
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
                MouseEventKind::ScrollUp => screen.set_scrollback(offset + wheel as usize),
                MouseEventKind::ScrollDown => {
                    screen.set_scrollback(offset.saturating_sub(wheel as usize))
                }
                _ => {}
            }
            return;
        }

        if l.keybar.contains(pos) {
            // Clicking a key label presses that key, like in Far: on
            // release over the same key.
            let key = keybar_keys(l.keybar.width)
                .iter()
                .position(|(start, end)| {
                    (l.keybar.x + start..l.keybar.x + end).contains(&ev.column)
                })
                .map(|i| i as u8 + 1);
            match ev.kind {
                MouseEventKind::Down(MouseButton::Left) => self.keybar_pressed = key,
                MouseEventKind::Up(MouseButton::Left) => {
                    if let Some(n) = key.filter(|k| self.keybar_pressed.take() == Some(*k))
                        && self.focus == Focus::Panels
                    {
                        // With modifiers held, the label shown is that key.
                        self.screen_key(KeyEvent::new(KeyCode::F(n), ev.modifiers));
                    }
                }
                _ => {}
            }
            return;
        }

        if l.cmdline.contains(pos) {
            if pressed {
                self.focus = Focus::Panels;
            }
            return;
        }

        if !l.user.contains(pos) {
            return;
        }
        if let Some(i) = self.shown_editor().filter(|_| !self.viewer_peek) {
            self.editor_mouse(i, &ev, wheel);
            return;
        }
        if let Some(i) = self.shown_viewer().filter(|_| !self.viewer_peek) {
            let v = &mut self.viewers[i];
            match ev.kind {
                MouseEventKind::ScrollUp => v.scroll(-wheel),
                MouseEventKind::ScrollDown => v.scroll(wheel),
                MouseEventKind::Down(MouseButton::Left) => {
                    let shift = ev.modifiers.contains(KeyModifiers::SHIFT);
                    v.mouse_down(ev.column, ev.row, shift);
                    self.focus = Focus::Panels;
                }
                MouseEventKind::Drag(MouseButton::Left) => v.mouse_drag(ev.row),
                MouseEventKind::Up(MouseButton::Left) => v.mouse_up(),
                MouseEventKind::Down(_) => self.focus = Focus::Panels,
                _ => {}
            }
            return;
        }
        if !self.panels_visible() || l.top.height < 5 || !l.top.contains(pos) {
            // User screen: the running command gets the mouse if it wants
            // it; Ctrl+click on a link opens it.
            if let Some(uri) = self
                .link_click(&ev, self.last_live, false)
                .or_else(|| self.kept_link_click(&ev))
            {
                self.open_link(&uri);
                return;
            }
            if self.link_pressed && matches!(ev.kind, MouseEventKind::Up(_)) {
                self.link_pressed = false;
                return;
            }
            if self.user_wheel(&ev, wheel) {
                return;
            }
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
        // The information panel's description: a click opens the file.
        if ev.kind == MouseEventKind::Down(MouseButton::Left)
            && self.info_panel.as_ref().is_some_and(|i| i.side == side)
        {
            self.info_panel_click(ev.column, ev.row);
            return;
        }
        // The sort mode letter (Far's FileList::ProcessMouse): the left
        // button opens the drive menu, the right one the sort menu.
        let r = l.panels[side];
        if ev.row == r.y + 1 && (r.x + 1..=r.x + 2).contains(&ev.column) {
            match ev.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    self.focus = Focus::Panels;
                    self.active = side;
                    self.drive_menu(side);
                    return;
                }
                MouseEventKind::Down(MouseButton::Right) => {
                    self.focus = Focus::Panels;
                    self.active = side;
                    self.sort_menu();
                    return;
                }
                _ => {}
            }
        }
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
            MouseEventKind::ScrollUp if self.quick_view_scroll(ev.column, ev.row, -wheel) => {}
            MouseEventKind::ScrollDown if self.quick_view_scroll(ev.column, ev.row, wheel) => {}
            MouseEventKind::ScrollUp => self.panels[side].move_cursor(-(wheel as isize)),
            MouseEventKind::ScrollDown => self.panels[side].move_cursor(wheel as isize),
            _ => {}
        }
    }

    // ------------------------------------------------------------- layout

    fn layout(&self, area: Rect) -> Layout {
        // A viewer hides the command line (unless set to keep it) and,
        // with Ctrl+B, its key bar.
        let editor = self.shown_editor().is_some() && !self.viewer_peek;
        let viewer = (self.shown_viewer().is_some() || editor) && !self.viewer_peek;
        let keybar_h = u16::from(
            !viewer
                || if editor {
                    self.editor_keybar
                } else {
                    self.viewer_keybar
                },
        );
        let keybar = Rect::new(
            area.x,
            area.bottom().saturating_sub(keybar_h),
            area.width,
            keybar_h,
        );
        let cmdline_h = u16::from(!viewer || !editor && self.config.viewer.command_line);
        let cmdline = Rect::new(
            area.x,
            keybar.y.saturating_sub(cmdline_h),
            area.width,
            cmdline_h,
        );
        let desktop = Rect::new(area.x, area.y, area.width, cmdline.y.saturating_sub(area.y));
        let arrangement = self.wm.arrange(desktop);
        let rect = |w| arrangement.rect(w).unwrap_or_default();
        let agent_frame = rect(WinId::Agent(0));
        let agent = Rect::new(
            agent_frame.x + 1,
            agent_frame.y + 1,
            agent_frame.width.saturating_sub(2),
            agent_frame.height.saturating_sub(2),
        );
        let user = if self.wm.is_hidden(WinId::Agent(0)) {
            desktop
        } else {
            arrangement.screen_area
        };
        Layout {
            user,
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
        let top = self
            .last_layout
            .as_ref()
            .map(|l| l.user)
            .unwrap_or_default();
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
        self.panels[self.active].page()
    }

    // --------------------------------------------------------------- draw

    fn draw(&mut self, area: Rect, buf: &mut Buffer) -> Option<Position> {
        let l = self.layout(area);
        self.last_layout = Some(l.clone());
        let mut cursor = None;

        if let Some(agent) = &self.agent.pty
            && l.agent.height > 0
            && l.agent.width > 0
        {
            let _ = agent.resize(l.agent.height, l.agent.width);
        }
        if let Some(run) = &self.running {
            let _ = run.pty.resize(l.user.height, l.user.width);
        }

        // Top area: an editor, a viewer, the panels or the user screen.
        if let Some(i) = self.shown_editor().filter(|_| !self.viewer_peek) {
            if l.user != l.top {
                self.draw_user_screen(l.user, buf);
            }
            let clock = l.top.y == area.y && l.top.right() == area.right() && self.editor_status;
            let c = self.draw_editor(i, l.top, buf, clock);
            if clock {
                let t = chrono::Local::now().format("%H:%M").to_string();
                let x = area.right() - t.len() as u16;
                buf.set_stringn(x, area.y, &t, t.len(), theme::EDITOR_STATUS);
            }
            if self.focus == Focus::Panels && !self.has_overlay() {
                cursor = c;
            }
        } else if let Some(i) = self.shown_viewer().filter(|_| !self.viewer_peek) {
            // The agent pane hidden by Ctrl+O shows the output there.
            if l.user != l.top {
                self.draw_user_screen(l.user, buf);
            }
            // Far's clock at the right of the viewer's status line.
            let clock = chrono::Local::now().format("%H:%M").to_string();
            let v = &mut self.viewers[i];
            let clock_w = if l.top.y == area.y && l.top.right() == area.right() && v.status_line {
                clock.len() as u16
            } else {
                0
            };
            v.draw(l.top, buf, clock_w);
            if clock_w > 0 {
                let x = area.right() - clock_w;
                buf.set_stringn(x, area.y, &clock, clock.len(), theme::VIEWER_STATUS);
            }
        } else if self.panels_visible() && l.top.height >= 5 {
            self.apply_agent_marks();
            let panels_active = self.focus == Focus::Panels;
            // The clock overlays the top border at the right edge, as in Far.
            let clock = chrono::Local::now().format("%H:%M").to_string();
            for side in 0..2 {
                let touches = l.panels[side].right() == area.right();
                self.panels[side].clock_cells = if touches { clock.len() as u16 } else { 0 };
            }
            // A hidden panel (or agent pane) shows the user screen below
            // it, as in Far.
            if (0..2).any(|side| self.panel_hidden(side)) || l.user != l.top {
                self.draw_user_screen(l.user, buf);
            }
            for side in 0..2 {
                if self.quick_view.as_ref().is_some_and(|q| q.side == side)
                    && !self.panel_hidden(side)
                {
                    self.draw_quick_view(l.panels[side], buf);
                } else if self.info_panel.as_ref().is_some_and(|i| i.side == side)
                    && !self.panel_hidden(side)
                {
                    self.draw_info_panel(l.panels[side], buf);
                } else if !self.panel_hidden(side) {
                    self.panels[side].draw(
                        l.panels[side],
                        buf,
                        panels_active && self.active == side,
                    );
                }
            }
            if l.top.y == area.y {
                let x = area.right().saturating_sub(clock.len() as u16);
                buf.set_stringn(x, area.y, &clock, clock.len(), theme::MESSAGE);
            }
        } else {
            let c = self.draw_user_screen(l.user, buf);
            if self.focus == Focus::Command {
                cursor = c;
            }
        }

        // Agent pane: a Far-style frame on the panel's blue background.
        let agent_focused = self.focus == Focus::Agent && !self.has_overlay();
        let status = match &self.agent.pty {
            None => tr!("agent-not-started"),
            Some(a) if a.has_exited() => tr!(
                "agent-exited",
                code = a.exit_code().map_or(-1, |c| c as i64)
            ),
            Some(_) => match &self.agent.state {
                agent::AgentState::Ready => tr!("agent-ready"),
                agent::AgentState::Working => tr!("agent-working"),
                agent::AgentState::Waiting(_) => tr!("agent-waiting"),
            },
        };
        let waiting = matches!(self.agent.state, agent::AgentState::Waiting(_));
        let frame = l.agent_frame;
        crate::panel::draw_frame(buf, frame, theme::PANEL_BOX);
        let title_style = if waiting && !agent_focused {
            theme::AGENT_WAITING
        } else if agent_focused {
            theme::PANEL_TITLE_SELECTED
        } else {
            theme::PANEL_TITLE
        };
        let name = self.agent.name.clone().unwrap_or_else(|| "claude".into());
        let title = format!(" {} ", tr!("agent-title", name = name, status = status));
        crate::panel::put_title(buf, frame, frame.y, &title, title_style);
        let unseen = self.journal.last_seq().saturating_sub(self.agent.seen_seq);
        let mode = if self.agent.live {
            tr!("observe-live")
        } else {
            tr!("observe-on-demand")
        };
        let scrolled = self
            .agent
            .pty
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
        if let Some(agent) = &self.agent.pty {
            let c = termview::draw_rows_links(
                &crate::term::view(&agent.parser()),
                0,
                l.agent,
                buf,
                Style::reset(),
                true,
            );
            if agent_focused {
                cursor = c;
            }
        }

        // Command line.
        if l.cmdline.height > 0 {
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
            // The ghost suggestion after the line, with the cursor at its end.
            if let Some(rest) = self.shown_cmd_ghost() {
                let x = line.chars().count() as u16;
                if x < l.cmdline.width {
                    put(
                        buf,
                        l.cmdline.x + x,
                        l.cmdline.y,
                        l.cmdline.width - x,
                        &rest,
                        theme::GHOST_COMMAND_LINE,
                    );
                }
            }
            if self.focus == Focus::Panels
                && self.shown_viewer().is_none()
                && self.shown_editor().is_none()
            {
                let x = (prompt.chars().count() + self.cmd_cursor) as u16;
                if x < l.cmdline.width {
                    cursor = Some(Position::new(l.cmdline.x + x, l.cmdline.y));
                }
            }
        }

        self.draw_keybar(l.keybar, buf);
        // Messages: at the right of the command line, or of the key bar
        // (or the last row) when a viewer hides the command line.
        if let Some((msg, _)) = &self.message {
            let row = if l.cmdline.height > 0 {
                l.cmdline
            } else if l.keybar.height > 0 {
                l.keybar
            } else {
                Rect::new(l.top.x, l.top.bottom().saturating_sub(1), l.top.width, 1)
            };
            let text = format!(" {msg} ");
            let w = (text.chars().count() as u16).min(row.width);
            put(buf, row.right() - w, row.y, w, &text, theme::MESSAGE);
        }
        if let Some(side) = self.quick_search_side()
            && let Some(c) = self.draw_quick_search(area, l.panels[side], buf)
        {
            cursor = Some(c);
        }

        // Overlay: dialogs and progress windows over everything but the
        // bottom bars.
        if self.has_overlay() {
            let over = Rect::new(
                area.x,
                area.y,
                area.width,
                l.cmdline.y.saturating_sub(area.y),
            );
            let bar = Rect::new(
                area.x,
                l.top.y,
                area.width,
                l.cmdline.y.saturating_sub(l.top.y),
            );
            // The agent pane's menu bar: on the pane's top row.
            let agent_bar = Rect::new(
                area.x,
                l.agent_frame.y,
                area.width,
                l.cmdline.y.saturating_sub(l.agent_frame.y),
            );
            self.fill_dialogs_from_history();
            // The test tools: where the dialogs and menus are (the cells
            // they changed), so a screenshot or `expect:` includes them.
            let before = self.config.agent.test_tools.then(|| buf.clone());
            cursor = self.draw_overlays(over, bar, agent_bar, buf);
            if let Some(before) = before {
                self.overlay_area = changed_area(&before, buf);
            }
        } else {
            self.overlay_area = None;
        }
        // The completion list, over everything.
        let prompt = format!("{}>", self.panels[self.active].path.display())
            .chars()
            .count() as u16;
        self.draw_completion(area, l.cmdline, prompt, buf);
        self.draw_link_tooltip(area, buf);
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
        self.user_height = area.height;
        if self.user_scroll > 0 && self.draw_user_screen_scrolled(area, buf) {
            return None;
        }
        let mut live_rows = 0u16;
        let mut cursor = None;
        let text: Vec<&termview::Line> = match &self.running {
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
                cursor = termview::draw_rows_links(
                    &crate::term::view(&parser),
                    0,
                    live,
                    buf,
                    Style::reset(),
                    true,
                );
                self.history.iter().chain(run.captured.iter()).collect()
            }
            None => self.history.iter().collect(),
        };
        // The kept lines wrapped as a terminal shows them: the last rows
        // that fit above the live screen.
        let rows = (area.height - live_rows) as usize;
        let mut shown: Vec<termview::Line> = Vec::new();
        'fill: for line in text.iter().rev() {
            for row in termview::wrap_line(line, usize::from(area.width))
                .into_iter()
                .rev()
            {
                if shown.len() == rows {
                    break 'fill;
                }
                shown.push(row);
            }
        }
        shown.reverse();
        let first_y = area.bottom() - live_rows - shown.len() as u16;
        draw_kept_rows(buf, area.x, first_y, area.width, &shown);
        // Where the kept rows are, for Ctrl+click and the link tooltip.
        self.user_rows = (
            Rect::new(area.x, first_y, area.width, shown.len() as u16),
            shown,
        );
        // Counted afresh when scrolling back starts.
        self.user_total = 0;
        cursor
    }

    /// The wheel over the user screen: Alt+wheel scrolls the output
    /// (Shift+wheel too where the terminal lets it through: Windows
    /// Terminal keeps it to scroll its own buffer, which is how Far
    /// scrolls); the plain wheel too while a command that does not want the
    /// mouse runs; with nothing running and the panels hidden it goes
    /// through the command history (Far: Ctrl+E / Ctrl+X). `true`: taken.
    fn user_wheel(&mut self, ev: &crossterm::event::MouseEvent, wheel: i32) -> bool {
        let up = match ev.kind {
            MouseEventKind::ScrollUp => true,
            MouseEventKind::ScrollDown => false,
            _ => return false,
        };
        let shift = ev
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT);
        let program_mouse = self.running.as_ref().is_some_and(|r| {
            r.pty.parser().screen().mouse_protocol_mode() != vt100::MouseProtocolMode::None
        });
        if !shift && program_mouse {
            return false;
        }
        if !shift && self.running.is_none() && !self.panels_visible() {
            let command = if up {
                crate::command::Command::HistoryPrev
            } else {
                crate::command::Command::HistoryNext
            };
            let before = self.cmdline.clone();
            self.run_command(command);
            self.cmdline_edited(&before);
            return true;
        }
        let lines = wheel as isize;
        self.scroll_user_screen(if up { lines } else { -lines });
        true
    }

    /// The user screen shown: the panels hidden and no viewer or editor
    /// on top.
    fn user_screen_shown(&self) -> bool {
        !self.panels_visible() && self.shown_viewer().is_none() && self.shown_editor().is_none()
    }

    fn is_screen_scroll(command: crate::command::Command) -> bool {
        use crate::command::Command::*;
        matches!(
            command,
            ScreenLineUp
                | ScreenLineDown
                | ScreenPageUp
                | ScreenPageDown
                | ScreenTop
                | ScreenBottom
        )
    }

    /// A scroll command; `false` when the user screen is not shown (the
    /// key goes on to the command line).
    fn scroll_user_screen_by(&mut self, command: crate::command::Command) -> bool {
        use crate::command::Command::*;
        if !self.user_screen_shown() {
            return false;
        }
        let page = usize::from(self.user_height.saturating_sub(1)).max(1);
        match command {
            ScreenLineUp => self.scroll_user_screen(1),
            ScreenLineDown => self.scroll_user_screen(-1),
            ScreenPageUp => self.scroll_user_screen(page as isize),
            ScreenPageDown => self.scroll_user_screen(-(page as isize)),
            ScreenTop => self.user_scroll = usize::MAX,
            ScreenBottom => self.user_scroll = 0,
            _ => return false,
        }
        true
    }

    /// Scrolls the user screen back (`lines` > 0) or forth; the drawing
    /// keeps it within the output.
    fn scroll_user_screen(&mut self, lines: isize) {
        self.user_scroll = self.user_scroll.saturating_add_signed(lines);
    }

    /// The user screen scrolled back: the kept lines and the running
    /// command's screen as text, a note where the view is. `false`: there
    /// is nothing to scroll (back to the normal drawing).
    fn draw_user_screen_scrolled(&mut self, area: Rect, buf: &mut Buffer) -> bool {
        let live: Vec<termview::Line> = match &self.running {
            Some(run) => {
                let parser = run.pty.parser();
                let screen = parser.screen();
                // A full-screen program has no output to scroll through.
                if screen.alternate_screen() {
                    Vec::new()
                } else {
                    termview::screen_lines_links(screen)
                }
            }
            None => Vec::new(),
        };
        let captured: &[termview::Line] = self
            .running
            .as_ref()
            .map(|r| r.captured.as_slice())
            .unwrap_or(&[]);
        // Rows as the terminal shows them: long lines wrapped.
        let width = usize::from(area.width);
        let all: Vec<termview::Line> = self
            .history
            .iter()
            .chain(captured)
            .chain(live.iter())
            .flat_map(|l| termview::wrap_line(l, width))
            .collect();
        let total = all.len();
        // New output while scrolled back does not move the view.
        if total > self.user_total && self.user_total > 0 {
            self.user_scroll = self.user_scroll.saturating_add(total - self.user_total);
        }
        self.user_total = total;
        let rows = usize::from(area.height);
        self.user_scroll = self.user_scroll.min(total.saturating_sub(rows));
        if self.user_scroll == 0 {
            return false;
        }
        let end = total - self.user_scroll;
        let start = end.saturating_sub(rows);
        let shown = all[start..end].to_vec();
        draw_kept_rows(buf, area.x, area.y, area.width, &shown);
        // Ctrl+click on the rows shown; the running program gets no mouse.
        self.user_rows = (
            Rect::new(area.x, area.y, area.width, shown.len() as u16),
            shown,
        );
        self.last_live = Rect::default();
        // Where the view is, at the top right.
        let note = format!(
            " {} ",
            tr!("user-scroll", from = start + 1, to = end, total = total)
        );
        let w = note.chars().count() as u16;
        if w < area.width {
            buf.set_string(area.right() - w, area.y, &note, theme::MESSAGE);
        }
        true
    }
}

/// Rows of kept output on the user screen; links underlined and colored
/// (Ctrl+click opens them).
fn draw_kept_rows(buf: &mut Buffer, x0: u16, y0: u16, width: u16, rows: &[termview::Line]) {
    for (i, line) in rows.iter().enumerate() {
        let y = y0 + i as u16;
        buf.set_stringn(
            x0,
            y,
            line.text.as_str(),
            usize::from(width),
            theme::COMMAND_LINE,
        );
        if !line.links.is_empty() {
            for x in 0..width {
                if line.link_at_column(usize::from(x)).is_some() {
                    let cell = &mut buf[(x0 + x, y)];
                    cell.modifier |= ratatui::style::Modifier::UNDERLINED;
                    cell.fg = theme::LINK_FG;
                }
            }
        }
    }
}

impl App {
    /// Far's key bar (keybar.cpp): number, label of at least 6 cells, a
    /// space; at 98 columns and more the labels widen, below that the bar
    /// is cut off at the right edge.
    fn draw_keybar(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 {
            return;
        }
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
        let group = modifier_group(self.held);
        let labels: Vec<String> = match (self.shown_editor(), self.shown_viewer()) {
            (Some(e), _) => self.editor_keybar_labels(e, group),
            (None, Some(v)) => self.viewer_keybar_labels(v, group),
            (None, None) => (1..=12)
                .map(|n| crate::i18n::plain(&tr!(&format!("M{group}F{n}"))))
                .collect(),
        };
        for (i, (pos, end)) in keybar_keys(width).into_iter().enumerate() {
            let i = i as u16;
            let num = (i + 1).to_string();
            let num_w = num.len() as u16;
            let gap = u16::from(i < 11);
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
                let label = labels.get(usize::from(i)).cloned().unwrap_or_default();
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
        }
    }
}

/// Far's key bar layout (keybar.cpp): for each key from F1, the columns it
/// takes (start, end) — a number, a label of at least 6 cells and a space;
/// at 98 columns and more the labels widen, below that the bar is cut off.
fn keybar_keys(width: u16) -> Vec<(u16, u16)> {
    const MIN_LABEL: u16 = 6;
    let mut keys = Vec::new();
    let mut pos = 0u16;
    for i in 0..12u16 {
        if pos >= width {
            break;
        }
        let num_w = if i < 9 { 1 } else { 2 };
        let gap = u16::from(i < 11);
        let min_end = pos + num_w + MIN_LABEL + gap;
        let end = if width >= 98 {
            min_end.max((i + 1) * width / 12)
        } else {
            min_end
        }
        .min(width);
        keys.push((pos, end));
        pos = end;
    }
    keys
}

/// Far's key bar groups by the modifiers held (keybar.hpp), as they are
/// named in its message ids: `MCtrlShiftF3`.
fn modifier_group(m: KeyModifiers) -> &'static str {
    let (ctrl, alt, shift) = (
        m.contains(KeyModifiers::CONTROL),
        m.contains(KeyModifiers::ALT),
        m.contains(KeyModifiers::SHIFT),
    );
    match (ctrl, alt, shift) {
        (false, false, false) => "",
        (false, false, true) => "Shift",
        (false, true, false) => "Alt",
        (true, false, false) => "Ctrl",
        (false, true, true) => "AltShift",
        (true, false, true) => "CtrlShift",
        (true, true, false) => "CtrlAlt",
        (true, true, true) => "CtrlAltShift",
    }
}

/// The row that opens the menu bar on a click: the top row of the panels
/// (below the agent pane when it is on top).
fn area_top(l: &Layout) -> u16 {
    l.top.y
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

/// Windows' "lines per wheel notch" (SPI_GETWHEELSCROLLLINES); 3 when it
/// cannot be read or is "a page".
fn windows_wheel_lines() -> i32 {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SPI_GETWHEELSCROLLLINES, SystemParametersInfoW,
    };
    let mut lines: u32 = 3;
    // SAFETY: the out pointer is a valid u32.
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETWHEELSCROLLLINES,
            0,
            (&mut lines as *mut u32).cast(),
            0,
        )
    };
    if ok == 0 || lines == 0 || lines > 100 {
        3
    } else {
        lines as i32
    }
}

/// The smallest rectangle holding every cell that differs between two
/// frames of the same size.
fn changed_area(a: &Buffer, b: &Buffer) -> Option<Rect> {
    let area = b.area;
    if a.area != area {
        return Some(area);
    }
    let (mut x0, mut y0, mut x1, mut y1) = (u16::MAX, u16::MAX, 0, 0);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if a[(x, y)] != b[(x, y)] {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    (x0 <= x1).then(|| Rect::new(x0, y0, x1 - x0 + 1, y1 - y0 + 1))
}
