//! The layer of proposals (docs/11 «Три слоя», plan step 5): an agent's
//! change not taken into the text yet — "lines `start..start + old.len()`
//! become `new`". The old lines stay in the text, drawn struck out, the
//! new ones show as rows of their own under them. Accepting puts the
//! change in as the agent's (one undo step, its lines marked); rejecting
//! drops it. A change of the lines it stands on drops it too: its ground
//! is gone.

use super::{AgentEdit, Editor, Pos};

/// One proposal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proposal {
    pub id: u64,
    /// The first line it replaces (from 0); with no old lines, the line
    /// the new ones go before.
    pub start: usize,
    /// The lines it replaces, as they are in the text.
    pub old: Vec<String>,
    /// What they become (none: they go).
    pub new: Vec<String>,
}

impl Proposal {
    /// The text's lines it stands on (an insertion: none).
    pub fn old_end(&self) -> usize {
        self.start + self.old.len()
    }
}

/// A row of the screen: a line of the text, a proposal's new line, or
/// past the end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenRow {
    Line(usize),
    /// Proposal (index in `proposals`), its new line.
    Proposed(usize, usize),
    End,
}

impl Editor {
    fn next_proposal_id(&mut self) -> u64 {
        self.proposal_seq += 1;
        self.proposal_seq
    }

    /// Whether the agent's `old` → `new` touches only its own lines not
    /// accepted yet (mixed mode: those it may change at once).
    pub fn replace_is_agents_own(&self, old: &str) -> bool {
        let text = self.plain_lines().join("\n");
        text.match_indices(old).all(|(at, _)| {
            let s = self.pos_of_offset(at);
            let e = self.pos_of_offset(at + old.len());
            (s.line..=e.line).all(|l| self.lines[l].by_agent)
        })
    }

    /// `afar_buffer_edit` as a proposal: each match's lines, the match
    /// replaced in them. Returns the proposals made.
    pub fn propose_replace(
        &mut self,
        old: &str,
        new: &str,
        all: bool,
    ) -> Result<Vec<Proposal>, String> {
        if old.is_empty() {
            return Err("old_string is empty".into());
        }
        let text = self.plain_lines().join("\n");
        let found: Vec<usize> = text.match_indices(old).map(|(i, _)| i).collect();
        match found.len() {
            0 => {
                return Err(
                    "old_string not found in the buffer (read it with afar_buffer_read)".into(),
                );
            }
            n if n > 1 && !all => {
                return Err(format!(
                    "old_string found {n} times; give more context to make it unique, or set replace_all"
                ));
            }
            _ => {}
        }
        let new = new.replace("\r\n", "\n");
        // The lines of each match; matches on touching lines make one. A
        // match ending at the start of a line (it takes the break before)
        // stands on the lines before that one.
        let mut groups: Vec<(usize, usize, Vec<usize>)> = Vec::new();
        let ends_at_break = old.ends_with('\n');
        for &at in &found {
            let end = self.pos_of_offset(at + old.len());
            let s = self.pos_of_offset(at).line;
            let e = if ends_at_break && end.col == 0 && end.line > s {
                end.line - 1
            } else {
                end.line
            };
            match groups.last_mut() {
                Some(g) if s <= g.1 + 1 => {
                    g.1 = g.1.max(e);
                    g.2.push(at);
                }
                _ => groups.push((s, e, vec![at])),
            }
        }
        // Where each line starts in `text`.
        let mut line_at = Vec::with_capacity(self.lines.len());
        let mut off = 0;
        for l in &self.lines {
            line_at.push(off);
            off += l.text.len() + 1;
        }
        let mut made = Vec::new();
        for (s, e, ats) in groups {
            if self
                .proposals
                .iter()
                .any(|p| p.start <= e && s < p.old_end().max(p.start + 1))
            {
                return Err(format!(
                    "lines {}-{} have a proposal waiting for the user's answer already",
                    s + 1,
                    e + 1
                ));
            }
            let from = line_at[s];
            // The lines with their last break when the match takes it.
            let to = line_at[e] + self.lines[e].text.len() + usize::from(ends_at_break);
            let to = to.min(text.len());
            let mut block = String::new();
            let mut last = from;
            for at in ats {
                block.push_str(&text[last..at]);
                block.push_str(&new);
                last = at + old.len();
            }
            block.push_str(&text[last.min(to)..to]);
            let new_lines: Vec<String> = if ends_at_break {
                match block.strip_suffix('\n') {
                    // All of it goes, the breaks too.
                    _ if block.is_empty() => Vec::new(),
                    Some(b) => b.split('\n').map(str::to_string).collect(),
                    None => block.split('\n').map(str::to_string).collect(),
                }
            } else {
                block.split('\n').map(str::to_string).collect()
            };
            let id = self.next_proposal_id();
            let p = Proposal {
                id,
                start: s,
                old: (s..=e).map(|l| self.lines[l].text.clone()).collect(),
                new: new_lines,
            };
            made.push(p);
        }
        self.proposals.extend(made.iter().cloned());
        self.proposals.sort_by_key(|p| p.start);
        Ok(made)
    }

    /// `afar_buffer_insert` as a proposal: new lines after line `after`
    /// (from 1; 0: before the first).
    pub fn propose_insert(&mut self, after: usize, text: &str) -> Result<Proposal, String> {
        let n = self.lines.len();
        if after > n {
            return Err(format!(
                "line {after} is past the end: the buffer has {n} lines"
            ));
        }
        let text = text.replace("\r\n", "\n");
        let text = text.strip_suffix('\n').unwrap_or(&text);
        let id = self.next_proposal_id();
        let p = Proposal {
            id,
            start: after,
            old: Vec::new(),
            new: text.split('\n').map(str::to_string).collect(),
        };
        self.proposals.push(p.clone());
        self.proposals.sort_by_key(|p| p.start);
        Ok(p)
    }

    /// The proposal the cursor is on: on one of its old lines, or (an
    /// insertion) on the line it follows.
    pub fn proposal_at_cursor(&self) -> Option<u64> {
        let c = self.cursor.line;
        self.proposals
            .iter()
            .find(|p| {
                if p.old.is_empty() {
                    c + 1 == p.start || (p.start == 0 && c == 0)
                } else {
                    (p.start..p.old_end()).contains(&c)
                }
            })
            .map(|p| p.id)
    }

    /// Accepts proposal `id`: its change goes into the text as the
    /// agent's.
    pub fn accept_proposal(&mut self, id: u64) -> Result<(Proposal, AgentEdit), String> {
        let k = self
            .proposals
            .iter()
            .position(|p| p.id == id)
            .ok_or("no such proposal")?;
        let p = self.proposals.remove(k);
        let here: Vec<String> = self.lines[p.start.min(self.lines.len())..]
            .iter()
            .take(p.old.len())
            .map(|l| l.text.clone())
            .collect();
        if here != p.old {
            return Err("the text under the proposal changed".into());
        }
        let edit = self.agent_portion(|ed| {
            let joined = p.new.join("\n");
            if p.old.is_empty() {
                if p.start == 0 {
                    ed.agent_change(Pos::default(), Pos::default(), &format!("{joined}\n"));
                } else {
                    let line = p.start - 1;
                    let end = Pos::new(line, ed.line_len(line));
                    ed.agent_change(end, end, &format!("\n{joined}"));
                }
            } else if p.new.is_empty() {
                // The lines go, with their breaks.
                let last = p.old_end() - 1;
                if p.old_end() < ed.lines.len() {
                    ed.agent_change(Pos::new(p.start, 0), Pos::new(p.old_end(), 0), "");
                } else if p.start > 0 {
                    let s = Pos::new(p.start - 1, ed.line_len(p.start - 1));
                    ed.agent_change(s, Pos::new(last, ed.line_len(last)), "");
                } else {
                    ed.agent_change(Pos::new(0, 0), Pos::new(last, ed.line_len(last)), "");
                }
            } else {
                let last = p.old_end() - 1;
                ed.agent_change(
                    Pos::new(p.start, 0),
                    Pos::new(last, ed.line_len(last)),
                    &joined,
                );
            }
        });
        Ok((p, edit))
    }

    /// Rejects proposal `id`.
    pub fn reject_proposal(&mut self, id: u64) -> Option<Proposal> {
        let k = self.proposals.iter().position(|p| p.id == id)?;
        Some(self.proposals.remove(k))
    }

    /// Proposals after a change of lines `s..=e` that left `delta` more
    /// lines: those below move, those it touched go.
    pub(super) fn proposals_after_change(&mut self, s: usize, e: usize, delta: isize) {
        let before: Vec<u64> = self.proposals.iter().map(|p| p.id).collect();
        self.proposals.retain_mut(|p| {
            if p.old.is_empty() {
                if e < p.start {
                    p.start = (p.start as isize + delta).max(0) as usize;
                    true
                } else {
                    // An edit across the place it goes in.
                    !(s < p.start)
                }
            } else if e < p.start {
                p.start = (p.start as isize + delta).max(0) as usize;
                true
            } else {
                // Below it stays; on its lines it goes.
                s >= p.old_end()
            }
        });
        self.note_dropped(&before);
    }

    /// Proposals gone since `before` (their lines changed): kept for the
    /// journal (`take_dropped_proposals`).
    pub(super) fn note_dropped(&mut self, before: &[u64]) {
        let now: Vec<u64> = self.proposals.iter().map(|p| p.id).collect();
        self.dropped_proposals
            .extend(before.iter().filter(|id| !now.contains(id)));
    }

    /// The proposals dropped since last asked (for the journal).
    pub fn take_dropped_proposals(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.dropped_proposals)
    }

    /// Several answers as one undo step (Ctrl+Alt+F5).
    pub fn in_one_step(&mut self, f: impl FnOnce(&mut Self)) {
        self.step(f);
    }

    /// The rows of the screen from line `top`: lines, and under a
    /// proposal's old lines (or before the line it goes before) its new
    /// ones.
    pub fn screen_rows(&self, height: usize) -> Vec<ScreenRow> {
        let mut rows = Vec::with_capacity(height);
        let mut n = self.top;
        while rows.len() < height {
            for (k, p) in self.proposals.iter().enumerate() {
                if p.old.is_empty() && p.start == n {
                    rows.extend((0..p.new.len()).map(|j| ScreenRow::Proposed(k, j)));
                }
            }
            if n >= self.lines.len() {
                rows.push(ScreenRow::End);
            } else {
                rows.push(ScreenRow::Line(n));
                for (k, p) in self.proposals.iter().enumerate() {
                    if !p.old.is_empty() && p.old_end() == n + 1 {
                        rows.extend((0..p.new.len()).map(|j| ScreenRow::Proposed(k, j)));
                    }
                }
            }
            n += 1;
        }
        rows.truncate(height);
        rows
    }

    /// The proposal whose old lines hold line `n`, if any.
    pub fn proposal_over(&self, n: usize) -> Option<&Proposal> {
        self.proposals
            .iter()
            .find(|p| (p.start..p.old_end()).contains(&n))
    }
}

#[cfg(test)]
mod tests {
    use crate::editor::{Editor, Eol, Line, Pos};
    use std::path::Path;

    fn ed(lines: &[&str]) -> Editor {
        let lines = lines
            .iter()
            .map(|t| Line::new(t.to_string(), Eol::Lf))
            .collect();
        Editor::new(1, Path::new("t.txt"), lines, 65001, false, Eol::Lf)
    }

    #[test]
    fn a_proposal_waits_and_accepting_puts_it_in() {
        let mut e = ed(&["one", "two", "three"]);
        let made = e.propose_replace("two", "2\n2b", false).unwrap();
        assert_eq!(made.len(), 1);
        assert_eq!(
            e.plain_lines(),
            vec!["one", "two", "three"],
            "the text waits"
        );
        e.cursor = Pos::new(1, 0);
        let id = e.proposal_at_cursor().unwrap();
        e.accept_proposal(id).unwrap();
        assert_eq!(e.plain_lines(), vec!["one", "2", "2b", "three"]);
        assert!(e.lines()[1].by_agent);
        e.undo();
        assert_eq!(e.plain_lines(), vec!["one", "two", "three"]);
    }

    #[test]
    fn an_insertion_and_a_deletion_as_proposals() {
        let mut e = ed(&["a", "b", "c"]);
        let p = e.propose_insert(1, "new").unwrap();
        e.accept_proposal(p.id).unwrap();
        assert_eq!(e.plain_lines(), vec!["a", "new", "b", "c"]);
        let made = e.propose_replace("b\nc", "", false).unwrap();
        assert_eq!(made[0].new, vec![String::new()]);
        e.reject_proposal(made[0].id);
        // A whole line with its break: that line only, nothing new.
        let made = e.propose_replace("b\n", "", false).unwrap();
        assert_eq!((made[0].start, made[0].old.len()), (2, 1));
        assert!(made[0].new.is_empty());
        e.accept_proposal(made[0].id).unwrap();
        assert_eq!(e.plain_lines(), vec!["a", "new", "c"]);
        let mut e = ed(&["a", "b", "c"]);
        e.proposals.push(super::Proposal {
            id: 99,
            start: 1,
            old: vec!["b".into()],
            new: vec![],
        });
        e.accept_proposal(99).unwrap();
        assert_eq!(e.plain_lines(), vec!["a", "c"]);
    }

    #[test]
    fn proposals_move_with_the_text_and_go_when_their_lines_change() {
        let mut e = ed(&["a", "b", "c"]);
        e.propose_replace("c", "C", false).unwrap();
        e.cursor = Pos::new(0, 0);
        e.insert_text("x\n");
        assert_eq!(e.proposals[0].start, 3);
        e.cursor = Pos::new(3, 1);
        e.type_char('!');
        assert!(e.proposals.is_empty(), "its ground changed");
        e.propose_replace("b", "B", false).unwrap();
        let rows = e.screen_rows(6);
        assert_eq!(
            rows[..4],
            [
                super::ScreenRow::Line(0),
                super::ScreenRow::Line(1),
                super::ScreenRow::Line(2),
                super::ScreenRow::Proposed(0, 0)
            ]
        );
        e.reject_proposal(e.proposals[0].id);
        assert!(e.proposals.is_empty());
    }
}
