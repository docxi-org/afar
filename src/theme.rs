//! Far Manager's classic palette.

use ratatui::style::{Color, Style};

pub const PANEL: Style = Style::new().fg(Color::LightCyan).bg(Color::Blue);
pub const TITLE_ACTIVE: Style = Style::new().fg(Color::Black).bg(Color::Cyan);
pub const SELECTED_INFO: Style = Style::new().fg(Color::Yellow).bg(Color::Blue);
pub const KEYBAR_NUM: Style = Style::new().fg(Color::Gray).bg(Color::Black);
pub const KEYBAR_LABEL: Style = Style::new().fg(Color::Black).bg(Color::Cyan);
pub const CMDLINE: Style = Style::new().fg(Color::Gray).bg(Color::Black);
pub const MESSAGE: Style = Style::new().fg(Color::Black).bg(Color::Yellow);

pub fn entry_style(is_dir: bool, selected: bool, cursor: bool) -> Style {
    let fg = match (selected, is_dir) {
        (true, _) => Color::Yellow,
        (false, true) => Color::White,
        (false, false) => {
            if cursor {
                Color::Black
            } else {
                Color::LightCyan
            }
        }
    };
    let bg = if cursor { Color::Cyan } else { Color::Blue };
    Style::new().fg(fg).bg(bg)
}
