//! Code pages of the viewer, as Far has them (Windows code page numbers):
//! UTF-8, UTF-16 LE/BE and the system's single- and double-byte pages
//! (`MaxCharSize <= 2`). Text is decoded one character at a time from a
//! byte position, so the viewer can work with byte offsets (Far's model).

pub const UTF8: u32 = 65001;
pub const UTF16LE: u32 = 1200;
pub const UTF16BE: u32 = 1201;

const REPLACEMENT: char = '\u{FFFD}';

enum Kind {
    Utf8,
    Utf16 {
        be: bool,
    },
    /// One byte per character; the table maps every byte.
    Single(Box<[char; 256]>),
    /// Lead bytes start two-byte characters (932, 936, 949, 950).
    Double {
        single: Box<[char; 256]>,
        lead: Box<[bool; 256]>,
    },
}

pub struct Codec {
    cp: u32,
    kind: Kind,
}

impl Codec {
    /// A supported code page, or `None`.
    pub fn new(cp: u32) -> Option<Self> {
        let kind = match cp {
            UTF8 => Kind::Utf8,
            UTF16LE => Kind::Utf16 { be: false },
            UTF16BE => Kind::Utf16 { be: true },
            _ => system::kind(cp)?,
        };
        Some(Self { cp, kind })
    }

    pub fn cp(&self) -> u32 {
        self.cp
    }

    /// Size of a code unit: 2 for UTF-16, otherwise 1.
    pub fn unit(&self) -> usize {
        match self.kind {
            Kind::Utf16 { .. } => 2,
            _ => 1,
        }
    }

    /// The character at the start of `bytes` and its length in bytes
    /// (never 0 for non-empty input). Broken sequences give U+FFFD, one
    /// unit at a time.
    pub fn decode(&self, bytes: &[u8]) -> (char, usize) {
        let Some(&b0) = bytes.first() else {
            return (REPLACEMENT, 0);
        };
        match &self.kind {
            Kind::Utf8 => decode_utf8(bytes),
            Kind::Utf16 { be } => {
                let unit = |i: usize| -> Option<u16> {
                    let b = bytes.get(i..i + 2)?;
                    Some(if *be {
                        u16::from_be_bytes([b[0], b[1]])
                    } else {
                        u16::from_le_bytes([b[0], b[1]])
                    })
                };
                let Some(u) = unit(0) else {
                    // An odd last byte.
                    return (REPLACEMENT, 1);
                };
                if (0xD800..0xDC00).contains(&u)
                    && let Some(lo) = unit(2)
                    && (0xDC00..0xE000).contains(&lo)
                {
                    let c = 0x10000 + ((u32::from(u) - 0xD800) << 10) + (u32::from(lo) - 0xDC00);
                    return (char::from_u32(c).unwrap_or(REPLACEMENT), 4);
                }
                (char::from_u32(u32::from(u)).unwrap_or(REPLACEMENT), 2)
            }
            Kind::Single(table) => (table[usize::from(b0)], 1),
            Kind::Double { single, lead } => {
                if lead[usize::from(b0)] {
                    match bytes.get(1) {
                        Some(&b1) => (system::decode_pair(self.cp, b0, b1), 2),
                        None => (REPLACEMENT, 1),
                    }
                } else {
                    (single[usize::from(b0)], 1)
                }
            }
        }
    }

    /// Text in this code page (for searching bytes); characters it cannot
    /// encode are skipped.
    pub fn encode(&self, text: &str) -> Vec<u8> {
        match &self.kind {
            Kind::Utf8 => text.as_bytes().to_vec(),
            Kind::Utf16 { be } => text
                .encode_utf16()
                .flat_map(|u| {
                    if *be {
                        u.to_be_bytes()
                    } else {
                        u.to_le_bytes()
                    }
                })
                .collect(),
            Kind::Single(table) => text
                .chars()
                .filter_map(|c| table.iter().position(|t| *t == c).map(|b| b as u8))
                .collect(),
            Kind::Double { .. } => system::encode(self.cp, text),
        }
    }

    /// Far's short name in the status line (`ShortReadableCodepageName`).
    pub fn short_name(&self) -> String {
        short_name(self.cp)
    }
}

pub fn short_name(cp: u32) -> String {
    match cp {
        UTF8 => "UTF-8".into(),
        UTF16LE => "U16LE".into(),
        UTF16BE => "U16BE".into(),
        65000 => "UTF-7".into(),
        _ if cp == ansi() => "ANSI".into(),
        _ if cp == oem() => "OEM".into(),
        _ => cp.to_string(),
    }
}

/// A name for menus: "1251 (ANSI - Cyrillic)" in Far; here the number and
/// the system's name when it has one.
pub fn long_name(cp: u32) -> String {
    match cp {
        UTF8 => "65001 (UTF-8)".into(),
        UTF16LE => "1200 (UTF-16 Little endian)".into(),
        UTF16BE => "1201 (UTF-16 Big endian)".into(),
        _ => system::name(cp).unwrap_or_else(|| cp.to_string()),
    }
}

pub fn ansi() -> u32 {
    system::ansi()
}

pub fn oem() -> u32 {
    system::oem()
}

/// Installed code pages the viewer can show (single- and double-byte).
pub fn installed() -> Vec<u32> {
    system::installed()
}

fn decode_utf8(bytes: &[u8]) -> (char, usize) {
    let b0 = bytes[0];
    let len = match b0 {
        0x00..=0x7F => return (char::from(b0), 1),
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => return (REPLACEMENT, 1),
    };
    let Some(seq) = bytes.get(..len) else {
        return (REPLACEMENT, 1);
    };
    match std::str::from_utf8(seq) {
        Ok(s) => (s.chars().next().unwrap_or(REPLACEMENT), len),
        Err(_) => (REPLACEMENT, 1),
    }
}

/// The code page a byte order mark selects, and the mark's length.
pub fn bom(bytes: &[u8]) -> Option<(u32, usize)> {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        Some((UTF8, 3))
    } else if bytes.starts_with(&[0xFF, 0xFE]) {
        Some((UTF16LE, 2))
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        Some((UTF16BE, 2))
    } else {
        None
    }
}

/// Far's `GetFileCodepage`: a BOM; then valid UTF-8 with non-ASCII
/// bytes; then UTF-16 by its zero bytes; then a statistical guess
/// (`chardetng` in place of Far's uchardet). `None`: plain ASCII or no
/// usable guess — the caller takes the default page. `head` is the start
/// of the file (Far reads 32 KB).
pub fn detect(head: &[u8], whole_file: bool) -> Option<u32> {
    if let Some((cp, _)) = bom(head) {
        return Some(cp);
    }
    let ascii = head.is_ascii();
    if !ascii && is_utf8(head, whole_file) {
        return Some(UTF8);
    }
    if let Some(cp) = utf16_by_zeros(head) {
        return Some(cp);
    }
    if ascii {
        return None;
    }
    let mut detector = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
    detector.feed(head, whole_file);
    let guess = detector.guess(None, chardetng::Utf8Detection::Deny);
    from_encoding(guess).filter(|cp| Codec::new(*cp).is_some())
}

/// Valid UTF-8, allowing a sequence cut at the end of a partial read.
fn is_utf8(bytes: &[u8], whole: bool) -> bool {
    match std::str::from_utf8(bytes) {
        Ok(_) => true,
        Err(e) => !whole && e.error_len().is_none() && bytes.len() - e.valid_up_to() < 4,
    }
}

/// Text in UTF-16 has zero high bytes in one parity (ASCII and Latin
/// characters), and almost none in the other.
fn utf16_by_zeros(bytes: &[u8]) -> Option<u32> {
    let pairs = bytes.len() / 2;
    if pairs < 2 {
        return None;
    }
    let zeros = |parity: usize| {
        bytes
            .iter()
            .skip(parity)
            .step_by(2)
            .filter(|b| **b == 0)
            .count()
    };
    let (even, odd) = (zeros(0), zeros(1));
    if odd * 3 >= pairs && even * 20 < pairs {
        Some(UTF16LE)
    } else if even * 3 >= pairs && odd * 20 < pairs {
        Some(UTF16BE)
    } else {
        None
    }
}

/// Windows code page of an encoding_rs encoding.
fn from_encoding(e: &'static encoding_rs::Encoding) -> Option<u32> {
    let name = e.name();
    if let Some(n) = name.strip_prefix("windows-") {
        return n.parse().ok();
    }
    if let Some(n) = name.strip_prefix("ISO-8859-") {
        let n: u32 = n.parse().ok()?;
        return Some(match n {
            13 => 28603,
            15 => 28605,
            _ => 28590 + n,
        });
    }
    Some(match name {
        "UTF-8" => UTF8,
        "UTF-16LE" => UTF16LE,
        "UTF-16BE" => UTF16BE,
        "IBM866" => 866,
        "KOI8-R" => 20866,
        "KOI8-U" => 21866,
        "Shift_JIS" => 932,
        "GBK" | "gb18030" => 936,
        "EUC-KR" => 949,
        "Big5" => 950,
        "macintosh" => 10000,
        "x-mac-cyrillic" => 10007,
        _ => return None,
    })
}

#[cfg(windows)]
mod system {
    use super::{Kind, REPLACEMENT};
    use windows_sys::Win32::Globalization::{
        CP_INSTALLED, CPINFOEXW, EnumSystemCodePagesW, GetACP, GetCPInfoExW, GetOEMCP,
        MB_ERR_INVALID_CHARS, MultiByteToWideChar, WideCharToMultiByte,
    };

    pub fn ansi() -> u32 {
        unsafe { GetACP() }
    }

    pub fn oem() -> u32 {
        unsafe { GetOEMCP() }
    }

    fn info(cp: u32) -> Option<CPINFOEXW> {
        let mut info: CPINFOEXW = unsafe { std::mem::zeroed() };
        (unsafe { GetCPInfoExW(cp, 0, &mut info) } != 0).then_some(info)
    }

    pub fn name(cp: u32) -> Option<String> {
        let info = info(cp)?;
        let end = info
            .CodePageName
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(info.CodePageName.len());
        let name = String::from_utf16_lossy(&info.CodePageName[..end]);
        Some(if name.is_empty() {
            cp.to_string()
        } else {
            name
        })
    }

    fn decode(cp: u32, bytes: &[u8]) -> char {
        let mut out = [0u16; 4];
        let n = unsafe {
            MultiByteToWideChar(
                cp,
                MB_ERR_INVALID_CHARS,
                bytes.as_ptr(),
                bytes.len() as i32,
                out.as_mut_ptr(),
                out.len() as i32,
            )
        };
        if n <= 0 {
            return REPLACEMENT;
        }
        char::decode_utf16(out[..n as usize].iter().copied())
            .next()
            .and_then(Result::ok)
            .unwrap_or(REPLACEMENT)
    }

    pub fn decode_pair(cp: u32, b0: u8, b1: u8) -> char {
        decode(cp, &[b0, b1])
    }

    pub fn encode(cp: u32, text: &str) -> Vec<u8> {
        let wide: Vec<u16> = text.encode_utf16().collect();
        if wide.is_empty() {
            return Vec::new();
        }
        let mut out = vec![0u8; wide.len() * 2 + 4];
        let n = unsafe {
            WideCharToMultiByte(
                cp,
                0,
                wide.as_ptr(),
                wide.len() as i32,
                out.as_mut_ptr(),
                out.len() as i32,
                std::ptr::null(),
                std::ptr::null_mut(),
            )
        };
        out.truncate(n.max(0) as usize);
        out
    }

    pub fn kind(cp: u32) -> Option<Kind> {
        let info = info(cp)?;
        if info.MaxCharSize > 2 {
            return None;
        }
        let mut single = Box::new([REPLACEMENT; 256]);
        for (b, c) in single.iter_mut().enumerate() {
            *c = decode(cp, &[b as u8]);
        }
        // Control characters map to themselves in every page.
        for b in 0..0x20u8 {
            single[usize::from(b)] = char::from(b);
        }
        if info.MaxCharSize == 1 {
            return Some(Kind::Single(single));
        }
        let mut lead = Box::new([false; 256]);
        for range in info.LeadByte.chunks(2) {
            if range[0] == 0 {
                break;
            }
            for b in range[0]..=range[1] {
                lead[usize::from(b)] = true;
            }
        }
        Some(Kind::Double { single, lead })
    }

    thread_local! {
        static FOUND: std::cell::RefCell<Vec<u32>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    unsafe extern "system" fn collect(name: windows_sys::core::PCWSTR) -> i32 {
        let mut len = 0;
        while unsafe { *name.add(len) } != 0 {
            len += 1;
        }
        let text = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(name, len) });
        if let Ok(cp) = text.parse() {
            FOUND.with(|f| f.borrow_mut().push(cp));
        }
        1
    }

    pub fn installed() -> Vec<u32> {
        FOUND.with(|f| f.borrow_mut().clear());
        unsafe { EnumSystemCodePagesW(Some(collect), CP_INSTALLED) };
        let mut pages: Vec<u32> = FOUND
            .with(|f| f.borrow().clone())
            .into_iter()
            .filter(|cp| info(*cp).is_some_and(|i| i.MaxCharSize <= 2))
            .collect();
        pages.sort_unstable();
        pages.dedup();
        pages
    }
}

#[cfg(not(windows))]
mod system {
    use super::{Kind, REPLACEMENT};

    pub fn ansi() -> u32 {
        1252
    }

    pub fn oem() -> u32 {
        437
    }

    fn encoding(cp: u32) -> Option<&'static encoding_rs::Encoding> {
        let label = match cp {
            866 => "ibm866".to_string(),
            20866 => "koi8-r".to_string(),
            21866 => "koi8-u".to_string(),
            28591..=28605 => format!("iso-8859-{}", cp - 28590),
            _ => format!("windows-{cp}"),
        };
        encoding_rs::Encoding::for_label(label.as_bytes())
    }

    pub fn name(cp: u32) -> Option<String> {
        encoding(cp).map(|e| format!("{cp} ({})", e.name()))
    }

    pub fn decode_pair(_cp: u32, _b0: u8, _b1: u8) -> char {
        REPLACEMENT
    }

    pub fn encode(_cp: u32, _text: &str) -> Vec<u8> {
        Vec::new()
    }

    pub fn kind(cp: u32) -> Option<Kind> {
        let e = encoding(cp)?;
        if !e.is_single_byte() {
            return None;
        }
        let mut single = Box::new([REPLACEMENT; 256]);
        for (b, c) in single.iter_mut().enumerate() {
            let (s, _) = e.decode_without_bom_handling(&[b as u8]);
            *c = s.chars().next().unwrap_or(REPLACEMENT);
        }
        Some(Kind::Single(single))
    }

    pub fn installed() -> Vec<u32> {
        [
            866, 1250, 1251, 1252, 1253, 1254, 1255, 1256, 1257, 1258, 20866, 21866,
        ]
        .into_iter()
        .filter(|cp| encoding(*cp).is_some())
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_utf8_and_utf16() {
        let c = Codec::new(UTF8).unwrap();
        assert_eq!(c.decode("я".as_bytes()), ('я', 2));
        assert_eq!(c.decode(&[0xD1]), (REPLACEMENT, 1));
        assert_eq!(c.decode(&[0xFF, b'a']), (REPLACEMENT, 1));
        let c = Codec::new(UTF16LE).unwrap();
        assert_eq!(c.decode(&[0x4F, 0x04]), ('я', 2));
        assert_eq!(c.decode(&[0x3D, 0xD8, 0x00, 0xDE]), ('😀', 4));
        assert_eq!(c.decode(&[0x41]), (REPLACEMENT, 1));
        let c = Codec::new(UTF16BE).unwrap();
        assert_eq!(c.decode(&[0x04, 0x4F]), ('я', 2));
        assert_eq!(c.encode("я"), vec![0x04, 0x4F]);
    }

    #[test]
    fn decodes_single_byte_pages() {
        let c = Codec::new(1251).unwrap();
        assert_eq!(c.decode(&[0xFF]), ('я', 1));
        assert_eq!(c.encode("яA"), vec![0xFF, b'A']);
        let c = Codec::new(866).unwrap();
        assert_eq!(c.decode(&[0xEF]), ('я', 1));
        assert_eq!(c.decode(&[0x09]), ('\t', 1));
    }

    #[test]
    fn detects_like_far() {
        assert_eq!(detect(b"plain ascii", true), None);
        assert_eq!(detect(&[0xEF, 0xBB, 0xBF, b'a'], true), Some(UTF8));
        assert_eq!(detect("привет, мир".as_bytes(), true), Some(UTF8));
        // A cut multibyte character at the end of a partial read.
        let text = "привет".as_bytes();
        assert_eq!(detect(&text[..text.len() - 1], false), Some(UTF8));
        let utf16: Vec<u8> = "hello world"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(detect(&utf16, true), Some(UTF16LE));
        let cp1251 = Codec::new(1251)
            .unwrap()
            .encode("Съешь же ещё этих мягких французских булок, да выпей чаю. Широкая электрификация южных губерний даст мощный толчок подъёму сельского хозяйства.");
        assert_eq!(detect(&cp1251, true), Some(1251));
        let cp866 = Codec::new(866)
            .unwrap()
            .encode("Съешь же ещё этих мягких французских булок, да выпей чаю. Широкая электрификация южных губерний даст мощный толчок подъёму сельского хозяйства.");
        assert_eq!(detect(&cp866, true), Some(866));
    }

    #[test]
    fn short_names() {
        assert_eq!(short_name(UTF8), "UTF-8");
        assert_eq!(short_name(UTF16LE), "U16LE");
        assert_eq!(short_name(ansi()), "ANSI");
    }
}
