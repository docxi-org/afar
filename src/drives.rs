//! Disk drives for Far's change-drive menu (far/diskmenu.cpp): letters,
//! types and sizes.

use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DriveKind {
    Unknown,
    Removable,
    Fixed,
    Network,
    CdRom,
    Ram,
    Subst,
}

impl DriveKind {
    /// Far's label id (MChangeDrive…); unknown types have none.
    pub fn label_id(self) -> Option<&'static str> {
        match self {
            DriveKind::Unknown => None,
            DriveKind::Removable => Some("MChangeDriveRemovable"),
            DriveKind::Fixed => Some("MChangeDriveFixed"),
            DriveKind::Network => Some("MChangeDriveNetwork"),
            DriveKind::CdRom => Some("MChangeDriveCDROM"),
            DriveKind::Ram => Some("MChangeDriveRAM"),
            DriveKind::Subst => Some("MChangeDriveSUBST"),
        }
    }

    /// Far shows no sizes for removable and network drives by default
    /// (they may be slow or ask for a disk).
    fn shows_size(self) -> bool {
        !matches!(self, DriveKind::Removable | DriveKind::Network)
    }
}

#[derive(Clone, Debug)]
pub struct Drive {
    pub letter: char,
    pub root: PathBuf,
    pub kind: DriveKind,
    /// Total and free bytes, when shown and readable.
    pub sizes: Option<(u64, u64)>,
}

#[cfg(windows)]
pub fn list() -> Vec<Drive> {
    use windows_sys::Win32::Storage::FileSystem::{
        GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, QueryDosDeviceW,
    };
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    // SAFETY: plain calls with valid NUL-terminated buffers.
    let mask = unsafe { GetLogicalDrives() };
    let mut out = Vec::new();
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = char::from(b'A' + i as u8);
        let root = format!("{letter}:\\");
        let kind = match unsafe { GetDriveTypeW(wide(&root).as_ptr()) } {
            2 => DriveKind::Removable,
            3 => DriveKind::Fixed,
            4 => DriveKind::Network,
            5 => DriveKind::CdRom,
            6 => DriveKind::Ram,
            _ => DriveKind::Unknown,
        };
        // SUBST drives map to "\??\C:\path".
        let mut target = [0u16; 512];
        let n = unsafe {
            QueryDosDeviceW(
                wide(&format!("{letter}:")).as_ptr(),
                target.as_mut_ptr(),
                target.len() as u32,
            )
        };
        let kind = if n > 4 && target[..4] == [92, 63, 63, 92] {
            DriveKind::Subst
        } else {
            kind
        };
        let sizes = kind.shows_size().then(|| {
            let (mut free, mut total) = (0u64, 0u64);
            let ok = unsafe {
                GetDiskFreeSpaceExW(
                    wide(&root).as_ptr(),
                    &mut free,
                    &mut total,
                    std::ptr::null_mut(),
                )
            };
            (ok != 0).then_some((total, free))
        });
        out.push(Drive {
            letter,
            root: PathBuf::from(root),
            kind,
            sizes: sizes.flatten(),
        });
    }
    out
}

#[cfg(not(windows))]
pub fn list() -> Vec<Drive> {
    vec![Drive {
        letter: '/',
        root: PathBuf::from("/"),
        kind: DriveKind::Fixed,
        sizes: None,
    }]
}

/// The drive letter of a path ("C:\x" → 'C').
pub fn letter_of(path: &std::path::Path) -> Option<char> {
    let s = path.to_str()?;
    let mut chars = s.chars();
    let letter = chars.next()?;
    (chars.next() == Some(':') && letter.is_ascii_alphabetic()).then(|| letter.to_ascii_uppercase())
}
