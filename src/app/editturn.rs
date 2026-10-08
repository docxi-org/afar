//! Passing the turn to the agent from the editor (docs/11 «Передача
//! хода», plan step 1): Ctrl+Enter — the agent decides what to do,
//! Ctrl+Alt+Enter — it only answers. The line under the cursor, when the
//! user typed it here, is the instruction and leaves the text. The event
//! goes through the agent's channel: the instruction, the changes since
//! the version the agent saw, the cursor and the selection — not the whole
//! file. Without the channel: a mention in the agent's input (IDE
//! protocol), Enter is the user's.

use std::time::Instant;

use serde_json::json;

use super::{App, Focus};
use crate::tr;

/// Lines shown around the cursor when the agent has not read the buffer.
const AROUND: usize = 10;
/// The selection's text is cut after this many characters.
const SELECTION_LIMIT: usize = 4000;

impl App {
    pub(super) fn editor_agent_turn(&mut self, i: usize, answer: bool) {
        if !self.agent_alive() {
            self.say(tr!("editor-agent-off"));
            return;
        }
        let taken = self.editors[i].take_instruction();
        let instruction = taken.as_ref().map(|(_, t)| t.clone());
        let e = &self.editors[i];
        let path = e.path().to_path_buf();
        self.journal.push(
            crate::journal::Actor::User,
            crate::journal::Event::EditorTurn {
                path: path.clone(),
                instruction: instruction.clone().unwrap_or_default(),
                answer,
            },
        );
        if !self.config.agent.channels {
            self.editor_mention(i, instruction.as_deref());
            return;
        }
        let content = self.editor_turn_text(i, taken.as_ref(), answer);
        let e = &mut self.editors[i];
        let (id, version) = (e.id.to_string(), e.version.to_string());
        e.agent_read();
        e.agent_turn = Some(Instant::now());
        let path = path.display().to_string();
        let kind = if answer { "answer" } else { "auto" };
        self.channel_send(
            content,
            &[
                ("source", "afar-editor"),
                ("editor", &id),
                ("path", &path),
                ("version", &version),
                ("mode", kind),
            ],
        );
    }

    /// What the agent gets with the turn.
    /// `instruction`: its line (from 0, where it was) and its text.
    fn editor_turn_text(
        &self,
        i: usize,
        instruction: Option<&(usize, String)>,
        answer: bool,
    ) -> String {
        let e = &self.editors[i];
        let lines = e.plain_lines();
        let mut out = format!(
            "[afar editor #{} {} v{} | the user passes you the turn | mode: {}]\n",
            e.id,
            e.path().display(),
            e.version,
            if answer {
                "answer — do not change the text, answer in your pane"
            } else {
                "auto — act in the buffer as the instruction asks (afar_buffer_insert to write \
                 after a line, afar_buffer_edit to change text)"
            }
        );
        match instruction {
            // Where it was: "after this line" means after the line above.
            Some((n, text)) => out.push_str(&format!(
                "instruction (typed at line {}, taken out of the text; \"this line\" is now line \
                 {} — 0: before line 1): {text}\n",
                n + 1,
                n
            )),
            None => out.push_str("instruction: (none — continue from the text and the cursor)\n"),
        }
        out.push_str(&format!(
            "cursor: line {}, column {}\n",
            e.cursor.line + 1,
            e.cursor.col + 1
        ));
        if let Some(t) = e.block_text() {
            let text: String = t.text.chars().take(SELECTION_LIMIT).collect();
            let what = match (e.vblock(), e.selection()) {
                (Some(b), _) if t.vertical => format!(
                    "vertical block (lines {}-{}, columns {}-{})",
                    b.top + 1,
                    b.bottom + 1,
                    b.left + 1,
                    b.right
                ),
                // Both ends inclusive, as the vertical block's columns.
                (_, Some((s, t))) if t.col == 0 && t.line > s.line => format!(
                    "selection (line {} column {} to the end of line {}, its line break included)",
                    s.line + 1,
                    s.col + 1,
                    t.line
                ),
                (_, Some((s, t))) => format!(
                    "selection (line {} column {} to line {} column {}, inclusive)",
                    s.line + 1,
                    s.col + 1,
                    t.line + 1,
                    t.col
                ),
                _ => "selection".to_string(),
            };
            out.push_str(&format!("{what}:\n{text}\n"));
        }
        match e
            .agent_last_read()
            .and_then(|v| Some((v, e.agent_version(v)?)))
        {
            // The same text under a newer version (an edit undone) is no
            // change either.
            Some((v, old)) if v == e.version || old == lines.as_slice() => {
                out.push_str("no changes since the version you last saw\n");
            }
            Some((v, old)) => {
                let a = old.join("\n");
                let b = lines.join("\n");
                let diff = similar::TextDiff::from_lines(&a, &b)
                    .unified_diff()
                    .context_radius(2)
                    .header(&format!("v{v}"), &format!("v{}", e.version))
                    .to_string();
                out.push_str(&format!(
                    "changes since v{v} (the version you last saw):\n{diff}"
                ));
            }
            None => {
                // Not read yet: the lines around the cursor, the rest
                // with afar_buffer_read.
                let from = e.cursor.line.saturating_sub(AROUND);
                let to = (e.cursor.line + AROUND + 1).min(lines.len());
                out.push_str(&format!(
                    "you have not read this buffer yet ({} lines; afar_buffer_read for the rest); \
                     lines {}-{}:\n",
                    lines.len(),
                    from + 1,
                    to
                ));
                for (n, l) in lines.iter().enumerate().take(to).skip(from) {
                    out.push_str(&format!("{:>6}\t{l}\n", n + 1));
                }
            }
        }
        out
    }

    /// Without the channel: the file (and the cursor's line or the
    /// selection) mentioned in the agent's input, the instruction typed
    /// after it, the focus on the agent — Enter is the user's.
    fn editor_mention(&mut self, i: usize, instruction: Option<&str>) {
        let Some(ide) = &self.agent.ide else {
            self.say(tr!("editor-agent-no-channel"));
            return;
        };
        let e = &self.editors[i];
        let (l1, l2) = match e.selection() {
            Some((s, t)) => (s.line, t.line),
            None => (e.cursor.line, e.cursor.line),
        };
        ide.notify(
            "at_mentioned",
            json!({
                "filePath": e.path().display().to_string(),
                "lineStart": l1,
                "lineEnd": l2,
            }),
        );
        if let Some(text) = instruction
            && let Some(agent) = &self.agent.pty
        {
            let _ = agent.write(format!(" {text}").as_bytes());
        }
        self.focus = Focus::Agent;
    }
}
