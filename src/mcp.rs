//! MCP server for the agent and the endpoint for Claude Code hooks.
//!
//! Runs on its own thread with a tokio runtime, listens on 127.0.0.1 and
//! requires a per-session bearer token. Every request is forwarded to the
//! main loop as `AppMsg::Mcp` and answered through a oneshot channel: the
//! main loop is the only owner of the application state.

use std::sync::mpsc::Sender;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{Request as HttpRequest, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::post;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ServerCapabilities, ServerConfig};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData as McpError, ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;
use tokio::sync::oneshot;

use crate::app::AppMsg;

#[derive(Debug)]
pub enum Request {
    State,
    Journal {
        since: Option<u64>,
        limit: usize,
    },
    Commands {
        limit: usize,
    },
    CommandOutput {
        cmd_id: Option<u64>,
        tail: Option<usize>,
        head: Option<usize>,
        grep: Option<String>,
    },
    TerminalScreen,
    Navigate {
        side: String,
        path: String,
        cursor: Option<String>,
    },
    Select {
        side: String,
        names: Vec<String>,
        add: bool,
    },
    MkDir {
        side: String,
        names: Vec<String>,
    },
    /// Answered after the user confirms and the copy/move finishes.
    Copy {
        side: String,
        names: Vec<String>,
        dest: String,
        moving: bool,
    },
    /// Answered after the user confirms and the deletion finishes.
    Delete {
        side: String,
        names: Vec<String>,
        permanent: bool,
    },
    /// `UserPromptSubmit` hook: journal delta (live) or a summary.
    HookPrompt,
    /// `SessionStart` hook: short description of the environment.
    HookSessionStart(String),
    /// `PreToolUse` hook (Bash): the hook's JSON input.
    HookPreTool(String),
    /// `PostToolUse` hook: the hook's JSON input.
    HookPostTool(String),
    /// PostToolUseFailure: the input of the hook.
    HookPostToolFailure(String),
    /// `Stop` hook: the agent finished its turn.
    HookStop,
    /// `Notification` hook (the agent asks for permission or waits for
    /// input): the hook's JSON input.
    HookNotification(String),
    /// `afar channel` waits for events for the agent (answered when there
    /// are some, or with none after a while).
    ChannelWait,
    View {
        path: String,
        line: Option<u64>,
        pattern: Option<String>,
        highlight: Option<crate::app::MarkSpec>,
    },
    Highlight {
        path: String,
        marks: Vec<crate::app::MarkSpec>,
        flash: bool,
        ttl_s: Option<u64>,
        clear: bool,
    },
    ViewerState,
}

pub type Reply = Result<String, String>;

pub struct McpMsg {
    pub request: Request,
    pub reply: oneshot::Sender<Reply>,
}

async fn ask(tx: &Sender<AppMsg>, request: Request) -> Reply {
    ask_within(tx, request, Duration::from_secs(30)).await
}

async fn ask_within(tx: &Sender<AppMsg>, request: Request, limit: Duration) -> Reply {
    let (reply, rx) = oneshot::channel();
    tx.send(AppMsg::Mcp(McpMsg { request, reply }))
        .map_err(|_| "afar is shutting down".to_string())?;
    match tokio::time::timeout(limit, rx).await {
        Ok(Ok(r)) => r,
        Ok(Err(_)) => Err("afar did not answer".into()),
        Err(_) => Err(format!("no answer from afar within {} s", limit.as_secs())),
    }
}

// ------------------------------------------------------------------- tools

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct JournalParams {
    /// Return entries after this sequence number, oldest first (page
    /// forward with the last number shown). Without it: the newest entries.
    pub since: Option<u64>,
    /// Maximum number of entries (default 100).
    pub limit: Option<usize>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct CommandsParams {
    /// Maximum number of commands, newest last (default 20).
    pub limit: Option<usize>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct OutputParams {
    /// Command id as in the journal (`cmd-17` → 17); the last command if omitted.
    pub cmd_id: Option<u64>,
    /// Only the last N lines (default 200 when neither head nor grep is set).
    pub tail_lines: Option<usize>,
    /// Only the first N lines (with `tail_lines` too: the beginning and
    /// the end, the lines between left out).
    pub head_lines: Option<usize>,
    /// Only lines containing this substring (case-insensitive).
    pub grep: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct NavigateParams {
    /// `left`, `right`, `active` or `passive`.
    pub side: String,
    /// Directory to open; absolute or relative to the panel's directory.
    pub path: String,
    /// Name of the item to put the cursor on.
    pub cursor: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct ViewParams {
    /// File to open; absolute or relative to the active panel's directory.
    pub path: String,
    /// Line to show (from 1).
    pub line: Option<u64>,
    /// A regular expression: show its first match (from `line`, if given).
    pub pattern: Option<String>,
    /// Mark the place too.
    pub highlight: Option<crate::app::MarkSpec>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct HighlightParams {
    /// The file (opened in the viewer if it is not).
    pub path: String,
    /// The places to mark.
    pub marks: Vec<crate::app::MarkSpec>,
    /// Blink them for a moment to draw the eye.
    pub flash: Option<bool>,
    /// Take them away after this many seconds (by default they stay until
    /// the user presses Esc or the file is closed).
    pub ttl_s: Option<u64>,
    /// Remove your earlier marks in this file first.
    pub clear: Option<bool>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct SelectParams {
    /// `left`, `right`, `active` or `passive`.
    pub side: String,
    /// File names in the panel's directory.
    pub names: Vec<String>,
    /// Add to the current selection instead of replacing it.
    pub add: Option<bool>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct MkDirParams {
    /// `left`, `right`, `active` (default) or `passive`.
    pub side: Option<String>,
    /// Directory names relative to the panel's directory (nested paths like
    /// `a/b/c` are fine) or absolute paths.
    pub names: Vec<String>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct DeleteParams {
    /// `left`, `right`, `active` (default) or `passive`.
    pub side: Option<String>,
    /// File or directory names in the panel's directory, or absolute paths.
    pub names: Vec<String>,
    /// Delete permanently instead of moving to the recycle bin.
    pub permanent: Option<bool>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct CopyParams {
    /// Panel with the items: `left`, `right`, `active` (default) or `passive`.
    pub side: Option<String>,
    /// File or directory names in the panel's directory, or absolute paths.
    pub names: Vec<String>,
    /// Destination: a directory to put the items into (absolute, or relative
    /// to the panel's directory), or a new name for a single item.
    pub dest: String,
}

#[derive(Clone)]
pub struct AfarMcp {
    tx: Sender<AppMsg>,
}

fn result(r: Reply) -> Result<CallToolResult, McpError> {
    Ok(match r {
        Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
        Err(text) => CallToolResult::error(vec![ContentBlock::text(text)]),
    })
}

#[tool_router]
impl AfarMcp {
    pub fn new(tx: Sender<AppMsg>) -> Self {
        Self { tx }
    }

    #[tool(
        description = "State of the afar file manager as JSON: left/right panels {path, cursor \
        (item under cursor), items, selected_count, selected (up to 20 names)}, active_panel, focus \
        (panels/agent/command), panels_visible, running_command {cmd_id, text} or null, \
        journal_last_seq, observe_mode (live/on-demand). Call first to see what the user sees."
    )]
    async fn afar_state(&self) -> Result<CallToolResult, McpError> {
        result(ask(&self.tx, Request::State).await)
    }

    #[tool(
        description = "Journal of actions in afar, oldest first, one per line: \
        `#seq HH:MM:SS actor kind details`. actor: user, agent (done via afar_* tools) or sys. \
        kinds: cd (panel, from → to), select (panel, size of the selection after the change and \
        sample names, or `selection cleared`), cmd (command line text, cwd, [cmd-N]), done (exit \
        code, duration, output lines — read them with afar_command_output; or a file operation's \
        result, [op-N]), copy / move / delete / trash / wipe / mkdir (a file operation started, [op-N]), fs (files changed in a panel's folder; \
        actor ext: outside afar), tool (your own Bash or edit), view (a file opened in the viewer), \
        vsel (lines the user selected in the viewer), start. With `since`, the first `limit` \
        entries after it; a last line says how to get the rest."
    )]
    async fn afar_journal(
        &self,
        Parameters(p): Parameters<JournalParams>,
    ) -> Result<CallToolResult, McpError> {
        result(
            ask(
                &self.tx,
                Request::Journal {
                    since: p.since,
                    limit: p.limit.unwrap_or(100),
                },
            )
            .await,
        )
    }

    #[tool(
        description = "Commands the user ran from afar's command line: id, text, directory, \
        exit code, duration, number of output lines."
    )]
    async fn afar_commands(
        &self,
        Parameters(p): Parameters<CommandsParams>,
    ) -> Result<CallToolResult, McpError> {
        result(
            ask(
                &self.tx,
                Request::Commands {
                    limit: p.limit.unwrap_or(20),
                },
            )
            .await,
        )
    }

    #[tool(
        description = "Output of a command run from afar's command line (what the user saw on \
        the screen), with line numbers. Works for a running command too."
    )]
    async fn afar_command_output(
        &self,
        Parameters(p): Parameters<OutputParams>,
    ) -> Result<CallToolResult, McpError> {
        let request = Request::CommandOutput {
            cmd_id: p.cmd_id,
            tail: p.tail_lines,
            head: p.head_lines,
            grep: p.grep,
        };
        result(ask(&self.tx, request).await)
    }

    #[tool(
        description = "Current screen of the command running in afar (e.g. a prompt it waits on)."
    )]
    async fn afar_terminal_screen(&self) -> Result<CallToolResult, McpError> {
        result(ask(&self.tx, Request::TerminalScreen).await)
    }

    #[tool(
        description = "Show the user a directory: open it in an afar panel and optionally put \
        the cursor on a file. Use it when you refer to a file or folder. The panel becomes the \
        active one, so `active` / `passive` in the next call mean the panels after this one; \
        name them `left` / `right` to be sure."
    )]
    async fn afar_navigate(
        &self,
        Parameters(p): Parameters<NavigateParams>,
    ) -> Result<CallToolResult, McpError> {
        result(
            ask(
                &self.tx,
                Request::Navigate {
                    side: p.side,
                    path: p.path,
                    cursor: p.cursor,
                },
            )
            .await,
        )
    }

    #[tool(
        description = "Open a file in afar's viewer for the user at a line or at the first \
        match of a regular expression, optionally marking the place. The viewer comes to the \
        screen if the user is on the panels (or follows you); otherwise it opens behind and the \
        user is told. Use it to point at a place in a file."
    )]
    async fn afar_view(
        &self,
        Parameters(p): Parameters<ViewParams>,
    ) -> Result<CallToolResult, McpError> {
        result(
            ask(
                &self.tx,
                Request::View {
                    path: p.path,
                    line: p.line,
                    pattern: p.pattern,
                    highlight: p.highlight,
                },
            )
            .await,
        )
    }

    #[tool(
        description = "Mark lines in a file in afar's viewer with labels (kind: info, warning \
        or error) so the user sees them; flash to draw the eye, ttl_s to remove them later. The \
        user goes from mark to mark with Alt+Down / Alt+Up and removes them with Esc."
    )]
    async fn afar_highlight(
        &self,
        Parameters(p): Parameters<HighlightParams>,
    ) -> Result<CallToolResult, McpError> {
        result(
            ask(
                &self.tx,
                Request::Highlight {
                    path: p.path,
                    marks: p.marks,
                    flash: p.flash.unwrap_or(false),
                    ttl_s: p.ttl_s,
                    clear: p.clear.unwrap_or(false),
                },
            )
            .await,
        )
    }

    #[tool(
        description = "What the user has open in afar's viewer: the files, which one is on the \
        screen, the visible lines, the selection (lines and text) and the marks."
    )]
    async fn afar_viewer_state(&self) -> Result<CallToolResult, McpError> {
        result(ask(&self.tx, Request::ViewerState).await)
    }

    #[tool(
        description = "Select files in an afar panel so the user sees them highlighted \
        (e.g. before proposing an operation on them)."
    )]
    async fn afar_select(
        &self,
        Parameters(p): Parameters<SelectParams>,
    ) -> Result<CallToolResult, McpError> {
        let request = Request::Select {
            side: p.side,
            names: p.names,
            add: p.add.unwrap_or(false),
        };
        result(ask(&self.tx, request).await)
    }

    #[tool(
        description = "Create directories in an afar panel (no confirmation needed): names \
        relative to the panel's folder (nested allowed) or absolute. A directory created in the \
        panel's folder gets the cursor; the panel does not go elsewhere. Returns the created paths."
    )]
    async fn afar_mkdir(
        &self,
        Parameters(p): Parameters<MkDirParams>,
    ) -> Result<CallToolResult, McpError> {
        let side = p.side.unwrap_or_else(|| "active".into());
        result(
            ask(
                &self.tx,
                Request::MkDir {
                    side,
                    names: p.names,
                },
            )
            .await,
        )
    }

    #[tool(
        description = "Copy files or directories through afar: the user sees the copy dialog \
        filled in (\"requested by the agent\"), may change it and confirms; afar copies with \
        progress and asks the user about existing files. Waits for the user (up to 10 minutes); \
        returns the result or that the user declined."
    )]
    async fn afar_copy(
        &self,
        Parameters(p): Parameters<CopyParams>,
    ) -> Result<CallToolResult, McpError> {
        let request = Request::Copy {
            side: p.side.unwrap_or_else(|| "active".into()),
            names: p.names,
            dest: p.dest,
            moving: false,
        };
        result(ask_within(&self.tx, request, Duration::from_secs(600)).await)
    }

    #[tool(
        description = "Move or rename files or directories through afar, confirmed by the \
        user in the move dialog like afar_copy. For a rename, pass the new name as dest."
    )]
    async fn afar_move(
        &self,
        Parameters(p): Parameters<CopyParams>,
    ) -> Result<CallToolResult, McpError> {
        let request = Request::Copy {
            side: p.side.unwrap_or_else(|| "active".into()),
            names: p.names,
            dest: p.dest,
            moving: true,
        };
        result(ask_within(&self.tx, request, Duration::from_secs(600)).await)
    }

    #[tool(
        description = "Delete files or directories through afar: the user sees the items \
        selected in the panel and confirms in a dialog (\"requested by the agent\"). Goes to the \
        recycle bin unless permanent. Waits for the user (up to 10 minutes); returns the result \
        or that the user declined. Prefer this over deleting with shell commands."
    )]
    async fn afar_delete(
        &self,
        Parameters(p): Parameters<DeleteParams>,
    ) -> Result<CallToolResult, McpError> {
        let request = Request::Delete {
            side: p.side.unwrap_or_else(|| "active".into()),
            names: p.names,
            permanent: p.permanent.unwrap_or(false),
        };
        result(ask_within(&self.tx, request, Duration::from_secs(600)).await)
    }
}

const INSTRUCTIONS: &str = "afar is the two-panel file manager (Far Manager style) the user works \
in; you run in its bottom pane. Use afar_state/afar_journal to learn what the user did, \
afar_command_output to read output of commands they ran, afar_navigate/afar_select to show things.";

#[tool_handler]
impl ServerHandler for AfarMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(INSTRUCTIONS)
    }
}

// ------------------------------------------------------------------ server

#[derive(Clone)]
struct Shared {
    tx: Sender<AppMsg>,
    token: String,
}

async fn auth(
    State(s): State<Shared>,
    req: HttpRequest<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let ok = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| t == s.token);
    if ok {
        Ok(next.run(req).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

async fn hook(
    State(s): State<Shared>,
    Path(event): Path<String>,
    body: String,
) -> (StatusCode, String) {
    let request = match event.as_str() {
        "user-prompt" => Request::HookPrompt,
        "session-start" => Request::HookSessionStart(body),
        "pre-tool" => Request::HookPreTool(body),
        "post-tool" => Request::HookPostTool(body),
        "post-tool-failure" => Request::HookPostToolFailure(body),
        "stop" => Request::HookStop,
        "notification" => Request::HookNotification(body),
        // `afar channel` waits here for events (long polling).
        "channel-wait" => Request::ChannelWait,
        _ => return (StatusCode::NOT_FOUND, String::new()),
    };
    let limit = if matches!(request, Request::ChannelWait) {
        CHANNEL_WAIT + Duration::from_secs(5)
    } else {
        Duration::from_secs(30)
    };
    match ask_within(&s.tx, request, limit).await {
        Ok(text) => (StatusCode::OK, text),
        Err(e) => (StatusCode::SERVICE_UNAVAILABLE, e),
    }
}

/// Starts the server; returns its port.
pub fn start(tx: Sender<AppMsg>, token: String) -> anyhow::Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    std::thread::Builder::new()
        .name("mcp".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(_) => return,
            };
            rt.block_on(async move {
                let Ok(listener) = tokio::net::TcpListener::from_std(listener) else {
                    return;
                };
                let shared = Shared {
                    tx: tx.clone(),
                    token,
                };
                let factory_tx = tx.clone();
                let mcp = StreamableHttpService::new(
                    move || Ok(AfarMcp::new(factory_tx.clone())),
                    LocalSessionManager::default().into(),
                    StreamableHttpServerConfig::default(),
                );
                let app = axum::Router::new()
                    .nest_service("/mcp", mcp)
                    .route("/hook/{event}", post(hook))
                    .layer(middleware::from_fn_with_state(shared.clone(), auth))
                    .with_state(shared);
                let _ = axum::serve(listener, app).await;
            });
        })?;
    Ok(port)
}

// ----------------------------------------------------------- channel bridge

/// How long `afar channel` waits for events in one request.
pub const CHANNEL_WAIT: Duration = Duration::from_secs(25);

/// A POST to afar's server from a helper process (`afar hook`, `afar
/// channel`): the body of a 200 answer.
fn post_to_afar(path: &str, body: &str, timeout: Duration) -> Option<String> {
    use std::io::{Read, Write};
    let endpoint = std::env::var("AFAR_ENDPOINT").ok()?;
    let token = std::env::var("AFAR_TOKEN").ok()?;
    let addr = endpoint.trim_start_matches("http://").trim_end_matches('/');
    let mut stream = std::net::TcpStream::connect(addr).ok()?;
    let _ = stream.set_read_timeout(Some(timeout));
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {token}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).ok()?;
    let mut response = Vec::new();
    let _ = stream.read_to_end(&mut response);
    let response = String::from_utf8_lossy(&response).into_owned();
    let (head, body) = response.split_once("\r\n\r\n")?;
    head.starts_with("HTTP/1.1 200").then(|| body.to_string())
}

/// `afar channel`: the agent's channel server (stdio MCP, Claude Code's
/// Channels). It declares `claude/channel` and turns afar's events into
/// `notifications/claude/channel`, which start the agent's turn.
pub fn run_channel() -> anyhow::Result<()> {
    use std::io::{BufRead, Write};
    use std::sync::{Arc, Mutex};
    let out = Arc::new(Mutex::new(std::io::stdout()));
    let send = |out: &Arc<Mutex<std::io::Stdout>>, msg: serde_json::Value| {
        if let Ok(mut o) = out.lock() {
            let _ = writeln!(o, "{msg}");
            let _ = o.flush();
        }
    };
    {
        let out = out.clone();
        std::thread::spawn(move || {
            loop {
                match post_to_afar(
                    "/hook/channel-wait",
                    "",
                    CHANNEL_WAIT + Duration::from_secs(10),
                ) {
                    Some(body) => {
                        let events: Vec<serde_json::Value> =
                            serde_json::from_str(&body).unwrap_or_default();
                        for e in events {
                            send(
                                &out,
                                serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "method": "notifications/claude/channel",
                                    "params": e,
                                }),
                            );
                        }
                    }
                    // afar is gone or restarting: try again later.
                    None => std::thread::sleep(Duration::from_secs(2)),
                }
            }
        });
    }
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(req) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let Some(id) = req.get("id").cloned() else {
            continue;
        };
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let result = match method {
            "initialize" => serde_json::json!({
                "protocolVersion": req["params"]["protocolVersion"].clone(),
                "capabilities": {"experimental": {"claude/channel": {}}, "tools": {}},
                "serverInfo": {"name": "afar-channel", "version": env!("CARGO_PKG_VERSION")},
                "instructions": "Events from afar, the file manager the user works in, \
                    arrive as <channel source=\"afar-channel\">. Each is something the user \
                    asked for in afar (or afar noticed); act on it with afar's tools.",
            }),
            // One harmless tool: a server without tools is shown with a
            // warning in Claude Code's /mcp.
            "tools/list" => serde_json::json!({"tools": [{
                "name": "channel_status",
                "description": "Whether afar's event channel is connected. Events \
                    arrive by themselves as <channel source=\"afar-channel\">; there is \
                    no need to call this.",
                "inputSchema": {"type": "object", "properties": {}},
            }]}),
            "tools/call" => serde_json::json!({"content": [{
                "type": "text",
                "text": "afar's channel is connected; its events arrive as <channel source=\"afar-channel\">.",
            }]}),
            _ => serde_json::json!({}),
        };
        send(
            &out,
            serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}),
        );
    }
    Ok(())
}

// ------------------------------------------------------------- hook client

/// `afar hook <event>`: called by Claude Code; prints what afar returns.
/// Never fails loudly — a broken hook must not get in the agent's way.
pub fn run_hook(event: &str) -> anyhow::Result<()> {
    use std::io::Read;
    // Claude Code passes the hook input on stdin; drain it.
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    if let Some(body) = post_to_afar(&format!("/hook/{event}"), &input, Duration::from_secs(10))
        && !body.is_empty()
    {
        // JSON hook output: the text goes into the model's context as
        // additional context rather than as plain hook output.
        let event_name = match event {
            "session-start" => "SessionStart",
            _ => "UserPromptSubmit",
        };
        let out = serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": event_name,
                "additionalContext": body,
            }
        });
        print!("{out}");
    }
    Ok(())
}
