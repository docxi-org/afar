//! Development mode (`afar --dev`): rebuild on source changes and restart
//! in place, keeping the state.
//!
//! - The process started by `cargo run -- --dev` becomes a supervisor. It
//!   renames its own executable (Windows allows renaming a running file) so
//!   that cargo can write a new one, and runs the app from a copy in
//!   `%TEMP%\afar-dev`. When the app exits with `RESTART_EXIT_CODE`, the
//!   supervisor starts a copy of the fresh build.
//! - The app watches the sources and runs `cargo build` (the same profile)
//!   in the background; after a successful build it saves its state to a
//!   file the next instance reads, and exits with `RESTART_EXIT_CODE`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use crate::wm::Extent;

pub const RESTART_EXIT_CODE: i32 = 75;
const CHILD_ENV: &str = "AFAR_DEV_CHILD";
const STATE_ENV: &str = "AFAR_DEV_STATE";
/// Where cargo writes the executable (the supervisor's original path).
const TARGET_ENV: &str = "AFAR_DEV_TARGET";

pub fn requested(args: &[String]) -> bool {
    args.iter().any(|a| a == "--dev")
}

/// Running as the app under a dev supervisor.
pub fn is_child() -> bool {
    std::env::var_os(CHILD_ENV).is_some()
}

/// The supervisor: runs the app from a copy, restarts it on request;
/// returns the app's exit code.
pub fn supervise() -> anyhow::Result<i32> {
    let target = std::env::current_exe()?;
    let pid = std::process::id();
    let dir = target.parent().map(Path::to_path_buf).unwrap_or_default();
    let run_dir = std::env::temp_dir().join("afar-dev");
    std::fs::create_dir_all(&run_dir)?;
    // Leftovers of earlier sessions (files still running cannot be deleted).
    for d in [&dir, &run_dir] {
        if let Ok(rd) = std::fs::read_dir(d) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with("afar-dev-supervisor-") || name.starts_with("afar-run-") {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }
    // Free the build output for cargo.
    let parked = dir.join(format!("afar-dev-supervisor-{pid}.exe"));
    let parked_ok = std::fs::rename(&target, &parked).is_ok();
    let mut source = if parked_ok {
        parked.clone()
    } else {
        target.clone()
    };
    // Windows Terminal's ConPTY beside the build (tools/conpty.py) goes
    // with the copy: portable-pty looks for it next to the program. Files
    // in use by a running copy stay as they are.
    for name in ["conpty.dll", "x64/OpenConsole.exe", "arm64/OpenConsole.exe"] {
        let from = dir.join(name);
        let to = run_dir.join(name);
        if from.is_file() {
            if let Some(parent) = to.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::copy(&from, &to);
        }
    }
    let state = run_dir.join(format!("state-{pid}.json"));
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    for n in 0.. {
        let copy = run_dir.join(format!("afar-run-{pid}-{n}.exe"));
        std::fs::copy(&source, &copy)?;
        let status = Command::new(&copy)
            .args(&args)
            .env(CHILD_ENV, "1")
            .env(STATE_ENV, &state)
            .env(TARGET_ENV, &target)
            .status()?;
        let _ = std::fs::remove_file(&copy);
        if status.code() != Some(RESTART_EXIT_CODE) {
            let _ = std::fs::remove_file(&state);
            // Nothing was rebuilt: put the executable back for `cargo run`.
            if parked_ok && !target.exists() {
                let _ = std::fs::rename(&parked, &target);
            }
            return Ok(status.code().unwrap_or(1));
        }
        if target.exists() {
            source = target.clone();
        }
    }
    unreachable!()
}

/// State carried over a restart.
#[derive(Serialize, Deserialize, Default, Debug)]
pub struct DevState {
    pub panels: Vec<PanelState>,
    pub active: usize,
    pub panels_visible: bool,
    pub live: bool,
    pub splits: Vec<(u16, Extent)>,
    /// The agent's Claude Code session, resumed by the next instance.
    #[serde(default)]
    pub agent_session: Option<String>,
    /// Its folder and name.
    #[serde(default)]
    pub agent_cwd: Option<PathBuf>,
    #[serde(default)]
    pub agent_name: Option<String>,
    #[serde(default)]
    pub agent_permission_mode: Option<String>,
    /// Open viewers (restored only by a restart in development mode).
    #[serde(default)]
    pub viewers: Vec<ViewerState>,
    /// The agent pane hidden by Ctrl+O.
    #[serde(default)]
    pub agent_hidden: bool,
    /// The viewer shown, if a viewer screen was current.
    #[serde(default)]
    pub viewer_shown: Option<usize>,
    /// The session folder whose journal the next instance continues
    /// (a restart only), and how far the agent has read it.
    #[serde(default)]
    pub journal_dir: Option<PathBuf>,
    #[serde(default)]
    pub seen_seq: u64,
    /// The user screen's kept lines, the commands and the next command's
    /// number (a restart only).
    #[serde(default)]
    pub user_screen: Vec<crate::termview::Line>,
    #[serde(default)]
    pub commands: Vec<crate::app::CmdRecord>,
    #[serde(default)]
    pub next_cmd_id: u64,
    /// The next file operation's number (they go on in the journal).
    #[serde(default)]
    pub next_op_id: u64,
    /// Open editors (none modified: a restart waits for saving), and the
    /// one shown.
    #[serde(default)]
    pub editors: Vec<EditorState>,
    #[serde(default)]
    pub editor_shown: Option<usize>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct EditorState {
    pub path: PathBuf,
    pub cp: u32,
    pub line: usize,
    pub col: usize,
    pub top: usize,
    pub left: usize,
    #[serde(default)]
    pub line_numbers: bool,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ViewerState {
    pub path: PathBuf,
    pub remembered: crate::viewer::Remembered,
    pub mode: crate::viewer::Mode,
    pub wrap: bool,
    pub word_wrap: bool,
    #[serde(default)]
    pub list: Vec<PathBuf>,
}

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct PanelState {
    pub path: PathBuf,
    pub cursor: Option<String>,
    #[serde(default)]
    pub view: crate::panel::ViewMode,
    #[serde(default)]
    pub sort: crate::panel::Sort,
}

pub fn save_state(state: &DevState) -> anyhow::Result<()> {
    let path =
        std::env::var_os(STATE_ENV).ok_or_else(|| anyhow::anyhow!("not under a dev supervisor"))?;
    std::fs::write(path, serde_json::to_vec_pretty(state)?)?;
    Ok(())
}

/// The state left by the previous instance, if any (read once).
pub fn take_state() -> Option<DevState> {
    let path = std::env::var_os(STATE_ENV)?;
    let data = std::fs::read(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    serde_json::from_slice(&data).ok()
}

pub enum DevMsg {
    BuildStarted,
    BuildFinished {
        ok: bool,
        output: Vec<String>,
        duration: Duration,
    },
}

/// The project the running executable was built from.
pub fn project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Watches the sources and rebuilds after changes (on its own thread).
pub fn spawn_watcher(send: impl Fn(DevMsg) + Send + 'static) -> std::io::Result<()> {
    let root = project_root();
    let release = std::env::var_os(TARGET_ENV)
        .map(PathBuf::from)
        .is_some_and(|p| p.components().any(|c| c.as_os_str() == "release"));
    std::thread::Builder::new()
        .name("dev-watcher".into())
        .spawn(move || {
            let mut last = fingerprint(&root);
            loop {
                std::thread::sleep(Duration::from_millis(500));
                let mut now = fingerprint(&root);
                if now == last {
                    continue;
                }
                // Wait until the files stop changing (an editor saving
                // several files, the agent editing in a row).
                loop {
                    std::thread::sleep(Duration::from_millis(500));
                    let again = fingerprint(&root);
                    if again == now {
                        break;
                    }
                    now = again;
                }
                last = now;
                send(DevMsg::BuildStarted);
                let started = Instant::now();
                let mut cmd = Command::new("cargo");
                cmd.arg("build")
                    .arg("--color")
                    .arg("never")
                    .current_dir(&root);
                if release {
                    cmd.arg("--release");
                }
                let (ok, output) = match cmd.output() {
                    Ok(out) => {
                        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
                        text.push_str(&String::from_utf8_lossy(&out.stderr));
                        (
                            out.status.success(),
                            text.lines().map(str::to_string).collect(),
                        )
                    }
                    Err(e) => (false, vec![format!("cargo: {e}")]),
                };
                send(DevMsg::BuildFinished {
                    ok,
                    output,
                    duration: started.elapsed(),
                });
            }
        })?;
    Ok(())
}

/// Changes to the sources: paths, sizes and modification times.
fn fingerprint(root: &Path) -> Vec<(PathBuf, u64, Option<SystemTime>)> {
    let mut out = Vec::new();
    for name in ["Cargo.toml", "Cargo.lock"] {
        add_file(&root.join(name), &mut out);
    }
    for dir in ["src", "vendor", "examples"] {
        walk(&root.join(dir), &mut out);
    }
    out.sort();
    out
}

fn add_file(p: &Path, out: &mut Vec<(PathBuf, u64, Option<SystemTime>)>) {
    if let Ok(m) = std::fs::metadata(p) {
        out.push((p.to_path_buf(), m.len(), m.modified().ok()));
    }
}

fn walk(dir: &Path, out: &mut Vec<(PathBuf, u64, Option<SystemTime>)>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() => walk(&p, out),
            Ok(_) => add_file(&p, out),
            Err(_) => {}
        }
    }
}
