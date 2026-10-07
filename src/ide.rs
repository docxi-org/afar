//! Claude Code's IDE protocol (docs/11-viewer-editor.md, "Протокол IDE
//! Claude Code"): afar presents itself to the agent as an IDE. A WebSocket
//! server on 127.0.0.1 speaks MCP JSON-RPC; the lock file
//! `~/.claude/ide/<port>.lock` and `CLAUDE_CODE_SSE_PORT` let the agent find
//! it. afar → agent: `selection_changed`, `at_mentioned` (notifications);
//! agent → afar: `openDiff` (a proposed edit to accept or reject),
//! `close_tab`, `getDiagnostics`.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use serde_json::{Value, json};
use tokio::sync::{broadcast, oneshot};

use crate::app::AppMsg;

/// What the agent asks of afar through the IDE connection.
pub enum IdeMsg {
    Connected {
        pid: Option<u64>,
    },
    Disconnected,
    /// A proposed edit: answered `FILE_SAVED` (with the final text),
    /// `DIFF_REJECTED` or `TAB_CLOSED`.
    OpenDiff {
        path: PathBuf,
        new_contents: String,
        tab_name: String,
        reply: oneshot::Sender<DiffAnswer>,
    },
    CloseTab {
        tab_name: String,
    },
}

pub enum DiffAnswer {
    Saved(String),
    Rejected,
    Closed,
}

pub struct IdeServer {
    pub port: u16,
    lock: PathBuf,
    notify: broadcast::Sender<String>,
    log: Log,
}

/// `ide.log` in the session folder: connections, requests, notifications.
#[derive(Clone)]
struct Log(std::sync::Arc<std::sync::Mutex<Option<std::fs::File>>>);

impl Log {
    fn open(path: &Path) -> Self {
        let file = std::fs::File::options()
            .create(true)
            .append(true)
            .open(path)
            .ok();
        Self(std::sync::Arc::new(std::sync::Mutex::new(file)))
    }

    fn write(&self, line: &str) {
        use std::io::Write as _;
        if let Ok(mut f) = self.0.lock()
            && let Some(f) = f.as_mut()
        {
            let time = chrono::Local::now().format("%H:%M:%S%.3f");
            let line: String = line.chars().take(500).collect();
            let _ = writeln!(f, "{time} {line}");
        }
    }
}

impl IdeServer {
    /// Sends a notification to the connected agent (if any).
    pub fn notify(&self, method: &str, params: Value) {
        let msg = json!({"jsonrpc": "2.0", "method": method, "params": params});
        self.log.write(&format!("-> {msg}"));
        let _ = self.notify.send(msg.to_string());
    }
}

impl Drop for IdeServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.lock);
    }
}

/// `CLAUDE_CONFIG_DIR` or `~/.claude`.
pub fn claude_dir() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .map(|h| PathBuf::from(h).join(".claude"))
        })
        .unwrap_or_else(|| PathBuf::from(".claude"))
}

#[derive(Clone)]
struct Shared {
    tx: Sender<AppMsg>,
    token: String,
    notify: broadcast::Sender<String>,
    log: Log,
}

/// Starts the server and writes the lock file. `workspace` goes into the
/// lock file: afar's own session folder, so that other `claude` sessions
/// do not take afar for their IDE (the agent finds it by the port).
pub fn start(tx: Sender<AppMsg>, token: String, workspace: &Path) -> anyhow::Result<IdeServer> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    let (notify, _) = broadcast::channel(64);
    let log = Log::open(&workspace.join("ide.log"));
    let shared = Shared {
        tx,
        token: token.clone(),
        notify: notify.clone(),
        log: log.clone(),
    };
    std::thread::Builder::new()
        .name("ide".into())
        .spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            rt.block_on(async move {
                let Ok(listener) = tokio::net::TcpListener::from_std(listener) else {
                    return;
                };
                let app = axum::Router::new()
                    .route("/", axum::routing::get(upgrade))
                    .with_state(shared);
                let _ = axum::serve(listener, app).await;
            });
        })?;
    let dir = claude_dir().join("ide");
    std::fs::create_dir_all(&dir)?;
    let lock = dir.join(format!("{port}.lock"));
    let content = json!({
        "pid": std::process::id(),
        "workspaceFolders": [workspace.display().to_string()],
        "ideName": "afar",
        "transport": "ws",
        "authToken": token,
    });
    std::fs::write(&lock, content.to_string())?;
    log.write(&format!("listening on {port}, lock {}", lock.display()));
    Ok(IdeServer {
        port,
        lock,
        notify,
        log,
    })
}

async fn upgrade(State(s): State<Shared>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    let token = headers
        .get("x-claude-code-ide-authorization")
        .and_then(|v| v.to_str().ok());
    if token != Some(s.token.as_str()) {
        s.log.write("connection refused: bad token");
        return Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(axum::body::Body::empty())
            .unwrap_or_default();
    }
    s.log.write("connected");
    ws.protocols(["mcp"])
        .on_upgrade(move |socket| connection(socket, s))
}

async fn connection(mut socket: WebSocket, s: Shared) {
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let mut notifications = s.notify.subscribe();
    loop {
        let outgoing = tokio::select! {
            msg = socket.recv() => {
                let Some(Ok(msg)) = msg else { break };
                let Message::Text(text) = msg else { continue };
                let Ok(req) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                let (s, out) = (s.clone(), out_tx.clone());
                // Requests may wait for the user (openDiff): each on its own.
                tokio::spawn(async move {
                    if let Some(reply) = handle(&s, req).await {
                        let _ = out.send(reply.to_string());
                    }
                });
                continue;
            }
            Some(text) = out_rx.recv() => text,
            Ok(text) = notifications.recv() => text,
        };
        if socket.send(Message::Text(outgoing.into())).await.is_err() {
            break;
        }
    }
    s.log.write("disconnected");
    let _ = s.tx.send(AppMsg::Ide(IdeMsg::Disconnected));
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {"type": "object", "properties": properties, "required": required},
    })
}

fn text_result(texts: &[&str]) -> Value {
    json!({"content": texts.iter().map(|t| json!({"type": "text", "text": t})).collect::<Vec<_>>()})
}

/// Answers a JSON-RPC message (`None` for notifications).
async fn handle(s: &Shared, req: Value) -> Option<Value> {
    s.log.write(&format!("<- {req}"));
    let method = req.get("method")?.as_str()?.to_string();
    let params = req.get("params").cloned().unwrap_or(Value::Null);
    let id = req.get("id").cloned();
    let result = match method.as_str() {
        "initialize" => json!({
            "protocolVersion": params.get("protocolVersion").cloned().unwrap_or(json!("2025-06-18")),
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {"name": "afar", "version": env!("CARGO_PKG_VERSION")},
        }),
        "ide_connected" => {
            let pid = params.get("pid").and_then(Value::as_u64);
            let _ = s.tx.send(AppMsg::Ide(IdeMsg::Connected { pid }));
            return None;
        }
        "tools/list" => json!({"tools": [
            tool("openDiff", "Show a proposed edit to the user to accept or reject",
                json!({
                    "old_file_path": {"type": "string"},
                    "new_file_path": {"type": "string"},
                    "new_file_contents": {"type": "string"},
                    "tab_name": {"type": "string"},
                }),
                &["old_file_path", "new_file_path", "new_file_contents", "tab_name"]),
            tool("close_tab", "Close a diff tab", json!({"tab_name": {"type": "string"}}), &["tab_name"]),
            tool("closeAllDiffTabs", "Close all diff tabs", json!({}), &[]),
            tool("getDiagnostics", "Diagnostics of a file or of all files",
                json!({"uri": {"type": "string"}}), &[]),
        ]}),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(Value::Null);
            let arg = |k: &str| {
                args.get(k)
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string()
            };
            match name {
                "openDiff" => {
                    let (reply, answer) = oneshot::channel();
                    let tab_name = arg("tab_name");
                    let _ = s.tx.send(AppMsg::Ide(IdeMsg::OpenDiff {
                        path: PathBuf::from(arg("new_file_path")),
                        new_contents: arg("new_file_contents"),
                        tab_name: tab_name.clone(),
                        reply,
                    }));
                    match answer.await {
                        Ok(DiffAnswer::Saved(text)) => text_result(&["FILE_SAVED", &text]),
                        Ok(DiffAnswer::Rejected) => text_result(&["DIFF_REJECTED", &tab_name]),
                        Ok(DiffAnswer::Closed) | Err(_) => text_result(&["TAB_CLOSED"]),
                    }
                }
                "close_tab" => {
                    let _ = s.tx.send(AppMsg::Ide(IdeMsg::CloseTab {
                        tab_name: arg("tab_name"),
                    }));
                    text_result(&["TAB_CLOSED"])
                }
                "closeAllDiffTabs" => text_result(&["CLOSED_0_DIFF_TABS"]),
                "getDiagnostics" => text_result(&["[]"]),
                _ => return id.map(|id| error(id, -32601, &format!("unknown tool {name}"))),
            }
        }
        "ping" => json!({}),
        _ if id.is_none() => return None,
        _ => return id.map(|id| error(id, -32601, &format!("unknown method {method}"))),
    };
    id.map(|id| json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}
