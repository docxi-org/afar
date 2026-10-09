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
/// in Far but cannot be changed yet. The last one is afar's own.
const CONFIRMATIONS: [(&str, Option<usize>); 15] = [
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
    ("MSetConfirmAllowReedit", Some(4)),
    ("MSetConfirmHistoryClear", None),
    ("MSetConfirmExit", None),
    ("confirm-agent", Some(3)),
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
        let values = [c.read_only, c.delete_folder, c.esc, c.agent, c.reedit];
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
                Some(3) => self.config.confirm.agent = value,
                Some(4) => self.config.confirm.reedit = value,
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
        let ide = tr!("agent-settings-ide");
        let channels = tr!("agent-settings-channels");
        let confirm_channels = tr!("agent-settings-confirm-channels");
        let note = tr!("agent-settings-note");
        let position_label = tr!("agent-settings-position");
        let positions = [tr!("agent-position-bottom"), tr!("agent-position-top")];
        let position_w = positions.iter().map(|p| chars(p)).max().unwrap_or(10) + 3;
        let content = (label_w + 1 + COMBO)
            .max(chars(&live) + 4)
            .max(chars(&ide) + 4)
            .max(chars(&channels) + 4)
            .max(chars(&confirm_channels) + 8)
            .max(chars(&note))
            .max(56);
        let field_x = 5 + chars(&command_label).max(chars(&args_label)) + 1;
        let field_w = content + 5 - field_x;
        let mut d = Dialog::new(tr!("agent-settings-title"), content)
            .row(vec![
                text_at(5, command_label),
                input_at(field_x, field_w, a.command.clone(), None),
            ])
            .row(vec![
                text_at(5, args_label),
                input_at(field_x, field_w, a.args.join(" "), None),
            ])
            .row(vec![check_at(5, live, a.live)])
            .row(vec![check_at(5, ide, a.ide)])
            .row(vec![check_at(5, channels, a.channels)])
            .row(vec![check_at(9, confirm_channels, a.confirm_channels)])
            .check_depends(3, 2)
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
            self.config.agent.ide,
            self.config.agent.channels,
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
        a.ide = dialog.checked(1);
        a.channels = dialog.checked(2);
        a.confirm_channels = dialog.checked(3);
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
        let changed_command = old_command != (a.command.clone(), a.args.clone(), a.ide, a.channels);
        let ide = a.ide;
        self.wm.set_agent_on_top(top);
        self.set_ide(ide);
        self.save_config();
        if changed_command && self.agent_alive() {
            self.say(tr!("agent-settings-restart"));
        }
    }

    /// F9 → Options → Viewer settings (Far's `ViewerConfig`): the external
    /// viewer, then two columns of the built-in one's options and the
    /// default code page.
    pub(super) fn viewer_settings_dialog(&mut self) {
        let v = &self.config.viewer;
        let t = |id: &str| tr!(id);
        let left = [
            t("MViewConfigPersistentSelection"),
            t("MViewConfigSavePos"),
            t("MViewConfigSaveCodepage"),
            t("MViewConfigSaveShortPos"),
        ];
        let tab_label = t("MViewConfigTabSize");
        let max_label = t("MViewConfigMaxLineSize");
        // A check box is its text and 4 cells; a number field, its width
        // and the text after it.
        let left_w = left
            .iter()
            .map(|l| chars(l) + 4)
            .chain([4 + chars(&tab_label), 7 + chars(&max_label)])
            .max()
            .unwrap_or(20);
        let right_x = 5 + left_w + 2;
        let right = [
            t("MViewConfigArrows"),
            t("MViewConfigVisible0x00"),
            t("MViewConfigScrollbar"),
            t("MViewConfigSaveViewMode"),
            t("MViewConfigSaveWrapMode"),
            t("MViewConfigDetectDumpMode"),
            t("MViewAutoDetectCodePage"),
        ];
        let right_w = right.iter().map(|l| chars(l) + 4).max().unwrap_or(20);
        let external = t("MViewConfigExternalF3");
        let content = (left_w + 2 + right_w).max(chars(&external) + 4).max(65);
        let (pages, labels) = default_codepages();
        let selected = pages
            .iter()
            .position(|cp| *cp == v.default_codepage)
            .unwrap_or(0);
        let dialog = Dialog::new(t("MViewConfigTitle"), content)
            .row(vec![check_at(5, external, v.external_f3)])
            .row(vec![text_at(5, t("MViewConfigExternalCommand"))])
            .row(vec![
                input_at(5, 64, v.external_command.clone(), Some("ExternalViewer")).exec(),
            ])
            .caption(t("MViewConfigInternal"))
            .row(vec![
                check_at(5, left[0].clone(), v.persistent_selection),
                check_at(right_x, right[0].clone(), v.show_arrows),
            ])
            .row(vec![
                input_at(5, 3, v.tab_size.to_string(), None),
                text_at(9, tab_label),
                check_at(right_x, right[1].clone(), v.show_zero),
            ])
            .row(vec![check_at(right_x, right[2].clone(), v.scrollbar)])
            .separator()
            .row(vec![
                check_at(5, left[1].clone(), v.save_position),
                check_at(right_x, right[3].clone(), v.save_mode),
            ])
            .row(vec![
                check_at(5, left[2].clone(), v.save_codepage || v.save_position),
                check_at(right_x, right[4].clone(), v.save_wrap),
            ])
            .row(vec![
                check_at(5, left[3].clone(), v.save_bookmarks),
                check_at(right_x, right[5].clone(), v.detect_dump),
            ])
            .row(vec![
                input_at(5, 6, v.max_line.to_string(), None),
                text_at(12, max_label),
                check_at(right_x, right[6].clone(), v.autodetect_codepage),
            ])
            .row(vec![text_at(5, t("MViewConfigDefaultCodePage"))])
            .row(vec![combo_at(
                5,
                64,
                labels.into_iter().map(Some).collect(),
                selected,
            )])
            .separator()
            .buttons(&[&tr!("MOk"), &tr!("MCancel")], 0);
        self.overlays.push(Overlay::Dialog {
            dialog,
            purpose: Purpose::ViewerSettings,
        });
    }

    pub(super) fn viewer_settings_from_dialog(&mut self, dialog: &Dialog) {
        let v = &mut self.config.viewer;
        // Check boxes in reading order.
        let c = |n: usize| dialog.checked(n);
        v.external_f3 = c(0);
        v.persistent_selection = c(1);
        v.show_arrows = c(2);
        v.show_zero = c(3);
        v.scrollbar = c(4);
        v.save_position = c(5);
        v.save_mode = c(6);
        // As in Far: saving the position saves the code page.
        v.save_codepage = c(7) || v.save_position;
        v.save_wrap = c(8);
        v.save_bookmarks = c(9);
        v.detect_dump = c(10);
        v.autodetect_codepage = c(11);
        v.external_command = dialog.input_value(0).trim().to_string();
        if let Ok(n) = dialog.input_value(1).trim().parse::<usize>() {
            v.tab_size = n.clamp(1, 512);
        }
        if let Ok(n) = dialog.input_value(2).trim().parse::<usize>() {
            v.max_line = n.clamp(100, 100_000);
        }
        let (pages, _) = default_codepages();
        v.default_codepage = pages.get(dialog.combo(0)).copied().unwrap_or(0);
        self.viewer_defaults.scrollbar = v.scrollbar;
        self.save_config();
    }

    /// F9 → Options → Editor settings (`id` `None`), or Alt+Shift+F9 in an
    /// editor — only that window's part, applied to it at once (Far's
    /// `EditorConfig`, docs/17 §9).
    pub(super) fn editor_settings_dialog(&mut self, id: Option<u32>) {
        use crate::config::{ExpandTabs, ShowWhitespace};
        let t = |id: &str| tr!(id);
        // The values: the window's own, or the configured ones.
        let c = self.config.editor.clone();
        let local = id.and_then(|id| self.editor_index(id));
        let (
            expand,
            tab_size,
            persistent,
            del_removes,
            auto_indent,
            ws,
            beyond,
            sel_found,
            at_end,
            bar,
            numbers,
        ) = match local {
            Some(i) => {
                let e = &self.editors[i];
                let s = &e.settings;
                (
                    usize::from(s.expand_tabs.min(2)),
                    s.tab_size,
                    s.persistent_blocks,
                    s.del_removes_blocks,
                    s.auto_indent,
                    s.show_whitespace,
                    s.cursor_beyond_eol,
                    s.search_select_found,
                    s.search_cursor_at_end,
                    s.scrollbar,
                    e.line_numbers,
                )
            }
            None => (
                match c.expand_tabs {
                    ExpandTabs::Keep => 0,
                    ExpandTabs::New => 1,
                    ExpandTabs::All => 2,
                },
                c.tab_size,
                c.persistent_blocks,
                c.del_removes_blocks,
                c.auto_indent,
                match c.show_whitespace {
                    ShowWhitespace::Off => 0,
                    ShowWhitespace::All => 1,
                    ShowWhitespace::NoEol => 2,
                },
                c.cursor_beyond_eol,
                c.search_select_found,
                c.search_cursor_at_end,
                c.scrollbar,
                c.line_numbers,
            ),
        };
        let left = [
            t("MEditConfigPersistentBlocks"),
            t("MEditConfigDelRemovesBlocks"),
            t("MEditConfigAutoIndent"),
            t("MEditShowWhiteSpace"),
        ];
        let tab_label = t("MEditConfigTabSize");
        let left_w = left
            .iter()
            .map(|l| chars(l) + 4)
            .chain([6 + chars(&tab_label)])
            .max()
            .unwrap_or(20);
        let right_x = 5 + left_w + 2;
        let right = [
            t("MEditCursorBeyondEnd"),
            t("MEditConfigSelFound"),
            t("MEditConfigCursorAtEnd"),
            t("MEditConfigScrollbar"),
            t("MEditConfigLineNumbers"),
        ];
        let right_w = right.iter().map(|l| chars(l) + 4).max().unwrap_or(20);
        let expand_label = t("MEditConfigExpandTabsTitle");
        let expand_items: Vec<Option<String>> = [
            "MEditConfigDoNotExpandTabs",
            "MEditConfigExpandTabs",
            "MEditConfigConvertAllTabsToSpaces",
        ]
        .iter()
        .map(|id| Some(t(id)))
        .collect();
        let expand_w = expand_items
            .iter()
            .flatten()
            .map(|s| chars(s))
            .max()
            .unwrap_or(30)
            + 3;
        let content = (left_w + 2 + right_w)
            .max(chars(&expand_label) + 1 + expand_w + 1)
            .max(if local.is_none() { 65 } else { 0 });
        let mut d = Dialog::new(t("MEditConfigTitle"), content);
        if local.is_none() {
            d = d
                .row(vec![check_at(5, t("MEditConfigEditorF4"), c.external_f4)])
                .row(vec![text_at(5, t("MEditConfigEditorCommand"))])
                .row(vec![
                    input_at(5, 64, c.external_command.clone(), Some("ExternalEditor")).exec(),
                ])
                .caption(t("MEditConfigInternal"));
        }
        let ws_check = check_at(5, left[3].clone(), ws == 1).three_state(ws == 2);
        d = d
            .row(vec![
                text_at(5, expand_label.clone()),
                combo_at(5 + chars(&expand_label) + 1, expand_w, expand_items, expand),
            ])
            .row(vec![
                check_at(5, left[0].clone(), persistent),
                check_at(right_x, right[0].clone(), beyond),
            ])
            .row(vec![
                check_at(5, left[1].clone(), del_removes),
                check_at(right_x, right[1].clone(), sel_found),
            ])
            .row(vec![
                check_at(5, left[2].clone(), auto_indent),
                check_at(right_x, right[2].clone(), at_end),
            ])
            .row(vec![
                input_at(5, 5, tab_size.to_string(), None),
                text_at(11, tab_label),
                check_at(right_x, right[3].clone(), bar),
            ])
            .row(vec![ws_check, check_at(right_x, right[4].clone(), numbers)]);
        if local.is_none() {
            let (pages, labels) = default_codepages();
            let selected = pages
                .iter()
                .position(|cp| *cp == c.default_codepage)
                .unwrap_or(0);
            d = d
                .separator()
                .row(vec![check_at(5, t("MEditConfigSavePos"), c.save_position)])
                .row(vec![check_at(
                    5,
                    t("MEditConfigSaveShortPos"),
                    c.save_bookmarks,
                )])
                .row(vec![check_at(
                    5,
                    t("MEditAutoDetectCodePage"),
                    c.autodetect_codepage,
                )])
                .row(vec![text_at(5, t("MEditConfigDefaultCodePage"))])
                .row(vec![combo_at(
                    5,
                    64,
                    labels.into_iter().map(Some).collect(),
                    selected,
                )]);
        }
        let dialog = d.separator().buttons(&[&tr!("MOk"), &tr!("MCancel")], 0);
        self.overlays.push(Overlay::Dialog {
            dialog,
            purpose: Purpose::EditorSettings { id },
        });
    }

    pub(super) fn editor_settings_from_dialog(&mut self, id: Option<u32>, dialog: &Dialog) {
        use crate::config::{ExpandTabs, ShowWhitespace};
        // Check boxes and fields in reading order; the global dialog has
        // the external editor first.
        let g = usize::from(id.is_none());
        let c = |n: usize| dialog.checked(n + g);
        let expand = dialog.combo(0).min(2) as u8;
        let tab_size = dialog
            .input_value(g)
            .trim()
            .parse::<usize>()
            .map(|n| n.clamp(1, 512));
        let ws = match dialog.check_state(7 + g) {
            None => 2,
            Some(true) => 1,
            Some(false) => 0,
        };
        let (persistent, beyond, del_removes, sel_found, auto_indent, at_end, bar, numbers) =
            (c(0), c(1), c(2), c(3), c(4), c(5), c(6), c(8));
        match id {
            Some(id) => {
                let Some(i) = self.editor_index(id) else {
                    return;
                };
                let e = &mut self.editors[i];
                let was_all = e.settings.expand_tabs == 2;
                let s = &mut e.settings;
                s.expand_tabs = expand;
                if let Ok(n) = tab_size {
                    s.tab_size = n;
                }
                s.persistent_blocks = persistent;
                s.cursor_beyond_eol = beyond;
                s.del_removes_blocks = del_removes;
                s.search_select_found = sel_found;
                s.auto_indent = auto_indent;
                s.search_cursor_at_end = at_end;
                s.scrollbar = bar;
                s.show_whitespace = ws;
                e.line_numbers = numbers;
                // Far: "all tabs as spaces" rewrites the text at once.
                if expand == 2 && !was_all {
                    e.expand_all_tabs(true);
                }
                e.scroll_to_cursor();
            }
            None => {
                let ed = &mut self.config.editor;
                ed.external_f4 = dialog.checked(0);
                ed.external_command = dialog.input_value(0).trim().to_string();
                ed.expand_tabs = match expand {
                    0 => ExpandTabs::Keep,
                    1 => ExpandTabs::New,
                    _ => ExpandTabs::All,
                };
                if let Ok(n) = tab_size {
                    ed.tab_size = n;
                }
                ed.persistent_blocks = persistent;
                ed.cursor_beyond_eol = beyond;
                ed.del_removes_blocks = del_removes;
                ed.search_select_found = sel_found;
                ed.auto_indent = auto_indent;
                ed.search_cursor_at_end = at_end;
                ed.scrollbar = bar;
                ed.show_whitespace = match ws {
                    0 => ShowWhitespace::Off,
                    1 => ShowWhitespace::All,
                    _ => ShowWhitespace::NoEol,
                };
                ed.line_numbers = numbers;
                ed.save_position = dialog.checked(10);
                ed.save_bookmarks = dialog.checked(11);
                ed.autodetect_codepage = dialog.checked(12);
                let (pages, _) = default_codepages();
                ed.default_codepage = pages.get(dialog.combo(1)).copied().unwrap_or(0);
                self.save_config();
            }
        }
    }

    /// F9 → Options → AutoComplete settings: Far's three check boxes,
    /// where completion works and its sources (always / Ctrl+Space / never).
    pub(super) fn autocomplete_settings_dialog(&mut self) {
        use crate::config::Use;
        let a = &self.config.autocomplete;
        let checks = [
            tr!("ac-fuzzy"),
            tr!("MConfigAutoCompleteModalList"),
            tr!("MConfigAutoCompleteAutoAppend"),
            tr!("MConfigDialogsAutoComplete"),
            tr!("ac-command-line"),
        ];
        let sources = [
            tr!("ac-source-history"),
            tr!("ac-source-files"),
            tr!("ac-source-variables"),
            tr!("ac-source-programs"),
        ];
        let suggest_label = tr!("ac-suggest");
        let fuzzy_label = checks[0].clone();
        let label_w = sources
            .iter()
            .chain([&suggest_label])
            .map(|l| chars(l))
            .max()
            .unwrap_or(10);
        let suggest_w = label_w;
        const COMBO: u16 = 26;
        let content = checks
            .iter()
            .map(|l| chars(l) + 8)
            // The combo's arrow after it.
            .chain([label_w + 1 + COMBO + 2])
            .max()
            .unwrap_or(40);
        let index = |u: Use| match u {
            Use::Always => 0,
            Use::CtrlSpace => 1,
            Use::Never => 2,
        };
        let uses = [a.history, a.files, a.variables, a.programs];
        let mut d = Dialog::new(tr!("MConfigAutoCompleteTitle"), content)
            .row(vec![
                text_at(5, suggest_label.clone()),
                combo_at(
                    5 + suggest_w + 1,
                    COMBO,
                    vec![
                        Some(tr!("ac-suggest-ghost")),
                        Some(tr!("ac-suggest-list")),
                        Some(tr!("ac-suggest-off")),
                    ],
                    a.suggest as usize,
                ),
            ])
            .row(vec![check_at(5, checks[1].clone(), a.modal)])
            .row(vec![check_at(5, fuzzy_label.clone(), a.fuzzy)])
            // Appending the first match is not done (the ghost shows it).
            .row(vec![check_at(5, checks[2].clone(), false).disabled()])
            .separator()
            .row(vec![check_at(5, checks[3].clone(), a.dialogs)])
            .row(vec![check_at(5, checks[4].clone(), a.command_line)])
            .caption(tr!("ac-sources"));
        for (label, u) in sources.iter().zip(uses) {
            d = d.row(vec![
                text_at(5, label.clone()),
                combo_at(
                    5 + label_w + 1,
                    COMBO,
                    vec![
                        Some(tr!("ac-use-always")),
                        Some(tr!("ac-use-ctrl-space")),
                        Some(tr!("ac-use-never")),
                    ],
                    index(u),
                ),
            ]);
        }
        let dialog = d.separator().buttons(&[&tr!("MOk"), &tr!("MCancel")], 0);
        self.overlays.push(Overlay::Dialog {
            dialog,
            purpose: Purpose::AutocompleteSettings,
        });
    }

    pub(super) fn autocomplete_settings_from_dialog(&mut self, dialog: &Dialog) {
        use crate::config::Use;
        let a = &mut self.config.autocomplete;
        a.suggest = match dialog.combo(0) {
            0 => crate::config::Suggest::Ghost,
            1 => crate::config::Suggest::List,
            _ => crate::config::Suggest::Off,
        };
        a.modal = dialog.checked(0);
        a.fuzzy = dialog.checked(1);
        a.dialogs = dialog.checked(3);
        a.command_line = dialog.checked(4);
        let use_of = |i: usize| match dialog.combo(i + 1) {
            0 => Use::Always,
            1 => Use::CtrlSpace,
            _ => Use::Never,
        };
        a.history = use_of(0);
        a.files = use_of(1);
        a.variables = use_of(2);
        a.programs = use_of(3);
        self.save_config();
    }

    /// F9 → Options → Hints: on, the delay, the kinds.
    pub(super) fn hint_settings_dialog(&mut self) {
        let h = &self.config.hints;
        let checks = [
            (tr!("hints-on"), h.enabled),
            (tr!("hints-files"), h.files),
            (tr!("hints-keybar"), h.keybar),
            (tr!("hints-agent"), h.agent),
            (tr!("hints-panels"), h.panels),
            (tr!("hints-status"), h.status),
            (tr!("hints-marks"), h.marks),
            (tr!("hints-menus"), h.menus),
        ];
        let delay = tr!("hints-delay");
        let content = checks
            .iter()
            .map(|(l, _)| chars(l) + 8)
            .chain([chars(&delay) + 12])
            .max()
            .unwrap_or(40);
        let mut d = Dialog::new(tr!("hints-title"), content)
            .row(vec![check_at(5, checks[0].0.clone(), checks[0].1)])
            .row(vec![
                text_at(5, delay.clone()),
                input_at(5 + chars(&delay) + 1, 6, h.delay_ms.to_string(), None),
            ])
            .caption(tr!("hints-kinds"));
        for (label, on) in &checks[1..] {
            d = d.row(vec![check_at(5, label.clone(), *on)]);
        }
        let dialog = d.separator().buttons(&[&tr!("MOk"), &tr!("MCancel")], 0);
        self.overlays.push(Overlay::Dialog {
            dialog,
            purpose: Purpose::HintSettings,
        });
    }

    pub(super) fn hint_settings_from_dialog(&mut self, dialog: &Dialog) {
        let h = &mut self.config.hints;
        h.enabled = dialog.checked(0);
        if let Ok(ms) = dialog.input_value(0).trim().parse::<u64>() {
            h.delay_ms = ms.clamp(100, 5000);
        }
        h.files = dialog.checked(1);
        h.keybar = dialog.checked(2);
        h.agent = dialog.checked(3);
        h.panels = dialog.checked(4);
        h.status = dialog.checked(5);
        h.marks = dialog.checked(6);
        h.menus = dialog.checked(7);
        self.hover = None;
        self.save_config();
    }

    fn save_config(&mut self) {
        let path = crate::config::config_path();
        match self.config.save(&path) {
            Ok(()) => self.say(tr!("settings-saved", path = path.display().to_string())),
            Err(e) => self.say(tr!("settings-save-failed", error = e)),
        }
    }
}

/// The default code pages to choose from: ANSI (0), OEM, Unicode, the rest
/// of the installed ones; and their names.
fn default_codepages() -> (Vec<u32>, Vec<String>) {
    use crate::viewer::codepage;
    let (ansi, oem) = (codepage::ansi(), codepage::oem());
    let mut pages = vec![0, oem, codepage::UTF8];
    pages.extend(
        codepage::installed()
            .into_iter()
            .filter(|cp| ![ansi, oem, codepage::UTF8].contains(cp)),
    );
    let labels = pages
        .iter()
        .map(|cp| match *cp {
            0 => format!("ANSI — {}", codepage::long_name(ansi)),
            cp if cp == oem => format!("OEM — {}", codepage::long_name(oem)),
            cp => codepage::long_name(cp),
        })
        .collect();
    (pages, labels)
}

/// The setting (`section`, `key` of `config.toml`) a control of a settings
/// dialog changes, in the order the `*_from_dialog` functions read them.
pub(super) fn setting_of(
    purpose: &Purpose,
    control: crate::dialog::Control,
    n: usize,
) -> Option<(&'static str, &'static str)> {
    use crate::dialog::Control::{Check, Combo, Input};
    let pick = |keys: &[&'static str]| keys.get(n).copied();
    let found = match (purpose, control) {
        (Purpose::ViewerSettings, Check) => (
            "viewer",
            pick(&[
                "external_f3",
                "persistent_selection",
                "show_arrows",
                "show_zero",
                "scrollbar",
                "save_position",
                "save_mode",
                "save_codepage",
                "save_wrap",
                "save_bookmarks",
                "detect_dump",
                "autodetect_codepage",
            ])?,
        ),
        (Purpose::ViewerSettings, Input) => (
            "viewer",
            pick(&["external_command", "tab_size", "max_line"])?,
        ),
        (Purpose::ViewerSettings, Combo) => ("viewer", pick(&["default_codepage"])?),
        (Purpose::EditorSettings { id }, Check) => {
            let keys = [
                "persistent_blocks",
                "cursor_beyond_eol",
                "del_removes_blocks",
                "search_select_found",
                "auto_indent",
                "search_cursor_at_end",
                "scrollbar",
                "show_whitespace",
                "line_numbers",
                "save_position",
                "save_bookmarks",
                "autodetect_codepage",
            ];
            // The global dialog has the external editor first.
            let key = match (id, n) {
                (None, 0) => "external_f4",
                (None, n) => *keys.get(n - 1)?,
                (Some(_), n) => *keys.get(n)?,
            };
            ("editor", key)
        }
        (Purpose::EditorSettings { id }, Input) => {
            let keys: &[&str] = if id.is_none() {
                &["external_command", "tab_size"]
            } else {
                &["tab_size"]
            };
            ("editor", *keys.get(n)?)
        }
        (Purpose::EditorSettings { .. }, Combo) => {
            ("editor", pick(&["expand_tabs", "default_codepage"])?)
        }
        (Purpose::AutocompleteSettings, Check) => (
            "autocomplete",
            pick(&["modal", "fuzzy", "", "dialogs", "command_line"]).filter(|k| !k.is_empty())?,
        ),
        (Purpose::AutocompleteSettings, Combo) => (
            "autocomplete",
            pick(&["suggest", "history", "files", "variables", "programs"])?,
        ),
        (Purpose::Confirmations, Check) => (
            "confirm",
            match CONFIRMATIONS.get(n)?.1? {
                0 => "read_only",
                1 => "delete_folder",
                2 => "esc",
                3 => "agent",
                _ => "reedit",
            },
        ),
        (Purpose::AgentSettings, Input) => ("agent", pick(&["command", "args"])?),
        (Purpose::AgentSettings, Check) => (
            "agent",
            pick(&["live", "ide", "channels", "confirm_channels"])?,
        ),
        (Purpose::AgentSettings, Combo) if n == 0 => ("agent", "position"),
        (Purpose::AgentSettings, Combo) => (
            "agent.permissions",
            *[
                "navigate",
                "mkdir",
                "copy",
                "move",
                "delete",
                "delete_permanent",
                "run_command",
            ]
            .get(n - 1)?,
        ),
        (Purpose::HintSettings, Check) => (
            "hints",
            pick(&[
                "enabled", "files", "keybar", "agent", "panels", "status", "marks", "menus",
            ])?,
        ),
        (Purpose::HintSettings, Input) => ("hints", pick(&["delay_ms"])?),
        _ => return None,
    };
    Some(found)
}
