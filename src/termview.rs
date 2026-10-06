//! Drawing a `vt100` screen into a ratatui buffer.

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

/// Draws screen rows starting at `first_row` into `area` from its top;
/// returns the cursor position in `area` when it is visible. The program's
/// default colors are drawn with `defaults` (e.g. Far's blue panel colors).
pub fn draw_rows(
    screen: &vt100::Screen,
    first_row: u16,
    area: Rect,
    buf: &mut Buffer,
    defaults: Style,
) -> Option<Position> {
    let default_fg = defaults.fg.unwrap_or(Color::Reset);
    let default_bg = defaults.bg.unwrap_or(Color::Reset);
    let (rows, cols) = screen.size();
    for y in 0..area.height {
        let row = first_row + y;
        for x in 0..area.width {
            let pos = (area.x + x, area.y + y);
            let target = &mut buf[pos];
            target.reset();
            target.set_style(defaults);
            if row >= rows || x >= cols {
                continue;
            }
            let Some(cell) = screen.cell(row, x) else {
                continue;
            };
            if cell.is_wide_continuation() {
                // Covered by the wide character to the left; ratatui's diff
                // skips such cells.
                continue;
            }
            let or = |c: Color, d: Color| if c == Color::Reset { d } else { c };
            let mut fg = or(color(cell.fgcolor()), default_fg);
            let mut bg = or(color(cell.bgcolor()), default_bg);
            if cell.inverse() {
                // Inverting the terminal defaults must still produce visible text.
                (fg, bg) = (or(bg, Color::Black), or(fg, Color::Gray));
            }
            let mut modifier = Modifier::empty();
            modifier.set(Modifier::BOLD, cell.bold());
            modifier.set(Modifier::DIM, cell.dim());
            modifier.set(Modifier::ITALIC, cell.italic());
            modifier.set(Modifier::UNDERLINED, cell.underline());
            let contents = cell.contents();
            target.set_symbol(if contents.is_empty() { " " } else { contents });
            target.set_style(Style::default().fg(fg).bg(bg).add_modifier(modifier));
        }
    }
    let (cy, cx) = screen.cursor_position();
    (!screen.hide_cursor() && cy >= first_row && cy - first_row < area.height && cx < area.width)
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
