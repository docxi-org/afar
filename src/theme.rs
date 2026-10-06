//! Far Manager's default palette (far/palette.cpp) and default file
//! highlighting (far/hilight.cpp, masks from far/config.cpp).
//!
//! Far names colors by the Windows console order (1 = blue, 6 = brown,
//! 14 = yellow); ratatui's named colors follow ANSI (3 = dark yellow,
//! 4 = blue, 11 = bright yellow). `con` maps the former to the latter, so a
//! terminal shows afar exactly as it shows Far.

use ratatui::style::{Color, Style};

/// Windows console colors by their Far names.
#[allow(dead_code)]
mod con {
    use ratatui::style::Color;
    pub const BLACK: Color = Color::Black;
    pub const BLUE: Color = Color::Blue;
    pub const GREEN: Color = Color::Green;
    pub const CYAN: Color = Color::Cyan;
    pub const RED: Color = Color::Red;
    pub const MAGENTA: Color = Color::Magenta;
    /// Dark yellow.
    pub const BROWN: Color = Color::Yellow;
    pub const LIGHTGRAY: Color = Color::Gray;
    pub const DARKGRAY: Color = Color::DarkGray;
    pub const LIGHTBLUE: Color = Color::LightBlue;
    pub const LIGHTGREEN: Color = Color::LightGreen;
    pub const LIGHTCYAN: Color = Color::LightCyan;
    pub const LIGHTRED: Color = Color::LightRed;
    pub const LIGHTMAGENTA: Color = Color::LightMagenta;
    /// Bright yellow.
    pub const YELLOW: Color = Color::LightYellow;
    pub const WHITE: Color = Color::White;
}

const fn c(fg: Color, bg: Color) -> Style {
    Style::new().fg(fg).bg(bg)
}

// Panels.
pub const PANEL_TEXT: Style = c(con::LIGHTCYAN, con::BLUE);
pub const PANEL_SELECTED_TEXT: Style = c(con::YELLOW, con::BLUE);
pub const PANEL_CURSOR: Style = c(con::BLACK, con::CYAN);
pub const PANEL_SELECTED_CURSOR: Style = c(con::YELLOW, con::CYAN);
pub const PANEL_TITLE: Style = c(con::LIGHTCYAN, con::BLUE);
pub const PANEL_TITLE_SELECTED: Style = c(con::BLACK, con::CYAN);
pub const PANEL_COLUMN_TITLE: Style = c(con::YELLOW, con::BLUE);
pub const PANEL_INFO_SELECTED: Style = c(con::YELLOW, con::CYAN);
pub const PANEL_BOX: Style = c(con::LIGHTCYAN, con::BLUE);

// Dialogs.
pub const DIALOG_TEXT: Style = c(con::BLACK, con::LIGHTGRAY);
pub const DIALOG_BOX: Style = c(con::BLACK, con::LIGHTGRAY);
pub const DIALOG_EDIT: Style = c(con::BLACK, con::CYAN);
pub const DIALOG_EDIT_UNCHANGED: Style = c(con::LIGHTGRAY, con::CYAN);
pub const DIALOG_BUTTON_SELECTED: Style = c(con::BLACK, con::CYAN);
pub const WARN_TEXT: Style = c(con::WHITE, con::RED);
pub const WARN_BOX: Style = c(con::WHITE, con::RED);
pub const WARN_BUTTON_SELECTED: Style = c(con::BLACK, con::LIGHTGRAY);
pub const DIALOG_HIGHLIGHT: Style = c(con::YELLOW, con::LIGHTGRAY);
pub const DIALOG_BUTTON_SELECTED_HIGHLIGHT: Style = c(con::YELLOW, con::CYAN);
pub const DIALOG_DISABLED: Style = c(con::DARKGRAY, con::LIGHTGRAY);
pub const DIALOG_EDIT_SELECTED: Style = c(con::WHITE, con::BLACK);
pub const DIALOG_LIST_TEXT: Style = c(con::BLACK, con::LIGHTGRAY);
pub const DIALOG_LIST_SELECTED: Style = c(con::WHITE, con::BLACK);
pub const DIALOG_LIST_HIGHLIGHT: Style = c(con::YELLOW, con::LIGHTGRAY);
pub const DIALOG_LIST_SELECTED_HIGHLIGHT: Style = c(con::YELLOW, con::BLACK);
pub const DIALOG_LIST_DISABLED: Style = c(con::DARKGRAY, con::LIGHTGRAY);
/// The drop-down list of a combo box (Dialog.Combo.*), also in warnings.
pub const COMBO_TEXT: Style = c(con::WHITE, con::CYAN);
pub const COMBO_SELECTED: Style = c(con::WHITE, con::BLACK);
pub const COMBO_HIGHLIGHT: Style = c(con::YELLOW, con::CYAN);
pub const COMBO_SELECTED_HIGHLIGHT: Style = c(con::YELLOW, con::BLACK);
pub const COMBO_BOX: Style = c(con::WHITE, con::CYAN);
pub const WARN_HIGHLIGHT: Style = c(con::YELLOW, con::RED);
pub const WARN_BUTTON_SELECTED_HIGHLIGHT: Style = c(con::YELLOW, con::LIGHTGRAY);
pub const WARN_DISABLED: Style = c(con::DARKGRAY, con::RED);
/// Far dims what is under a shadow to dark gray on black.
pub const SHADOW: Style = c(con::DARKGRAY, con::BLACK);

// Bottom bars.
pub const KEYBAR_NUM: Style = c(con::LIGHTGRAY, con::BLACK);
pub const KEYBAR_TEXT: Style = c(con::BLACK, con::CYAN);
/// The command line and the user screen use the terminal's own colors.
pub const COMMAND_LINE: Style = Style::new().fg(Color::Reset).bg(Color::Reset);
/// afar's status messages (Far shows them as dialogs): like the clock.
pub const MESSAGE: Style = c(con::BLACK, con::CYAN);

/// What file highlighting looks at.
pub struct FileAttrs<'a> {
    pub name: &'a str,
    pub is_dir: bool,
    pub hidden: bool,
    pub system: bool,
}

/// Far's default highlighting groups, first match wins: (normal, under
/// the cursor) foregrounds.
fn highlight(f: &FileAttrs) -> Option<(Color, Color)> {
    if f.hidden || f.system {
        return Some((con::CYAN, con::DARKGRAY));
    }
    if f.is_dir {
        return Some((con::WHITE, con::WHITE));
    }
    let ext = f
        .name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    if is_exec(&ext) {
        Some((con::LIGHTGREEN, con::LIGHTGREEN))
    } else if is_archive(&ext) {
        Some((con::LIGHTMAGENTA, con::LIGHTMAGENTA))
    } else if matches!(ext.as_str(), "bak" | "tmp") {
        Some((con::BROWN, con::BROWN))
    } else {
        None
    }
}

/// `<exec>`: `*.exe,*.cmd,*.bat,*.com,%PATHEXT%`.
fn is_exec(ext: &str) -> bool {
    if ext.is_empty() {
        return false;
    }
    if matches!(ext, "exe" | "cmd" | "bat" | "com") {
        return true;
    }
    std::env::var("PATHEXT").is_ok_and(|p| {
        p.split(';')
            .any(|e| e.trim_start_matches('.').eq_ignore_ascii_case(ext))
    })
}

/// `<arc>` (the common part of Far's mask).
fn is_archive(ext: &str) -> bool {
    matches!(
        ext,
        "zip"
            | "rar"
            | "7z"
            | "bz"
            | "gz"
            | "xz"
            | "lz"
            | "bzip"
            | "gzip"
            | "tar"
            | "tgz"
            | "tbz"
            | "txz"
            | "tlz"
            | "taz"
            | "z"
            | "arc"
            | "arj"
            | "bz2"
            | "cab"
            | "jar"
            | "lha"
            | "lzh"
            | "ha"
            | "cpio"
            | "rpm"
            | "zoo"
            | "zst"
            | "uc2"
            | "sit"
            | "hqx"
    ) || (ext.len() == 3
        && (ext.starts_with('r') || ext.starts_with('a'))
        && ext[1..].bytes().all(|b| b.is_ascii_digit()))
}

/// Style of a file in a panel, as Far draws it by default.
pub fn file_style(f: &FileAttrs, selected: bool, cursor: bool) -> Style {
    let base = match (selected, cursor) {
        (true, true) => PANEL_SELECTED_CURSOR,
        (true, false) => PANEL_SELECTED_TEXT,
        (false, true) => PANEL_CURSOR,
        (false, false) => PANEL_TEXT,
    };
    // The selection color wins over highlighting (Far's default groups
    // leave the selected colors unset).
    if selected {
        return base;
    }
    match highlight(f) {
        Some((normal, under_cursor)) => base.fg(if cursor { under_cursor } else { normal }),
        None => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attrs(name: &str) -> FileAttrs<'_> {
        FileAttrs {
            name,
            is_dir: false,
            hidden: false,
            system: false,
        }
    }

    #[test]
    fn highlights_like_far() {
        assert_eq!(file_style(&attrs("a.txt"), false, false), PANEL_TEXT);
        assert_eq!(
            file_style(&attrs("run.EXE"), false, false).fg,
            Some(Color::LightGreen)
        );
        assert_eq!(
            file_style(&attrs("x.zip"), false, false).fg,
            Some(Color::LightMagenta)
        );
        assert_eq!(
            file_style(&attrs("x.r01"), false, false).fg,
            Some(Color::LightMagenta)
        );
        assert_eq!(
            file_style(&attrs("x.bak"), false, false).fg,
            Some(Color::Yellow)
        );
        let dir = FileAttrs {
            name: "src",
            is_dir: true,
            hidden: false,
            system: false,
        };
        assert_eq!(file_style(&dir, false, true), PANEL_CURSOR.fg(Color::White));
        let hidden = FileAttrs {
            name: ".git",
            is_dir: true,
            hidden: true,
            system: false,
        };
        assert_eq!(file_style(&hidden, false, false).fg, Some(Color::Cyan));
        assert_eq!(file_style(&hidden, false, true).fg, Some(Color::DarkGray));
        // Selected: Far's bright yellow, not ANSI's dark yellow.
        assert_eq!(file_style(&dir, true, false).fg, Some(Color::LightYellow));
    }
}
