//! The selection in a line being edited (Far's `Edit`): dialog fields and
//! the command line. The owner keeps the text, the cursor and the anchor
//! (where the selection began); `key` does what a key does to the
//! selection and leaves the rest of the editing to the owner.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// What became of a key.
#[derive(Debug, PartialEq, Eq)]
pub enum Done {
    /// Done here.
    Yes,
    /// Not a selection key: the owner edits (a typed character has already
    /// replaced the selection).
    No,
    /// To the clipboard (Ctrl+Ins; Shift+Del has also cut it out).
    Copy(String),
    /// Shift+Ins: the clipboard's text goes in (`insert`).
    Paste,
}

/// The selection, `from..to` in characters, when there is one.
pub fn range(text: &str, cursor: usize, anchor: Option<usize>) -> Option<(usize, usize)> {
    let len = text.chars().count();
    let (a, c) = (anchor?.min(len), cursor.min(len));
    (a != c).then(|| (a.min(c), a.max(c)))
}

/// The selected text.
pub fn selected(text: &str, cursor: usize, anchor: Option<usize>) -> Option<String> {
    let (from, to) = range(text, cursor, anchor)?;
    Some(text.chars().skip(from).take(to - from).collect())
}

/// Deletes the selection; whether there was one.
pub fn delete(text: &mut String, cursor: &mut usize, anchor: &mut Option<usize>) -> bool {
    let Some((from, to)) = range(text, *cursor, *anchor) else {
        *anchor = None;
        return false;
    };
    *text = text
        .chars()
        .take(from)
        .chain(text.chars().skip(to))
        .collect();
    *cursor = from;
    *anchor = None;
    true
}

/// Puts `s` at the cursor in place of the selection (line breaks become
/// spaces, as Far's `flatten_string`).
pub fn insert(text: &mut String, cursor: &mut usize, anchor: &mut Option<usize>, s: &str) {
    delete(text, cursor, anchor);
    let s: String = s
        .trim_end_matches(['\r', '\n'])
        .replace("\r\n", "\n")
        .chars()
        .map(|c| if c == '\r' || c == '\n' { ' ' } else { c })
        .collect();
    let at = text
        .char_indices()
        .nth(*cursor)
        .map_or(text.len(), |(i, _)| i);
    text.insert_str(at, &s);
    *cursor += s.chars().count();
}

/// Far's WordDiv (config.cpp) plus blanks.
const WORD_DIV: &str = "~!%^&*()+|{}:\"<>?`-=\\[];',./";

pub fn is_div(c: char) -> bool {
    c.is_whitespace() || WORD_DIV.contains(c)
}

/// Far's Ctrl+←: to the start of the previous word.
pub fn word_left(s: &[char], cur: usize) -> usize {
    let mut p = cur.min(s.len()).saturating_sub(1);
    while p > 0 && !(!is_div(s[p]) && is_div(s[p - 1]) && !s[p].is_whitespace()) {
        if !s[p].is_whitespace() && s[p - 1].is_whitespace() {
            break;
        }
        p -= 1;
    }
    p
}

/// Far's Ctrl+→: to the end of the word.
pub fn word_right(s: &[char], cur: usize) -> usize {
    if cur >= s.len() {
        return cur;
    }
    let mut p = cur + 1;
    while p < s.len() && !(is_div(s[p]) && !is_div(s[p - 1])) {
        if !s[p].is_whitespace() && s[p - 1].is_whitespace() {
            break;
        }
        p += 1;
    }
    p
}

/// Far's Ctrl+Backspace: deletes back to a word boundary.
fn word_start_for_delete(s: &[char], cur: usize) -> usize {
    let mut p = cur.min(s.len());
    while p > 0 {
        let stop = p > 1 && s[p - 1].is_whitespace() != s[p - 2].is_whitespace();
        p -= 1;
        if p == 0 || stop || is_div(s[p - 1]) {
            break;
        }
    }
    p
}

/// Far's Ctrl+T / Ctrl+Del: deletes forward to a word boundary.
fn word_end_for_delete(s: &[char], cur: usize) -> usize {
    let mut end = cur;
    while end < s.len() {
        let stop = end + 1 < s.len() && s[end].is_whitespace() && !s[end + 1].is_whitespace();
        end += 1;
        if end >= s.len() || stop || is_div(s[end]) {
            break;
        }
    }
    end
}

/// The editing keys (after `key`): typing, BS / Del (with Ctrl by words,
/// Ctrl+Shift+BS to the start), Ctrl+T, Ctrl+Y, Ctrl+K, the arrows (with
/// Ctrl by words; Ctrl+S / Ctrl+D as in WordStar), Home, End. Whether the
/// key was one of them.
pub fn edit(text: &mut String, cursor: &mut usize, key: &KeyEvent) -> bool {
    let m = key.modifiers;
    let (ctrl, alt, shift) = (
        m.contains(KeyModifiers::CONTROL),
        m.contains(KeyModifiers::ALT),
        m.contains(KeyModifiers::SHIFT),
    );
    let mut chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let cur = (*cursor).min(len);
    match key.code {
        // Ctrl+Alt is AltGr on many layouts.
        KeyCode::Char(c) if !(ctrl ^ alt) => {
            chars.insert(cur, c);
            *cursor = cur + 1;
        }
        KeyCode::Backspace if ctrl && shift => {
            chars.drain(..cur);
            *cursor = 0;
        }
        KeyCode::Backspace if ctrl => {
            let start = word_start_for_delete(&chars, cur);
            chars.drain(start..cur);
            *cursor = start;
        }
        KeyCode::Backspace if cur > 0 => {
            chars.remove(cur - 1);
            *cursor = cur - 1;
        }
        KeyCode::Delete | KeyCode::Char('t') if ctrl && cur < len => {
            let end = word_end_for_delete(&chars, cur);
            chars.drain(cur..end);
        }
        KeyCode::Delete if cur < len => {
            chars.remove(cur);
        }
        KeyCode::Char('y') if ctrl => {
            chars.clear();
            *cursor = 0;
        }
        KeyCode::Char('k') if ctrl => chars.truncate(cur),
        KeyCode::Left if ctrl => *cursor = word_left(&chars, cur),
        KeyCode::Right if ctrl => *cursor = word_right(&chars, cur),
        KeyCode::Left => *cursor = cur.saturating_sub(1),
        KeyCode::Char('s') if ctrl => *cursor = cur.saturating_sub(1),
        KeyCode::Right => *cursor = (cur + 1).min(len),
        KeyCode::Char('d') if ctrl => *cursor = (cur + 1).min(len),
        KeyCode::Home => *cursor = 0,
        KeyCode::End => *cursor = len,
        KeyCode::Backspace | KeyCode::Delete => {}
        _ => return false,
    }
    *text = chars.into_iter().collect();
    *cursor = (*cursor).min(text.chars().count());
    true
}

/// The selection keys (Far's `Edit::ProcessKey`): Shift with the arrows,
/// Home and End, Ctrl+Shift with the arrows by words, Ctrl+A all; Ctrl+Ins
/// / Ctrl+C copy (the whole line without a selection), Shift+Del / Ctrl+X
/// cut, Shift+Ins / Ctrl+V paste; Del, BS and Ctrl+D delete the selection,
/// a typed character replaces it. Any other key drops it.
pub fn key(
    text: &mut String,
    cursor: &mut usize,
    anchor: &mut Option<usize>,
    key: &KeyEvent,
) -> Done {
    let m = key.modifiers;
    let (ctrl, alt, shift) = (
        m.contains(KeyModifiers::CONTROL),
        m.contains(KeyModifiers::ALT),
        m.contains(KeyModifiers::SHIFT),
    );
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    *cursor = (*cursor).min(len);
    // Shift moves: the selection goes from where it began to the cursor.
    let moved = match key.code {
        KeyCode::Left if shift && !alt && ctrl => Some(word_left(&chars, *cursor)),
        KeyCode::Right if shift && !alt && ctrl => Some(word_right(&chars, *cursor)),
        KeyCode::Left if shift && !alt => Some(cursor.saturating_sub(1)),
        KeyCode::Right if shift && !alt => Some((*cursor + 1).min(len)),
        KeyCode::Home if shift && !alt && !ctrl => Some(0),
        KeyCode::End if shift && !alt && !ctrl => Some(len),
        _ => None,
    };
    if let Some(to) = moved {
        let from = anchor.unwrap_or(*cursor);
        *cursor = to;
        *anchor = (from != to).then_some(from);
        return Done::Yes;
    }
    let plain_ctrl = ctrl && !alt && !shift;
    match key.code {
        KeyCode::Char('a') if plain_ctrl => {
            *anchor = Some(0);
            *cursor = len;
            Done::Yes
        }
        KeyCode::Insert if plain_ctrl => {
            Done::Copy(selected(text, *cursor, *anchor).unwrap_or_else(|| text.clone()))
        }
        KeyCode::Char('c') if plain_ctrl => {
            Done::Copy(selected(text, *cursor, *anchor).unwrap_or_else(|| text.clone()))
        }
        KeyCode::Delete if shift && !ctrl && !alt => cut(text, cursor, anchor),
        KeyCode::Char('x') if plain_ctrl => cut(text, cursor, anchor),
        KeyCode::Insert if shift && !ctrl && !alt => Done::Paste,
        KeyCode::Char('v') if plain_ctrl => Done::Paste,
        KeyCode::Backspace | KeyCode::Delete if m.is_empty() && delete(text, cursor, anchor) => {
            Done::Yes
        }
        KeyCode::Char('d') if plain_ctrl && delete(text, cursor, anchor) => Done::Yes,
        // Ctrl+Alt is AltGr on many layouts.
        KeyCode::Char(_) if !(ctrl ^ alt) => {
            delete(text, cursor, anchor);
            Done::No
        }
        _ => {
            *anchor = None;
            Done::No
        }
    }
}

fn cut(text: &mut String, cursor: &mut usize, anchor: &mut Option<usize>) -> Done {
    match selected(text, *cursor, *anchor) {
        Some(s) => {
            delete(text, cursor, anchor);
            Done::Copy(s)
        }
        None => Done::Yes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn words_like_far() {
        let s = chars("copy foo.txt d:\\x");
        assert_eq!(word_left(&s, s.len()), 16);
        assert_eq!(word_left(&s, 16), 13);
        assert_eq!(word_left(&s, 9), 5);
        assert_eq!(word_left(&s, 5), 0);
        assert_eq!(word_right(&s, 0), 4);
        assert_eq!(word_right(&s, 4), 5);
        assert_eq!(word_right(&s, 5), 8);
        let s = chars("git commit");
        assert_eq!(word_start_for_delete(&s, s.len()), 4);
        assert_eq!(word_end_for_delete(&s, 0), 3);
    }

    #[test]
    fn selects_copies_cuts_and_types_over() {
        let (mut t, mut c, mut a) = ("copy file.txt".to_string(), 13, None);
        let shift = KeyModifiers::SHIFT;
        let cs = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        assert_eq!(
            key(&mut t, &mut c, &mut a, &k(KeyCode::Left, cs)),
            Done::Yes
        );
        assert_eq!(range(&t, c, a), Some((10, 13)));
        assert_eq!(
            key(
                &mut t,
                &mut c,
                &mut a,
                &k(KeyCode::Insert, KeyModifiers::CONTROL)
            ),
            Done::Copy("txt".into())
        );
        // A typed character replaces the selection; the owner inserts it.
        assert_eq!(
            key(
                &mut t,
                &mut c,
                &mut a,
                &k(KeyCode::Char('m'), KeyModifiers::NONE)
            ),
            Done::No
        );
        assert_eq!((t.as_str(), c, a), ("copy file.", 10, None));
        assert_eq!(
            key(&mut t, &mut c, &mut a, &k(KeyCode::Home, shift)),
            Done::Yes
        );
        assert_eq!(
            key(&mut t, &mut c, &mut a, &k(KeyCode::Delete, shift)),
            Done::Copy("copy file.".into())
        );
        assert_eq!((t.as_str(), c), ("", 0));
        // Without a selection Ctrl+Ins copies the whole line; other keys
        // drop the selection.
        let (mut t, mut c, mut a) = ("dir".to_string(), 1, Some(3));
        assert_eq!(
            key(
                &mut t,
                &mut c,
                &mut a,
                &k(KeyCode::Left, KeyModifiers::NONE)
            ),
            Done::No
        );
        assert_eq!(a, None);
        assert_eq!(
            key(
                &mut t,
                &mut c,
                &mut a,
                &k(KeyCode::Char('c'), KeyModifiers::CONTROL)
            ),
            Done::Copy("dir".into())
        );
        insert(&mut t, &mut c, &mut a, "a\r\nb\r\n");
        assert_eq!(t, "da bir");
    }
}
