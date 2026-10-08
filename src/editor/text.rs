//! The editor's text as Far keeps it (`editor.cpp`, `edit.hpp`): a list of
//! lines, each with its own line ending, so mixed endings survive a save
//! (docs/17 §2–3). Reading decodes the whole file in its code page;
//! writing encodes it back.

use crate::viewer::codepage::{self, Codec};

/// A line's ending (Far's `eol::type`): `CR CR LF` is one ending, the trace
/// of a double text conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Eol {
    /// The last line of the file has none.
    #[default]
    None,
    Lf,
    CrLf,
    Cr,
    CrCrLf,
}

impl Eol {
    pub fn as_str(self) -> &'static str {
        match self {
            Eol::None => "",
            Eol::Lf => "\n",
            Eol::CrLf => "\r\n",
            Eol::Cr => "\r",
            Eol::CrCrLf => "\r\r\n",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    pub eol: Eol,
    /// Written by the agent and not yet accepted by the user (a user's
    /// edit of the line or a save accepts it; docs/11 «Три слоя»).
    pub by_agent: bool,
    /// The user typed (changed) it in this window — such a line can be an
    /// instruction for the agent; a line read from the file is not.
    pub typed: bool,
}

impl Line {
    pub fn new(text: impl Into<String>, eol: Eol) -> Self {
        Self {
            text: text.into(),
            eol,
            by_agent: false,
            typed: false,
        }
    }

    /// Length in characters.
    pub fn len(&self) -> usize {
        self.text.chars().count()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Byte index of character `col` (the end for `col` past it).
    pub fn byte(&self, col: usize) -> usize {
        self.text
            .char_indices()
            .nth(col)
            .map_or(self.text.len(), |(i, _)| i)
    }
}

/// A file read for editing.
pub struct Loaded {
    pub lines: Vec<Line>,
    pub cp: u32,
    /// It began with a byte order mark (written back on save).
    pub bom: bool,
    /// The first line ending in the file (Far's `GlobalEOL`): the one a
    /// line without an ending gets when a new line follows it.
    pub eol: Option<Eol>,
    /// The first bytes the code page could not read (shown as U+FFFD;
    /// Far's `BadConversion`).
    pub bad: Option<Vec<u8>>,
}

/// Splits decoded text into lines, as Far's `enum_lines`: `LF`, `CR LF`,
/// `CR CR LF`, a lone `CR`; text ending with a line break gets an empty
/// last line without an ending.
pub fn split_lines(text: &str) -> (Vec<Line>, Option<Eol>) {
    let mut lines = Vec::new();
    let mut first = None;
    let mut cur = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let eol = match c {
            '\n' => Eol::Lf,
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                    Eol::CrLf
                } else if chars.peek() == Some(&'\r') {
                    // CR CR LF is one ending; CR CR alone is two lines.
                    let mut ahead = chars.clone();
                    ahead.next();
                    if ahead.peek() == Some(&'\n') {
                        chars.next();
                        chars.next();
                        Eol::CrCrLf
                    } else {
                        Eol::Cr
                    }
                } else {
                    Eol::Cr
                }
            }
            _ => {
                cur.push(c);
                continue;
            }
        };
        first.get_or_insert(eol);
        lines.push(Line::new(std::mem::take(&mut cur), eol));
    }
    lines.push(Line::new(cur, Eol::None));
    (lines, first)
}

/// Decodes `bytes` (a BOM already cut off) in `codec`.
pub fn decode(codec: &Codec, mut bytes: &[u8]) -> String {
    if codec.cp() == codepage::UTF8 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut out = String::with_capacity(bytes.len());
    while !bytes.is_empty() {
        let (c, n) = codec.decode(bytes);
        out.push(c);
        bytes = &bytes[n.max(1).min(bytes.len())..];
    }
    out
}

/// Reads a file: the code page as the viewer chooses it (`cp`: one asked
/// for or remembered; `None`: detected, else `default_cp`).
pub fn load(data: &[u8], cp: Option<u32>, autodetect: bool, default_cp: u32) -> Loaded {
    let bom = codepage::bom(data);
    let detected = if autodetect {
        codepage::detect(&data[..data.len().min(32768)], data.len() <= 32768)
    } else {
        bom.map(|(cp, _)| cp)
    };
    let cp = cp
        .filter(|cp| Codec::new(*cp).is_some())
        .or(detected)
        .filter(|cp| Codec::new(*cp).is_some())
        .unwrap_or(default_cp);
    let codec = Codec::new(cp)
        .or_else(|| Codec::new(codepage::UTF8))
        .expect("UTF-8 is always supported");
    // The BOM of the page actually used is skipped and remembered.
    let skip = match bom {
        Some((bom_cp, len)) if bom_cp == codec.cp() => len,
        _ => 0,
    };
    let bad = first_bad(&codec, &data[skip..]);
    let mut text = decode(&codec, &data[skip..]);
    // A U+FEFF left at the start (page given, not detected) counts too.
    let mut has_bom = skip > 0;
    if !has_bom
        && matches!(
            codec.cp(),
            codepage::UTF8 | codepage::UTF16LE | codepage::UTF16BE
        )
        && text.starts_with('\u{FEFF}')
    {
        text.remove(0);
        has_bom = true;
    }
    let (lines, eol) = split_lines(&text);
    Loaded {
        lines,
        cp: codec.cp(),
        bom: has_bom,
        eol,
        bad,
    }
}

/// The first byte sequence `codec` cannot read, if any.
fn first_bad(codec: &Codec, bytes: &[u8]) -> Option<Vec<u8>> {
    if codec.cp() == codepage::UTF8 {
        return std::str::from_utf8(bytes).err().map(|e| {
            let s = e.valid_up_to();
            let n = e.error_len().unwrap_or(bytes.len() - s);
            bytes[s..s + n].to_vec()
        });
    }
    let mut i = 0;
    while i < bytes.len() {
        let (c, n) = codec.decode(&bytes[i..]);
        let n = n.max(1);
        if c == '\u{FFFD}' {
            return Some(bytes[i..(i + n).min(bytes.len())].to_vec());
        }
        i += n;
    }
    None
}

/// The bytes of `lines` in code page `cp` (with a BOM for UTF pages when
/// `bom`); `Err` with the first character the page cannot hold.
pub fn encode(lines: &[Line], cp: u32, bom: bool) -> Result<Vec<u8>, char> {
    let codec = Codec::new(cp).ok_or('\u{FFFD}')?;
    let mut out = Vec::new();
    if bom {
        match cp {
            codepage::UTF8 => out.extend_from_slice(&[0xEF, 0xBB, 0xBF]),
            codepage::UTF16LE => out.extend_from_slice(&[0xFF, 0xFE]),
            codepage::UTF16BE => out.extend_from_slice(&[0xFE, 0xFF]),
            _ => {}
        }
    }
    let utf = matches!(cp, codepage::UTF8 | codepage::UTF16LE | codepage::UTF16BE);
    for line in lines {
        for part in [line.text.as_str(), line.eol.as_str()] {
            if part.is_empty() {
                continue;
            }
            let bytes = codec.encode(part);
            if !utf {
                // A page that skips what it cannot hold: check by decoding.
                let back = decode(&codec, &bytes);
                if back != part {
                    let bad = part
                        .chars()
                        .zip(back.chars().chain(std::iter::repeat('\0')))
                        .find(|(a, b)| a != b)
                        .map_or('\u{FFFD}', |(a, _)| a);
                    return Err(bad);
                }
            }
            out.extend_from_slice(&bytes);
        }
    }
    Ok(out)
}

/// The temporary file `write_file` writes first.
pub fn temp_path(path: &std::path::Path) -> std::path::PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = path.parent().unwrap_or(std::path::Path::new("."));
    dir.join(format!(".{name}.afar-{}.tmp", std::process::id()))
}

/// Writes `data` to `path` safely (Far's `SaveSafely`): a temporary file in
/// the same folder, then `ReplaceFileW` (the original's attributes stay);
/// a new file is renamed into place. Streams (`:` in the name) are
/// written directly.
pub fn write_file(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
    if name.as_deref().is_some_and(|n| n.contains(':')) || !path.exists() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        return std::fs::write(path, data);
    }
    let tmp = temp_path(path);
    std::fs::write(&tmp, data)?;
    match replace_file(path, &tmp) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            // Hard links, reparse points: write in place, as Far does.
            std::fs::write(path, data).map_err(|_| e)
        }
    }
}

#[cfg(windows)]
fn replace_file(target: &std::path::Path, source: &std::path::Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Storage::FileSystem::{REPLACEFILE_IGNORE_MERGE_ERRORS, ReplaceFileW};
    let wide =
        |p: &std::path::Path| -> Vec<u16> { p.as_os_str().encode_wide().chain([0]).collect() };
    let (t, s) = (wide(target), wide(source));
    // SAFETY: NUL-terminated paths; no backup file.
    let ok = unsafe {
        ReplaceFileW(
            t.as_ptr(),
            s.as_ptr(),
            std::ptr::null(),
            REPLACEFILE_IGNORE_MERGE_ERRORS,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if ok != 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(windows))]
fn replace_file(target: &std::path::Path, source: &std::path::Path) -> std::io::Result<()> {
    std::fs::rename(source, target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_endings_are_kept_per_line() {
        let (lines, first) = split_lines("a\r\nb\nc\rd\r\r\ne\r\rf");
        let got: Vec<(&str, Eol)> = lines.iter().map(|l| (l.text.as_str(), l.eol)).collect();
        assert_eq!(
            got,
            [
                ("a", Eol::CrLf),
                ("b", Eol::Lf),
                ("c", Eol::Cr),
                ("d", Eol::CrCrLf),
                ("e", Eol::Cr),
                ("", Eol::Cr),
                ("f", Eol::None),
            ]
        );
        assert_eq!(first, Some(Eol::CrLf));
        // A final line break: an empty last line without an ending.
        let (lines, _) = split_lines("x\n");
        assert_eq!(lines, [Line::new("x", Eol::Lf), Line::new("", Eol::None)]);
    }

    #[test]
    fn round_trip_keeps_bytes() {
        let data = b"\xEF\xBB\xBFone\r\ntwo\nthree\r\r\n";
        let l = load(data, None, true, 1251);
        assert_eq!(l.cp, codepage::UTF8);
        assert!(l.bom);
        assert_eq!(encode(&l.lines, l.cp, l.bom).unwrap(), data);
    }

    #[test]
    fn a_page_reports_what_it_cannot_hold() {
        let lines = [Line::new("abc", Eol::None)];
        assert!(encode(&lines, 1251, false).is_ok());
        let lines = [Line::new("a€文", Eol::None)];
        assert_eq!(encode(&lines, 1251, false), Err('文'));
    }
}
