use std::hash::{BuildHasher, Hasher};
use std::path::PathBuf;
use std::sync::mpsc;

use afar::app::{AgentLink, App, AppMsg};

fn data_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("afar")
}

/// Session token for the local MCP endpoint: 256 bits from the randomly
/// keyed std hasher (no extra dependency for the prototype).
fn new_token() -> String {
    (0..4)
        .map(|i| {
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u32(i);
            h.write_u128(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos(),
            );
            format!("{:016x}", h.finish())
        })
        .collect()
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("hook") {
        return afar::mcp::run_hook(args.get(2).map_or("", String::as_str));
    }

    let session = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let session_dir = data_dir().join("sessions").join(session);

    let (tx, rx) = mpsc::channel();
    {
        let tx = tx.clone();
        std::thread::Builder::new()
            .name("input".into())
            .spawn(move || {
                while let Ok(ev) = crossterm::event::read() {
                    if tx.send(AppMsg::Input(ev)).is_err() {
                        break;
                    }
                }
            })?;
    }
    let token = new_token();
    let port = afar::mcp::start(tx.clone(), token.clone())?;

    // ratatui::init enters raw mode and the alternate screen and restores
    // the terminal on panic.
    let mut terminal = ratatui::init();
    // Mouse capture: Shift+drag still selects text in the terminal.
    crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture)?;
    let ratatui_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
        ratatui_hook(info);
    }));
    let result = App::new(tx, session_dir, AgentLink { port, token }).run(&mut terminal, rx);
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
    ratatui::restore();
    result
}
