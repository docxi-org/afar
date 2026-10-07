//! Far's file search (Alt+F7, `findfile.cpp`): the dialog of what and
//! where to look, then the results window — found files under their
//! folders while the search goes on in the background (`crate::find`);
//! Enter goes to the file, F3 views it (the window comes back after the
//! viewer), "New search", "Stop".

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};

use super::fileops::{Overlay, Purpose};
use super::{App, AppMsg, Focus};
use crate::dialog::{Button, Dialog, check_at, combo_at, input_at, radio_at, text_at};
use crate::find::{Event, Found, Pages, Query, Scope};
use crate::theme;
use crate::tr;

/// The dialog's options, kept for the next search.
#[derive(Clone, Debug)]
pub(super) struct FindOptions {
    pub case: bool,
    pub whole_words: bool,
    pub not_containing: bool,
    pub folders: bool,
    pub links: bool,
    pub page: usize,
    pub area: usize,
    pub hex: bool,
    pub fuzzy: bool,
    pub streams: bool,
    /// "Search only in the first": `1M`, `100K`, `4096`; empty: whole files.
    pub first: String,
    /// The drive chosen with "Drive" for "From the root of".
    pub drive: Option<PathBuf>,
}

impl Default for FindOptions {
    fn default() -> Self {
        Self {
            case: false,
            whole_words: false,
            not_containing: false,
            folders: true,
            links: false,
            page: 0,
            // From the current folder.
            area: 4,
            hex: false,
            fuzzy: false,
            streams: false,
            first: String::new(),
            drive: None,
        }
    }
}

/// The search areas in the dialog's order.
const AREAS: [&str; 7] = [
    "MFindFileSearchAllDisks",
    "MFindFileSearchAllButNetwork",
    "MFindFileSearchInPATH",
    "MFindFileSearchFromRootOfDrive",
    "MFindFileSearchFromCurrent",
    "MFindFileSearchInCurrent",
    "MFindFileSearchInSelected",
];

/// The code pages offered (0: all standard ones).
fn pages() -> Vec<(String, Pages)> {
    use crate::viewer::codepage;
    let mut v = vec![(tr!("MFindFileAllCodePages"), Pages::Standard)];
    for cp in [codepage::UTF8, 1200, codepage::ansi(), codepage::oem()] {
        v.push((codepage::long_name(cp), Pages::One(cp)));
    }
    v
}

enum Line {
    Folder(PathBuf),
    /// An index into the results.
    Item(usize),
}

/// The results window.
pub(super) struct FindView {
    query: Query,
    cancel: Arc<AtomicBool>,
    results: Vec<Found>,
    lines: Vec<Line>,
    /// The folder being searched; `None` when done.
    searching: Option<PathBuf>,
    current: usize,
    top: usize,
    /// Where the list and the buttons were drawn.
    list: Rect,
    buttons: Vec<(Rect, usize)>,
}

impl FindView {
    fn running(&self) -> bool {
        self.searching.is_some()
    }

    fn add(&mut self, found: Vec<Found>) {
        for f in found {
            let dir = f.path.parent().map(PathBuf::from).unwrap_or_default();
            let new_dir = match self.lines.iter().rev().find_map(|l| match l {
                Line::Folder(d) => Some(d),
                _ => None,
            }) {
                Some(d) => *d != dir,
                None => true,
            };
            if new_dir {
                self.lines.push(Line::Folder(dir));
            }
            self.results.push(f);
            self.lines.push(Line::Item(self.results.len() - 1));
            if !matches!(self.lines.get(self.current), Some(Line::Item(_))) {
                self.current = self.lines.len() - 1;
            }
        }
    }

    fn selected(&self) -> Option<&Found> {
        match self.lines.get(self.current)? {
            Line::Item(i) => self.results.get(*i),
            Line::Folder(_) => None,
        }
    }

    /// Moves `by` lines, landing on a file.
    fn step(&mut self, by: isize) {
        let n = self.lines.len() as isize;
        if n == 0 {
            return;
        }
        let mut i = (self.current as isize + by).clamp(0, n - 1);
        let dir = if by < 0 { -1 } else { 1 };
        while (0..n).contains(&i) && matches!(self.lines[i as usize], Line::Folder(_)) {
            i += dir;
        }
        if !(0..n).contains(&i) {
            // Back the other way to the nearest file.
            i = (self.current as isize + by).clamp(0, n - 1);
            while (0..n).contains(&i) && matches!(self.lines[i as usize], Line::Folder(_)) {
                i -= dir;
            }
        }
        if (0..n).contains(&i) {
            self.current = i as usize;
        }
    }

    fn counts(&self) -> (usize, usize) {
        let dirs = self.results.iter().filter(|f| f.is_dir).count();
        (self.results.len() - dirs, dirs)
    }

    /// The buttons: label, enabled.
    fn button_labels(&self) -> Vec<(String, bool)> {
        let has = self.selected().is_some();
        vec![
            (tr!("MFindFileNewSearch"), true),
            (tr!("MFindFileGoTo"), has),
            (
                tr!("MFindFileView"),
                has && !self.selected().is_some_and(|f| f.is_dir),
            ),
            (tr!("MFindFilePanel"), !self.results.is_empty()),
            (
                if self.running() {
                    tr!("MFindFileStop")
                } else {
                    tr!("MFindFileCancel")
                },
                true,
            ),
        ]
    }
}

impl Drop for FindView {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// What a button does.
#[derive(Clone, Copy)]
enum Action {
    NewSearch,
    GoTo,
    View,
    Panel,
    StopOrClose,
}

const BUTTON_ACTIONS: [Option<Action>; 5] = [
    Some(Action::NewSearch),
    Some(Action::GoTo),
    Some(Action::View),
    Some(Action::Panel),
    Some(Action::StopOrClose),
];

impl App {
    /// Alt+F7: what and where to look (`mask` and `text` come back after
    /// the Drive and Advanced buttons).
    pub(super) fn find_dialog(&mut self, mask: &str, text: &str) {
        let o = self.find_options.clone();
        let drive = o
            .drive
            .clone()
            .or_else(|| {
                self.panels[self.active]
                    .path
                    .ancestors()
                    .last()
                    .map(PathBuf::from)
            })
            .map(|r| r.display().to_string())
            .unwrap_or_default();
        let areas: Vec<Option<String>> = AREAS
            .iter()
            .map(|id| {
                if *id == "MFindFileSearchFromRootOfDrive" {
                    Some(format!("{} {}", tr!(id), drive))
                } else {
                    Some(tr!(id))
                }
            })
            .collect();
        let page_names: Vec<Option<String>> = pages().into_iter().map(|(n, _)| Some(n)).collect();
        // Wide enough for the right column (Far's 76, or more in some
        // languages).
        let right = [
            "MFindFileArchives",
            "MFindFileFolders",
            "MFindFileSymLinks",
            "MFindFileAlternateStreams",
            "MFindFileUseFilter",
        ]
        .iter()
        .map(|id| crate::dialog::visible(&tr!(id)).chars().count() as u16)
        .max()
        .unwrap_or(0);
        let width = 76.max(41 + 4 + right + 6);
        let field = width - 10;
        let mut mask_field = input_at(5, field, mask, Some("Masks"));
        if mask.is_empty() {
            mask_field = mask_field.use_last();
        }
        let containing = tr!("MFindFileContaining");
        let text_x = 5 + crate::dialog::visible(&containing).chars().count() as u16 + 1;
        let hex_x = text_x
            + 4
            + crate::dialog::visible(&tr!("MFindFileText"))
                .chars()
                .count() as u16
            + 2;
        let mut d = Dialog::far(tr!("MFindFileTitle"), width);
        let group = d.new_group();
        let dialog = d
            .text(tr!("MFindFileMasks"))
            .row(vec![mask_field])
            .separator()
            .row(vec![
                text_at(5, containing),
                radio_at(text_x, tr!("MFindFileText"), !o.hex, group),
                radio_at(hex_x, tr!("MFindFileHex"), o.hex, group),
            ])
            .row(vec![input_at(5, field, text, Some("SearchText"))])
            .text(tr!("MFindFileCodePage"))
            .row(vec![combo_at(5, field, page_names, o.page)])
            .separator()
            .row(vec![
                check_at(5, tr!("MFindFileCase"), o.case),
                check_at(41, tr!("MFindFileArchives"), false).disabled(),
            ])
            .row(vec![
                check_at(5, tr!("MFindFileWholeWords"), o.whole_words),
                check_at(41, tr!("MFindFileFolders"), o.folders),
            ])
            .row(vec![
                check_at(5, tr!("MFindFileFuzzy"), o.fuzzy),
                check_at(41, tr!("MFindFileSymLinks"), o.links),
            ])
            .row(vec![
                check_at(5, tr!("MFindFileNotContaining"), o.not_containing),
                check_at(41, tr!("MFindFileAlternateStreams"), o.streams),
            ])
            .separator()
            .text(tr!("MFindFileSearchArea"))
            .row(vec![
                combo_at(5, 32, areas, o.area),
                check_at(41, tr!("MFindFileUseFilter"), false).disabled(),
            ])
            .separator()
            .button_row(vec![
                Button::new(tr!("MFindFileFind")).default(),
                Button::new(tr!("MFindFileDrive")),
                Button::new(tr!("MFindFileSetFilter")).disabled(),
                Button::new(tr!("MFindFileAdvanced")),
                Button::new(tr!("MCancel")),
            ]);
        self.overlays.push(Overlay::Dialog {
            dialog,
            purpose: Purpose::FindAsk,
        });
    }

    /// The dialog's options into `find_options`; its mask and text.
    fn find_read(&mut self, dialog: &Dialog) -> (String, String) {
        let o = &mut self.find_options;
        o.case = dialog.checked(0);
        o.whole_words = dialog.checked(2);
        o.folders = dialog.checked(3);
        o.fuzzy = dialog.checked(4);
        o.links = dialog.checked(5);
        o.not_containing = dialog.checked(6);
        o.streams = dialog.checked(7);
        o.hex = dialog.radio(0) == 1;
        o.page = dialog.combo(0);
        o.area = dialog.combo(1);
        (dialog.input_value(0), dialog.input_value(1))
    }

    /// A button of the dialog: Find, Drive, Advanced.
    pub(super) fn find_ask_closed(&mut self, dialog: &Dialog, button: Option<usize>) {
        let (mask, text) = self.find_read(dialog);
        match button {
            Some(0) => self.find_start(mask, text),
            Some(1) => self.find_drive_menu(mask, text),
            Some(3) => {
                let dialog = Dialog::far(tr!("MFindFileAdvancedTitle"), 56)
                    .text(tr!("MFindFileSearchFirst"))
                    .row(vec![input_at(5, 46, self.find_options.first.clone(), None)])
                    .separator()
                    .buttons(&[&tr!("MOk"), &tr!("MCancel")], 0);
                self.overlays.push(Overlay::Dialog {
                    dialog,
                    purpose: Purpose::FindAdvanced { mask, text },
                });
            }
            _ => {}
        }
    }

    /// The advanced options closed: back to the dialog.
    pub(super) fn find_advanced_closed(
        &mut self,
        dialog: &Dialog,
        button: Option<usize>,
        mask: String,
        text: String,
    ) {
        if button == Some(0) {
            let first = dialog.input_value(0).trim().to_string();
            if !first.is_empty() && parse_size(&first).is_none() {
                self.say(tr!("find-bad-size", size = first.as_str()));
            } else {
                self.find_options.first = first;
            }
        }
        self.find_dialog(&mask, &text);
    }

    /// "Drive": the drive for "From the root of"; back to the dialog.
    fn find_drive_menu(&mut self, mask: String, text: String) {
        let drives = crate::drives::list();
        let items: Vec<crate::menu::Item> = drives
            .iter()
            .map(|d| crate::menu::Item::new(format!("&{}:", d.letter)))
            .collect();
        let roots = drives.into_iter().map(|d| d.root).collect();
        let menu = crate::menu::Menu::new(tr!("MFindFileDrive").replace('&', ""), items);
        self.overlays.push(Overlay::Menu {
            menu,
            purpose: super::panelcmds::MenuPurpose::FindDrive { roots, mask, text },
        });
    }

    /// A drive chosen (or not): back to the dialog.
    pub(super) fn find_drive_chosen(&mut self, root: Option<PathBuf>, mask: &str, text: &str) {
        if let Some(root) = root {
            self.find_options.drive = Some(root);
            self.find_options.area = AREAS
                .iter()
                .position(|a| *a == "MFindFileSearchFromRootOfDrive")
                .unwrap_or(self.find_options.area);
        }
        self.find_dialog(mask, text);
    }

    /// "Find": the search starts, the results window opens.
    fn find_start(&mut self, mask: String, text: String) {
        let o = self.find_options.clone();
        if o.hex && !text.is_empty() && crate::find::parse_hex(&text).is_none() {
            self.say(tr!("find-bad-hex"));
            self.find_dialog(&mask, &text);
            return;
        }
        let panel = &self.panels[self.active];
        let here = panel.path.clone();
        let scope = match AREAS.get(o.area).copied() {
            Some(id @ ("MFindFileSearchAllDisks" | "MFindFileSearchAllButNetwork")) => {
                use crate::drives::DriveKind as K;
                let network = id == "MFindFileSearchAllDisks";
                Scope::Trees(
                    crate::drives::list()
                        .into_iter()
                        .filter(|d| {
                            matches!(d.kind, K::Fixed | K::Ram | K::Subst)
                                || network && d.kind == K::Network
                        })
                        .map(|d| d.root)
                        .collect(),
                )
            }
            Some("MFindFileSearchInPATH") => Scope::Trees(
                std::env::var_os("PATH")
                    .map(|p| std::env::split_paths(&p).filter(|d| d.is_dir()).collect())
                    .unwrap_or_default(),
            ),
            Some("MFindFileSearchFromRootOfDrive") => Scope::Trees(vec![
                o.drive
                    .clone()
                    .or_else(|| here.ancestors().last().map(PathBuf::from))
                    .unwrap_or(here.clone()),
            ]),
            Some("MFindFileSearchInCurrent") => Scope::Folder(here.clone()),
            Some("MFindFileSearchInSelected") => {
                let dirs: Vec<PathBuf> = panel
                    .selected()
                    .filter(|e| e.is_dir)
                    .map(|e| here.join(&e.name))
                    .collect();
                if dirs.is_empty() {
                    Scope::Trees(vec![here.clone()])
                } else {
                    Scope::Trees(dirs)
                }
            }
            _ => Scope::Trees(vec![here.clone()]),
        };
        let page = pages().get(o.page).map_or(Pages::Standard, |(_, p)| *p);
        let query = Query {
            masks: mask,
            text,
            pages: page,
            case: o.case,
            whole_words: o.whole_words,
            not_containing: o.not_containing,
            folders: o.folders,
            links: o.links,
            scope,
            hex: o.hex,
            fuzzy: o.fuzzy,
            streams: o.streams,
            first_bytes: parse_size(&o.first),
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let tx = self.tx.clone();
        crate::find::start(query.clone(), cancel.clone(), move |e| {
            let _ = tx.send(AppMsg::Find(e));
        });
        self.find_parked = None;
        self.overlays.push(Overlay::Find(Box::new(FindView {
            query,
            cancel,
            results: Vec::new(),
            lines: Vec::new(),
            searching: Some(here),
            current: 0,
            top: 0,
            list: Rect::default(),
            buttons: Vec::new(),
        })));
    }

    /// The results window, wherever it is (shown, or parked under a
    /// viewer).
    fn find_view(&mut self) -> Option<&mut FindView> {
        if let Some(v) = self.find_parked.as_deref_mut() {
            return Some(v);
        }
        self.overlays.iter_mut().find_map(|o| match o {
            Overlay::Find(v) => Some(v.as_mut()),
            _ => None,
        })
    }

    pub(super) fn find_event(&mut self, e: Event) {
        let Some(v) = self.find_view() else { return };
        match e {
            Event::Found(f) => v.add(f),
            Event::In(dir) => {
                if v.searching.is_some() {
                    v.searching = Some(dir);
                }
            }
            Event::Done => v.searching = None,
        }
    }

    /// The results window comes back after its viewer closed.
    pub(super) fn find_unpark(&mut self) {
        if let Some(v) = self.find_parked.take() {
            self.overlays.push(Overlay::Find(v));
        }
    }

    fn find_action(&mut self, action: Action) {
        let Some(Overlay::Find(v)) = self.overlays.last_mut() else {
            return;
        };
        match action {
            Action::NewSearch => {
                let (mask, text) = (v.query.masks.clone(), v.query.text.clone());
                self.overlays.pop();
                self.find_dialog(&mask, &text);
            }
            Action::Panel => {
                let paths: Vec<PathBuf> = v.results.iter().map(|f| f.path.clone()).collect();
                if paths.is_empty() {
                    return;
                }
                self.overlays.pop();
                self.focus = Focus::Panels;
                let side = self.active;
                self.panels[side].show_list(paths, tr!("find-panel-title"));
            }
            Action::GoTo => {
                let Some(f) = v.selected().cloned() else {
                    return;
                };
                self.overlays.pop();
                self.focus = Focus::Panels;
                let side = self.active;
                if let Some(dir) = f.path.parent() {
                    self.change_dir(side, dir);
                    if let Some(name) = f.path.file_name() {
                        self.panels[side].set_cursor_by_name(&name.to_string_lossy());
                    }
                }
            }
            Action::View => {
                let Some(f) = v.selected().cloned().filter(|f| !f.is_dir) else {
                    return;
                };
                let Some(Overlay::Find(v)) = self.overlays.pop() else {
                    return;
                };
                self.find_parked = Some(v);
                self.focus = Focus::Panels;
                self.record_view(&f.path);
                if self.open_viewer(&f.path, vec![f.path.clone()]).is_none() {
                    self.find_unpark();
                }
            }
            Action::StopOrClose => {
                if v.running() {
                    v.cancel.store(true, Ordering::Relaxed);
                    v.searching = None;
                } else {
                    self.overlays.pop();
                }
            }
        }
    }

    pub(super) fn find_key(&mut self, key: &KeyEvent) {
        let Some(Overlay::Find(v)) = self.overlays.last_mut() else {
            return;
        };
        let page = v.list.height.max(1) as isize;
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Up => v.step(-1),
            KeyCode::Down => v.step(1),
            KeyCode::PageUp => v.step(-page),
            KeyCode::PageDown => v.step(page),
            KeyCode::Home => v.step(-(v.lines.len() as isize)),
            KeyCode::End => v.step(v.lines.len() as isize),
            KeyCode::Enter if v.selected().is_some() => self.find_action(Action::GoTo),
            KeyCode::Enter => self.find_action(Action::NewSearch),
            KeyCode::F(3) | KeyCode::F(4) => self.find_action(Action::View),
            KeyCode::Esc | KeyCode::F(10) => self.find_action(Action::StopOrClose),
            KeyCode::Char(c) if alt => {
                let c = c.to_lowercase().next().unwrap_or(c);
                let c = crate::keys::latin_equivalent(c).unwrap_or(c);
                let hit = v
                    .button_labels()
                    .iter()
                    .position(|(l, on)| *on && crate::dialog::hotkey(l) == Some(c));
                if let Some(action) = hit.and_then(|i| BUTTON_ACTIONS[i]) {
                    self.find_action(action);
                }
            }
            _ => {}
        }
    }

    pub(super) fn find_mouse(&mut self, ev: &MouseEvent) {
        let wheel = self.wheel_lines();
        let Some(Overlay::Find(v)) = self.overlays.last_mut() else {
            return;
        };
        let pos = Position::new(ev.column, ev.row);
        match ev.kind {
            MouseEventKind::ScrollUp => v.step(-(wheel as isize)),
            MouseEventKind::ScrollDown => v.step(wheel as isize),
            MouseEventKind::Down(MouseButton::Left) => {
                if v.list.contains(pos) {
                    let i = v.top + usize::from(pos.y - v.list.y);
                    if matches!(v.lines.get(i), Some(Line::Item(_))) {
                        let again = i == v.current && self.find_double_click();
                        if let Some(Overlay::Find(v)) = self.overlays.last_mut() {
                            v.current = i;
                        }
                        if again {
                            self.find_action(Action::GoTo);
                        }
                    }
                } else if let Some(i) = v
                    .buttons
                    .iter()
                    .find(|(r, _)| r.contains(pos))
                    .map(|(_, i)| *i)
                    && let Some(action) = BUTTON_ACTIONS[i]
                {
                    self.find_action(action);
                }
            }
            _ => {}
        }
    }

    /// A second click on the same line soon after the first.
    fn find_double_click(&mut self) -> bool {
        let now = std::time::Instant::now();
        let double = self
            .find_last_click
            .is_some_and(|t| now.duration_since(t) < std::time::Duration::from_millis(500));
        self.find_last_click = Some(now);
        double
    }
}

/// Draws the results window: Far's full-screen dialog — the list, a
/// double separator, what is being searched (or the totals), the buttons.
pub(super) fn draw_find(v: &mut FindView, area: Rect, buf: &mut Buffer) {
    if area.width < 30 || area.height < 10 {
        return;
    }
    let outer = Rect::new(area.x + 1, area.y + 1, area.width - 2, area.height - 2);
    buf.set_style(outer, theme::DIALOG_TEXT);
    for y in outer.top()..outer.bottom() {
        for x in outer.left()..outer.right() {
            buf[(x, y)].set_symbol(" ");
        }
    }
    crate::dialog::draw_shadow(buf, outer, area);
    let frame = Rect::new(outer.x + 2, outer.y + 1, outer.width - 4, outer.height - 2);
    crate::panel::draw_frame(buf, frame, theme::DIALOG_BOX);
    let mut title = tr!("MFindFileTitle");
    if !v.query.masks.is_empty() {
        title = format!("{title}: {}", v.query.masks);
    }
    crate::panel::put_title(
        buf,
        frame,
        frame.y,
        &format!(" {title} "),
        theme::DIALOG_TEXT,
    );

    let inner_x = frame.x + 1;
    let inner_w = frame.width.saturating_sub(2);
    let bottom = frame.bottom() - 1;
    // Far: the list down to H-7, ═ at H-6, the status at H-5, ─ at H-4,
    // the buttons at H-3.
    let list = Rect::new(
        inner_x,
        frame.y + 1,
        inner_w,
        (bottom - 4).saturating_sub(frame.y + 1),
    );
    v.list = list;
    let rows = usize::from(list.height).max(1);
    if v.current < v.top {
        v.top = v.current;
    } else if v.current >= v.top + rows {
        v.top = v.current + 1 - rows;
    }
    for (k, line) in v.lines.iter().enumerate().skip(v.top).take(rows) {
        let y = list.y + (k - v.top) as u16;
        let selected = k == v.current;
        let style = if selected {
            theme::DIALOG_LIST_SELECTED
        } else {
            theme::DIALOG_LIST_TEXT
        };
        for x in list.left()..list.right() {
            buf[(x, y)].set_symbol(" ").set_style(style);
        }
        let text = match line {
            Line::Folder(d) => d.display().to_string(),
            Line::Item(i) => {
                let f = &v.results[*i];
                let name = f
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let size = if f.is_dir {
                    tr!("MListFolder")
                } else {
                    f.size.to_string()
                };
                let when = f
                    .modified
                    .map(|t| {
                        let t: chrono::DateTime<chrono::Local> = t.into();
                        t.format("%d.%m.%y %H:%M").to_string()
                    })
                    .unwrap_or_default();
                let right = format!("{size:>14} {when}");
                let room = usize::from(list.width).saturating_sub(right.chars().count() + 4);
                let name: String = name.chars().take(room).collect();
                format!("    {name:<room$}{right}")
            }
        };
        let style = match line {
            Line::Folder(_) if !selected => theme::DIALOG_LIST_HIGHLIGHT,
            _ => style,
        };
        buf.set_stringn(list.x, y, &text, usize::from(list.width), style);
    }
    let sep2 = bottom - 4;
    for x in frame.x + 1..frame.right() - 1 {
        buf[(x, sep2)].set_symbol("═").set_style(theme::DIALOG_BOX);
    }
    buf[(frame.x, sep2)]
        .set_symbol("╠")
        .set_style(theme::DIALOG_BOX);
    buf[(frame.right() - 1, sep2)]
        .set_symbol("╣")
        .set_style(theme::DIALOG_BOX);
    let (files, dirs) = v.counts();
    let status = match &v.searching {
        Some(dir) => format!(
            "{} {}   {}",
            tr!("MFindFileSearchingIn", p0 = format!("\"{}\"", v.query.text)),
            dir.display(),
            tr!("MFindFileFound", p0 = files, p1 = dirs)
        ),
        None => tr!("MFindFileDone", p0 = files, p1 = dirs),
    };
    buf.set_stringn(
        inner_x + 1,
        bottom - 3,
        &status,
        usize::from(inner_w.saturating_sub(2)),
        theme::DIALOG_TEXT,
    );
    let sep = bottom - 2;
    for x in frame.x + 1..frame.right() - 1 {
        buf[(x, sep)].set_symbol("─").set_style(theme::DIALOG_BOX);
    }
    buf[(frame.x, sep)]
        .set_symbol("╟")
        .set_style(theme::DIALOG_BOX);
    buf[(frame.right() - 1, sep)]
        .set_symbol("╢")
        .set_style(theme::DIALOG_BOX);
    // The buttons, centred as a group.
    let labels = v.button_labels();
    let shown: Vec<String> = labels
        .iter()
        .map(|(l, _)| format!("[ {} ]", crate::dialog::visible(l)))
        .collect();
    let total: usize = shown.iter().map(|s| s.chars().count() + 1).sum::<usize>() - 1;
    let mut x = frame.x + (frame.width.saturating_sub(total as u16)) / 2;
    let y = bottom - 1;
    v.buttons.clear();
    for (i, ((label, on), text)) in labels.iter().zip(&shown).enumerate() {
        let w = text.chars().count() as u16;
        let style = if *on {
            theme::DIALOG_TEXT
        } else {
            theme::DIALOG_DISABLED
        };
        buf.set_stringn(x, y, text, usize::from(w), style);
        // The hotkey letter.
        if *on && let Some(pos) = label.find('&') {
            let k = label[..pos].chars().count() as u16;
            if let Some(cell) = buf.cell_mut((x + 2 + k, y)) {
                cell.set_style(theme::DIALOG_HIGHLIGHT);
            }
        }
        v.buttons.push((Rect::new(x, y, w, 1), i));
        x += w + 1;
    }
}

/// A size as typed: `4096`, `100K`, `1M`, `2G` (Far's "first bytes").
fn parse_size(text: &str) -> Option<u64> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    let (num, mult) = match t.chars().last()?.to_ascii_uppercase() {
        'K' | 'К' => (&t[..t.len() - t.chars().last()?.len_utf8()], 1u64 << 10),
        'M' | 'М' => (&t[..t.len() - t.chars().last()?.len_utf8()], 1 << 20),
        'G' | 'Г' => (&t[..t.len() - t.chars().last()?.len_utf8()], 1 << 30),
        _ => (t, 1),
    };
    num.trim().parse::<u64>().ok().map(|n| n * mult)
}

#[cfg(test)]
mod tests {
    #[test]
    fn sizes() {
        assert_eq!(super::parse_size("4096"), Some(4096));
        assert_eq!(super::parse_size("100K"), Some(102_400));
        assert_eq!(super::parse_size("1m"), Some(1 << 20));
        assert_eq!(super::parse_size(""), None);
        assert_eq!(super::parse_size("x"), None);
    }
}
