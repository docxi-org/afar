//! Embedded terminal: a child process in a pseudo-console (ConPTY / pty)
//! whose output is parsed into a `vt100` screen.
//!
//! The reader thread always drains the PTY (a full pipe would stall the
//! child), feeds the parser, answers terminal queries (cursor position,
//! device attributes, colors) and notifies the UI through `on_output`.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use std::borrow::Cow;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::termview::Snapshot;

/// Lines kept in the emulator's scrollback.
const SCROLLBACK: usize = 5000;
/// Upper bound for one `process()` call, so that the scrolled-off line
/// capture never has to deal with huge bursts at once.
const CHUNK: usize = 4096;

/// A synchronized frame (`?2026h` … `?2026l`) that takes longer than this
/// is shown as it is: the program may have died mid-frame.
const SYNC_LIMIT: Duration = Duration::from_millis(250);

/// Terminal state outside the `vt100` screen: answers to the child's
/// queries and synchronized output.
#[derive(Default)]
pub struct Replies {
    out: Vec<u8>,
    /// The child is inside a synchronized frame since then.
    sync_since: Option<Instant>,
    /// The screen as it was when the frame started: shown until it ends.
    frozen: Option<Snapshot>,
}

impl Replies {
    fn in_sync_frame(&self) -> bool {
        self.sync_since.is_some_and(|t| t.elapsed() < SYNC_LIMIT)
    }
}

/// What to draw for a session: the live screen, or the last complete frame
/// while the program draws the next one.
pub fn view(parser: &Parser) -> Cow<'_, Snapshot> {
    match &parser.callbacks().frozen {
        Some(frozen) if parser.callbacks().in_sync_frame() => Cow::Borrowed(frozen),
        _ => Cow::Owned(Snapshot::of(parser.screen())),
    }
}

impl vt100::Callbacks for Replies {
    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        _i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        let first = params.first().and_then(|p| p.first()).copied().unwrap_or(0);
        match (i1, c, first) {
            // DSR: cursor position report. ConPTY sends it on start-up and
            // waits for the answer.
            (None, 'n', 6) => {
                let (row, col) = screen.cursor_position();
                self.out
                    .extend_from_slice(format!("\x1b[{};{}R", row + 1, col + 1).as_bytes());
            }
            // DSR: status report.
            (None, 'n', 5) => self.out.extend_from_slice(b"\x1b[0n"),
            // DA1: VT100 with advanced video option.
            (None, 'c', _) => self.out.extend_from_slice(b"\x1b[?1;2c"),
            // DA2.
            (Some(b'>'), 'c', _) => self.out.extend_from_slice(b"\x1b[>0;0;0c"),
            // Synchronized output: begin / end of a frame.
            (Some(b'?'), 'h' | 'l', _) if params.iter().any(|p| *p == [2026]) => {
                if c == 'h' {
                    if self.sync_since.is_none() {
                        self.frozen = Some(Snapshot::of(screen));
                        self.sync_since = Some(Instant::now());
                    }
                } else {
                    self.sync_since = None;
                    self.frozen = None;
                }
            }
            _ => {}
        }
    }

    fn unhandled_osc(&mut self, _screen: &mut vt100::Screen, params: &[&[u8]]) {
        // OSC 10/11 ? — default foreground / background color queries: a
        // dark terminal, so programs pick a dark-background theme.
        if let [code, b"?"] = params {
            let color = match *code {
                b"10" => "c0c0/c0c0/c0c0",
                b"11" => "0000/0000/0000",
                _ => return,
            };
            let code = std::str::from_utf8(code).unwrap_or_default();
            self.out
                .extend_from_slice(format!("\x1b]{code};rgb:{color}\x1b\\").as_bytes());
        }
    }
}

pub type Parser = vt100::Parser<Replies>;

pub struct PtySession {
    parser: Arc<Mutex<Parser>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    // Dropping the master closes the pseudo-console; done by the waiter
    // thread once the child exits (ConPTY keeps the output pipe open until
    // then).
    master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    exited: Arc<AtomicBool>,
    exit_code: Arc<Mutex<Option<u32>>>,
    scrolled: Arc<Mutex<Vec<(String, bool)>>>,
}

pub struct SpawnOptions<'a> {
    pub program: &'a str,
    pub args: &'a [String],
    pub cwd: Option<&'a std::path::Path>,
    pub env: &'a [(String, String)],
    /// Inherited environment variables to drop.
    pub env_remove: &'a [&'a str],
    pub rows: u16,
    pub cols: u16,
    /// Capture text of lines scrolled off the top (see `take_scrolled_lines`).
    pub capture_lines: bool,
}

impl PtySession {
    /// Spawns the program; `on_output` is called from a background thread
    /// after each chunk of output has been parsed and when the session ends.
    pub fn spawn(
        opts: SpawnOptions<'_>,
        on_output: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self> {
        let on_output = Arc::new(on_output);
        let pty = native_pty_system()
            .openpty(PtySize {
                rows: opts.rows,
                cols: opts.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("cannot open pseudo-console")?;

        let mut cmd = CommandBuilder::new(opts.program);
        cmd.args(opts.args);
        if let Some(cwd) = opts.cwd {
            cmd.cwd(cwd);
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        for k in opts.env_remove {
            cmd.env_remove(k);
        }
        for (k, v) in opts.env {
            cmd.env(k, v);
        }

        let mut child = pty
            .slave
            .spawn_command(cmd)
            .with_context(|| format!("cannot start {}", opts.program))?;
        drop(pty.slave);
        let killer = child.clone_killer();

        // Each session needs its own screen: ConPTY repaints from its own
        // copy of the screen and would overwrite anything drawn before it.
        let mut parser =
            Parser::new_with_callbacks(opts.rows, opts.cols, SCROLLBACK, Replies::default());
        parser.screen_mut().set_line_capture(opts.capture_lines);
        let parser = Arc::new(Mutex::new(parser));
        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(pty.master.take_writer()?));
        let mut reader = pty.master.try_clone_reader()?;

        let exited = Arc::new(AtomicBool::new(false));
        let exit_code = Arc::new(Mutex::new(None));
        let scrolled = Arc::new(Mutex::new(Vec::new()));
        let master = Arc::new(Mutex::new(Some(pty.master)));

        {
            let master = master.clone();
            let exit_code = exit_code.clone();
            let on_output = on_output.clone();
            std::thread::Builder::new()
                .name("pty-waiter".into())
                .spawn(move || {
                    let code = child.wait().map(|s| s.exit_code()).unwrap_or(u32::MAX);
                    *exit_code.lock().unwrap() = Some(code);
                    // Closing the pseudo-console flushes the last output and
                    // lets the reader see EOF.
                    drop(master.lock().unwrap().take());
                    on_output();
                })?;
        }

        {
            let parser = parser.clone();
            let writer = writer.clone();
            let exited = exited.clone();
            let scrolled = scrolled.clone();
            let on_output = on_output.clone();
            std::thread::Builder::new()
                .name("pty-reader".into())
                .spawn(move || {
                    let mut buf = [0u8; CHUNK];
                    // AFAR_PTY_RAW=<file>: the raw output, for debugging.
                    let mut raw = std::env::var_os("AFAR_PTY_RAW").and_then(|f| {
                        std::fs::File::options()
                            .create(true)
                            .append(true)
                            .open(f)
                            .ok()
                    });
                    loop {
                        let n = match reader.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => n,
                        };
                        if let Some(f) = &mut raw {
                            let _ = f.write_all(&buf[..n]);
                        }
                        let (replies, mid_frame) = {
                            let mut p = parser.lock().unwrap();
                            p.process(&buf[..n]);
                            let lines = p.screen_mut().take_scrolled_lines();
                            if !lines.is_empty() {
                                scrolled.lock().unwrap().extend(lines);
                            }
                            let mid_frame = p.callbacks().in_sync_frame();
                            (std::mem::take(&mut p.callbacks_mut().out), mid_frame)
                        };
                        if !replies.is_empty() {
                            let mut w = writer.lock().unwrap();
                            let _ = w.write_all(&replies);
                            let _ = w.flush();
                        }
                        // Nothing new to show until the frame is complete.
                        if !mid_frame {
                            on_output();
                        }
                    }
                    exited.store(true, Ordering::SeqCst);
                    on_output();
                })?;
        }

        Ok(Self {
            parser,
            writer,
            master,
            killer,
            exited,
            exit_code,
            scrolled,
        })
    }

    pub fn write(&self, bytes: &[u8]) -> Result<()> {
        let mut w = self.writer.lock().unwrap();
        w.write_all(bytes)?;
        w.flush()?;
        Ok(())
    }

    pub fn resize(&self, rows: u16, cols: u16) -> Result<()> {
        if rows == 0 || cols == 0 {
            return Ok(());
        }
        let mut p = self.parser.lock().unwrap();
        if p.screen().size() == (rows, cols) {
            return Ok(());
        }
        p.screen_mut().set_size(rows, cols);
        if let Some(master) = self.master.lock().unwrap().as_ref() {
            master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })?;
        }
        Ok(())
    }

    /// Locks the parser to read the screen.
    pub fn parser(&self) -> MutexGuard<'_, Parser> {
        self.parser.lock().unwrap()
    }

    /// Lines scrolled off the top since the previous call, `(text, wrapped)`.
    pub fn take_scrolled_lines(&self) -> Vec<(String, bool)> {
        std::mem::take(&mut *self.scrolled.lock().unwrap())
    }

    /// True once the child has exited and all its output has been parsed.
    pub fn has_exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }

    /// Exit code once the process has finished.
    pub fn exit_code(&self) -> Option<u32> {
        *self.exit_code.lock().unwrap()
    }

    pub fn kill(&mut self) {
        let _ = self.killer.kill();
    }

    /// Asks the program to exit the way a user would (Ctrl+C twice), so it
    /// can save its state; kills it if it is still running after `grace`.
    pub fn shutdown(&mut self, grace: Duration) {
        if self.has_exited() {
            return;
        }
        let start = Instant::now();
        let _ = self.write(b"");
        std::thread::sleep(Duration::from_millis(150));
        let _ = self.write(b"");
        while start.elapsed() < grace {
            if self.exit_code().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        self.kill();
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        if !self.has_exited() {
            self.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(view: &Snapshot) -> String {
        let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 10, 1));
        crate::termview::draw_rows(
            view,
            0,
            ratatui::layout::Rect::new(0, 0, 10, 1),
            &mut buf,
            ratatui::style::Style::reset(),
        );
        (0..10)
            .map(|x| buf[(x, 0)].symbol().to_string())
            .collect::<String>()
    }

    #[test]
    fn shows_last_complete_frame_during_synchronized_output() {
        let mut p = Parser::new_with_callbacks(1, 10, 0, Replies::default());
        p.process(b"old");
        p.process(b"\x1b[?2026h\r\x1b[Knew");
        assert_eq!(
            text(&view(&p)).trim_end(),
            "old",
            "mid-frame: the previous frame"
        );
        p.process(b"\x1b[?2026l");
        assert_eq!(text(&view(&p)).trim_end(), "new");
    }
}
