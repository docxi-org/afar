//! Runs a program in the embedded terminal without UI and prints what the
//! emulator sees: `cargo run --example pty_probe -- <secs> <program> [args...]`.
//!
//! `PROBE_INPUT`: input steps separated by `||`, typed one per second starting
//! at a third of the time; `\r` (backslash, r) stands for Enter and `^X` for
//! Ctrl+X. `PROBE_CMD` is passed to the program as `AFAR_CMD`.

use std::time::{Duration, Instant};

use afar::term::{PtySession, SpawnOptions};

fn encode_step(step: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut chars = step.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, chars.peek().copied()) {
            ('\\', Some('r')) => {
                chars.next();
                bytes.push(b'\r');
            }
            ('^', Some(n)) if n.is_ascii_uppercase() || n == '@' || n == '[' => {
                chars.next();
                bytes.push(n as u8 & 0x1f);
            }
            _ => {
                let mut b = [0u8; 4];
                bytes.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
            }
        }
    }
    bytes
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let secs: u64 = args.next().unwrap_or("5".into()).parse()?;
    let program = args.next().unwrap_or("cmd".into());
    let rest: Vec<String> = args.collect();

    let env: Vec<(String, String)> = std::env::var("PROBE_CMD")
        .map(|c| vec![("AFAR_CMD".to_string(), c)])
        .unwrap_or_default();
    let pty = PtySession::spawn(
        SpawnOptions {
            program: &program,
            args: &rest,
            cwd: Some(&std::env::current_dir()?),
            env: &env,
            env_remove: &[],
            rows: 30,
            cols: 110,
            capture_lines: true,
        },
        || {},
    )?;

    let steps: Vec<String> = std::env::var("PROBE_INPUT")
        .map(|s| s.split("||").map(str::to_string).collect())
        .unwrap_or_default();
    let start = Instant::now();
    let mut next_step = 0;
    let mut scrolled = Vec::new();
    while start.elapsed() < Duration::from_secs(secs) && !pty.has_exited() {
        std::thread::sleep(Duration::from_millis(100));
        scrolled.extend(pty.take_scrolled_lines());
        let due = Duration::from_secs(secs / 3) + Duration::from_secs(next_step as u64);
        if next_step < steps.len() && start.elapsed() > due {
            pty.write(&encode_step(&steps[next_step]))?;
            next_step += 1;
        }
    }
    scrolled.extend(pty.take_scrolled_lines());

    let p = pty.parser();
    let screen = p.screen();
    println!(
        "=== exited: {} (code {:?}), alt screen: {}, cursor: {:?}, bracketed paste: {}",
        pty.has_exited(),
        pty.exit_code(),
        screen.alternate_screen(),
        screen.cursor_position(),
        screen.bracketed_paste()
    );
    println!("=== scrolled-off lines captured: {}", scrolled.len());
    for (line, wrapped, _) in scrolled.iter().rev().take(5).rev() {
        println!("  | {line}{}", if *wrapped { " ⏎" } else { "" });
    }
    println!("=== screen:");
    let (_, cols) = screen.size();
    for line in screen.rows(0, cols) {
        println!("  | {line}");
    }
    // PROBE_CELLS="row,col;row,col": colors of those cells.
    if let Ok(cells) = std::env::var("PROBE_CELLS") {
        for rc in cells.split(';') {
            let mut it = rc.split(',').filter_map(|n| n.trim().parse::<u16>().ok());
            if let (Some(r), Some(c)) = (it.next(), it.next())
                && let Some(cell) = screen.cell(r, c)
            {
                println!(
                    "=== cell {r},{c}: {:?} fg {:?} bg {:?}",
                    cell.contents(),
                    cell.fgcolor(),
                    cell.bgcolor()
                );
            }
        }
    }
    drop(p);
    Ok(())
}
