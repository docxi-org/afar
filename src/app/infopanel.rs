//! Far's information panel (Ctrl+L, `infolist.cpp`): in place of the
//! passive panel — the computer and the user, the active panel's disk
//! (type, file system, space, label, serial number), memory, power, and
//! the folder's description file (`DirInfo`, `File_Id.diz`,
//! `Descript.ion`, `ReadMe.*`, `Read.Me`). Labels on the left, values on
//! the right; sections under titled separators. Read again once a second.

use std::path::{Path, PathBuf};
use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::App;
use crate::panel::size_float;
use crate::theme;
use crate::tr;

/// What the panel shows, read at most once a second.
#[derive(Default)]
struct Facts {
    computer: String,
    user: String,
    disk_title: String,
    disk: Vec<(String, String)>,
    /// The memory in short: the physical one.
    memory: Vec<(String, String)>,
    /// The memory in full, as Far shows it: committable, addressable,
    /// physical, installed.
    memory_full: Vec<(String, String)>,
    power: Vec<(String, String)>,
    /// The description file: its name, path and lines.
    description: Option<(String, PathBuf, Vec<String>)>,
}

pub(super) struct InfoPanel {
    /// The panel it covers.
    pub side: usize,
    facts: Facts,
    /// When and for which folder the facts were read.
    read: Option<(Instant, PathBuf)>,
    /// Where the description was drawn, and its file (a click opens it).
    pub description_at: Option<(Rect, PathBuf)>,
    /// The memory section in full (a click on it switches).
    memory_full: bool,
    /// Where the memory section was drawn.
    memory_at: Option<Rect>,
}

impl InfoPanel {
    pub fn new(side: usize) -> Self {
        Self {
            side,
            facts: Facts::default(),
            read: None,
            description_at: None,
            memory_full: false,
            memory_at: None,
        }
    }
}

/// Far's folder description files (`InfoPanel.strFolderInfoFiles`).
const DESCRIPTION_FILES: &[&str] = &[
    "DirInfo",
    "File_Id.diz",
    "Descript.ion",
    "ReadMe.*",
    "Read.Me",
];

fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt as _;
    s.encode_wide().chain([0]).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let len = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

/// "N%, size" of a part of a whole.
fn metric(kind: &str, total: u64, available: u64) -> Vec<(String, String)> {
    let pct = (available * 100).checked_div(total).unwrap_or(0);
    vec![
        (
            format!("{kind}, {}", tr!("MInfoMetricTotal")),
            size_float(total),
        ),
        (
            format!("{kind}, {}", tr!("MInfoMetricAvailable")),
            format!("{pct}%, {}", size_float(available)),
        ),
        (
            format!("{kind}, {}", tr!("MInfoMetricUsed")),
            format!(
                "{}%, {}",
                100 - pct,
                size_float(total.saturating_sub(available))
            ),
        ),
    ]
}

fn read_facts(dir: &Path) -> Facts {
    let mut f = Facts {
        computer: std::env::var("COMPUTERNAME").unwrap_or_default(),
        user: match (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
            (Ok(d), Ok(u)) => format!("{d}\\{u}"),
            (_, Ok(u)) => u,
            _ => String::new(),
        },
        ..Default::default()
    };

    // The disk of the folder.
    if let Some(root) = dir.ancestors().last() {
        let mut root_s = root.as_os_str().to_os_string();
        if !root_s.to_string_lossy().ends_with('\\') {
            root_s.push("\\");
        }
        let w = wide(&root_s);
        let mut label = [0u16; 261];
        let mut fs = [0u16; 261];
        let (mut serial, mut maxlen, mut flags) = (0u32, 0u32, 0u32);
        // SAFETY: buffers with their lengths, a NUL-terminated root.
        let ok = unsafe {
            windows_sys::Win32::Storage::FileSystem::GetVolumeInformationW(
                w.as_ptr(),
                label.as_mut_ptr(),
                label.len() as u32,
                &mut serial,
                &mut maxlen,
                &mut flags,
                fs.as_mut_ptr(),
                fs.len() as u32,
            )
        };
        let kind = crate::drives::letter_of(root)
            .and_then(|l| crate::drives::list().into_iter().find(|d| d.letter == l))
            .map(|d| {
                use crate::drives::DriveKind as K;
                match d.kind {
                    K::Fixed => tr!("MInfoFixed"),
                    K::Removable => tr!("MInfoRemovable"),
                    K::Network => tr!("MInfoNetwork"),
                    K::CdRom => tr!("MInfoCDROM"),
                    K::Ram => tr!("MInfoRAM"),
                    K::Subst => tr!("MInfoSUBST"),
                    K::Unknown => String::new(),
                }
            })
            .unwrap_or_default();
        f.disk_title = format!(
            "{kind} {} {} ({})",
            tr!("MInfoDisk"),
            root_s.to_string_lossy(),
            from_wide(&fs)
        )
        .trim()
        .to_string();
        let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
        // SAFETY: out pointers.
        if unsafe {
            windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
                w.as_ptr(),
                &mut avail,
                &mut total,
                &mut free,
            )
        } != 0
        {
            f.disk = metric(&tr!("MInfoDiskSpace"), total, avail);
        }
        if ok != 0 {
            f.disk.push((tr!("MInfoDiskLabel"), from_wide(&label)));
            f.disk.push((
                tr!("MInfoDiskNumber"),
                format!("{:04X}-{:04X}", serial >> 16, serial & 0xFFFF),
            ));
        }
    }

    // Memory.
    {
        use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        // SAFETY: the structure with its length set.
        let mut ms: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
        ms.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        if unsafe { GlobalMemoryStatusEx(&mut ms) } != 0 {
            let physical = metric(
                &tr!("MInfoMemoryPhysical"),
                ms.ullTotalPhys,
                ms.ullAvailPhys,
            );
            f.memory_full.extend(metric(
                &tr!("MInfoMemoryCommittable"),
                ms.ullTotalPageFile,
                ms.ullAvailPageFile,
            ));
            f.memory_full.extend(metric(
                &tr!("MInfoMemoryAddressable"),
                ms.ullTotalVirtual,
                ms.ullAvailVirtual,
            ));
            f.memory_full.extend(physical.iter().cloned());
            f.memory = physical;
            let mut kb = 0u64;
            // SAFETY: an out-value.
            if unsafe {
                windows_sys::Win32::System::SystemInformation::GetPhysicallyInstalledSystemMemory(
                    &mut kb,
                )
            } != 0
            {
                f.memory_full.push((
                    format!(
                        "{}, {}",
                        tr!("MInfoMemoryPhysical"),
                        tr!("MInfoMetricMemoryInstalled")
                    ),
                    size_float(kb * 1024),
                ));
            }
        }
    }

    // Power.
    {
        use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
        // SAFETY: an out-structure.
        let mut ps: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
        if unsafe { GetSystemPowerStatus(&mut ps) } != 0 {
            let ac = match ps.ACLineStatus {
                0 => tr!("MInfoPowerStatusACOffline"),
                1 => tr!("MInfoPowerStatusACOnline"),
                _ => tr!("MInfoPowerStatusBCLifePercentUnknown"),
            };
            f.power.push((tr!("MInfoPowerStatusAC"), ac));
            let battery = if ps.BatteryFlag & 128 != 0 {
                tr!("MInfoPowerStatusBCNoSysBat")
            } else if ps.BatteryLifePercent <= 100 {
                let mut s = format!("{}%", ps.BatteryLifePercent);
                if ps.BatteryFlag & 8 != 0 {
                    s = format!("{s} ({})", tr!("MInfoPowerStatusBCCharging"));
                }
                s
            } else {
                tr!("MInfoPowerStatusBCLifePercentUnknown")
            };
            f.power.push((tr!("MInfoPowerStatusBC"), battery));
        }
    }

    // The folder's description.
    if let Ok(read) = std::fs::read_dir(dir) {
        let names: Vec<(String, PathBuf)> = read
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
            .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
            .collect();
        let found = DESCRIPTION_FILES.iter().find_map(|mask| {
            let m = crate::masks::FileMasks::parse(mask)?;
            names.iter().find(|(n, _)| m.matches(n)).cloned()
        });
        if let Some((name, path)) = found {
            let text = std::fs::read(&path).unwrap_or_default();
            let text = String::from_utf8_lossy(&text[..text.len().min(64 * 1024)]).into_owned();
            let lines = text
                .lines()
                .map(|l| l.replace('\t', "    ").trim_end().to_string())
                .collect();
            f.description = Some((name, path, lines));
        }
    }
    f
}

impl App {
    /// Ctrl+L: the information panel on the passive panel, or the panel
    /// back (it replaces a quick view).
    pub(super) fn toggle_info_panel(&mut self) {
        self.info_panel = match self.info_panel {
            Some(_) => None,
            None => {
                self.quick_view = None;
                Some(InfoPanel::new(1 - self.active))
            }
        };
    }

    /// Draws the information panel into the panel's place.
    pub(super) fn draw_info_panel(&mut self, area: Rect, buf: &mut Buffer) {
        let dir = self.panels[self.active].path.clone();
        let Some(ip) = &mut self.info_panel else {
            return;
        };
        let stale = ip
            .read
            .as_ref()
            .is_none_or(|(t, d)| t.elapsed().as_secs() >= 1 || *d != dir);
        if stale {
            ip.facts = read_facts(&dir);
            ip.read = Some((Instant::now(), dir));
        }
        buf.set_style(area, theme::PANEL_TEXT);
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                buf[(x, y)].set_symbol(" ");
            }
        }
        crate::panel::draw_frame(buf, area, theme::PANEL_BOX);
        let title = format!(" {} ", tr!("MInfoTitle"));
        crate::panel::put_title(buf, area, area.y, &title, theme::PANEL_TITLE);
        if area.width < 10 || area.height < 4 {
            return;
        }
        let f = &ip.facts;
        let inner_w = area.width - 2;
        let mut y = area.y + 1;
        let bottom = area.bottom() - 1;
        let row = |buf: &mut Buffer, y: u16, label: &str, value: &str| {
            buf.set_stringn(
                area.x + 2,
                y,
                label,
                usize::from(inner_w - 2),
                theme::PANEL_TEXT,
            );
            let room = usize::from(inner_w).saturating_sub(label.chars().count() + 4);
            let n = value.chars().count();
            // Far: a long value is cut from the left.
            let v: String = value.chars().skip(n.saturating_sub(room)).collect();
            let x = area.right() - 2 - v.chars().count() as u16;
            buf.set_stringn(x, y, &v, room, theme::PANEL_INFO_TEXT);
        };
        let section = |buf: &mut Buffer, y: u16, title: &str| {
            buf[(area.x, y)].set_symbol("╟").set_style(theme::PANEL_BOX);
            buf[(area.right() - 1, y)]
                .set_symbol("╢")
                .set_style(theme::PANEL_BOX);
            for x in area.x + 1..area.right() - 1 {
                buf[(x, y)].set_symbol("─").set_style(theme::PANEL_BOX);
            }
            crate::panel::put_title(buf, area, y, &format!(" {title} "), theme::PANEL_TEXT);
        };
        // Rows: a section's title or a label and a value; `true` for the
        // memory section's rows.
        let mut lines: Vec<(Option<String>, String, String, bool)> = vec![
            (None, tr!("MInfoCompName"), f.computer.clone(), false),
            (None, tr!("MInfoUserName"), f.user.clone(), false),
        ];
        let mut push_section = |title: String, rows: &[(String, String)], memory: bool| {
            if !rows.is_empty() {
                lines.push((Some(title), String::new(), String::new(), memory));
                for (l, v) in rows {
                    lines.push((None, l.clone(), v.clone(), memory));
                }
            }
        };
        push_section(f.disk_title.clone(), &f.disk, false);
        let memory = if ip.memory_full {
            &f.memory_full
        } else {
            &f.memory
        };
        push_section(tr!("MInfoMemory"), memory, true);
        // Far's order: the description after the memory, the power last —
        // the power at the bottom, the description in between.
        let power_h = if f.power.is_empty() {
            0
        } else {
            1 + f.power.len() as u16
        };
        let desc_bottom = bottom.saturating_sub(power_h);
        // The description keeps a few rows: the sections above give way.
        let desc_rows = match &f.description {
            Some((_, _, text)) => 1 + text.len().min(4) as u16,
            None => 2,
        };
        let upper_end = desc_bottom.saturating_sub(desc_rows).max(area.y + 1);
        let mut memory_at: Option<Rect> = None;
        for (title, label, value, memory) in &lines {
            if y >= upper_end {
                break;
            }
            match title {
                Some(t) => section(buf, y, t),
                None => row(buf, y, label, value),
            }
            if *memory {
                match &mut memory_at {
                    Some(r) => r.height += 1,
                    None => {
                        // Far's marker: [+] — more to show, [-] — all shown.
                        let mark = if ip.memory_full { "[-]" } else { "[+]" };
                        buf.set_string(area.x + 1, y, mark, theme::PANEL_TEXT);
                        memory_at = Some(Rect::new(area.x + 1, y, inner_w, 1));
                    }
                }
            }
            y += 1;
        }
        ip.memory_at = memory_at;
        ip.description_at = None;
        if y < desc_bottom {
            match &f.description {
                Some((name, path, text)) => {
                    section(buf, y, &format!("{} {name}", tr!("MInfoDescription")));
                    y += 1;
                    let top = y;
                    for line in text {
                        if y >= desc_bottom {
                            break;
                        }
                        buf.set_stringn(
                            area.x + 1,
                            y,
                            line,
                            usize::from(inner_w),
                            theme::PANEL_TEXT,
                        );
                        y += 1;
                    }
                    ip.description_at = Some((
                        Rect::new(area.x + 1, top, inner_w, desc_bottom.saturating_sub(top)),
                        path.clone(),
                    ));
                }
                None => {
                    section(buf, y, &tr!("MInfoDescription"));
                    y += 1;
                    if y < desc_bottom {
                        let t = tr!("MInfoDizAbsent");
                        let x = area.x + (area.width.saturating_sub(t.chars().count() as u16)) / 2;
                        buf.set_stringn(x, y, &t, usize::from(inner_w), theme::PANEL_TEXT);
                    }
                }
            }
        }
        // The power at the bottom.
        if power_h > 0 && desc_bottom > area.y + 1 {
            let mut y = desc_bottom;
            section(buf, y, &tr!("MInfoPowerStatus"));
            y += 1;
            for (l, v) in &f.power {
                if y >= bottom {
                    break;
                }
                row(buf, y, l, v);
                y += 1;
            }
        }
    }

    /// A click on the description: the file opens (Far: in the editor; in
    /// the viewer until afar has one). `true`: taken.
    pub(super) fn info_panel_click(&mut self, column: u16, row: u16) -> bool {
        let pos = ratatui::layout::Position::new(column, row);
        // The memory section: short or in full.
        if let Some(ip) = &mut self.info_panel
            && ip.memory_at.is_some_and(|r| r.contains(pos))
        {
            ip.memory_full = !ip.memory_full;
            return true;
        }
        let Some((rect, path)) = self
            .info_panel
            .as_ref()
            .and_then(|ip| ip.description_at.clone())
        else {
            return false;
        };
        if !rect.contains(pos) {
            return false;
        }
        self.record_view(&path);
        self.open_viewer(&path, vec![path.clone()]);
        true
    }
}
