//! The system clipboard: text out (the viewer's Ctrl+C) and in (the
//! editor's Ctrl+V). On Windows through the clipboard API (works in any
//! console host); elsewhere out through the terminal (OSC 52), and in not
//! at all.

/// Puts `text` on the clipboard.
pub fn set_text(text: &str) -> Result<(), String> {
    system::set_text(text)
}

/// The clipboard's text, if any.
pub fn get_text() -> Option<String> {
    system::get_text()
}

#[cfg(windows)]
mod system {
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable,
        OpenClipboard, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock,
    };

    const CF_UNICODETEXT: u32 = 13;

    pub fn set_text(text: &str) -> Result<(), String> {
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = wide.len() * 2;
        unsafe {
            let mem = GlobalAlloc(GMEM_MOVEABLE, bytes);
            if mem.is_null() {
                return Err(std::io::Error::last_os_error().to_string());
            }
            let ptr = GlobalLock(mem) as *mut u16;
            if ptr.is_null() {
                GlobalFree(mem);
                return Err(std::io::Error::last_os_error().to_string());
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
            GlobalUnlock(mem);
            if OpenClipboard(std::ptr::null_mut()) == 0 {
                GlobalFree(mem);
                return Err(std::io::Error::last_os_error().to_string());
            }
            EmptyClipboard();
            // The clipboard owns the memory once it is set.
            let set = !SetClipboardData(CF_UNICODETEXT, mem).is_null();
            let error = std::io::Error::last_os_error();
            CloseClipboard();
            if !set {
                GlobalFree(mem);
                return Err(error.to_string());
            }
        }
        Ok(())
    }

    pub fn get_text() -> Option<String> {
        unsafe {
            if IsClipboardFormatAvailable(CF_UNICODETEXT) == 0
                || OpenClipboard(std::ptr::null_mut()) == 0
            {
                return None;
            }
            let mem = GetClipboardData(CF_UNICODETEXT);
            let mut text = None;
            if !mem.is_null() {
                let ptr = GlobalLock(mem) as *const u16;
                if !ptr.is_null() {
                    let mut len = 0;
                    while *ptr.add(len) != 0 {
                        len += 1;
                    }
                    text = Some(String::from_utf16_lossy(std::slice::from_raw_parts(
                        ptr, len,
                    )));
                    GlobalUnlock(mem);
                }
            }
            CloseClipboard();
            text
        }
    }
}

#[cfg(not(windows))]
mod system {
    use std::io::Write as _;

    pub fn get_text() -> Option<String> {
        None
    }

    pub fn set_text(text: &str) -> Result<(), String> {
        let mut out = std::io::stdout();
        write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()))
            .and_then(|()| out.flush())
            .map_err(|e| e.to_string())
    }

    fn base64(data: &[u8]) -> String {
        const ABC: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in data.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }
}
