//! Settings dialogs of F9 → Options, laid out as Far's DialogBuilder does
//! (items from column 5, a label before its field on the same row, the
//! width from the longest row): Confirmations (Far's own) and the agent's
//! settings. OK writes `config.toml`.

use super::App;
use super::fileops::{Overlay, Purpose};
use crate::config::{AgentPosition, Level};
use crate::dialog::{Dialog, check_at, combo_at, input_at, text_at};
use crate::tr;

/// Far's confirmations; afar has three of them, the others are shown as
/// in Far but cannot be changed yet.
const CONFIRMATIONS: [(&str, Option<usize>); 14] = [
    ("MSetConfirmCopy", None),
    ("MSetConfirmMove", None),
    ("MSetConfirmRO", Some(0)),
    ("MSetConfirmDrag", None),
    ("MSetConfirmDelete", None),
    ("MSetConfirmDeleteFolders", Some(1)),
    ("MSetConfirmEsc", Some(2)),
    ("MSetConfirmRemoveConnection", None),
    ("MSetConfirmRemoveSUBST", None),
    ("MSetConfirmDetachVHD", None),
    ("MSetConfirmRemoveHotPlug", None),
    ("MSetConfirmAllowReedit", None),
    ("MSetConfirmHistoryClear", None),
    ("MSetConfirmExit", None),
];

/// The agent's permissions in the dialog: label, whether "ask" is offered.
const PERMISSIONS: [(&str, bool); 7] = [
    ("perm-navigate", false),
    ("perm-mkdir", true),
    ("perm-copy", true),
    ("perm-move", true),
    ("perm-delete", true),
    ("perm-delete-permanent", true),
    ("perm-run-command", true),
];

fn chars(s: &str) -> u16 {
    s.chars().filter(|c| *c != '&').count() as u16
}

impl App {
    /// F9 → Options → Confirmations.
    pub(super) fn confirmations_dialog(&mut self) {
        let c = &self.config.confirm;
        let values = [c.read_only, c.delete_folder, c.esc];
        let labels: Vec<String> = CONFIRMATIONS.iter().map(|(id, _)| tr!(id)).collect();
        // A check box is its text and 4 cells.
        let content = labels.iter().map(|l| chars(l) + 4).max().unwrap_or(20);
        let mut d = Dialog::new(tr!("MSetConfirmTitle"), content);
        for ((_, setting), label) in CONFIRMATIONS.iter().zip(&labels) {
            let elem = match setting {
                Some(i) => check_at(5, label.clone(), values[*i]),
                None => check_at(5, label.clone(), true).disabled(),
            };
            d = d.row(vec![elem]);
        }
        let dialog = d.separator().buttons(&[&tr!("MOk"), &tr!("MCancel")], 0);
        self.overlays.push(Overlay::Dialog {
            dialog,
            purpose: Purpose::Confirmations,
        });
    }

    pub(super) fn confirmations_from_dialog(&mut self, dialog: &Dialog) {
        // Check boxes count in order; ours are at the positions of their
        // Far rows.
        for (row, (_, setting)) in CONFIRMATIONS.iter().enumerate() {
            let value = dialog.checked(row);
            match setting {
                Some(0) => self.config.confirm.read_only = value,
                Some(1) => self.config.confirm.delete_folder = value,
                Some(2) => self.config.confirm.esc = value,
                _ => {}
            }
        }
        self.save_config();
    }

    /// F9 → Options → the agent and its permissions.
    pub(super) fn agent_settings_dialog(&mut self) {
        let a = &self.config.agent;
        let p = &a.permissions;
        let levels = [
            p.navigate,
            p.mkdir,
            p.copy,
            p.move_,
            p.delete,
            p.delete_permanent,
            p.run_command,
        ];
        let labels: Vec<String> = PERMISSIONS.iter().map(|(id, _)| tr!(id)).collect();
        let label_w = labels.iter().map(|l| chars(l)).max().unwrap_or(10);
        const COMBO: u16 = 16;
        let command_label = tr!("agent-settings-command");
        let args_label = tr!("agent-settings-args");
        let live = tr!("agent-settings-live");
        let note = tr!("agent-settings-note");
        let position_label = tr!("agent-settings-position");
        let positions = [tr!("agent-position-bottom"), tr!("agent-position-top")];
        let position_w = positions.iter().map(|p| chars(p)).max().unwrap_or(10) + 3;
        let content = (label_w + 1 + COMBO)
            .max(chars(&live) + 4)
            .max(chars(&note))
            .max(56);
        let field_x = 5 + chars(&command_label).max(chars(&args_label)) + 1;
        let field_w = content + 5 - field_x;
        let mut d = Dialog::new(tr!("agent-settings-title"), content)
            .row(vec![
                text_at(5, command_label),
                input_at(field_x, field_w, a.command.clone(), false),
            ])
            .row(vec![
                text_at(5, args_label),
                input_at(field_x, field_w, a.args.join(" "), false),
            ])
            .row(vec![check_at(5, live, a.live)])
            .row(vec![
                text_at(5, position_label.clone()),
                combo_at(
                    5 + chars(&position_label) + 1,
                    position_w,
                    positions.iter().cloned().map(Some).collect(),
                    usize::from(a.position == AgentPosition::Top),
                ),
            ])
            .caption(tr!("agent-settings-permissions"));
        for (((_, ask), label), level) in PERMISSIONS.iter().zip(&labels).zip(levels) {
            let mut items = vec![Some(tr!("perm-allow"))];
            if *ask {
                items.push(Some(tr!("perm-confirm")));
            }
            items.push(Some(tr!("perm-deny")));
            let selected = match (level, ask) {
                (Level::Allow, _) => 0,
                (Level::Confirm, true) => 1,
                (Level::Confirm, false) => 0,
                (Level::Deny, true) => 2,
                (Level::Deny, false) => 1,
            };
            d = d.row(vec![
                text_at(5, label.clone()),
                combo_at(5 + label_w + 1, COMBO, items, selected),
            ]);
        }
        let dialog = d
            .separator()
            .row(vec![text_at(5, note).literal()])
            .separator()
            .buttons(&[&tr!("MOk"), &tr!("MCancel")], 0);
        self.overlays.push(Overlay::Dialog {
            dialog,
            purpose: Purpose::AgentSettings,
        });
    }

    pub(super) fn agent_settings_from_dialog(&mut self, dialog: &Dialog) {
        let old_command = (
            self.config.agent.command.clone(),
            self.config.agent.args.clone(),
        );
        let a = &mut self.config.agent;
        let command = dialog.input_value(0).trim().to_string();
        if !command.is_empty() {
            a.command = command;
        }
        a.args = dialog
            .input_value(1)
            .split_whitespace()
            .map(str::to_string)
            .collect();
        a.live = dialog.checked(0);
        a.position = if dialog.combo(0) == 1 {
            AgentPosition::Top
        } else {
            AgentPosition::Bottom
        };
        let top = a.position == AgentPosition::Top;
        // Combo 0 is the position; the permissions follow.
        let level = |i: usize, ask: bool| match (dialog.combo(i + 1), ask) {
            (0, _) => Level::Allow,
            (1, true) => Level::Confirm,
            _ => Level::Deny,
        };
        let p = &mut a.permissions;
        p.navigate = level(0, false);
        p.mkdir = level(1, true);
        p.copy = level(2, true);
        p.move_ = level(3, true);
        p.delete = level(4, true);
        p.delete_permanent = level(5, true);
        p.run_command = level(6, true);
        let changed_command = old_command != (a.command.clone(), a.args.clone());
        self.wm.set_agent_on_top(top);
        self.save_config();
        if changed_command && self.agent_alive() {
            self.say(tr!("agent-settings-restart"));
        }
    }

    fn save_config(&mut self) {
        let path = crate::config::config_path();
        match self.config.save(&path) {
            Ok(()) => self.say(tr!("settings-saved", path = path.display().to_string())),
            Err(e) => self.say(tr!("settings-save-failed", error = e)),
        }
    }
}
