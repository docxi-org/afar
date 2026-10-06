//! Encoding of key presses into the byte sequences a program running in a
//! terminal expects (xterm conventions).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

/// xterm modifier parameter: 1 + shift(1) + alt(2) + ctrl(4).
fn modifier_param(m: KeyModifiers) -> u8 {
    1 + u8::from(m.contains(KeyModifiers::SHIFT))
        + 2 * u8::from(m.contains(KeyModifiers::ALT))
        + 4 * u8::from(m.contains(KeyModifiers::CONTROL))
}

/// `CSI <final>` or `SS3 <final>` (application mode) without modifiers,
/// `CSI 1;<mod> <final>` with them.
fn cursor_key(out: &mut Vec<u8>, m: KeyModifiers, app_cursor: bool, fin: u8) {
    let param = modifier_param(m);
    if param > 1 {
        out.extend_from_slice(format!("\x1b[1;{param}").as_bytes());
        out.push(fin);
    } else if app_cursor {
        out.extend_from_slice(b"\x1bO");
        out.push(fin);
    } else {
        out.extend_from_slice(b"\x1b[");
        out.push(fin);
    }
}

/// `CSI <n> ~` or `CSI <n>;<mod> ~`.
fn tilde_key(out: &mut Vec<u8>, m: KeyModifiers, n: u8) {
    let param = modifier_param(m);
    if param > 1 {
        out.extend_from_slice(format!("\x1b[{n};{param}~").as_bytes());
    } else {
        out.extend_from_slice(format!("\x1b[{n}~").as_bytes());
    }
}

/// Latin key at the same position as a Russian/Ukrainian letter (ЙЦУКЕН).
fn latin_equivalent(c: char) -> Option<char> {
    const RU: &str = "йцукенгшщзхъфывапролджэячсмитьбюёіїєґ";
    const EN: &str = "qwertyuiop[]asdfghjkl;'zxcvbnm,.`s]'\\";
    let lower = c.to_lowercase().next()?;
    RU.chars()
        .position(|r| r == lower)
        .and_then(|i| EN.chars().nth(i))
}

/// Makes Ctrl/Alt shortcuts independent of the keyboard layout: with a
/// Cyrillic layout Ctrl+O arrives as Ctrl+Щ. AltGr (Ctrl+Alt) is left alone
/// because it produces real characters.
pub fn normalize(mut key: KeyEvent) -> KeyEvent {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    if (ctrl ^ alt)
        && let KeyCode::Char(c) = key.code
    {
        if let Some(l) = latin_equivalent(c) {
            key.code = KeyCode::Char(l);
        }
    }
    key
}

pub fn encode(key: &KeyEvent, app_cursor: bool) -> Option<Vec<u8>> {
    let m = key.modifiers;
    let alt = m.contains(KeyModifiers::ALT);
    let ctrl = m.contains(KeyModifiers::CONTROL);
    let mut out = Vec::new();
    match key.code {
        KeyCode::Char(c) => {
            if alt {
                out.push(0x1b);
            }
            if ctrl {
                let b = match c.to_ascii_lowercase() {
                    c @ 'a'..='z' => c as u8 - b'a' + 1,
                    ' ' | '@' | '2' => 0,
                    '[' | '3' => 0x1b,
                    '\\' | '4' => 0x1c,
                    ']' | '5' => 0x1d,
                    '^' | '6' => 0x1e,
                    '_' | '-' | '7' => 0x1f,
                    '?' | '8' => 0x7f,
                    _ => return None,
                };
                out.push(b);
            } else {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
        KeyCode::Enter => {
            // Shift/Alt+Enter: ESC CR, which line editors (Claude Code
            // included) treat as "insert newline".
            if alt || m.contains(KeyModifiers::SHIFT) {
                out.push(0x1b);
            }
            out.push(b'\r');
        }
        KeyCode::Tab => out.push(b'\t'),
        KeyCode::BackTab => out.extend_from_slice(b"\x1b[Z"),
        KeyCode::Backspace => {
            if alt {
                out.push(0x1b);
            }
            out.push(if ctrl { 0x08 } else { 0x7f });
        }
        KeyCode::Esc => out.push(0x1b),
        KeyCode::Up => cursor_key(&mut out, m, app_cursor, b'A'),
        KeyCode::Down => cursor_key(&mut out, m, app_cursor, b'B'),
        KeyCode::Right => cursor_key(&mut out, m, app_cursor, b'C'),
        KeyCode::Left => cursor_key(&mut out, m, app_cursor, b'D'),
        KeyCode::Home => cursor_key(&mut out, m, app_cursor, b'H'),
        KeyCode::End => cursor_key(&mut out, m, app_cursor, b'F'),
        KeyCode::Insert => tilde_key(&mut out, m, 2),
        KeyCode::Delete => tilde_key(&mut out, m, 3),
        KeyCode::PageUp => tilde_key(&mut out, m, 5),
        KeyCode::PageDown => tilde_key(&mut out, m, 6),
        KeyCode::F(n @ 1..=4) => {
            let fin = b"PQRS"[usize::from(n - 1)];
            let param = modifier_param(m);
            if param > 1 {
                out.extend_from_slice(format!("\x1b[1;{param}").as_bytes());
                out.push(fin);
            } else {
                out.extend_from_slice(b"\x1bO");
                out.push(fin);
            }
        }
        KeyCode::F(n @ 5..=12) => {
            let code = [15, 17, 18, 19, 20, 21, 23, 24][usize::from(n - 5)];
            tilde_key(&mut out, m, code);
        }
        _ => return None,
    }
    Some(out)
}

/// Encodes a mouse event at `col`/`row` (0-based, relative to the
/// program's screen) as the program asked for; `None` if the program's
/// mouse mode does not report this kind of event.
pub fn encode_mouse(
    ev: &MouseEvent,
    col: u16,
    row: u16,
    mode: vt100::MouseProtocolMode,
    encoding: vt100::MouseProtocolEncoding,
) -> Option<Vec<u8>> {
    use vt100::MouseProtocolMode as Mode;
    let button = |b: MouseButton| match b {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    };
    // (button code, is release, is motion)
    let (mut code, release, motion) = match ev.kind {
        MouseEventKind::Down(b) => (button(b), false, false),
        MouseEventKind::Up(b) => (button(b), true, false),
        MouseEventKind::Drag(b) => (button(b) + 32, false, true),
        MouseEventKind::Moved => (3 + 32, false, true),
        MouseEventKind::ScrollUp => (64, false, false),
        MouseEventKind::ScrollDown => (65, false, false),
        MouseEventKind::ScrollLeft => (66, false, false),
        MouseEventKind::ScrollRight => (67, false, false),
    };
    let reported = match mode {
        Mode::None => false,
        Mode::Press => !release && !motion,
        Mode::PressRelease => !motion,
        Mode::ButtonMotion => ev.kind != MouseEventKind::Moved,
        Mode::AnyMotion => true,
    };
    if !reported {
        return None;
    }
    let m = ev.modifiers;
    code += 4 * u8::from(m.contains(KeyModifiers::SHIFT))
        + 8 * u8::from(m.contains(KeyModifiers::ALT))
        + 16 * u8::from(m.contains(KeyModifiers::CONTROL));
    let (x, y) = (u32::from(col) + 1, u32::from(row) + 1);
    match encoding {
        vt100::MouseProtocolEncoding::Sgr => {
            Some(format!("\x1b[<{code};{x};{y}{}", if release { 'm' } else { 'M' }).into_bytes())
        }
        legacy => {
            // X10 style: the release of any button is button 3.
            let code = if release { 3 | (code & !3) } else { code };
            let mut out = b"\x1b[M".to_vec();
            for v in [u32::from(code), x, y] {
                let v = v + 32;
                match legacy {
                    vt100::MouseProtocolEncoding::Utf8 => {
                        let mut b = [0u8; 4];
                        out.extend_from_slice(char::from_u32(v)?.encode_utf8(&mut b).as_bytes());
                    }
                    _ => out.push(u8::try_from(v).ok()?),
                }
            }
            Some(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, m: KeyModifiers) -> Vec<u8> {
        encode(&KeyEvent::new(code, m), false).unwrap()
    }

    #[test]
    fn encodes_common_keys() {
        assert_eq!(key(KeyCode::Char('a'), KeyModifiers::NONE), b"a");
        assert_eq!(key(KeyCode::Char('ж'), KeyModifiers::NONE), "ж".as_bytes());
        assert_eq!(key(KeyCode::Char('c'), KeyModifiers::CONTROL), b"\x03");
        assert_eq!(key(KeyCode::Char('x'), KeyModifiers::ALT), b"\x1bx");
        assert_eq!(key(KeyCode::Enter, KeyModifiers::NONE), b"\r");
        assert_eq!(key(KeyCode::Enter, KeyModifiers::SHIFT), b"\x1b\r");
        assert_eq!(key(KeyCode::Up, KeyModifiers::NONE), b"\x1b[A");
        assert_eq!(key(KeyCode::Up, KeyModifiers::CONTROL), b"\x1b[1;5A");
        assert_eq!(key(KeyCode::Delete, KeyModifiers::NONE), b"\x1b[3~");
        assert_eq!(key(KeyCode::F(1), KeyModifiers::NONE), b"\x1bOP");
        assert_eq!(key(KeyCode::F(5), KeyModifiers::SHIFT), b"\x1b[15;2~");
        assert_eq!(
            encode(&KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), true).unwrap(),
            b"\x1bOA"
        );
    }

    #[test]
    fn encodes_mouse() {
        use vt100::{MouseProtocolEncoding as Enc, MouseProtocolMode as Mode};
        let ev = |kind| MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        let down = ev(MouseEventKind::Down(MouseButton::Left));
        let up = ev(MouseEventKind::Up(MouseButton::Left));
        assert_eq!(
            encode_mouse(&down, 4, 2, Mode::PressRelease, Enc::Sgr).unwrap(),
            b"\x1b[<0;5;3M"
        );
        assert_eq!(
            encode_mouse(&up, 4, 2, Mode::PressRelease, Enc::Sgr).unwrap(),
            b"\x1b[<0;5;3m"
        );
        assert_eq!(
            encode_mouse(&down, 0, 0, Mode::Press, Enc::Default).unwrap(),
            b"\x1b[M !!"
        );
        assert_eq!(
            encode_mouse(&up, 0, 0, Mode::PressRelease, Enc::Default).unwrap(),
            b"\x1b[M#!!"
        );
        assert!(encode_mouse(&up, 0, 0, Mode::Press, Enc::Sgr).is_none());
        assert!(encode_mouse(&down, 0, 0, Mode::None, Enc::Sgr).is_none());
        let wheel = ev(MouseEventKind::ScrollDown);
        assert_eq!(
            encode_mouse(&wheel, 0, 0, Mode::Press, Enc::Sgr).unwrap(),
            b"\x1b[<65;1;1M"
        );
    }

    #[test]
    fn normalizes_cyrillic_shortcuts() {
        let n = |c, m| normalize(KeyEvent::new(KeyCode::Char(c), m)).code;
        assert_eq!(n('щ', KeyModifiers::CONTROL), KeyCode::Char('o'));
        assert_eq!(n('С', KeyModifiers::CONTROL), KeyCode::Char('c'));
        assert_eq!(n('ф', KeyModifiers::ALT), KeyCode::Char('a'));
        // Plain typing and AltGr keep the character.
        assert_eq!(n('щ', KeyModifiers::NONE), KeyCode::Char('щ'));
        assert_eq!(
            n('щ', KeyModifiers::CONTROL | KeyModifiers::ALT),
            KeyCode::Char('щ')
        );
    }
}
