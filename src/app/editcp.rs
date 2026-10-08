//! F8 / Shift+F8 in the editor (Far's `SetCodePageEx`, docs/17 §2): the
//! file's bytes read in another code page — the text is not converted
//! ("Save as" converts). With UTF-16 on either side the file is read
//! again from the disk (unsaved changes are lost, asked first).

use super::App;
use super::editors::Ask;
use super::fileops::{Overlay, Purpose};
use super::panelcmds::MenuPurpose;
use super::viewers::CpChoice;
use crate::dialog::Dialog;
use crate::editor::{CpProblem, Pos, text};
use crate::menu::Menu;
use crate::tr;
use crate::viewer::codepage::{self, Codec};

fn is_utf16(cp: u32) -> bool {
    matches!(cp, codepage::UTF16LE | codepage::UTF16BE)
}

impl App {
    /// F8: ANSI ↔ OEM (Far's default `Editor.F8CPs`).
    pub(super) fn editor_next_codepage(&mut self, i: usize) {
        let cp = next_f8(self.editors[i].cp);
        self.editor_set_codepage(i, cp);
    }

    /// Shift+F8: the code page menu (as the viewer's).
    pub(super) fn editor_codepage_menu(&mut self, i: usize) {
        let (items, choices, selected) = super::viewers::codepage_menu_parts(self.editors[i].cp);
        let menu = Menu::new(tr!("MGetCodePageTitle"), items).select(selected);
        let id = self.editors[i].id;
        self.overlays.push(Overlay::Menu {
            menu,
            purpose: MenuPurpose::EditorCodepage { id, choices },
        });
    }

    pub(super) fn editor_codepage_chosen(&mut self, id: u32, choice: CpChoice) {
        let Some(i) = self.editor_index(id) else {
            return;
        };
        let cp = match choice {
            CpChoice::Page(cp) => cp,
            // Far's CP_REDETECT: detected again from the file.
            CpChoice::Detect => {
                let fallback = match self.config.editor.default_codepage {
                    0 => codepage::ansi(),
                    cp => cp,
                };
                match std::fs::read(self.editors[i].path()) {
                    Ok(data) => text::load(&data, None, true, fallback).cp,
                    Err(_) => fallback,
                }
            }
        };
        self.editor_set_codepage(i, cp);
    }

    fn editor_set_codepage(&mut self, i: usize, cp: u32) {
        let e = &self.editors[i];
        if cp == e.cp {
            return;
        }
        if Codec::new(cp).is_none() {
            self.message(
                &tr!("MEditTitle"),
                &[tr!("MEditorCPNotSupported", p0 = cp.to_string())],
                true,
            );
            return;
        }
        let id = e.id;
        if !e.new_file && (is_utf16(e.cp) || is_utf16(cp)) {
            if e.modified() {
                let dialog = Dialog::message(
                    &tr!("MEditTitle"),
                    &[
                        tr!("MEditorReloadCPWarnLost1"),
                        tr!("MEditorReloadCPWarnLost2"),
                    ],
                    &[&tr!("MOk"), &tr!("MCancel")],
                    true,
                );
                self.overlays.push(Overlay::Dialog {
                    dialog,
                    purpose: Purpose::Editor(Ask::ReloadCp { id, cp }),
                });
            } else {
                self.editor_reload_cp(i, cp);
            }
            return;
        }
        let Some(problem) = e.try_codepage(cp) else {
            self.editors[i].reinterpret(cp);
            return;
        };
        // Far's warning: which page, what it cannot take; Show / OK /
        // Cancel.
        let (bad_cp, what, data, line, col) = match &problem {
            CpProblem::Char { line, col, ch } => (
                e.cp,
                "MUnsupportedCodePageCharacter",
                format!("'{ch}': U+{:04X}", u32::from(*ch)),
                *line,
                *col,
            ),
            CpProblem::Bytes { line, col, bytes } => (
                cp,
                "MUnsupportedCodePageByteSequence",
                format!(
                    "[{}]",
                    bytes
                        .iter()
                        .map(|b| format!("{b:02X}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
                *line,
                *col,
            ),
        };
        let lines = vec![
            tr!("MUnsupportedCodePageSelectedCodepage"),
            codepage::long_name(bad_cp),
            tr!("MUnsupportedCodePageDoesNotSupport", p0 = tr!(what)),
            data,
            tr!("MEditorSwitchCPConfirm"),
        ];
        let dialog = Dialog::message(
            &tr!("MWarning"),
            &lines,
            &[&tr!("MEditorSaveCPWarnShow"), &tr!("MOk"), &tr!("MCancel")],
            true,
        );
        self.overlays.push(Overlay::Dialog {
            dialog,
            purpose: Purpose::Editor(Ask::SwitchCp {
                id,
                cp,
                at: Pos::new(line, col),
            }),
        });
    }

    /// The answer to "the page does not support …": Show, OK, Cancel.
    pub(super) fn editor_switch_cp_answer(
        &mut self,
        id: u32,
        cp: u32,
        at: Pos,
        button: Option<usize>,
    ) {
        let Some(i) = self.editor_index(id) else {
            return;
        };
        match button {
            Some(0) => self.editors[i].go_to(at.line, Some(at.col)),
            Some(1) => self.editors[i].reinterpret(cp),
            _ => {}
        }
    }
}

/// The page F8 goes to from `cp`: ANSI from anything but ANSI, which goes
/// to OEM.
pub(super) fn next_f8(cp: u32) -> u32 {
    if cp == codepage::ansi() {
        codepage::oem()
    } else {
        codepage::ansi()
    }
}
