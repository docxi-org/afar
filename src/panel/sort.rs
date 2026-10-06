//! Sorting of a file panel as Far does it (far/filelist.cpp: SortModes,
//! list_less): a sort mode with its tie-breaking layers, the reverse
//! order, "directories first" and "selected first".

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use super::Entry;

/// Far's panel_sort, in Far's order.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub enum SortMode {
    Unsorted,
    #[default]
    Name,
    Ext,
    Modified,
    Created,
    Accessed,
    Size,
    Description,
    Owner,
    AllocatedSize,
    Links,
    Streams,
    StreamsSize,
    NameOnly,
    Changed,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Order {
    Ascend,
    Descend,
}

use Order::{Ascend, Descend};

/// A sort mode in the menu: its label, Ctrl+F key and layers.
pub struct ModeInfo {
    pub mode: SortMode,
    pub label: &'static str,
    /// Ctrl+F<n>.
    pub key: Option<u8>,
    layers: &'static [(SortMode, Order)],
    /// afar can sort by it (the others need descriptions, owners or
    /// NTFS details not read yet).
    pub supported: bool,
}

/// The sort modes in Far's menu order (MenuPosition).
#[rustfmt::skip]
pub const MODES: [ModeInfo; 15] = {
    use SortMode::*;
    [
        ModeInfo { mode: Name, label: "MMenuSortByName", key: Some(3), layers: &[(Name, Ascend), (Unsorted, Ascend)], supported: true },
        ModeInfo { mode: NameOnly, label: "MMenuSortByNameOnly", key: None, layers: &[(NameOnly, Ascend), (Ext, Ascend), (Unsorted, Ascend)], supported: true },
        ModeInfo { mode: Ext, label: "MMenuSortByExt", key: Some(4), layers: &[(Ext, Ascend), (NameOnly, Ascend), (Unsorted, Ascend)], supported: true },
        ModeInfo { mode: Modified, label: "MMenuSortByWrite", key: Some(5), layers: &[(Modified, Descend), (Name, Ascend), (Unsorted, Ascend)], supported: true },
        ModeInfo { mode: Size, label: "MMenuSortBySize", key: Some(6), layers: &[(Size, Descend), (Name, Ascend), (Unsorted, Ascend)], supported: true },
        ModeInfo { mode: Unsorted, label: "MMenuUnsorted", key: Some(7), layers: &[(Unsorted, Ascend)], supported: true },
        ModeInfo { mode: Created, label: "MMenuSortByCreation", key: Some(8), layers: &[(Created, Descend), (Name, Ascend), (Unsorted, Ascend)], supported: true },
        ModeInfo { mode: Accessed, label: "MMenuSortByAccess", key: Some(9), layers: &[(Accessed, Descend), (Name, Ascend), (Unsorted, Ascend)], supported: true },
        ModeInfo { mode: Changed, label: "MMenuSortByChange", key: None, layers: &[(Changed, Descend), (Name, Ascend), (Unsorted, Ascend)], supported: false },
        ModeInfo { mode: Description, label: "MMenuSortByDiz", key: Some(10), layers: &[(Description, Ascend), (Name, Ascend), (Unsorted, Ascend)], supported: false },
        ModeInfo { mode: Owner, label: "MMenuSortByOwner", key: Some(11), layers: &[(Owner, Ascend), (Name, Ascend), (Unsorted, Ascend)], supported: false },
        ModeInfo { mode: AllocatedSize, label: "MMenuSortByAllocatedSize", key: None, layers: &[(AllocatedSize, Descend), (Name, Ascend), (Unsorted, Ascend)], supported: false },
        ModeInfo { mode: Links, label: "MMenuSortByNumLinks", key: None, layers: &[(Links, Descend), (Name, Ascend), (Unsorted, Ascend)], supported: false },
        ModeInfo { mode: Streams, label: "MMenuSortByNumStreams", key: None, layers: &[(Streams, Descend), (Name, Ascend), (Unsorted, Ascend)], supported: false },
        ModeInfo { mode: StreamsSize, label: "MMenuSortByStreamsSize", key: None, layers: &[(StreamsSize, Descend), (Name, Ascend), (Unsorted, Ascend)], supported: false },
    ]
};

impl SortMode {
    pub fn info(self) -> &'static ModeInfo {
        MODES.iter().find(|m| m.mode == self).unwrap_or(&MODES[0])
    }

    /// The mode of Ctrl+F<n>.
    pub fn from_key(n: u8) -> Option<Self> {
        MODES
            .iter()
            .find(|m| m.key == Some(n) && m.supported)
            .map(|m| m.mode)
    }

    /// Far starts a mode in the order of its first layer (newest first
    /// for times, largest first for sizes).
    pub fn default_reverse(self) -> bool {
        self.info().layers[0].1 == Descend
    }
}

/// How a panel is sorted.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Sort {
    pub mode: SortMode,
    pub reverse: bool,
    pub dirs_first: bool,
    pub selected_first: bool,
}

impl Default for Sort {
    fn default() -> Self {
        Self {
            mode: SortMode::Name,
            reverse: false,
            dirs_first: true,
            selected_first: false,
        }
    }
}

impl Sort {
    /// Far's SetSortMode: the same mode again flips the order, another
    /// one starts in its default order.
    pub fn set_mode(&mut self, mode: SortMode) {
        self.reverse = if mode == self.mode {
            !self.reverse
        } else {
            mode.default_reverse()
        };
        self.mode = mode;
    }

    /// Far's list_less.
    pub fn compare(&self, a: &Entry, b: &Entry) -> Ordering {
        match (a.is_up(), b.is_up()) {
            (true, true) => return a.position.cmp(&b.position),
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            _ => {}
        }
        if self.dirs_first && a.is_dir != b.is_dir {
            return b.is_dir.cmp(&a.is_dir);
        }
        if self.selected_first && a.selected != b.selected {
            return b.selected.cmp(&a.selected);
        }
        for &(layer, order) in self.mode.info().layers {
            let reverse = if layer == self.mode {
                self.reverse
            } else {
                order == Descend
            };
            let r = compare_by(layer, a, b);
            let r = if reverse { r.reverse() } else { r };
            if r != Ordering::Equal {
                return r;
            }
        }
        Ordering::Equal
    }
}

fn compare_by(mode: SortMode, a: &Entry, b: &Entry) -> Ordering {
    match mode {
        SortMode::Name => compare_names(&a.name, &b.name),
        SortMode::NameOnly => compare_names(name_ext(a).0, name_ext(b).0),
        SortMode::Ext => compare_names(name_ext(a).1, name_ext(b).1),
        SortMode::Modified => a.modified.cmp(&b.modified),
        SortMode::Created => a.created.cmp(&b.created),
        SortMode::Accessed => a.accessed.cmp(&b.accessed),
        SortMode::Size => a.size.cmp(&b.size),
        _ => a.position.cmp(&b.position),
    }
}

/// Far's name_ext: the extension starts at the last dot (".bashrc" is all
/// extension); folders have none (SortFolderExt is off by default).
fn name_ext(e: &Entry) -> (&str, &str) {
    if e.is_dir {
        return (&e.name, "");
    }
    match e.name.rfind('.') {
        Some(i) => e.name.split_at(i),
        None => (&e.name, ""),
    }
}

/// Far's default string sort: linguistic, case-insensitive, digits as
/// numbers ("file2" before "file10") — CompareString on Windows.
pub fn compare_names(a: &str, b: &str) -> Ordering {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Globalization::{
            CompareStringW, LINGUISTIC_IGNORECASE, SORT_DIGITSASNUMBERS, SORT_STRINGSORT,
        };
        const LOCALE_USER_DEFAULT: u32 = 0x400;
        let a16: Vec<u16> = a.encode_utf16().collect();
        let b16: Vec<u16> = b.encode_utf16().collect();
        // SAFETY: both buffers are valid for the lengths given.
        let r = unsafe {
            CompareStringW(
                LOCALE_USER_DEFAULT,
                SORT_STRINGSORT | LINGUISTIC_IGNORECASE | SORT_DIGITSASNUMBERS,
                a16.as_ptr(),
                a16.len() as i32,
                b16.as_ptr(),
                b16.len() as i32,
            )
        };
        match r {
            1 => return Ordering::Less,
            2 => return Ordering::Equal,
            3 => return Ordering::Greater,
            _ => {}
        }
    }
    natural_cmp(a, b)
}

/// Case-insensitive comparison with runs of digits compared as numbers.
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut s = String::new();
                    while let Some(c) = it.peek().copied().filter(char::is_ascii_digit) {
                        s.push(c);
                        it.next();
                    }
                    s
                };
                let (x, y) = (take(&mut a), take(&mut b));
                let (tx, ty) = (x.trim_start_matches('0'), y.trim_start_matches('0'));
                let r = tx.len().cmp(&ty.len()).then_with(|| tx.cmp(ty));
                if r != Ordering::Equal {
                    return r;
                }
            }
            (Some(x), Some(y)) => {
                let r = x.to_lowercase().cmp(y.to_lowercase());
                if r != Ordering::Equal {
                    return r;
                }
                a.next();
                b.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        assert_eq!(natural_cmp("file2", "file10"), Ordering::Less);
        assert_eq!(natural_cmp("File", "file"), Ordering::Equal);
        assert_eq!(compare_names("file2", "file10"), Ordering::Less);
        assert_eq!(compare_names("a", "B"), Ordering::Less);
    }

    #[test]
    fn same_mode_flips_order() {
        let mut s = Sort::default();
        s.set_mode(SortMode::Name);
        assert!(s.reverse);
        s.set_mode(SortMode::Size);
        assert!(s.reverse, "sizes start largest first");
        s.set_mode(SortMode::Ext);
        assert!(!s.reverse);
    }
}
