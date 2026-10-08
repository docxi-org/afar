//! Far's file attributes dialog (Ctrl+A, `setattr.cpp`): the attributes
//! of the selected files (or the one under the cursor) as check boxes —
//! three-state "?" when the files differ — their times of writing,
//! creation, access and change, and the owner. "Original" / "Current" /
//! "Blank" fill the times; "Set" changes only what was changed in the
//! dialog (through subfolders when asked), in the background; "System
//! properties" opens the shell's window of one file.

use std::path::{Path, PathBuf};

use super::fileops::{Overlay, Purpose};
use super::{App, AppMsg};
use crate::dialog::{Button, Dialog, check_at, input_at, text_at};
use crate::tr;
use crate::winfile;

/// The attributes in the dialog's order: Far's text id, the bit.
const ATTRS: &[(&str, u32)] = &[
    ("MSetAttrReadOnly", 0x1),
    ("MSetAttrArchive", 0x20),
    ("MSetAttrHidden", 0x2),
    ("MSetAttrSystem", 0x4),
    ("MSetAttrCompressed", COMPRESSED),
    ("MSetAttrEncrypted", ENCRYPTED),
    ("MSetAttrNotIndexed", 0x2000),
    ("MSetAttrSparse", SPARSE),
    ("MSetAttrTemporary", 0x100),
    ("MSetAttrOffline", 0x1000),
];
/// The names of the attributes set in `attrs` (Far's attribute dialog
/// words), for hints.
pub(super) fn attribute_names(attrs: u32) -> Vec<String> {
    ATTRS
        .iter()
        .filter(|(_, bit)| attrs & bit != 0)
        .map(|(id, _)| crate::i18n::plain(&tr!(id)))
        .collect()
}

const COMPRESSED: u32 = 0x800;
const ENCRYPTED: u32 = 0x4000;
const SPARSE: u32 = 0x200;
/// Set by SetFileAttributes; the others take their own calls.
const PLAIN: u32 = 0x1 | 0x20 | 0x2 | 0x4 | 0x2000 | 0x100 | 0x1000;

/// The times in the dialog's order.
const TIMES: &[&str] = &[
    "MSetAttrWrite",
    "MSetAttrCreation",
    "MSetAttrAccess",
    "MSetAttrChange",
];

/// The buttons (numbered through the dialog).
const ORIGINAL: usize = 1;
const CURRENT: usize = 2;
const BLANK: usize = 3;
const PROPERTIES: usize = 5;

/// What the dialog started from, to change only what the user changed.
#[derive(Clone, Debug)]
pub(super) struct AttrState {
    targets: Vec<PathBuf>,
    /// Per attribute: on for all, off for all, or `None` (they differ).
    attrs: Vec<Option<bool>>,
    /// Per time: (date, time) as shown; empty when the files differ.
    times: Vec<(String, String)>,
    owner: String,
    has_dirs: bool,
}

/// What "Set" changes.
#[derive(Clone, Debug, Default)]
struct Change {
    set: u32,
    clear: u32,
    times: [Option<i64>; 4],
    owner: Option<String>,
}

fn attributes_of(path: &Path) -> u32 {
    use std::os::windows::fs::MetadataExt as _;
    std::fs::symlink_metadata(path)
        .map(|m| m.file_attributes())
        .unwrap_or(0)
}

impl App {
    /// Ctrl+A: the dialog for the selected files or the one under the
    /// cursor.
    pub(super) fn attributes_dialog(&mut self) {
        let panel = &self.panels[self.active];
        let mut targets: Vec<PathBuf> =
            panel.selected().map(|e| panel.path.join(&e.name)).collect();
        if targets.is_empty() {
            match panel.current() {
                Some(e) if e.name != ".." => targets.push(panel.path.join(&e.name)),
                _ => return,
            }
        }
        let attrs: Vec<u32> = targets.iter().map(|p| attributes_of(p)).collect();
        let states: Vec<Option<bool>> = ATTRS
            .iter()
            .map(|(_, bit)| {
                let on = attrs.iter().filter(|a| *a & bit != 0).count();
                if on == attrs.len() {
                    Some(true)
                } else if on == 0 {
                    Some(false)
                } else {
                    None
                }
            })
            .collect();
        let all_times: Vec<[i64; 4]> = targets
            .iter()
            .map(|p| winfile::times(p).unwrap_or([0; 4]))
            .collect();
        let times: Vec<(String, String)> = (0..TIMES.len())
            .map(|i| {
                let first = winfile::filetime_shown(all_times[0][i]);
                if all_times
                    .iter()
                    .all(|t| winfile::filetime_shown(t[i]) == first)
                {
                    first
                } else {
                    (String::new(), String::new())
                }
            })
            .collect();
        let owners: Vec<String> = targets
            .iter()
            .map(|p| winfile::owner(p).unwrap_or_default())
            .collect();
        let owner = if owners.iter().all(|o| *o == owners[0]) {
            owners[0].clone()
        } else {
            String::new()
        };
        let has_dirs = targets.iter().any(|p| p.is_dir());
        let single = targets.len() == 1;
        let name = if single {
            targets[0]
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            format!("{} {}", targets.len(), tr!("MSetAttrSelectedObjects"))
        };

        // Far's 80-wide dialog: the attributes on the left, the times and
        // the owner on the right.
        const C2: u16 = 32;
        const DATE_X: u16 = 47;
        const TIME_X: u16 = 59;
        let mut d = Dialog::far(tr!("MSetAttrTitle"), 80)
            .center(tr!("MSetAttrFor"))
            .center(name);
        // A link: where it points.
        if single && let Ok(target) = std::fs::read_link(&targets[0]) {
            d = d.center(format!("{} {}", tr!("MSetAttrSymlink"), target.display()));
        }
        d = d.separator();
        for (i, (id, _)) in ATTRS.iter().enumerate() {
            let state = states[i];
            let mut check = check_at(5, tr!(id), state == Some(true));
            if state.is_none() {
                check = check.three_state(true);
            } else if !single {
                check = check.three_state(false);
            }
            let mut row = vec![check];
            match i {
                0 => {
                    row.push(text_at(C2, tr!("MSetAttrDate")));
                    // As the regional settings write dates and times.
                    let l = crate::locale::get();
                    let date = match l.order {
                        crate::locale::Order::Dmy => "attr-date-format-dmy",
                        crate::locale::Order::Mdy => "attr-date-format-mdy",
                        crate::locale::Order::Ymd => "attr-date-format-ymd",
                    };
                    let sep = l.date_sep.to_string();
                    row.push(text_at(DATE_X, tr!(date, sep = sep)));
                    let sep = l.time_sep.to_string();
                    row.push(text_at(TIME_X, tr!("attr-time-format", sep = sep)));
                }
                1..=4 => {
                    let k = i - 1;
                    row.push(text_at(C2, tr!(TIMES[k])));
                    row.push(input_at(DATE_X, 11, times[k].0.clone(), None));
                    row.push(input_at(TIME_X, 12, times[k].1.clone(), None));
                }
                7 => row.push(text_at(C2, tr!("MSetAttrOwner"))),
                8 => row.push(input_at(C2, 42, owner.clone(), None)),
                _ => {}
            }
            d = d.row(row);
        }
        d = d.button_row(vec![
            Button::new(tr!("MSetAttrOriginal")),
            Button::new(tr!("MSetAttrCurrent")),
            Button::new(tr!("MSetAttrBlank")),
        ]);
        if has_dirs {
            d = d
                .separator()
                .row(vec![check_at(5, tr!("MSetAttrSubfolders"), false)]);
        }
        let mut props = Button::new(tr!("MSetAttrSystemDialog"));
        if !single {
            props = props.disabled();
        }
        let dialog = d
            .separator()
            .button_row(vec![
                Button::new(tr!("MSetAttrSet")).default(),
                props,
                Button::new(tr!("MCancel")),
            ])
            // Far: the first check box has the focus.
            .focus_item(0);
        self.overlays.push(Overlay::Dialog {
            dialog,
            purpose: Purpose::Attributes(Box::new(AttrState {
                targets,
                attrs: states,
                times,
                owner,
                has_dirs,
            })),
        });
    }

    /// A button of the dialog: the time buttons fill the fields and keep
    /// it open; "System properties" and "Set" close it.
    pub(super) fn attributes_button(
        &mut self,
        state: Box<AttrState>,
        mut dialog: Dialog,
        button: Option<usize>,
    ) {
        let fill = |dialog: &mut Dialog, value: &dyn Fn(usize) -> (String, String)| {
            for k in 0..TIMES.len() {
                let (date, time) = value(k);
                dialog.set_input_value(2 * k, &date);
                dialog.set_input_value(2 * k + 1, &time);
            }
        };
        match button {
            Some(ORIGINAL) => {
                let times = state.times.clone();
                fill(&mut dialog, &|k| times[k].clone());
            }
            Some(CURRENT) => {
                let now = winfile::filetime_shown(winfile::filetime_now());
                fill(&mut dialog, &|_| now.clone());
            }
            Some(BLANK) => fill(&mut dialog, &|_| (String::new(), String::new())),
            Some(PROPERTIES) => {
                if let Some(p) = state.targets.first() {
                    winfile::show_properties(p);
                }
                return;
            }
            Some(0) => {
                self.attributes_set(*state, &dialog);
                return;
            }
            _ => return,
        }
        self.overlays.push(Overlay::Dialog {
            dialog,
            purpose: Purpose::Attributes(state),
        });
    }

    /// "Set": the attributes, times and owner that differ from what the
    /// dialog showed, on every target (and inside folders when asked), in
    /// the background.
    fn attributes_set(&mut self, state: AttrState, dialog: &Dialog) {
        let mut change = Change::default();
        for (i, (_, bit)) in ATTRS.iter().enumerate() {
            let now = dialog.check_state(i);
            if now == state.attrs[i] {
                continue;
            }
            match now {
                Some(true) => change.set |= bit,
                Some(false) => change.clear |= bit,
                None => {}
            }
        }
        for k in 0..TIMES.len() {
            let (date, time) = (dialog.input_value(2 * k), dialog.input_value(2 * k + 1));
            if (date.clone(), time.clone()) == state.times[k] || date.trim().is_empty() {
                continue;
            }
            match winfile::filetime_parse(&date, &time) {
                Some(t) => change.times[k] = Some(t),
                None => {
                    self.say(tr!("attr-bad-time", date = date, time = time));
                    return;
                }
            }
        }
        let owner = dialog.input_value(2 * TIMES.len());
        if !owner.trim().is_empty() && owner.trim() != state.owner {
            change.owner = Some(owner.trim().to_string());
        }
        let deep = state.has_dirs && dialog.checked(ATTRS.len());
        let tx = self.tx.clone();
        let targets = state.targets;
        self.say(tr!("MSetAttrSetting"));
        std::thread::spawn(move || {
            let mut all = targets.clone();
            if deep {
                for t in &targets {
                    if t.is_dir() {
                        collect_tree(t, &mut all);
                    }
                }
            }
            let mut failed = Vec::new();
            for path in &all {
                if let Err(e) = apply(path, &change) {
                    failed.push(format!("{}: {e}", path.display()));
                }
            }
            let _ = tx.send(AppMsg::AttributesDone(all.len(), failed));
        });
    }

    /// The background "Set" finished.
    pub(super) fn attributes_done(&mut self, count: usize, failed: Vec<String>) {
        for p in &mut self.panels {
            p.reload(None);
        }
        match failed.first() {
            Some(first) => self.message(
                &tr!("MSetAttrTitle"),
                &[tr!("MSetAttrCannotFor"), first.clone()],
                true,
            ),
            None => self.say(tr!("attr-done", count = count)),
        }
    }
}

/// Everything inside a folder (links are not followed).
fn collect_tree(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in read.flatten() {
            let path = e.path();
            let is_dir = e.file_type().is_ok_and(|t| t.is_dir() && !t.is_symlink());
            if is_dir {
                stack.push(path.clone());
            }
            out.push(path);
        }
    }
}

/// Times first (a read-only file still takes them), then compression,
/// sparseness and encryption (each its own call; an encrypted file cannot
/// be compressed), the plain attributes, the owner.
fn apply(path: &Path, c: &Change) -> std::io::Result<()> {
    winfile::set_times(path, c.times)?;
    let now = attributes_of(path);
    let wants = |bit: u32| {
        if c.set & bit != 0 {
            Some(true)
        } else if c.clear & bit != 0 {
            Some(false)
        } else {
            None
        }
    };
    // The special calls need the file writable: read-only goes off for
    // them (and comes back with the plain attributes below).
    let special = [ENCRYPTED, COMPRESSED, SPARSE]
        .iter()
        .any(|bit| wants(*bit).is_some_and(|on| on != (now & bit != 0)));
    if special && now & 0x1 != 0 {
        set_attributes(path, (now & !0x1) & PLAIN)?;
    }
    if let Some(on) = wants(ENCRYPTED).filter(|on| *on != (now & ENCRYPTED != 0)) {
        winfile::set_encrypted(path, on)?;
    }
    if let Some(on) = wants(COMPRESSED).filter(|on| *on != (now & COMPRESSED != 0)) {
        winfile::set_compressed(path, on)?;
    }
    if let Some(on) = wants(SPARSE).filter(|on| *on != (now & SPARSE != 0))
        && !path.is_dir()
    {
        winfile::set_sparse(path, on)?;
    }
    let (set, clear) = (c.set & PLAIN, c.clear & PLAIN);
    if set | clear != 0 || special {
        // Read-only as it was, unless changed.
        let was = now & 0x1;
        let now = attributes_of(path);
        let new = ((now | was) | set) & !clear;
        if new != now {
            set_attributes(path, new & PLAIN)?;
        }
    }
    if let Some(owner) = &c.owner {
        winfile::set_owner(path, owner)?;
    }
    Ok(())
}

fn set_attributes(path: &Path, attrs: u32) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    // FILE_ATTRIBUTE_NORMAL alone when nothing is left.
    let attrs = if attrs == 0 { 0x80 } else { attrs };
    // SAFETY: a NUL-terminated path.
    let ok = unsafe {
        windows_sys::Win32::Storage::FileSystem::SetFileAttributesW(wide.as_ptr(), attrs)
    };
    if ok == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_attributes_and_times() {
        let dir = std::env::temp_dir().join(format!("afar-attr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.txt");
        std::fs::write(&f, "x").unwrap();
        let t = winfile::filetime_parse("01.02.2020", "03:04:05").unwrap();
        let c = Change {
            set: 0x1 | 0x2,
            times: [Some(t), None, None, None],
            ..Default::default()
        };
        apply(&f, &c).unwrap();
        let a = attributes_of(&f);
        assert!(a & 0x1 != 0 && a & 0x2 != 0);
        assert_eq!(
            winfile::filetime_shown(winfile::times(&f).unwrap()[0]),
            ("01.02.2020".into(), "03:04:05".into())
        );
        // Compression (NTFS temp folders allow it).
        let c = Change {
            clear: 0x1 | 0x2,
            set: COMPRESSED,
            ..Default::default()
        };
        apply(&f, &c).unwrap();
        assert_eq!(attributes_of(&f) & 0x3, 0);
        assert!(attributes_of(&f) & COMPRESSED != 0);
        assert!(winfile::owner(&f).is_some());
        std::fs::remove_file(&f).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }
}
