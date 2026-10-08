//! File panel: directory listing, cursor, selection and drawing as Far
//! does it (far/filelist.cpp, far/panelmix.cpp; docs/09-far-ui-reference.md):
//! view modes with "stripes" of columns, column titles, the sort-mode
//! letter, the path title, the status line and the totals on the border.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Local};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::{theme, tr};

mod sort;
pub use sort::{MODES as SORT_MODES, Sort, SortMode};

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    /// A symbolic link or junction (`is_dir` tells what it points to).
    pub link: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub created: Option<SystemTime>,
    pub accessed: Option<SystemTime>,
    /// Order in which the directory listing returned it ("unsorted").
    pub position: usize,
    pub selected: bool,
    /// The selection before the last selecting command (Ctrl+M).
    pub prev_selected: bool,
    /// A folder's size, once counted (F3 on a folder, Far's CountDirSize).
    pub dir_size: Option<u64>,
    pub hidden: bool,
    pub system: bool,
}

impl Entry {
    fn is_up(&self) -> bool {
        self.name == ".."
    }
}

/// What Far's SelectFiles does with the matching items.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SelectMode {
    Add,
    Remove,
    /// Gray *: files; folders are only unselected.
    Invert,
    /// Ctrl+Gray *: folders too.
    InvertAll,
    /// Alt+Gray *: files only.
    InvertFiles,
}

/// Far's view modes (Ctrl+1 … Ctrl+4 so far).
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, Default)]
pub enum ViewMode {
    Brief,
    #[default]
    Medium,
    Full,
    Wide,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Col {
    Name,
    Size,
    Date,
    Time,
}

impl ViewMode {
    /// Columns of one stripe (width 0 = shares the rest), number of
    /// stripes, extensions aligned (config.cpp ResetViewModes).
    fn columns(self) -> (&'static [(Col, u16)], usize, bool) {
        match self {
            ViewMode::Brief => (&[(Col::Name, 0)], 3, true),
            ViewMode::Medium => (&[(Col::Name, 0)], 2, false),
            ViewMode::Full => (
                &[
                    (Col::Name, 0),
                    (Col::Size, 6),
                    (Col::Date, 8),
                    (Col::Time, 5),
                ],
                1,
                true,
            ),
            ViewMode::Wide => (&[(Col::Name, 0), (Col::Size, 6)], 1, false),
        }
    }

    pub fn from_key(n: u8) -> Option<Self> {
        match n {
            1 => Some(ViewMode::Brief),
            2 => Some(ViewMode::Medium),
            3 => Some(ViewMode::Full),
            4 => Some(ViewMode::Wide),
            _ => None,
        }
    }
}

/// A column as laid out: x, width, what, stripe, ends its stripe.
#[derive(Clone, Copy, Debug)]
struct Placed {
    x: u16,
    width: u16,
    col: Col,
    stripe: usize,
    stripe_end: bool,
}

pub struct FilePanel {
    pub path: PathBuf,
    pub entries: Vec<Entry>,
    pub cursor: usize,
    top: usize,
    pub error: Option<String>,
    pub view: ViewMode,
    pub sort: Sort,
    /// Names changed by the agent (lowercase on Windows), highlighted.
    pub agent_marked: std::collections::HashSet<String>,
    /// Cells at the right of the top border taken by the clock: the title
    /// moves left of it.
    pub clock_cells: u16,
    /// Found files shown instead of the folder (Far's temporary panel,
    /// Alt+F7 → "Panel"): full paths, and the title.
    pub list: Option<(Vec<PathBuf>, String)>,
    /// Geometry of the last drawing, for the mouse and paging.
    columns: Vec<Placed>,
    list_top: u16,
    rows: usize,
    stripes: usize,
}

impl FilePanel {
    pub fn new(path: PathBuf) -> Self {
        let mut panel = Self {
            path,
            entries: vec![],
            cursor: 0,
            top: 0,
            error: None,
            view: ViewMode::default(),
            sort: Sort::default(),
            agent_marked: Default::default(),
            clock_cells: 0,
            list: None,
            columns: Vec::new(),
            list_top: 0,
            rows: 1,
            stripes: 1,
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
        let listing: Result<Vec<(String, PathBuf)>, std::io::Error> = match &self.list {
            // The found files: names are full paths.
            Some((paths, _)) => Ok(paths
                .iter()
                .filter(|p| p.exists())
                .map(|p| (p.display().to_string(), p.clone()))
                .collect()),
            None => std::fs::read_dir(&self.path).map(|rd| {
                rd.flatten()
                    .map(|de| (de.file_name().to_string_lossy().into_owned(), de.path()))
                    .collect()
            }),
        };
        match listing {
            Ok(items) => {
                for (name, path) in items {
                    let own = std::fs::symlink_metadata(&path).ok();
                    let link = own.as_ref().is_some_and(|m| m.file_type().is_symlink());
                    // A link's kind is what it points to.
                    let meta = if link {
                        std::fs::metadata(&path).ok()
                    } else {
                        own.clone()
                    };
                    let (hidden, system) = hidden_system(&name, own.as_ref());
                    // Found files are listed whatever they are.
                    if (hidden || system) && !show_hidden() && self.list.is_none() {
                        continue;
                    }
                    entries.push(Entry {
                        hidden,
                        system,
                        link,
                        is_dir: meta.as_ref().is_some_and(|m| m.is_dir()),
                        size: meta.as_ref().map_or(0, |m| m.len()),
                        modified: meta.as_ref().and_then(|m| m.modified().ok()),
                        created: meta.as_ref().and_then(|m| m.created().ok()),
                        accessed: meta.as_ref().and_then(|m| m.accessed().ok()),
                        position: entries.len(),
                        selected: selected.contains(&name),
                        prev_selected: false,
                        dir_size: None,
                        name,
                    });
                }
            }
            Err(e) => self.error = Some(e.to_string()),
        }
        if self.path.parent().is_some() || self.list.is_some() {
            entries.insert(
                0,
                Entry {
                    name: "..".into(),
                    is_dir: true,
                    link: false,
                    size: 0,
                    modified: None,
                    created: None,
                    accessed: None,
                    position: 0,
                    selected: false,
                    prev_selected: false,
                    dir_size: None,
                    hidden: false,
                    system: false,
                },
            );
        }
        let sort = self.sort;
        entries.sort_by(|a, b| sort.compare(a, b));
        self.entries = entries;
        // When the item is gone (deleted), stay at the same position.
        self.cursor = keep
            .and_then(|k| self.entries.iter().position(|e| e.name == k))
            .unwrap_or(self.cursor)
            .min(self.entries.len().saturating_sub(1));
    }

    /// Re-sorts the listing, keeping the cursor on the same item.
    pub fn resort(&mut self) {
        let keep = self.current().map(|e| e.name.clone());
        let sort = self.sort;
        self.entries.sort_by(|a, b| sort.compare(a, b));
        if let Some(name) = keep {
            self.set_cursor_by_name(&name);
        }
    }

    /// Far's Ctrl+F3…F11: the same mode again flips the order.
    pub fn set_sort_mode(&mut self, mode: SortMode) {
        self.sort.set_mode(mode);
        self.resort();
    }

    pub fn current(&self) -> Option<&Entry> {
        self.entries.get(self.cursor)
    }

    /// Changes directory; returns the previous path on success.
    /// Shows `paths` (found files) instead of the folder, under `title`.
    pub fn show_list(&mut self, paths: Vec<PathBuf>, title: String) {
        self.list = Some((paths, title));
        self.cursor = 0;
        self.top = 0;
        self.reload(None);
    }

    /// Back from the found files to the folder.
    pub fn leave_list(&mut self) {
        self.list = None;
        self.cursor = 0;
        self.top = 0;
        self.reload(None);
    }

    pub fn change_dir(&mut self, path: &Path) -> Result<PathBuf, String> {
        self.list = None;
        let path = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
        let path = strip_verbatim(path);
        if !path.is_dir() {
            return Err(tr!("not-a-folder", path = path.display().to_string()));
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
        if y < self.list_top || usize::from(y - self.list_top) >= self.rows {
            return None;
        }
        let stripe = self
            .columns
            .iter()
            .find(|c| x >= c.x && x <= c.x + c.width)?
            .stripe;
        let i = self.top + stripe * self.rows + usize::from(y - self.list_top);
        (i < self.entries.len()).then_some(i)
    }

    pub fn move_cursor(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        let max = self.entries.len() as isize - 1;
        self.cursor = (self.cursor as isize + delta).clamp(0, max) as usize;
    }

    /// Puts the cursor on item `i` with it near the middle of the panel
    /// (Far's quick search: top = cursor - (panel height - 1) / 2).
    pub fn set_cursor_centered(&mut self, i: usize) {
        self.cursor = i.min(self.entries.len().saturating_sub(1));
        let height = self.rows + 5;
        let page = self.rows * self.stripes;
        let max_top = self.entries.len().saturating_sub(page);
        self.top = self.cursor.saturating_sub((height - 1) / 2).min(max_top);
    }

    /// Left/Right: one column over (a page of rows in a one-column mode).
    pub fn move_column(&mut self, delta: isize) {
        self.move_cursor(delta * self.rows as isize);
    }

    /// The view mode shows several columns of names.
    pub fn multi_column(&self) -> bool {
        self.stripes > 1
    }

    /// Items on a page (PgUp/PgDn).
    pub fn page(&self) -> usize {
        (self.rows * self.stripes).max(1)
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

    /// Selects or unselects item `i` (not `..`).
    pub fn set_selected(&mut self, i: usize, on: bool) {
        if let Some(e) = self.entries.get_mut(i)
            && !e.is_up()
        {
            e.selected = on;
        }
    }

    /// Where the list is on the screen: its first row and how many rows.
    pub fn list_rows(&self) -> (u16, usize) {
        (self.list_top, self.rows)
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

    /// Remembers the selection for Ctrl+M (Far's SaveSelection).
    pub fn save_selection(&mut self) {
        for e in &mut self.entries {
            e.prev_selected = e.selected;
        }
    }

    /// Ctrl+M: back to the remembered selection; the current one is
    /// remembered instead (Far's RestoreSelection).
    pub fn restore_selection(&mut self) {
        for e in &mut self.entries {
            if !e.is_up() {
                std::mem::swap(&mut e.selected, &mut e.prev_selected);
            }
        }
    }

    /// Far's SelectFiles: (un)selects the items matching `masks` (all
    /// items when inverting); folders are selected only when
    /// `select_folders` (Far's option, off by default), except by
    /// Ctrl+Gray *. Returns how many items were touched.
    pub fn select_masked(
        &mut self,
        masks: Option<&crate::masks::FileMasks>,
        mode: SelectMode,
        select_folders: bool,
    ) -> usize {
        self.save_selection();
        let inverting = matches!(
            mode,
            SelectMode::Invert | SelectMode::InvertAll | SelectMode::InvertFiles
        );
        let mut count = 0;
        for e in &mut self.entries {
            if e.is_up() || (!inverting && !masks.is_some_and(|m| m.matches(&e.name))) {
                continue;
            }
            let selection = match mode {
                SelectMode::Add => true,
                SelectMode::Remove => false,
                _ => !e.selected,
            };
            if !e.is_dir
                || (select_folders && mode != SelectMode::InvertFiles)
                || !selection
                || mode == SelectMode::InvertAll
            {
                e.selected = selection;
                count += 1;
            }
        }
        count
    }

    /// Shift+Gray + / Shift+Gray -: everything (folders by the option).
    pub fn select_all(&mut self, select: bool, select_folders: bool) {
        self.save_selection();
        for e in &mut self.entries {
            if !e.is_up() && (!select || !e.is_dir || select_folders) {
                e.selected = select;
            }
        }
    }

    /// Unselects the given names.
    pub fn unselect_names(&mut self, names: &[String]) {
        for e in &mut self.entries {
            if names.iter().any(|n| n.eq_ignore_ascii_case(&e.name)) {
                e.selected = false;
            }
        }
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

    /// Lays out the columns of the view mode in `inner` columns starting
    /// at `x` (Far's PrepareColumnWidths: fixed widths, one separator
    /// between columns, the rest shared by the auto columns).
    fn layout(&self, x: u16, inner: u16) -> Vec<Placed> {
        let (group, stripes, _) = self.view.columns();
        let all: Vec<(Col, u16, usize, bool)> = (0..stripes)
            .flat_map(|s| {
                group
                    .iter()
                    .enumerate()
                    .map(move |(i, (c, w))| (*c, *w, s, i + 1 == group.len()))
            })
            .collect();
        let fixed: u16 = all.iter().map(|c| c.1).sum();
        let seps = all.len().saturating_sub(1) as u16;
        let autos = all.iter().filter(|c| c.1 == 0).count() as u16;
        let rest = inner.saturating_sub(fixed + seps);
        let mut out = Vec::new();
        let mut pos = x;
        let mut auto_seen = 0;
        for (col, width, stripe, stripe_end) in all {
            let width = if width == 0 {
                auto_seen += 1;
                if auto_seen == autos {
                    rest - (rest / autos.max(1)) * (autos - 1)
                } else {
                    rest / autos.max(1)
                }
            } else {
                width
            };
            out.push(Placed {
                x: pos,
                width,
                col,
                stripe,
                stripe_end,
            });
            pos += width + 1;
        }
        out
    }

    pub fn draw(&mut self, area: Rect, buf: &mut Buffer, active: bool) {
        if area.width < 20 || area.height < 6 {
            return;
        }
        let (x0, y0, x1, y1) = (area.x, area.y, area.right() - 1, area.bottom() - 1);
        draw_frame(buf, area, theme::PANEL_BOX);
        buf.set_style(
            Rect::new(x0 + 1, y0 + 1, area.width - 2, area.height - 2),
            theme::PANEL_TEXT,
        );
        // Status separator: ╟───╢ above the status line.
        for x in x0 + 1..x1 {
            buf[(x, y1 - 2)].set_symbol("─");
        }
        buf[(x0, y1 - 2)].set_symbol("╟");
        buf[(x1, y1 - 2)].set_symbol("╢");

        let inner = area.width - 2;
        let columns = self.layout(x0 + 1, inner);
        let (_, stripes, align_ext) = self.view.columns();
        let list_top = y0 + 2;
        let rows = usize::from((y1 - 2).saturating_sub(list_top)).max(1);
        self.list_top = list_top;
        self.rows = rows;
        self.stripes = stripes;

        // Column separators: ║ at the end of a stripe, │ inside one; their
        // junctions with the top border and the status separator.
        for c in columns.iter().take(columns.len().saturating_sub(1)) {
            let sx = c.x + c.width;
            let (line, top, bottom) = if c.stripe_end {
                ("║", "╦", "╨")
            } else {
                ("│", "╤", "┴")
            };
            for y in y0 + 1..y1 - 2 {
                buf[(sx, y)].set_symbol(line);
            }
            buf[(sx, y0)].set_symbol(top);
            buf[(sx, y1 - 2)].set_symbol(bottom);
        }

        // Column titles.
        for c in &columns {
            let title = match c.col {
                Col::Name => tr!("MColumnName"),
                Col::Size => tr!("MColumnSize"),
                Col::Date => tr!("MColumnDate"),
                Col::Time => tr!("MColumnTime"),
            };
            put_centered(buf, c.x, y0 + 1, c.width, &title, theme::PANEL_COLUMN_TITLE);
        }
        // The sort mode letter over the first title cell: the hotkey of
        // the mode's label ("и" for "&Имя"), uppercase when reversed; "^"
        // after it when selected files go first.
        let letter = hotkey_letter(&tr!(self.sort.mode.info().label));
        let letter = if self.sort.reverse {
            letter.to_uppercase()
        } else {
            letter
        };
        buf[(x0 + 1, y0 + 1)]
            .set_symbol(&letter)
            .set_style(theme::PANEL_COLUMN_TITLE);
        if self.sort.selected_first {
            buf[(x0 + 2, y0 + 1)]
                .set_symbol("^")
                .set_style(theme::PANEL_COLUMN_TITLE);
        }

        // Keep the cursor on the page.
        let page = rows * stripes;
        if self.cursor < self.top {
            self.top = self.cursor;
        } else if self.cursor >= self.top + page {
            self.top = self.cursor + 1 - page;
        }

        for r in 0..rows {
            let y = list_top + r as u16;
            for s in 0..stripes {
                let i = self.top + s * rows + r;
                let Some(e) = self.entries.get(i) else {
                    continue;
                };
                let is_cursor = active && i == self.cursor;
                let attrs = theme::FileAttrs {
                    name: &e.name,
                    is_dir: e.is_dir,
                    hidden: e.hidden,
                    system: e.system,
                    agent: !self.agent_marked.is_empty()
                        && self.agent_marked.contains(&name_key(&e.name)),
                };
                let style = theme::file_style(&attrs, e.selected, is_cursor);
                let stripe_cols: Vec<&Placed> = columns.iter().filter(|c| c.stripe == s).collect();
                for (k, c) in stripe_cols.iter().enumerate() {
                    let text = match c.col {
                        Col::Name => {
                            draw_name(buf, c, y, e, align_ext, style);
                            continue;
                        }
                        Col::Size => size_cell(e, c.width),
                        Col::Date => e.modified.map(format_time).unwrap_or_default().0,
                        Col::Time => e.modified.map(format_time).unwrap_or_default().1,
                    };
                    put_right(buf, c.x, y, c.width, &text, style);
                    // Inside a stripe the cursor bar covers the separators
                    // too, drawn in the frame colour on its background.
                    if is_cursor && k + 1 < stripe_cols.len() {
                        let sep = &mut buf[(c.x + c.width, y)];
                        sep.set_style(style.fg(theme::PANEL_BOX.fg.unwrap_or_default()));
                    }
                }
                if is_cursor && stripe_cols.len() > 1 {
                    let first = stripe_cols[0];
                    buf[(first.x + first.width, y)]
                        .set_style(style.fg(theme::PANEL_BOX.fg.unwrap_or_default()));
                }
            }
        }

        // Title: " path " with the middle cut, centred on the top border.
        let max = usize::from(area.width.saturating_sub(1)).saturating_sub(2);
        let shown = match &self.list {
            Some((_, title)) => title.clone(),
            None => self.path.display().to_string(),
        };
        let title = format!(" {} ", truncate_path(&shown, max.saturating_sub(2)));
        let len = title.chars().count() as u16;
        let mut tx = x0 + 1 + (area.width - 2).saturating_sub(len) / 2;
        if self.clock_cells > 0 {
            let limit = (x1 + 1).saturating_sub(self.clock_cells + 1);
            if tx + len > limit {
                tx = limit.saturating_sub(len).max(x0 + 1);
            }
        }
        let title_style = if active {
            theme::PANEL_TITLE_SELECTED
        } else {
            theme::PANEL_TITLE
        };
        buf.set_stringn(tx, y0, &title, usize::from(len), title_style);

        // Status line: name, size 6, date 8, time 5, separated by spaces.
        let status = match (&self.error, self.current()) {
            (Some(err), _) => err.clone(),
            (None, Some(e)) => {
                let (date, time) = e.modified.map(format_time).unwrap_or_default();
                let right = format!("{:>6} {date:>8} {time:>5}", size_cell(e, 6));
                let name_w = usize::from(inner).saturating_sub(right.chars().count() + 1);
                // A long name is cut from the left, without a marker.
                let n = e.name.chars().count();
                let name: String = e.name.chars().skip(n.saturating_sub(name_w)).collect();
                format!("{name:<name_w$} {right}")
            }
            (None, None) => String::new(),
        };
        put(buf, x0 + 1, y1 - 1, inner, &status, theme::PANEL_TEXT);

        // Totals on the bottom border.
        let (files, dirs, bytes) =
            self.entries
                .iter()
                .filter(|e| !e.is_up())
                .fold((0, 0, 0), |(f, d, b), e| {
                    if e.is_dir {
                        (f, d + 1, b)
                    } else {
                        (f + 1, d, b + e.size)
                    }
                });
        let total = format!(
            " {} ",
            tr!(
                "MListFileSize",
                p0 = size_float(bytes),
                p1 = files,
                p2 = dirs
            )
        );
        let tw = total.chars().count() as u16;
        if tw + 4 <= area.width {
            let x = x0 + 1 + (area.width - 4 - tw) / 2;
            buf.set_stringn(x, y1, &total, usize::from(tw), theme::PANEL_BOX);
        }

        // Selection summary over the status separator.
        let (sfiles, sdirs, sbytes) = self.selected().fold((0, 0, 0), |(f, d, b), e| {
            if e.is_dir {
                (f, d + 1, b + e.dir_size.unwrap_or(0))
            } else {
                (f + 1, d, b + e.size)
            }
        });
        if sfiles + sdirs > 0 {
            let text = format!(
                " {} ",
                tr!(
                    "MListFileSize",
                    p0 = group_thousands(sbytes),
                    p1 = sfiles,
                    p2 = sdirs
                )
            );
            let w = (text.chars().count() as u16).min(area.width - 2);
            put(
                buf,
                x0 + 1 + (area.width - 2).saturating_sub(w) / 2,
                y1 - 2,
                w,
                &text,
                theme::PANEL_INFO_SELECTED,
            );
        }
        self.columns = columns;
    }
}

/// A file name in its column: extension aligned right when the mode asks
/// for it; a name that does not fit is cut and `}` put in the separator.
fn draw_name(buf: &mut Buffer, c: &Placed, y: u16, e: &Entry, align_ext: bool, style: Style) {
    let w = usize::from(c.width);
    // Found files (full paths): the name only; the status line has all.
    let name = e.name.rsplit(['\\', '/']).next().unwrap_or(&e.name);
    let split = (align_ext && !e.is_dir && !name.starts_with('.'))
        .then(|| name.rsplit_once('.'))
        .flatten()
        .filter(|(_, ext)| !ext.is_empty() && !ext.contains(' '));
    let (text, long) = match split {
        Some((stem, ext)) => {
            let ext_w = ext.chars().count().max(3);
            let stem_room = w.saturating_sub(ext_w + 1);
            let long = stem.chars().count() > stem_room;
            let stem: String = stem.chars().take(stem_room).collect();
            (format!("{stem:<stem_room$} {ext:>ext_w$}"), long)
        }
        None => {
            let long = name.chars().count() > w;
            (name.chars().take(w).collect(), long)
        }
    };
    put(buf, c.x, y, c.width, &text, style);
    if long {
        buf[(c.x + c.width, y)].set_symbol("}").set_style(style);
    }
}

/// The size column: Far's labels for folders and links, the number, or
/// the number in units when it does not fit (panelmix.cpp FormatStr_Size).
fn size_cell(e: &Entry, width: u16) -> String {
    let w = usize::from(width);
    let counted = e.is_dir && !e.link && e.dir_size.is_some();
    if (e.is_dir || e.link) && !counted {
        let label = if e.is_up() {
            tr!("MListUp")
        } else if e.link && e.is_dir {
            tr!("MListJunction")
        } else if e.link {
            tr!("MListSymlink")
        } else {
            tr!("MListFolder")
        };
        let field = [tr!("MListUp"), tr!("MListFolder")]
            .iter()
            .map(|s| s.chars().count())
            .max()
            .unwrap_or(5);
        // In brackets only when they fit: "<Папка>" at width 10, " Папка" at 6.
        return if label.chars().count() + 2 <= w {
            format!("<{label}>")
        } else {
            format!("{label:^field$}")
        };
    }
    let size = e.dir_size.unwrap_or(e.size);
    let plain = size.to_string();
    if plain.len() <= w {
        return plain;
    }
    const UNITS: [&str; 7] = [
        "MListBytes",
        "MListKb",
        "MListMb",
        "MListGb",
        "MListTb",
        "MListPb",
        "MListEb",
    ];
    let mut value = size;
    let mut unit = 0;
    while value.to_string().len() > w.saturating_sub(2) && unit + 1 < UNITS.len() {
        value /= 1024;
        unit += 1;
    }
    format!("{value} {}", tr!(UNITS[unit]))
}

/// Far's float size: "512 Б", "1,23 К", "12,3 М", "123 Г".
pub fn size_float(bytes: u64) -> String {
    const UNITS: [&str; 7] = [
        "MListBytes",
        "MListKb",
        "MListMb",
        "MListGb",
        "MListTb",
        "MListPb",
        "MListEb",
    ];
    if bytes < 1024 {
        return format!("{bytes} {}", tr!(UNITS[0]));
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    let decimals = if value < 10.0 {
        2
    } else if value < 100.0 {
        1
    } else {
        0
    };
    let number = format!("{value:.decimals$}").replace('.', crate::i18n::decimal_separator());
    format!("{number} {}", tr!(UNITS[unit]))
}

/// The hotkey letter of a label, lowercase ("&Имя" → "и").
fn hotkey_letter(label: &str) -> String {
    let mut chars = label.chars();
    while let Some(c) = chars.next() {
        if c == '&'
            && let Some(h) = chars.next()
        {
            return h.to_lowercase().to_string();
        }
    }
    label
        .chars()
        .next()
        .map(|c| c.to_lowercase().to_string())
        .unwrap_or_default()
}

/// Far's truncate_path: keeps the root, "…" in the middle.
pub fn truncate_path(p: &str, max: usize) -> String {
    let n = p.chars().count();
    if n <= max {
        return p.to_string();
    }
    let root_len = p.find(['\\', '/']).map_or(0, |i| i + 1);
    let root: String = p.chars().take(root_len).collect();
    let rest = max.saturating_sub(root.chars().count() + 1);
    let tail: String = p.chars().skip(n - rest).collect();
    format!("{root}…{tail}")
}

/// A name as compared on this system (Windows ignores case).
fn name_key(name: &str) -> String {
    if cfg!(windows) {
        name.to_lowercase()
    } else {
        name.to_string()
    }
}

/// Hidden and system files are listed (Far's `ShowHidden`, Ctrl+H; one
/// setting for both panels).
static SHOW_HIDDEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn set_show_hidden(show: bool) {
    SHOW_HIDDEN.store(show, std::sync::atomic::Ordering::Relaxed);
}

fn show_hidden() -> bool {
    SHOW_HIDDEN.load(std::sync::atomic::Ordering::Relaxed)
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
    let text: String = text.chars().take(room).collect();
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

/// Writes `text` into `w` cells, padding with spaces.
pub fn put(buf: &mut Buffer, x: u16, y: u16, w: u16, text: &str, style: Style) {
    let text: String = text.chars().take(usize::from(w)).collect();
    let text = format!("{text:<width$}", width = usize::from(w));
    buf.set_stringn(x, y, &text, usize::from(w), style);
}

fn put_right(buf: &mut Buffer, x: u16, y: u16, w: u16, text: &str, style: Style) {
    let text: String = text.chars().take(usize::from(w)).collect();
    let text = format!("{text:>width$}", width = usize::from(w));
    buf.set_stringn(x, y, &text, usize::from(w), style);
}

fn put_centered(buf: &mut Buffer, x: u16, y: u16, w: u16, text: &str, style: Style) {
    let n = text.chars().count() as u16;
    let pad = w.saturating_sub(n) / 2;
    buf.set_stringn(x + pad, y, text, usize::from(w - pad), style);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, is_dir: bool, size: u64) -> Entry {
        Entry {
            name: name.into(),
            is_dir,
            link: false,
            size,
            modified: None,
            created: None,
            accessed: None,
            position: 0,
            selected: false,
            prev_selected: false,
            dir_size: None,
            hidden: false,
            system: false,
        }
    }

    #[test]
    fn groups_thousands() {
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(4812), "4 812");
        assert_eq!(group_thousands(1234567), "1 234 567");
    }

    #[test]
    fn far_column_widths() {
        // A 40-wide panel (inner 38): Medium 18 + 19, Brief 12×3, Full 16 │ 6 │ 8 │ 5.
        let mut p = FilePanel::new(std::env::temp_dir());
        let widths = |p: &FilePanel| p.layout(1, 38).iter().map(|c| c.width).collect::<Vec<_>>();
        p.view = ViewMode::Medium;
        assert_eq!(widths(&p), vec![18, 19]);
        p.view = ViewMode::Brief;
        assert_eq!(widths(&p), vec![12, 12, 12]);
        p.view = ViewMode::Full;
        assert_eq!(widths(&p), vec![16, 6, 8, 5]);
    }

    #[test]
    fn far_size_cells() {
        crate::i18n::init("ru");
        assert_eq!(size_cell(&entry("dir", true, 0), 6), "Папка");
        assert_eq!(size_cell(&entry("..", true, 0), 6), "Вверх");
        assert_eq!(size_cell(&entry("f", false, 4812), 6), "4812");
        assert_eq!(size_cell(&entry("f", false, 12_345_678), 6), "11 М");
        assert_eq!(truncate_path(r"C:\a\b\c\d\e", 8), r"C:\…\d\e");
        assert_eq!(hotkey_letter("&Имя"), "и");
    }
}
