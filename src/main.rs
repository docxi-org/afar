use std::hash::{BuildHasher, Hasher};
use std::path::PathBuf;
use std::sync::mpsc;

use afar::app::{AgentLink, App, AppMsg, Exit};

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
    // `afar --dev`: this process supervises the app and restarts it after
    // rebuilds (see dev.rs).
    if afar::dev::requested(&args) && !afar::dev::is_child() {
        std::process::exit(afar::dev::supervise()?);
    }
    let dev = afar::dev::is_child();
    let restore = if dev { afar::dev::take_state() } else { None };

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
    if dev {
        let tx = tx.clone();
        afar::dev::spawn_watcher(move |m| {
            let _ = tx.send(AppMsg::Dev(m));
        })?;
    }

    // Raw mode, alternate screen, mouse; restored on exit and on panic.
    let mut terminal = afar::tui::Tui::init()?;
    let result =
        App::new(tx, session_dir, AgentLink { port, token }, dev, restore).run(&mut terminal, rx);
    afar::tui::restore();
    match result? {
        Exit::Quit => Ok(()),
        Exit::Restart(state) => {
            afar::dev::save_state(&state)?;
            std::process::exit(afar::dev::RESTART_EXIT_CODE);
        }
    }
}
