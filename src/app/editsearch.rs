//! Search, replace, "find all" and go-to in the editor (F7, Ctrl+F7,
//! Shift+F7, Alt+F7, Alt+F8; docs/17 §5–6). The query is shared with the
//! viewers, as in Far.

use super::App;
use super::editors::Ask;
use super::fileops::{Overlay, Purpose};
use super::panelcmds::MenuPurpose;
use crate::dialog::{Button, Dialog, button_at, button_width, check_at, input_at, text_at};
use crate::editor::{Finder, Found, Pos};
use crate::menu::{Item, Menu};
use crate::tr;
use crate::viewer::search::Query;

/// One coordinate of Alt+F8 from 0 (Far's `GoToPosition`): a number
/// counts from 1, a percentage of `whole` and `+N`/`-N` from `current`.
fn goto_index(v: crate::viewer::GotoValue, current: usize, whole: usize) -> i128 {
    let value = i128::from(v.value);
    let n = if v.percent {
        value.min(100) * whole as i128 / 100
    } else if v.sign.is_none() {
        value - 1
    } else {
        value
    };
    match v.sign {
        Some('+') => current as i128 + n,
        Some('-') => current as i128 - n,
        _ => n,
    }
}

/// A replace pass waiting for the user's answer about a match.
pub(super) struct ReplaceRun {
    id: u32,
    finder: Finder,
    with: String,
    backward: bool,
    /// A match was found in this pass (else "not found" at its end).
    any: bool,
}

impl App {
    /// Rows of the terminal (Far's `ScrY + 1`) for placing a match.
    fn editor_screen_rows(&self, i: usize) -> usize {
        usize::from(self.editors[i].area.height) + 2
    }

    // ------------------------------------------------------------- dialog

    /// F7 / Ctrl+F7: Far's search or replace dialog (stddlg.cpp
    /// `GetSearchReplaceString`, 76×12 / 76×14).
    pub(super) fn editor_search_dialog(&mut self, i: usize, replace: bool) {
        if replace && self.editors[i].locked {
            return;
        }
        let q = &self.viewer_query;
        let word = tr!("MSearchReplacePickWord");
        let sel = tr!("MSearchReplacePickSelection");
        let x_sel = 71 - button_width(&sel);
        let x_word = x_sel - 1 - button_width(&word);
        let title = if replace {
            tr!("MSearchReplaceReplaceTitle")
        } else {
            tr!("MSearchReplaceSearchTitle")
        };
        let mut d = Dialog::far(title, 76)
            .row(vec![
                text_at(5, tr!("MSearchReplaceSearchFor")),
                button_at(x_word, word),
                button_at(x_sel, sel),
            ])
            .row(vec![
                input_at(5, 65, q.text.clone(), Some("SearchText")).use_last(),
            ]);
        if replace {
            d = d
                .row(vec![text_at(5, tr!("MSearchReplaceReplaceWith"))])
                .row(vec![
                    input_at(
                        5,
                        65,
                        self.editor_replace.clone().unwrap_or_default(),
                        Some("ReplaceText"),
                    )
                    .use_last(),
                ]);
        }
        d = d
            .separator()
            .row(vec![
                check_at(5, tr!("MSearchReplaceCase"), q.case),
                check_at(39, tr!("MSearchReplaceRegexp"), q.regex),
            ])
            .row(vec![check_at(5, tr!("MSearchReplaceWholeWords"), q.words)]);
        let mut fuzzy = vec![check_at(5, tr!("MSearchReplaceFuzzy"), false).disabled()];
        if replace {
            fuzzy.push(check_at(39, tr!("MSearchReplacePreserveStyle"), false).disabled());
        }
        d = d.row(fuzzy).separator();
        let buttons = if replace {
            vec![
                Button::new(tr!("MSearchReplaceReplacePrev")),
                Button::new(tr!("MSearchReplaceReplaceNext")).default(),
                Button::new(tr!("MSearchReplaceCancel")),
            ]
        } else {
            vec![
                Button::new(tr!("MSearchReplaceFindPrev")),
                Button::new(tr!("MSearchReplaceFindNext")).default(),
                Button::new(tr!("MSearchReplaceAll")),
                Button::new(tr!("MSearchReplaceCancel")),
            ]
        };
        let dialog = d.button_row(buttons).focus_item(2);
        let id = self.editors[i].id;
        self.overlays.push(Overlay::Dialog {
            dialog,
            purpose: Purpose::Editor(Ask::Search { id, replace }),
        });
    }

    /// "Word" / "Selection" in the dialog: into the search field.
    pub(super) fn editor_search_pick(&mut self, id: u32, n: usize) {
        let Some(e) = self.editors.iter().find(|e| e.id == id) else {
            return;
        };
        let text = match n {
            0 => e.word_at_cursor().unwrap_or_default(),
            _ => e.selection_first_line(),
        };
        if let Some(Overlay::Dialog { dialog, .. }) = self.overlays.last_mut() {
            dialog.insert_input(0, &text);
        }
    }

    pub(super) fn editor_search_closed(
        &mut self,
        id: u32,
        replace: bool,
        button: Option<usize>,
        dialog: &crate::dialog::Dialog,
    ) {
        let (backward, all) = match button {
            Some(0) => (true, false),
            Some(1) => (false, false),
            Some(2) if !replace => (false, true),
            _ => return,
        };
        let query = Query {
            text: dialog.input_value(0),
            hex: false,
            case: dialog.checked(0),
            regex: dialog.checked(1),
            words: dialog.checked(2),
        };
        if query.text.is_empty() {
            return;
        }
        self.viewer_query = query;
        self.editor_replace = replace.then(|| dialog.input_value(1));
        let Some(i) = self.editor_index(id) else {
            return;
        };
        if all {
            self.editor_find_all(i);
        } else {
            self.editor_search_run(i, backward, false);
        }
    }

    /// Shift+F7 / Alt+F7: the last search or replace on, from the match
    /// the cursor is on.
    pub(super) fn editor_search_continue(&mut self, i: usize, backward: bool) {
        if self.viewer_query.text.is_empty() || self.viewer_query.hex {
            self.editor_search_dialog(i, false);
            return;
        }
        self.editor_search_run(i, backward, true);
    }

    fn editor_finder(&mut self) -> Option<Finder> {
        match Finder::new(&self.viewer_query) {
            Ok(f) => Some(f),
            Err(err) => {
                self.message(&tr!("MSearchReplaceSearchTitle"), &[err], true);
                None
            }
        }
    }

    fn editor_not_found(&mut self) {
        let q = format!("\"{}\"", self.viewer_query.text);
        self.message(
            &tr!("MSearchReplaceSearchTitle"),
            &[tr!("MEditNotFound"), q],
            true,
        );
    }

    /// A search (or a replace pass) from the cursor; `cont`: Far's
    /// "search next" moves past the match the cursor is on.
    fn editor_search_run(&mut self, i: usize, backward: bool, cont: bool) {
        let Some(finder) = self.editor_finder() else {
            return;
        };
        let e = &self.editors[i];
        let at_end = e.settings.search_cursor_at_end;
        let c = e.cursor;
        let mut from = c;
        if cont && let Some(f) = e.last_found {
            let anchor = if at_end { f.end } else { f.start };
            if c == anchor {
                from = if backward {
                    f.start
                } else {
                    Pos::new(f.start.line, f.start.col + 1)
                };
            }
        }
        if let Some(with) = self.editor_replace.clone() {
            if e.locked {
                return;
            }
            let run = ReplaceRun {
                id: e.id,
                finder,
                with,
                backward,
                any: false,
            };
            self.editor_replace_next(run, from);
            return;
        }
        let rows = self.editor_screen_rows(i);
        let e = &mut self.editors[i];
        match e.find(&finder, from, backward) {
            Some(m) => {
                let select = e.settings.search_select_found;
                e.last_found = Some(m);
                e.show_found(m, at_end, select, rows);
            }
            None => self.editor_not_found(),
        }
    }

    // ------------------------------------------------------------ replace

    /// The next match of a replace pass: shown and asked about.
    fn editor_replace_next(&mut self, mut run: ReplaceRun, from: Pos) {
        let Some(i) = self.editor_index(run.id) else {
            return;
        };
        let rows = self.editor_screen_rows(i);
        let e = &mut self.editors[i];
        let Some(m) = e.find(&run.finder, from, run.backward) else {
            if !run.any {
                self.editor_not_found();
            }
            return;
        };
        run.any = true;
        e.last_found = Some(m);
        e.show_found(m, false, false, rows);
        e.highlight = Some(m);
        let lines = vec![
            tr!("MEditAskReplace"),
            format!("\"{}\"", e.found_text(m)),
            tr!("MEditAskReplaceWith"),
            format!(
                "\"{}\"",
                e.replacement(&run.finder, m, &run.with).replace('\n', "↵")
            ),
        ];
        let dialog = Dialog::message(
            &tr!("MSearchReplaceReplaceTitle"),
            &lines,
            &[
                &tr!("MEditReplace"),
                &tr!("MEditReplaceAll"),
                &tr!("MEditSkip"),
                &tr!("MEditCancel"),
            ],
            false,
        );
        self.overlays.push(Overlay::Dialog {
            dialog,
            purpose: Purpose::Editor(Ask::Replace { run, found: m }),
        });
    }

    /// The answer about one match: replace, all, skip, cancel.
    pub(super) fn editor_replace_answer(
        &mut self,
        run: ReplaceRun,
        m: Found,
        button: Option<usize>,
    ) {
        let Some(i) = self.editor_index(run.id) else {
            return;
        };
        let e = &mut self.editors[i];
        e.highlight = None;
        let at_end = e.settings.search_cursor_at_end;
        match button {
            Some(0) => {
                let new = e.replacement(&run.finder, m, &run.with);
                let end = e.replace_found(m, &new);
                let next = match (run.backward, m.start == m.end) {
                    (true, _) => m.start,
                    (false, false) => end,
                    (false, true) => Pos::new(end.line, end.col + 1),
                };
                self.editor_replace_next(run, next);
            }
            Some(1) => {
                e.replace_rest(&run.finder, m, &run.with, run.backward);
                e.last_found = None;
            }
            Some(2) => {
                let next = if run.backward {
                    m.start
                } else {
                    Pos::new(m.start.line, m.start.col + 1)
                };
                self.editor_replace_next(run, next);
            }
            _ => e.cursor = if at_end { m.end } else { m.start },
        }
    }

    // ----------------------------------------------------------- find all

    /// "All" in the search dialog: every match in a menu at the bottom
    /// (Far's `find_all_list`): line │ position │ the line's text.
    fn editor_find_all(&mut self, i: usize) {
        let Some(finder) = self.editor_finder() else {
            return;
        };
        let e = &self.editors[i];
        let found = e.find_all(&finder);
        if found.is_empty() {
            self.editor_not_found();
            return;
        }
        let mut lines = found.iter().map(|m| m.start.line).collect::<Vec<_>>();
        lines.dedup();
        let lw = (found.iter().map(|m| m.start.line).max().unwrap_or(0) + 1)
            .to_string()
            .len();
        let pw = (found.iter().map(|m| m.start.col).max().unwrap_or(0) + 1)
            .to_string()
            .len();
        let items: Vec<Item> = found
            .iter()
            .map(|m| {
                let text = e.lines()[m.start.line].text.replace('\t', " ");
                Item::new(
                    format!(
                        "{:>lw$}│{:>pw$}│{}",
                        m.start.line + 1,
                        m.start.col + 1,
                        text
                    )
                    .replace('&', "&&"),
                )
            })
            .collect();
        let title = tr!(
            "MEditSearchStatistics",
            p0 = found.len().to_string(),
            p1 = lines.len().to_string()
        );
        let mut menu = Menu::new(title, items).max_items(10);
        // Far: the frame 20 rows above the screen's bottom (`ScrY - 20`),
        // here the editor window's bottom (the key bar's row).
        let (top, bottom) = (e.area.y.saturating_sub(1), e.area.bottom());
        menu.set_row(bottom.saturating_sub(20).max(top));
        let id = e.id;
        self.overlays.push(Overlay::Menu {
            menu,
            purpose: MenuPurpose::EditorFound { id, found },
        });
    }

    /// An entry of "find all" chosen: to that match.
    pub(super) fn editor_found_chosen(&mut self, id: u32, m: Found) {
        let Some(i) = self.editor_index(id) else {
            return;
        };
        let rows = self.editor_screen_rows(i);
        let e = &mut self.editors[i];
        let (at_end, select) = (
            e.settings.search_cursor_at_end,
            e.settings.search_select_found,
        );
        e.last_found = Some(m);
        e.show_found(m, at_end, select, rows);
    }

    // --------------------------------------------------------------- goto

    /// Alt+F8: Far's go-to dialog (`GoToRowCol`): line, then column.
    pub(super) fn editor_goto_dialog(&mut self, i: usize) {
        let dialog = Dialog::new(tr!("MGoTo"), 29)
            .row(vec![input_at(5, 28, "", Some("LineNumber")).use_last()])
            .separator()
            .row(vec![check_at(5, tr!("MGoToHex"), self.editors[i].goto_hex)])
            .separator()
            .buttons(&[&tr!("MOk"), &tr!("MCancel")], 0);
        let id = self.editors[i].id;
        self.overlays.push(Overlay::Dialog {
            dialog,
            purpose: Purpose::Editor(Ask::Goto { id }),
        });
    }

    pub(super) fn editor_goto_closed(&mut self, id: u32, dialog: &crate::dialog::Dialog) {
        let Some((row, col)) = crate::viewer::parse_goto(&dialog.input_value(0), dialog.checked(0))
        else {
            return;
        };
        let Some(i) = self.editor_index(id) else {
            return;
        };
        let e = &mut self.editors[i];
        e.goto_hex = dialog.checked(0);
        let count = e.line_count();
        // Far's `GoToPosition`: a number from 1, a percentage and an offset
        // from 0; under the first line (unsigned arithmetic) or past the
        // last is the last line.
        let line = row.map_or(e.cursor.line, |r| {
            let n = goto_index(r, e.cursor.line, count);
            if n < 0 || n >= count as i128 {
                count - 1
            } else {
                n as usize
            }
        });
        let len = e.lines()[line].len();
        let col = col.map(|c| goto_index(c, e.cursor.col, len).max(0) as usize);
        e.go_to(line, col);
    }
}
