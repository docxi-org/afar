//! Terminal setup and frame output.
//!
//! A frame is collected in memory and written in one go, wrapped in
//! synchronized-output markers (DEC mode 2026): the terminal shows it whole
//! instead of painting whatever part has arrived. Without this a full
//! redraw (tens of kilobytes; Rust writes the Windows console in 8 KB
//! pieces) is visibly drawn bit by bit.

use std::cell::RefCell;
use std::io::{self, Write};
use std::rc::Rc;
use std::time::{Duration, Instant};

use crossterm::event::{
    DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture,
};
use crossterm::terminal::{
    BeginSynchronizedUpdate, EndSynchronizedUpdate, EnterAlternateScreen, enable_raw_mode,
};
use crossterm::{execute, queue};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Size;
use ratatui::{Frame, Terminal};

#[derive(Default)]
struct Pending {
    buf: Vec<u8>,
    /// While set, `flush` does not write: the frame is not complete yet.
    hold: bool,
}

/// The backend's writer: collects output in the shared `Pending`.
pub struct FrameBuffer(Rc<RefCell<Pending>>);

impl Write for FrameBuffer {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.0.borrow().hold {
            return Ok(());
        }
        commit(&self.0)
    }
}

fn commit(pending: &RefCell<Pending>) -> io::Result<()> {
    let mut p = pending.borrow_mut();
    let mut out = io::stdout().lock();
    out.write_all(&p.buf)?;
    out.flush()?;
    p.buf.clear();
    Ok(())
}

pub struct Tui {
    terminal: Terminal<CrosstermBackend<FrameBuffer>>,
    pending: Rc<RefCell<Pending>>,
}

/// What drawing a frame cost (for `AFAR_DEBUG_FRAMES`).
pub struct FrameStats {
    pub bytes: usize,
    pub elapsed: Duration,
}

impl Tui {
    /// Raw mode, alternate screen and mouse capture; restored by `restore`
    /// and on panic.
    pub fn init() -> io::Result<Self> {
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore();
            hook(info);
        }));
        enable_raw_mode()?;
        // Mouse capture: Shift+drag still selects text in the terminal.
        // Focus events: a bell when the agent is done while afar is in the
        // background.
        execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableFocusChange
        )?;
        let pending = Rc::new(RefCell::new(Pending {
            buf: Vec::with_capacity(256 * 1024),
            hold: false,
        }));
        let terminal = Terminal::new(CrosstermBackend::new(FrameBuffer(pending.clone())))?;
        Ok(Self { terminal, pending })
    }

    pub fn size(&self) -> io::Result<Size> {
        self.terminal.size()
    }

    /// Draws a frame and sends it to the terminal as one synchronized write.
    pub fn draw(&mut self, render: impl FnOnce(&mut Frame)) -> io::Result<FrameStats> {
        let start = Instant::now();
        self.pending.borrow_mut().hold = true;
        queue!(self.terminal.backend_mut(), BeginSynchronizedUpdate)?;
        let drawn = self.terminal.draw(render).map(|_| ());
        queue!(self.terminal.backend_mut(), EndSynchronizedUpdate)?;
        let bytes = {
            let mut p = self.pending.borrow_mut();
            p.hold = false;
            p.buf.len()
        };
        commit(&self.pending)?;
        drawn?;
        Ok(FrameStats {
            bytes,
            elapsed: start.elapsed(),
        })
    }
}

impl Tui {
    /// Writes to the terminal outside a frame (OSC: title, progress).
    pub fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut out = io::stdout().lock();
        out.write_all(bytes)?;
        out.flush()
    }
}

pub fn restore() {
    // No progress left on the taskbar button.
    let _ = io::stdout().write_all(b"\x1b]9;4;0;0\x07");
    let _ = execute!(io::stdout(), DisableMouseCapture, DisableFocusChange);
    ratatui::restore();
}
