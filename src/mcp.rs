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
    HookSessionStart,
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
    /// Return entries after this sequence number.
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
    /// Only the first N lines.
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
        code, duration, output lines — read them with afar_command_output)."
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
        the cursor on a file. Use it when you refer to a file or folder."
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
        description = "Create directories in an afar panel (no confirmation needed); the panel \
        shows the new directory. Returns the created paths."
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

async fn hook(State(s): State<Shared>, Path(event): Path<String>) -> (StatusCode, String) {
    let request = match event.as_str() {
        "user-prompt" => Request::HookPrompt,
        "session-start" => Request::HookSessionStart,
        _ => return (StatusCode::NOT_FOUND, String::new()),
    };
    match ask(&s.tx, request).await {
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

// ------------------------------------------------------------- hook client

/// `afar hook <event>`: called by Claude Code; prints what afar returns.
/// Never fails loudly — a broken hook must not get in the agent's way.
pub fn run_hook(event: &str) -> anyhow::Result<()> {
    use std::io::{Read, Write};
    // Claude Code passes the hook input on stdin; drain it.
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let (Ok(endpoint), Ok(token)) = (std::env::var("AFAR_ENDPOINT"), std::env::var("AFAR_TOKEN"))
    else {
        return Ok(());
    };
    let addr = endpoint.trim_start_matches("http://").trim_end_matches('/');
    let Ok(mut stream) = std::net::TcpStream::connect(addr) else {
        return Ok(());
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let request = format!(
        "POST /hook/{event} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {token}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{input}",
        input.len()
    );
    stream.write_all(request.as_bytes())?;
    let mut response = Vec::new();
    let _ = stream.read_to_end(&mut response);
    let response = String::from_utf8_lossy(&response);
    if let Some((head, body)) = response.split_once("\r\n\r\n")
        && head.starts_with("HTTP/1.1 200")
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
