use super::*;
use crate::command::EditorCmd as C;

fn ed(text: &str) -> Editor {
    let (lines, eol) = text::split_lines(text);
    let mut e = Editor::new(
        1,
        Path::new("t.txt"),
        lines,
        65001,
        false,
        eol.unwrap_or(Eol::CrLf),
    );
    e.area = Rect::new(0, 0, 40, 10);
    e
}

fn typed(e: &mut Editor, s: &str) {
    for c in s.chars() {
        e.type_char(c);
    }
}

#[test]
fn typing_past_the_end_pads_and_undoes_in_one_step() {
    let mut e = ed("ab\r\ncd");
    e.cursor = Pos::new(0, 4);
    typed(&mut e, "xy");
    assert_eq!(e.text(), "ab  xy\r\ncd");
    assert!(e.modified());
    e.undo();
    assert_eq!(e.text(), "ab\r\ncd");
    assert!(!e.modified());
    e.redo();
    assert_eq!(e.text(), "ab  xy\r\ncd");
}

#[test]
fn enter_keeps_line_endings_as_far() {
    // The new line gets the current line's ending; the last line (no
    // ending) gets the file's.
    let mut e = ed("one\ntwo");
    e.cursor = Pos::new(0, 1);
    e.command(C::Enter);
    assert_eq!(e.text(), "o\nne\ntwo");
    e.command(C::FileEnd);
    e.command(C::Enter);
    assert_eq!(e.text(), "o\nne\ntwo\n");
    assert_eq!(e.cursor, Pos::new(3, 0));
}

#[test]
fn delete_and_backspace_join_lines() {
    let mut e = ed("ab\r\ncd");
    e.cursor = Pos::new(0, 2);
    e.command(C::Delete);
    assert_eq!(e.text(), "abcd");
    e.undo();
    e.cursor = Pos::new(1, 0);
    e.command(C::Backspace);
    assert_eq!(e.text(), "abcd");
    assert_eq!(e.cursor, Pos::new(0, 2));
    // Past the end, Del joins at the cursor.
    let mut e = ed("ab\ncd");
    e.cursor = Pos::new(0, 4);
    e.command(C::Delete);
    assert_eq!(e.text(), "ab  cd");
}

#[test]
fn selection_copy_typing_replaces_it() {
    let mut e = ed("hello world");
    e.command(C::SelWordRight);
    assert_eq!(e.selected_text().as_deref(), Some("hello "));
    e.type_char('X');
    assert_eq!(e.text(), "Xworld");
    e.undo();
    assert_eq!(e.text(), "hello world");
}

#[test]
fn paste_multiline_and_mixed_endings_survive() {
    let mut e = ed("a\r\nb\nc");
    e.cursor = Pos::new(1, 1);
    e.insert_text("1\n2");
    // The pasted break gets the ending of the line at the cursor (LF).
    assert_eq!(e.text(), "a\r\nb1\n2\nc");
    assert_eq!(e.cursor, Pos::new(2, 1));
}

#[test]
fn delete_line_and_words() {
    let mut e = ed("one two\nthree");
    e.cursor = Pos::new(0, 0);
    e.command(C::DeleteWordRight);
    assert_eq!(e.text(), "two\nthree");
    e.command(C::DeleteLine);
    assert_eq!(e.text(), "three");
    e.command(C::DeleteLine);
    assert_eq!(e.text(), "");
    assert_eq!(e.line_count(), 1);
}

#[test]
fn vertical_moves_keep_the_screen_column() {
    let mut e = ed("\tx\nabcdefghij\nab");
    e.cursor = Pos::new(1, 9);
    e.command(C::Up);
    // Column 9 is inside the tab's cells (0..8) or after: char 1 ('x').
    assert_eq!(e.cursor, Pos::new(0, 2));
    e.command(C::Down);
    assert_eq!(e.cursor, Pos::new(1, 9));
}

#[test]
fn lock_blocks_editing() {
    let mut e = ed("abc");
    e.command(C::Lock);
    e.type_char('x');
    e.command(C::Delete);
    assert_eq!(e.text(), "abc");
}
