//! Drawing a terminal screen into a ratatui buffer.
//!
//! Drawing goes through a `Snapshot` — a copy of the visible cells — so
//! that while the program is in the middle of a synchronized frame
//! (`?2026h` … `?2026l`) the last complete frame can be shown instead of a
//! half-drawn one (see `term::Replies`).

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};

fn color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

#[derive(Clone, Default)]
pub struct Cell {
    text: String,
    fg: Color,
    bg: Color,
    modifier: Modifier,
    inverse: bool,
    /// Covered by the wide character to the left.
    wide_continuation: bool,
}

/// The visible part of a screen.
#[derive(Clone, Default)]
pub struct Snapshot {
    rows: u16,
    cols: u16,
    cells: Vec<Cell>,
    cursor: (u16, u16),
    hide_cursor: bool,
}

impl Snapshot {
    pub fn of(screen: &vt100::Screen) -> Self {
        let (rows, cols) = screen.size();
        let mut cells = Vec::with_capacity(usize::from(rows) * usize::from(cols));
        for r in 0..rows {
            for c in 0..cols {
                let cell = screen.cell(r, c).map_or_else(Cell::default, |cell| {
                    let mut modifier = Modifier::empty();
                    modifier.set(Modifier::BOLD, cell.bold());
                    modifier.set(Modifier::DIM, cell.dim());
                    modifier.set(Modifier::ITALIC, cell.italic());
                    modifier.set(Modifier::UNDERLINED, cell.underline());
                    Cell {
                        text: cell.contents().to_string(),
                        fg: color(cell.fgcolor()),
                        bg: color(cell.bgcolor()),
                        modifier,
                        inverse: cell.inverse(),
                        wide_continuation: cell.is_wide_continuation(),
                    }
                });
                cells.push(cell);
            }
        }
        Self {
            rows,
            cols,
            cells,
            cursor: screen.cursor_position(),
            hide_cursor: screen.hide_cursor(),
        }
    }

    fn cell(&self, row: u16, col: u16) -> Option<&Cell> {
        (row < self.rows && col < self.cols)
            .then(|| &self.cells[usize::from(row) * usize::from(self.cols) + usize::from(col)])
    }
}

/// Draws rows starting at `first_row` into `area` from its top; returns
/// the cursor position in `area` when it is visible. The program's default
/// colors are drawn with `defaults`.
pub fn draw_rows(
    view: &Snapshot,
    first_row: u16,
    area: Rect,
    buf: &mut Buffer,
    defaults: Style,
) -> Option<Position> {
    let default_fg = defaults.fg.unwrap_or(Color::Reset);
    let default_bg = defaults.bg.unwrap_or(Color::Reset);
    for y in 0..area.height {
        let row = first_row + y;
        for x in 0..area.width {
            let target = &mut buf[(area.x + x, area.y + y)];
            target.reset();
            target.set_style(defaults);
            let Some(cell) = view.cell(row, x) else {
                continue;
            };
            if cell.wide_continuation {
                // Covered by the wide character to the left; ratatui's diff
                // skips such cells.
                continue;
            }
            let or = |c: Color, d: Color| if c == Color::Reset { d } else { c };
            let mut fg = or(cell.fg, default_fg);
            let mut bg = or(cell.bg, default_bg);
            if cell.inverse {
                // Inverting the terminal defaults must still produce visible text.
                (fg, bg) = (or(bg, Color::Black), or(fg, Color::Gray));
            }
            target.set_symbol(if cell.text.is_empty() {
                " "
            } else {
                &cell.text
            });
            target.set_style(Style::default().fg(fg).bg(bg).add_modifier(cell.modifier));
        }
    }
    let (cy, cx) = view.cursor;
    (!view.hide_cursor && cy >= first_row && cy - first_row < area.height && cx < area.width)
        .then(|| Position::new(area.x + cx, area.y + cy - first_row))
}

/// Text of the screen rows up to the last non-blank one (or the cursor row).
pub fn screen_lines(screen: &vt100::Screen) -> Vec<String> {
    let (_, cols) = screen.size();
    let mut lines: Vec<String> = screen
        .rows(0, cols)
        .map(|l| l.trim_end().to_string())
        .collect();
    let cursor_row = usize::from(screen.cursor_position().0);
    while lines.len() > cursor_row + 1 && lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    if lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

/// Joins `(text, wrapped)` rows into logical lines.
pub fn join_wrapped(rows: impl IntoIterator<Item = (String, bool)>, out: &mut Vec<String>) {
    let mut pending = String::new();
    let mut continues = false;
    for (text, wrapped) in rows {
        if continues {
            pending.push_str(&text);
        } else {
            pending = text;
        }
        continues = wrapped;
        if !wrapped {
            out.push(std::mem::take(&mut pending).trim_end().to_string());
        }
    }
    if continues {
        out.push(pending.trim_end().to_string());
    }
}
