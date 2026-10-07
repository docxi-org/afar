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
    /// Part of a hyperlink (OSC 8).
    link: bool,
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
                    modifier.set(Modifier::SLOW_BLINK, cell.blink());
                    modifier.set(Modifier::HIDDEN, cell.hidden());
                    modifier.set(Modifier::CROSSED_OUT, cell.strikethrough());
                    Cell {
                        text: cell.contents().to_string(),
                        fg: color(cell.fgcolor()),
                        bg: color(cell.bgcolor()),
                        modifier,
                        inverse: cell.inverse(),
                        wide_continuation: cell.is_wide_continuation(),
                        link: cell.hyperlink() != 0,
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
    draw_rows_links(view, first_row, area, buf, defaults, false)
}

/// `draw_rows`; `show_links`: hyperlinks are underlined (while Ctrl is
/// held: Ctrl+click opens them).
pub fn draw_rows_links(
    view: &Snapshot,
    first_row: u16,
    area: Rect,
    buf: &mut Buffer,
    defaults: Style,
    show_links: bool,
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
            let mut modifier = cell.modifier;
            if show_links && cell.link {
                modifier |= Modifier::UNDERLINED;
                if cell.fg == Color::Reset && !cell.inverse {
                    fg = crate::theme::LINK_FG;
                }
            }
            target.set_style(Style::default().fg(fg).bg(bg).add_modifier(modifier));
        }
    }
    let (cy, cx) = view.cursor;
    (!view.hide_cursor && cy >= first_row && cy - first_row < area.height && cx < area.width)
        .then(|| Position::new(area.x + cx, area.y + cy - first_row))
}

/// Hyperlinks of a line: `(start, end, uri)` in characters of its text.
pub type Links = Vec<(usize, usize, String)>;

/// A line of a command's output as kept on the user screen: the text and
/// its hyperlinks (Ctrl+click opens them after the command has ended).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Line {
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Links,
}

impl From<String> for Line {
    fn from(text: String) -> Self {
        Self {
            text,
            links: Vec::new(),
        }
    }
}

impl Line {
    /// The text for the agent: links as `[text](address)` (the address
    /// alone when it is the text).
    pub fn for_agent(&self) -> String {
        if self.links.is_empty() {
            return self.text.clone();
        }
        let chars: Vec<char> = self.text.chars().collect();
        let mut out = String::new();
        let mut at = 0;
        let mut links: Vec<&(usize, usize, String)> = self.links.iter().collect();
        links.sort_by_key(|l| l.0);
        for (a, b, uri) in links {
            let (a, b) = ((*a).max(at).min(chars.len()), (*b).min(chars.len()));
            if a >= b {
                continue;
            }
            out.extend(&chars[at..a]);
            let text: String = chars[a..b].iter().collect();
            if text.trim() == uri {
                out.push_str(&text);
            } else {
                out.push_str(&format!("[{text}]({uri})"));
            }
            at = b;
        }
        out.extend(&chars[at..]);
        out
    }

    /// The link at display column `col` (wide characters take two).
    pub fn link_at_column(&self, col: usize) -> Option<&str> {
        use unicode_width::UnicodeWidthChar as _;
        let mut x = 0;
        let i = self.text.chars().position(|c| {
            x += c.width().unwrap_or(0);
            x > col
        })?;
        self.links
            .iter()
            .find(|(a, b, _)| (*a..*b).contains(&i))
            .map(|(_, _, u)| u.as_str())
    }
}

/// `screen_lines` with each line's hyperlinks.
pub fn screen_lines_links(screen: &vt100::Screen) -> Vec<Line> {
    let (_, cols) = screen.size();
    let mut links = screen.row_links(0, cols).into_iter();
    screen_lines(screen)
        .into_iter()
        .map(|text| Line {
            text,
            links: links.next().unwrap_or_default(),
        })
        .collect()
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

/// Joins `(text, wrapped, links)` rows into logical lines. `continues`:
/// the last line in `out` is still being wrapped (rows come in batches).
pub fn join_wrapped(
    rows: impl IntoIterator<Item = vt100::ScrolledLine>,
    out: &mut Vec<Line>,
    continues: &mut bool,
) {
    let mut pending = if *continues {
        out.pop().unwrap_or_default()
    } else {
        Line::default()
    };
    let finish = |mut line: Line| {
        line.text = line.text.trim_end().to_string();
        line
    };
    for (text, wrapped, links) in rows {
        let shift = pending.text.chars().count();
        pending
            .links
            .extend(links.into_iter().map(|(a, b, u)| (a + shift, b + shift, u)));
        pending.text.push_str(&text);
        *continues = wrapped;
        if !wrapped {
            out.push(finish(std::mem::take(&mut pending)));
        }
    }
    // A row wrapped at the end: kept as it is (its trailing spaces are
    // text) until the next batch.
    if *continues {
        out.push(pending);
    }
}

/// A line cut into rows of `width` cells, as a terminal wraps it (an
/// empty line is one empty row).
pub fn wrap_line(line: &Line, width: usize) -> Vec<Line> {
    use unicode_width::UnicodeWidthChar as _;
    let chars: Vec<char> = line.text.chars().collect();
    if width == 0 {
        return vec![line.clone()];
    }
    let slice = |a: usize, b: usize| Line {
        text: chars[a..b].iter().collect(),
        links: line
            .links
            .iter()
            .filter(|(s, e, _)| *s < b && *e > a)
            .map(|(s, e, u)| ((*s).max(a) - a, (*e).min(b) - a, u.clone()))
            .collect(),
    };
    let mut rows = Vec::new();
    let (mut start, mut w) = (0, 0);
    for (i, c) in chars.iter().enumerate() {
        let cw = c.width().unwrap_or(0);
        if w + cw > width && i > start {
            rows.push(slice(start, i));
            start = i;
            w = 0;
        }
        w += cw;
    }
    rows.push(slice(start, chars.len()));
    rows
}

/// The screen's rows as `(text, wrapped, links)`, up to the last non-blank
/// one (or the cursor row), as `screen_lines` cuts them.
pub fn screen_rows(screen: &vt100::Screen) -> Vec<vt100::ScrolledLine> {
    let (_, cols) = screen.size();
    let n = screen_lines(screen).len();
    let mut links = screen.row_links(0, cols).into_iter();
    screen
        .rows(0, cols)
        .take(n)
        .enumerate()
        .map(|(i, text)| {
            (
                text,
                screen.row_wrapped(i as u16),
                links.next().unwrap_or_default(),
            )
        })
        .collect()
}

/// A command's whole output: the lines that scrolled off (`captured`,
/// `continues` as `join_wrapped` left it) and the screen, wrapped rows
/// joined into lines.
pub fn output_lines(captured: &[Line], continues: bool, screen: &vt100::Screen) -> Vec<Line> {
    let mut out = captured.to_vec();
    let mut continues = continues;
    join_wrapped(screen_rows(screen), &mut out, &mut continues);
    if continues && let Some(last) = out.last_mut() {
        last.text = last.text.trim_end().to_string();
    }
    out
}

#[cfg(test)]
mod link_tests {
    use super::*;

    #[test]
    fn wrapped_rows_join_across_batches_and_the_screen() {
        // A row wrapped at the end of one batch goes on in the next.
        let mut out = Vec::new();
        let mut continues = false;
        join_wrapped(
            vec![("abc".to_string(), true, vec![])],
            &mut out,
            &mut continues,
        );
        join_wrapped(
            vec![("def".to_string(), false, vec![])],
            &mut out,
            &mut continues,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "abcdef");
        // On the screen: a 25-character line in 10 columns is one line.
        let mut p = vt100::Parser::new(5, 10, 0);
        p.process(b"0123456789abcdefghijKLMNO\r\nnext\r\n");
        let lines = output_lines(&[], false, p.screen());
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["0123456789abcdefghijKLMNO", "next"]);
    }

    #[test]
    fn kept_lines_keep_links() {
        let mut p = vt100::Parser::new(5, 30, 0);
        p.process(b"see \x1b]8;;file:///C:/x\x1b\\readme\x1b]8;;\x1b\\ end\r\n");
        let lines = screen_lines_links(p.screen());
        assert_eq!(lines[0].text, "see readme end");
        assert_eq!(lines[0].links, vec![(4, 10, "file:///C:/x".to_string())]);
        assert_eq!(lines[0].link_at_column(5), Some("file:///C:/x"));
        assert_eq!(lines[0].link_at_column(2), None);
    }
}
