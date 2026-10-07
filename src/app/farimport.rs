//! Far's history into afar's (docs/15, improvement 5): offered once on
//! start when Far's history is found, then from the Commands menu.

use std::path::PathBuf;

use super::App;
use super::fileops::{Overlay, Purpose};
use crate::dialog::Dialog;
use crate::history::{FarCounts, far_counts, far_history_path};
use crate::tr;

/// The note in the history database: the import has been offered.
const ASKED: &str = "far_import";

impl App {
    /// On start: Far's history is there and the import has not been
    /// offered yet — offer it (once, whatever the answer).
    pub(super) fn offer_far_import(&mut self) {
        if self.store.meta(ASKED).is_some() {
            return;
        }
        let Some(path) = far_history_path() else {
            return;
        };
        let Ok(counts) = far_counts(&path) else {
            return;
        };
        self.store.set_meta(ASKED, "asked");
        if counts.total() > 0 {
            self.far_import_dialog(path, counts);
        }
    }

    /// F9 → Commands → "Import Far's history".
    pub(super) fn far_import_menu(&mut self) {
        let Some(path) = far_history_path() else {
            self.say(tr!("far-import-not-found"));
            return;
        };
        match far_counts(&path) {
            Ok(counts) if counts.total() > 0 => self.far_import_dialog(path, counts),
            Ok(_) => self.say(tr!("far-import-empty")),
            Err(e) => self.say(tr!("far-import-failed", error = e.to_string())),
        }
    }

    fn far_import_dialog(&mut self, path: PathBuf, c: FarCounts) {
        let lines = [
            tr!("far-import-found", path = path.display().to_string()),
            counts_line(&c),
            tr!("far-import-question"),
        ];
        let dialog = Dialog::message(
            &tr!("far-import-title"),
            &lines,
            &[&tr!("far-import-yes"), &tr!("far-import-no")],
            false,
        );
        self.overlays.push(Overlay::Dialog {
            dialog,
            purpose: Purpose::FarImport { path },
        });
    }

    /// Takes the records; the command line's Ctrl+E / Ctrl+X list is
    /// read again.
    pub(super) fn far_import(&mut self, path: PathBuf) {
        let redact = self.config.history.redact_secrets;
        match self.store.import_far(&path, redact) {
            Ok(c) => {
                self.store.set_meta(ASKED, "done");
                self.cmd_history = super::cmdline::History::load(&self.store, &PathBuf::new());
                if c.total() == 0 {
                    self.say(tr!("far-import-nothing-new"));
                } else {
                    self.say(tr!("far-import-done", counts = counts_line(&c)));
                }
            }
            Err(e) => self.say(tr!("far-import-failed", error = e.to_string())),
        }
    }
}

fn counts_line(c: &FarCounts) -> String {
    tr!(
        "far-import-counts",
        commands = c.commands,
        folders = c.folders,
        views = c.views,
        dialogs = c.dialogs
    )
}
