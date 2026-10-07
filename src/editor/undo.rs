//! Undo and redo (Far's `EditorUndoData`, docs/17 §7): a linear list of
//! steps; a step is one or more changes, each replacing a run of lines
//! (what was → what became). Typing in a line merges into one step until
//! the cursor jumps or another command breaks it; a new change after an
//! undo drops what could be redone. Returning to the saved state clears
//! "modified".

use super::{Line, Pos};

/// Lines `at..at + old.len()` became `new`.
pub struct Change {
    pub at: usize,
    pub old: Vec<Line>,
    pub new: Vec<Line>,
}

struct Step {
    changes: Vec<Change>,
    before: Pos,
    after: Pos,
}

pub struct History {
    steps: Vec<Step>,
    /// Steps applied (the rest can be redone).
    pos: usize,
    /// The step being recorded and how deep the groups are.
    open: Option<Step>,
    depth: usize,
    /// `pos` when the text was the file's; `None`: that state is gone.
    saved: Option<usize>,
    /// Typing may go on into the last step from this cursor.
    typing: Option<Pos>,
}

impl Default for History {
    fn default() -> Self {
        Self {
            steps: Vec::new(),
            pos: 0,
            open: None,
            depth: 0,
            saved: Some(0),
            typing: None,
        }
    }
}

impl History {
    pub fn modified(&self) -> bool {
        self.saved != Some(self.pos)
    }

    pub fn mark_saved(&mut self) {
        self.saved = Some(self.pos);
        self.typing = None;
    }

    pub fn begin(&mut self, before: Pos) {
        self.depth += 1;
        if self.depth == 1 {
            self.open = Some(Step {
                changes: Vec::new(),
                before,
                after: before,
            });
        }
    }

    pub fn end(&mut self, after: Pos) {
        self.depth = self.depth.saturating_sub(1);
        if self.depth > 0 {
            return;
        }
        let Some(mut step) = self.open.take() else {
            return;
        };
        self.typing = None;
        if step.changes.is_empty() {
            return;
        }
        step.after = after;
        self.steps.truncate(self.pos);
        if self.saved.is_some_and(|s| s > self.pos) {
            self.saved = None;
        }
        self.steps.push(step);
        self.pos += 1;
    }

    /// A change: into the open step, or merged into the last one (typing).
    pub fn record(&mut self, change: Change) {
        if let Some(step) = &mut self.open {
            step.changes.push(change);
            return;
        }
        let Some(step) = self.steps.last_mut() else {
            return;
        };
        // Typing in the same line: the last change's result changes again.
        if let Some(last) = step.changes.last_mut()
            && last.at == change.at
            && last.new.len() == 1
            && change.old.len() == 1
        {
            last.new = change.new;
        } else {
            step.changes.push(change);
        }
    }

    /// Typing at `at` can join the last step.
    pub fn can_merge(&self, at: Pos) -> bool {
        self.typing == Some(at)
            && self.open.is_none()
            && self.pos == self.steps.len()
            && self.saved != Some(self.pos)
    }

    /// A typing step ended with the cursor at `at`.
    pub fn typing(&mut self, at: Pos) {
        self.typing = Some(at);
    }

    /// Typing went on into the last step.
    pub fn merged(&mut self, at: Pos) {
        if let Some(step) = self.steps.last_mut() {
            step.after = at;
        }
        self.typing = Some(at);
    }

    /// A cursor jump: the next typing is a new step.
    pub fn break_merge(&mut self) {
        self.typing = None;
    }

    pub fn undo(&mut self, lines: &mut Vec<Line>) -> Option<Pos> {
        if self.open.is_some() || self.pos == 0 {
            return None;
        }
        self.pos -= 1;
        self.typing = None;
        let step = &self.steps[self.pos];
        for c in step.changes.iter().rev() {
            lines.splice(c.at..c.at + c.new.len(), c.old.iter().cloned());
        }
        Some(step.before)
    }

    pub fn redo(&mut self, lines: &mut Vec<Line>) -> Option<Pos> {
        if self.open.is_some() || self.pos == self.steps.len() {
            return None;
        }
        self.typing = None;
        let step = &self.steps[self.pos];
        for c in &step.changes {
            lines.splice(c.at..c.at + c.old.len(), c.new.iter().cloned());
        }
        self.pos += 1;
        Some(step.after)
    }
}
