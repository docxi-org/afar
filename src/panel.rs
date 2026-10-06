//! File panel: directory listing, cursor, selection and Far-style drawing.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Local};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::theme;

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub selected: bool,
    pub hidden: bool,
    pub system: bool,
}

impl Entry {
    fn is_up(&self) -> bool {
        self.name == ".."
    }
}

pub struct FilePanel {
    pub path: PathBuf,
    pub entries: Vec<Entry>,
    pub cursor: usize,
    top: usize,
    pub error: Option<String>,
    /// Rows of the file list as last drawn, for mouse hit tests.
    list_area: Rect,
}

impl FilePanel {
    pub fn new(path: PathBuf) -> Self {
        let mut panel = Self {
            path,
            entries: vec![],
            cursor: 0,
            top: 0,
            error: None,
            list_area: Rect::default(),
        };
        panel.reload(None);
        panel
    }

    /// Re-reads the directory, keeping the cursor on `focus` (a name) or on
    /// the current item, and keeping the selection.
    pub fn reload(&mut self, focus: Option<&str>) {
        let keep = focus
            .map(str::to_string)
            .or_else(|| self.current().map(|e| e.name.clone()));
        let selected: std::collections::HashSet<String> = self
            .entries
            .iter()
            .filter(|e| e.selected)
            .map(|e| e.name.clone())
            .collect();
        let mut entries = Vec::new();
        self.error = None;
        match std::fs::read_dir(&self.path) {
            Ok(rd) => {
                for de in rd.flatten() {
                    let meta = de.metadata().ok();
                    let name = de.file_name().to_string_lossy().into_owned();
                    let (hidden, system) = hidden_system(&name, meta.as_ref());
                    entries.push(Entry {
                        hidden,
                        system,
                        is_dir: meta.as_ref().is_some_and(|m| m.is_dir()),
                        size: meta.as_ref().map_or(0, |m| m.len()),
                        modified: meta.as_ref().and_then(|m| m.modified().ok()),
                        selected: selected.contains(&name),
                        name,
                    });
                }
            }
            Err(e) => self.error = Some(e.to_string()),
        }
        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        if self.path.parent().is_some() {
            entries.insert(
                0,
                Entry {
                    name: "..".into(),
                    is_dir: true,
                    size: 0,
                    modified: None,
                    selected: false,
                    hidden: false,
                    system: false,
                },
            );
        }
        self.entries = entries;
        // When the item is gone (deleted), stay at the same position.
        self.cursor = keep
            .and_then(|k| self.entries.iter().position(|e| e.name == k))
            .unwrap_or(self.cursor)
            .min(self.entries.len().saturating_sub(1));
    }

    pub fn current(&self) -> Option<&Entry> {
        self.entries.get(self.cursor)
    }

    /// Changes directory; returns the previous path on success.
    pub fn change_dir(&mut self, path: &Path) -> Result<PathBuf, String> {
        let path = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
        let path = strip_verbatim(path);
        if !path.is_dir() {
            return Err(format!("{} — не каталог", path.display()));
        }
        // Coming back up, put the cursor on the directory we left.
        let focus = self
            .path
            .strip_prefix(&path)
            .ok()
            .and_then(|rest| rest.components().next())
            .map(|c| c.as_os_str().to_string_lossy().into_owned());
        let old = std::mem::replace(&mut self.path, path);
        self.entries.clear();
        self.cursor = 0;
        self.top = 0;
        self.reload(focus.as_deref());
        Ok(old)
    }

    /// Index of the item drawn at screen position `x`, `y`.
    pub fn item_at(&self, x: u16, y: u16) -> Option<usize> {
        if !self
            .list_area
            .contains(ratatui::layout::Position::new(x, y))
        {
            return None;
        }
        let i = self.top + usize::from(y - self.list_area.y);
        (i < self.entries.len()).then_some(i)
    }

    pub fn move_cursor(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        let max = self.entries.len() as isize - 1;
        self.cursor = (self.cursor as isize + delta).clamp(0, max) as usize;
    }

    pub fn set_cursor_by_name(&mut self, name: &str) -> bool {
        match self
            .entries
            .iter()
            .position(|e| e.name.eq_ignore_ascii_case(name))
        {
            Some(i) => {
                self.cursor = i;
                true
            }
            None => false,
        }
    }

    pub fn toggle_selection(&mut self) {
        if let Some(e) = self.entries.get_mut(self.cursor)
            && !e.is_up()
        {
            e.selected = !e.selected;
        }
    }

    pub fn selected(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| e.selected)
    }

    /// Sets the selection to the given names; returns how many were found.
    pub fn select_names(&mut self, names: &[String], add: bool) -> usize {
        let mut found = 0;
        for e in &mut self.entries {
            let hit = names.iter().any(|n| n.eq_ignore_ascii_case(&e.name));
            if hit && !e.is_up() {
                e.selected = true;
                found += 1;
            } else if !add {
                e.selected = false;
            }
        }
        found
    }

    pub fn draw(&mut self, area: Rect, buf: &mut Buffer, active: bool) {
        if area.width < 20 || area.height < 5 {
            return;
        }
        let frame = theme::PANEL_BOX;
        draw_frame(buf, area, frame);
        let (x0, y0, x1, y1) = (area.x, area.y, area.right() - 1, area.bottom() - 1);
        // Single separator above the status line.
        for x in x0..=x1 {
            buf[(x, y1 - 2)].set_symbol("─");
        }
        buf[(x0, y1 - 2)].set_symbol("╟");
        buf[(x1, y1 - 2)].set_symbol("╢");

        // Columns: name │ size │ date │ time.
        let inner_w = area.width - 2;
        let (size_w, date_w, time_w) = (10u16, 8u16, 5u16);
        let name_w = inner_w.saturating_sub(size_w + date_w + time_w + 3).max(8);
        let col_x = [
            x0 + 1,
            x0 + 1 + name_w + 1,
            x0 + 1 + name_w + 1 + size_w + 1,
            x1 - time_w,
        ];
        let list_top = y0 + 2;
        let list_h = (y1 - 2).saturating_sub(list_top) as usize;
        self.list_area = Rect::new(x0 + 1, list_top, inner_w, list_h as u16);
        for y in y0 + 1..y1 - 2 {
            for &cx in &col_x[1..] {
                buf[(cx - 1, y)].set_symbol("│");
            }
        }
        for &cx in &col_x[1..] {
            buf[(cx - 1, y0)].set_symbol("╤");
            buf[(cx - 1, y1 - 2)].set_symbol("┴");
        }
        let header = theme::PANEL_COLUMN_TITLE;
        put_centered(buf, col_x[0], y0 + 1, name_w, "Имя", header);
        put_centered(buf, col_x[1], y0 + 1, size_w, "Размер", header);
        put_centered(buf, col_x[2], y0 + 1, date_w, "Дата", header);
        put_centered(buf, col_x[3], y0 + 1, time_w, "Время", header);

        // Scroll so that the cursor is visible.
        if list_h > 0 {
            if self.cursor < self.top {
                self.top = self.cursor;
            } else if self.cursor >= self.top + list_h {
                self.top = self.cursor + 1 - list_h;
            }
        }
        for (i, e) in self.entries.iter().enumerate().skip(self.top).take(list_h) {
            let y = list_top + (i - self.top) as u16;
            let is_cursor = active && i == self.cursor;
            let attrs = theme::FileAttrs {
                name: &e.name,
                is_dir: e.is_dir,
                hidden: e.hidden,
                system: e.system,
            };
            let style = theme::file_style(&attrs, e.selected, is_cursor);
            put(buf, col_x[0], y, name_w, &e.name, style);
            let size = if e.is_up() {
                "Вверх".to_string()
            } else if e.is_dir {
                "Папка".to_string()
            } else {
                group_thousands(e.size)
            };
            put_right(buf, col_x[1], y, size_w, &size, style);
            let (date, time) = e.modified.map(format_time).unwrap_or_default();
            put(buf, col_x[2], y, date_w, &date, style);
            put(buf, col_x[3], y, time_w, &time, style);
            if is_cursor {
                // The cursor bar spans the column separators too.
                for &cx in &col_x[1..] {
                    buf[(cx - 1, y)].set_style(style);
                }
            }
        }

        // Title: the path, highlighted on the active panel.
        let title = format!(" {} ", self.path.display());
        let title = truncate_left(&title, (inner_w as usize).saturating_sub(2));
        let title_w = title.chars().count() as u16;
        let tx = x0 + (area.width.saturating_sub(title_w)) / 2;
        let title_style = if active {
            theme::PANEL_TITLE_SELECTED
        } else {
            theme::PANEL_TITLE
        };
        put(buf, tx, y0, title_w, &title, title_style);

        // Status line: current item.
        let status = match (&self.error, self.current()) {
            (Some(err), _) => err.clone(),
            (None, Some(e)) if e.is_up() => "..".to_string(),
            (None, Some(e)) => {
                let (date, time) = e.modified.map(format_time).unwrap_or_default();
                let size = if e.is_dir {
                    "Папка".to_string()
                } else {
                    group_thousands(e.size)
                };
                let right = format!("{size:>10} {date} {time}");
                let name_room = (inner_w as usize).saturating_sub(right.chars().count() + 1);
                format!("{:<name_room$} {right}", truncate_right(&e.name, name_room))
            }
            (None, None) => String::new(),
        };
        put(buf, x0 + 1, y1 - 1, inner_w, &status, theme::PANEL_TEXT);

        // Footer: selection summary.
        let (count, bytes) = self
            .selected()
            .fold((0, 0), |(c, b), e| (c + 1, b + e.size));
        if count > 0 {
            let text = format!(" {} байт в {} эл. ", group_thousands(bytes), count);
            let w = text.chars().count() as u16;
            put(
                buf,
                x0 + (area.width.saturating_sub(w)) / 2,
                y1 - 2,
                w,
                &text,
                theme::PANEL_INFO_SELECTED,
            );
        }
    }
}

/// Hidden and system attributes (on Unix: hidden = dot file).
fn hidden_system(name: &str, meta: Option<&std::fs::Metadata>) -> (bool, bool) {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        let _ = name;
        let attrs = meta.map_or(0, |m| m.file_attributes());
        (attrs & 0x2 != 0, attrs & 0x4 != 0)
    }
    #[cfg(not(windows))]
    {
        let _ = meta;
        (name.starts_with('.') && name != "..", false)
    }
}

/// `\\?\C:\x` → `C:\x` (what `canonicalize` returns on Windows).
pub fn strip_verbatim(path: PathBuf) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path
    }
}

/// Far-style double frame around `area`, filled with `style`.
pub fn draw_frame(buf: &mut Buffer, area: Rect, style: Style) {
    if area.width < 2 || area.height < 2 {
        return;
    }
    buf.set_style(area, style);
    let (x0, y0, x1, y1) = (area.x, area.y, area.right() - 1, area.bottom() - 1);
    for x in x0..=x1 {
        buf[(x, y0)].set_symbol("═");
        buf[(x, y1)].set_symbol("═");
    }
    for y in y0..=y1 {
        buf[(x0, y)].set_symbol("║");
        buf[(x1, y)].set_symbol("║");
    }
    buf[(x0, y0)].set_symbol("╔");
    buf[(x1, y0)].set_symbol("╗");
    buf[(x0, y1)].set_symbol("╚");
    buf[(x1, y1)].set_symbol("╝");
}

/// Writes `text` centered on row `y` of `area` (a frame title).
pub fn put_title(buf: &mut Buffer, area: Rect, y: u16, text: &str, style: Style) {
    let room = area.width.saturating_sub(4) as usize;
    let text = truncate_right(text, room);
    let w = text.chars().count() as u16;
    let x = area.x + area.width.saturating_sub(w) / 2;
    buf.set_stringn(x, y, &text, w as usize, style);
}

fn format_time(t: SystemTime) -> (String, String) {
    let t: DateTime<Local> = t.into();
    (
        t.format("%d.%m.%y").to_string(),
        t.format("%H:%M").to_string(),
    )
}

pub fn group_thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

fn truncate_right(s: &str, w: usize) -> String {
    if s.chars().count() <= w {
        s.to_string()
    } else if w == 0 {
        String::new()
    } else {
        let mut t: String = s.chars().take(w - 1).collect();
        t.push('}');
        t
    }
}

fn truncate_left(s: &str, w: usize) -> String {
    let n = s.chars().count();
    if n <= w {
        s.to_string()
    } else {
        let mut t = String::from("…");
        t.extend(s.chars().skip(n - w + 1));
        t
    }
}

/// Writes `text` into `w` cells, padding with spaces; marks overflow with `}`
/// like Far.
pub fn put(buf: &mut Buffer, x: u16, y: u16, w: u16, text: &str, style: Style) {
    let text = truncate_right(text, w as usize);
    let text = format!("{text:<width$}", width = w as usize);
    buf.set_stringn(x, y, &text, w as usize, style);
}

fn put_right(buf: &mut Buffer, x: u16, y: u16, w: u16, text: &str, style: Style) {
    let text = format!("{text:>width$}", width = w as usize);
    buf.set_stringn(x, y, &text, w as usize, style);
}

fn put_centered(buf: &mut Buffer, x: u16, y: u16, w: u16, text: &str, style: Style) {
    let n = text.chars().count() as u16;
    let pad = w.saturating_sub(n) / 2;
    buf.set_stringn(x + pad, y, text, (w - pad) as usize, style);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_thousands() {
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(4812), "4 812");
        assert_eq!(group_thousands(1234567), "1 234 567");
    }
}
