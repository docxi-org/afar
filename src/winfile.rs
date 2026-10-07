//! Windows file properties beyond `std` (Far's setattr.cpp needs): the
//! four times of a file as FILETIMEs (with the change time), compression,
//! sparseness, encryption, the owner, the shell's properties window.

use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::fs::OpenOptionsExt as _;
use std::os::windows::io::AsRawHandle as _;
use std::path::Path;

use windows_sys::Win32::Storage::FileSystem::{
    FILE_BASIC_INFO, FileBasicInfo, GetFileInformationByHandleEx, SetFileInformationByHandle,
};

const FILE_READ_ATTRIBUTES: u32 = 0x80;
const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
const FILE_READ_DATA: u32 = 0x1;
const FILE_WRITE_DATA: u32 = 0x2;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
/// Opens the link itself, not its target.
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain([0]).collect()
}

fn open(path: &Path, access: u32) -> io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .access_mode(access)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

/// Write, creation, access and change times (FILETIME: 100 ns from 1601).
pub fn times(path: &Path) -> io::Result<[i64; 4]> {
    let f = open(path, FILE_READ_ATTRIBUTES)?;
    // SAFETY: plain data out-structure of the size given.
    let mut info: FILE_BASIC_INFO = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        GetFileInformationByHandleEx(
            f.as_raw_handle() as _,
            FileBasicInfo,
            (&mut info as *mut FILE_BASIC_INFO).cast(),
            std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok([
        info.LastWriteTime,
        info.CreationTime,
        info.LastAccessTime,
        info.ChangeTime,
    ])
}

/// Sets the times given (write, creation, access, change); the others stay.
pub fn set_times(path: &Path, times: [Option<i64>; 4]) -> io::Result<()> {
    if times.iter().all(Option::is_none) {
        return Ok(());
    }
    let f = open(path, FILE_WRITE_ATTRIBUTES)?;
    // Zero: "do not change" (the attributes too).
    let info = FILE_BASIC_INFO {
        LastWriteTime: times[0].unwrap_or(0),
        CreationTime: times[1].unwrap_or(0),
        LastAccessTime: times[2].unwrap_or(0),
        ChangeTime: times[3].unwrap_or(0),
        FileAttributes: 0,
    };
    // SAFETY: the structure and its size.
    let ok = unsafe {
        SetFileInformationByHandle(
            f.as_raw_handle() as _,
            FileBasicInfo,
            (&info as *const FILE_BASIC_INFO).cast(),
            std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn ioctl(path: &Path, code: u32, input: &[u8]) -> io::Result<()> {
    use windows_sys::Win32::System::IO::DeviceIoControl;
    let f = open(
        path,
        FILE_READ_DATA | FILE_WRITE_DATA | FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES,
    )?;
    let mut returned = 0u32;
    // SAFETY: input buffer and its size; no output; synchronous call.
    let ok = unsafe {
        DeviceIoControl(
            f.as_raw_handle() as _,
            code,
            input.as_ptr().cast(),
            input.len() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// NTFS compression on or off (FSCTL_SET_COMPRESSION).
pub fn set_compressed(path: &Path, on: bool) -> io::Result<()> {
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_COMPRESSION;
    let format: u16 = if on { 1 } else { 0 };
    ioctl(path, FSCTL_SET_COMPRESSION, &format.to_le_bytes())
}

/// Sparse on or off (FSCTL_SET_SPARSE).
pub fn set_sparse(path: &Path, on: bool) -> io::Result<()> {
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_SPARSE;
    ioctl(path, FSCTL_SET_SPARSE, &[u8::from(on)])
}

/// EFS encryption on or off.
pub fn set_encrypted(path: &Path, on: bool) -> io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::{DecryptFileW, EncryptFileW};
    let w = wide(path);
    // SAFETY: a NUL-terminated path.
    let ok = unsafe {
        if on {
            EncryptFileW(w.as_ptr())
        } else {
            DecryptFileW(w.as_ptr(), 0)
        }
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// The owner as `DOMAIN\name` (or the SID's name only).
pub fn owner(path: &Path) -> Option<String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{LookupAccountSidW, OWNER_SECURITY_INFORMATION, PSID};
    let w = wide(path);
    let mut sid: PSID = std::ptr::null_mut();
    let mut sd = std::ptr::null_mut();
    // SAFETY: out pointers; the descriptor is freed below.
    let err = unsafe {
        GetNamedSecurityInfoW(
            w.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut sid,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut sd,
        )
    };
    if err != 0 {
        return None;
    }
    let mut name = [0u16; 256];
    let mut domain = [0u16; 256];
    let (mut nl, mut dl) = (name.len() as u32, domain.len() as u32);
    let mut use_ = 0;
    // SAFETY: buffers with their lengths; the SID lives in `sd`.
    let ok = unsafe {
        LookupAccountSidW(
            std::ptr::null(),
            sid,
            name.as_mut_ptr(),
            &mut nl,
            domain.as_mut_ptr(),
            &mut dl,
            &mut use_,
        )
    };
    unsafe { LocalFree(sd) };
    if ok == 0 {
        return None;
    }
    let name = String::from_utf16_lossy(&name[..nl as usize]);
    let domain = String::from_utf16_lossy(&domain[..dl as usize]);
    Some(if domain.is_empty() {
        name
    } else {
        format!("{domain}\\{name}")
    })
}

/// Makes `account` the owner (needs the right to take ownership, or to
/// restore, for other accounts than one's own).
pub fn set_owner(path: &Path, account: &str) -> io::Result<()> {
    use windows_sys::Win32::Security::Authorization::{SE_FILE_OBJECT, SetNamedSecurityInfoW};
    use windows_sys::Win32::Security::{LookupAccountNameW, OWNER_SECURITY_INFORMATION};
    let acc: Vec<u16> = account.encode_utf16().chain([0]).collect();
    let mut sid = vec![0u8; 256];
    let mut sid_len = sid.len() as u32;
    let mut domain = [0u16; 256];
    let mut dl = domain.len() as u32;
    let mut use_ = 0;
    // SAFETY: buffers with their lengths.
    let ok = unsafe {
        LookupAccountNameW(
            std::ptr::null(),
            acc.as_ptr(),
            sid.as_mut_ptr().cast(),
            &mut sid_len,
            domain.as_mut_ptr(),
            &mut dl,
            &mut use_,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let w = wide(path);
    // SAFETY: the SID buffer filled above.
    let err = unsafe {
        SetNamedSecurityInfoW(
            w.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            sid.as_mut_ptr().cast(),
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if err != 0 {
        Err(io::Error::from_raw_os_error(err as i32))
    } else {
        Ok(())
    }
}

/// The shell's properties window of a file (Far's "System properties").
pub fn show_properties(path: &Path) -> bool {
    use windows_sys::Win32::UI::Shell::{SHOP_FILEPATH, SHObjectProperties};
    let w = wide(path);
    // SAFETY: a NUL-terminated path; no owner window.
    unsafe {
        SHObjectProperties(
            std::ptr::null_mut(),
            SHOP_FILEPATH as u32,
            w.as_ptr(),
            std::ptr::null(),
        ) != 0
    }
}

/// A FILETIME as local date and time text.
pub fn filetime_shown(ft: i64) -> (String, String) {
    if ft <= 0 {
        return (String::new(), String::new());
    }
    let secs = ft / 10_000_000 - 11_644_473_600;
    let nanos = (ft % 10_000_000) as u32 * 100;
    match chrono::DateTime::from_timestamp(secs, nanos) {
        Some(t) => {
            let t = t.with_timezone(&chrono::Local);
            (
                t.format("%d.%m.%Y").to_string(),
                t.format("%H:%M:%S").to_string(),
            )
        }
        None => (String::new(), String::new()),
    }
}

/// A local date and time as typed (`dd.mm.yyyy`, `hh:mm[:ss]`) as a
/// FILETIME.
pub fn filetime_parse(date: &str, time: &str) -> Option<i64> {
    use chrono::TimeZone as _;
    let d = chrono::NaiveDate::parse_from_str(date.trim(), "%d.%m.%Y").ok()?;
    let t = chrono::NaiveTime::parse_from_str(time.trim(), "%H:%M:%S")
        .or_else(|_| chrono::NaiveTime::parse_from_str(time.trim(), "%H:%M"))
        .ok()?;
    let local = chrono::Local
        .from_local_datetime(&d.and_time(t))
        .earliest()?;
    let utc = local.with_timezone(&chrono::Utc);
    Some(
        (utc.timestamp() + 11_644_473_600) * 10_000_000
            + i64::from(utc.timestamp_subsec_nanos() / 100),
    )
}

/// Now as a FILETIME.
pub fn filetime_now() -> i64 {
    let now = chrono::Utc::now();
    (now.timestamp() + 11_644_473_600) * 10_000_000 + i64::from(now.timestamp_subsec_nanos() / 100)
}
