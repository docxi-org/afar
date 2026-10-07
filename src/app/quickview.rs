//! Far's quick view (Ctrl+Q, `qview.cpp`, docs/12 §11): the passive panel
//! shows the file under the active panel's cursor — its text in a viewer
//! without a status line — or, for a folder, how many folders and files
//! it holds, their size, the space they take on the disk (counted in the
//! background). A double frame, the title "Quick view", the file's type
//! (from Windows) on the separator, the name on the bottom row. Tab moves
//! the keys to it (arrows, PgUp/PgDn, Home/End scroll it, F3 opens the
//! file in the viewer, Tab or Esc go back); the mouse wheel scrolls it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::{App, AppMsg};
use crate::theme;
use crate::tr;
use crate::viewer::Viewer;

/// A folder's contents, counted through its subfolders.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DirStats {
    pub folders: u64,
    pub files: u64,
    pub bytes: u64,
    /// The files' sizes rounded up to clusters.
    pub allocated: u64,
    pub cluster: u64,
    /// Still counting.
    pub partial: bool,
}

pub(super) struct QuickView {
    /// The panel it covers.
    pub side: usize,
    /// What it shows.
    path: Option<PathBuf>,
    viewer: Option<Viewer>,
    stats: Option<DirStats>,
    /// Why the file cannot be shown.
    error: Option<String>,
    /// Stops the count of the folder shown before.
    cancel: Arc<AtomicBool>,
    /// Where the viewer was drawn (for the wheel).
    pub area: Rect,
    /// The keys go to it (Tab).
    pub focused: bool,
    /// The file's type as Windows names it.
    kind: String,
}

impl QuickView {
    fn new(side: usize) -> Self {
        Self {
            side,
            path: None,
            viewer: None,
            stats: None,
            error: None,
            cancel: Arc::new(AtomicBool::new(false)),
            area: Rect::default(),
            focused: false,
            kind: String::new(),
        }
    }
}

impl Drop for QuickView {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl App {
    /// Ctrl+Q: the quick view on the passive panel, or the panel back.
    pub(super) fn toggle_quick_view(&mut self) {
        self.quick_view = match self.quick_view {
            Some(_) => None,
            None => {
                // It replaces the information panel.
                self.info_panel = None;
                Some(QuickView::new(1 - self.active))
            }
        };
    }

    /// The quick view follows the active panel's cursor.
    fn quick_view_follow(&mut self) {
        let Some(qv) = &self.quick_view else { return };
        if qv.side == self.active {
            // Tab went to its side: it shows the other panel's cursor.
            let other = 1 - self.active;
            if let Some(q) = &mut self.quick_view {
                q.side = other;
            }
        }
        let panel = &self.panels[self.active];
        let target = panel.current().and_then(|e| {
            if e.name == ".." {
                None
            } else {
                Some((panel.path.join(&e.name), e.is_dir))
            }
        });
        let Some(qv) = &mut self.quick_view else {
            return;
        };
        let path = target.as_ref().map(|(p, _)| p.clone());
        if qv.path == path {
            return;
        }
        qv.cancel.store(true, Ordering::Relaxed);
        qv.cancel = Arc::new(AtomicBool::new(false));
        qv.path = path;
        qv.viewer = None;
        qv.stats = None;
        qv.error = None;
        qv.kind = target
            .as_ref()
            .map(|(p, dir)| shell_type_name(p, *dir))
            .unwrap_or_default();
        match target {
            Some((p, true)) => {
                qv.stats = Some(DirStats {
                    partial: true,
                    ..Default::default()
                });
                let (tx, cancel) = (self.tx.clone(), qv.cancel.clone());
                std::thread::spawn(move || count_dir(&p, &cancel, &tx));
            }
            Some((p, false)) => {
                let mut defaults = self.viewer_defaults;
                defaults.status_line = false;
                defaults.scrollbar = false;
                match Viewer::open(0, &p, &defaults, None, &self.config.viewer) {
                    Ok(v) => qv.viewer = Some(v),
                    Err(e) => qv.error = Some(e.to_string()),
                }
            }
            None => {}
        }
    }

    /// A folder's count (partial while counting, then final).
    pub(super) fn quick_view_stats(&mut self, path: PathBuf, stats: DirStats) {
        if let Some(qv) = &mut self.quick_view
            && qv.path.as_deref() == Some(path.as_path())
        {
            qv.stats = Some(stats);
        }
    }

    /// The mouse wheel over the quick view; `true`: taken.
    pub(super) fn quick_view_scroll(&mut self, column: u16, row: u16, rows: i32) -> bool {
        let Some(qv) = &mut self.quick_view else {
            return false;
        };
        if !qv
            .area
            .contains(ratatui::layout::Position::new(column, row))
        {
            return false;
        }
        if let Some(v) = &mut qv.viewer {
            v.scroll(rows);
        }
        true
    }

    /// Draws the quick view into the panel's place.
    pub(super) fn draw_quick_view(&mut self, area: Rect, buf: &mut Buffer) {
        self.quick_view_follow();
        let Some(qv) = &mut self.quick_view else {
            return;
        };
        buf.set_style(area, theme::PANEL_TEXT);
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                buf[(x, y)].set_symbol(" ");
            }
        }
        crate::panel::draw_frame(buf, area, theme::PANEL_BOX);
        let title = format!(" {} ", tr!("MQuickViewTitle"));
        let title_style = if qv.focused {
            theme::PANEL_TITLE_SELECTED
        } else {
            theme::PANEL_TITLE
        };
        crate::panel::put_title(buf, area, area.y, &title, title_style);
        if area.width < 4 || area.height < 5 {
            return;
        }
        // Far: the text in {left+1, top+1, right-1, bottom-3}; a separator
        // and the name below.
        let inner = Rect::new(area.x + 1, area.y + 1, area.width - 2, area.height - 4);
        let sep = area.bottom() - 3;
        buf[(area.x, sep)]
            .set_symbol("╟")
            .set_style(theme::PANEL_BOX);
        buf[(area.right() - 1, sep)]
            .set_symbol("╢")
            .set_style(theme::PANEL_BOX);
        for x in area.x + 1..area.right() - 1 {
            buf[(x, sep)].set_symbol("─").set_style(theme::PANEL_BOX);
        }
        qv.area = inner;
        // The type on the separator (Far: COL_PANELSELECTEDINFO).
        if !qv.kind.is_empty() {
            let t = format!(" {} ", qv.kind);
            crate::panel::put_title(buf, area, sep, &t, theme::PANEL_INFO_SELECTED);
        }
        let put = |buf: &mut Buffer, y: u16, text: &str| {
            buf.set_stringn(
                inner.x + 1,
                y,
                text,
                usize::from(inner.width.saturating_sub(2)),
                theme::PANEL_TEXT,
            );
        };
        if let Some(path) = &qv.path {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            put(buf, area.bottom() - 2, &name);
        }
        if let Some(v) = &mut qv.viewer {
            v.draw(inner, buf, 0);
        } else if let Some(e) = &qv.error {
            put(buf, inner.y + 1, e);
        } else if let (Some(path), Some(s)) = (&qv.path, qv.stats) {
            let folder = format!("{} \"{}\"", tr!("MListFolder"), path.display());
            let lines = [
                folder,
                String::new(),
                format!("{} {}", tr!("MQuickViewFolders"), group(s.folders)),
                format!("{} {}", tr!("MQuickViewFiles"), group(s.files)),
                format!("{} {}", tr!("MQuickViewBytes"), group(s.bytes)),
                format!("{} {}", tr!("MQuickViewAllocated"), group(s.allocated)),
                format!("{} {}", tr!("MQuickViewCluster"), group(s.cluster)),
                format!(
                    "{} {}",
                    tr!("MQuickViewSlack"),
                    group(s.allocated.saturating_sub(s.bytes))
                ),
                if s.partial {
                    tr!("quick-view-counting")
                } else {
                    String::new()
                },
            ];
            for (i, line) in lines.iter().enumerate() {
                let y = inner.y + 1 + i as u16;
                if y < inner.bottom() {
                    put(buf, y, line);
                }
            }
        }
    }
}

impl App {
    /// Keys while the quick view has them (Tab); `true`: taken.
    pub(super) fn quick_view_key(&mut self, key: &crossterm::event::KeyEvent) -> bool {
        use crate::command::ViewerCmd as V;
        use crossterm::event::KeyCode;
        let Some(qv) = &mut self.quick_view else {
            return false;
        };
        if !qv.focused {
            return false;
        }
        let cmd = match key.code {
            KeyCode::Tab | KeyCode::Esc => {
                qv.focused = false;
                return true;
            }
            KeyCode::F(3) => {
                let path = qv.path.clone();
                qv.focused = false;
                if let Some(p) = path.filter(|p| p.is_file()) {
                    self.record_view(&p);
                    self.open_viewer(&p, vec![p.clone()]);
                }
                return true;
            }
            KeyCode::Up => V::Up,
            KeyCode::Down => V::Down,
            KeyCode::PageUp => V::PageUp,
            KeyCode::PageDown => V::PageDown,
            KeyCode::Left => V::Left,
            KeyCode::Right => V::Right,
            KeyCode::Home => V::Home,
            KeyCode::End => V::End,
            // Other keys go to the panels as usual.
            _ => {
                qv.focused = false;
                return false;
            }
        };
        if let Some(v) = &mut qv.viewer {
            let _ = v.command(cmd);
        }
        true
    }
}

/// The file's type as Explorer names it ("Text Document", "Folder").
fn shell_type_name(path: &Path, dir: bool) -> String {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::UI::Shell::{
        SHFILEINFOW, SHGFI_TYPENAME, SHGFI_USEFILEATTRIBUTES, SHGetFileInfoW,
    };
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: zeroed plain data out-structure, valid NUL-terminated path.
    let mut info: SHFILEINFOW = unsafe { std::mem::zeroed() };
    let attrs = if dir { 0x10 } else { 0x80 };
    let ok = unsafe {
        SHGetFileInfoW(
            wide.as_ptr(),
            attrs,
            &mut info,
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_TYPENAME | SHGFI_USEFILEATTRIBUTES,
        )
    };
    if ok == 0 {
        return String::new();
    }
    let len = info.szTypeName.iter().position(|c| *c == 0).unwrap_or(0);
    String::from_utf16_lossy(&info.szTypeName[..len])
}

/// The volume's cluster size (0 when unknown).
fn cluster_size(path: &Path) -> u64 {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceW;
    let Some(root) = path.ancestors().last() else {
        return 0;
    };
    let mut root = root.as_os_str().to_os_string();
    if !root.to_string_lossy().ends_with('\\') {
        root.push("\\");
    }
    let wide: Vec<u16> = root.encode_wide().chain([0]).collect();
    let (mut spc, mut bps, mut free, mut total) = (0u32, 0u32, 0u32, 0u32);
    // SAFETY: valid out pointers, NUL-terminated root.
    let ok = unsafe { GetDiskFreeSpaceW(wide.as_ptr(), &mut spc, &mut bps, &mut free, &mut total) };
    if ok == 0 {
        0
    } else {
        u64::from(spc) * u64::from(bps)
    }
}

/// Digits in groups of three (Far's quick view numbers).
fn group(n: u64) -> String {
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

/// Counts a folder through its subfolders (links are not followed),
/// reporting now and then; stops when cancelled.
fn count_dir(dir: &Path, cancel: &AtomicBool, tx: &std::sync::mpsc::Sender<AppMsg>) {
    let mut stats = DirStats {
        partial: true,
        cluster: cluster_size(dir),
        ..Default::default()
    };
    let round = |n: u64, c: u64| if c == 0 { n } else { n.div_ceil(c) * c };
    let mut stack = vec![dir.to_path_buf()];
    let mut last = std::time::Instant::now();
    while let Some(d) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let Ok(read) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in read.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                stats.folders += 1;
                stack.push(entry.path());
            } else {
                stats.files += 1;
                stats.bytes += meta.len();
                stats.allocated += round(meta.len(), stats.cluster);
            }
        }
        if last.elapsed() > std::time::Duration::from_millis(300) {
            last = std::time::Instant::now();
            let _ = tx.send(AppMsg::QuickViewStats(dir.to_path_buf(), stats));
        }
    }
    stats.partial = false;
    let _ = tx.send(AppMsg::QuickViewStats(dir.to_path_buf(), stats));
}

#[cfg(test)]
mod tests {
    #[test]
    fn groups_of_three() {
        assert_eq!(super::group(0), "0");
        assert_eq!(super::group(1234), "1 234");
        assert_eq!(super::group(1234567), "1 234 567");
    }
}
