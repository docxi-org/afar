//! Text pasted into the agent's pane. Windows Terminal sends a paste to
//! afar as typed keys, a line break as Enter — passed on as they are,
//! Claude Code would send the prompt at the first line break. Keys that
//! arrive in one burst, with a line break followed by more text, are a
//! paste: they go to the agent as one bracketed paste (`ESC[200~ … ESC[201~`)
//! when it asks for that.
//!
//! In the editor such a burst is a paste too (Windows Terminal keeps
//! Ctrl+V for itself): it goes in as one undo step, and when it is the
//! clipboard's text marked as a column (Far's vertical block), as a column.

use std::sync::mpsc::Receiver;
use std::time::Duration;

use crossterm::event::{Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::{App, AppMsg, Focus};

/// Keys closer than this belong to one burst (a person types slower).
const BURST_GAP: Duration = Duration::from_millis(8);
/// A burst is not waited for longer than this many messages.
const BURST_MAX: usize = 1_000_000;

/// A key that types text: a character (AltGr is Ctrl+Alt), Enter, Tab —
/// or a modifier, which comes along.
fn typed(msg: &AppMsg) -> Option<&KeyEvent> {
    let AppMsg::Input(TermEvent::Key(k)) = msg else {
        return None;
    };
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    match k.code {
        KeyCode::Char(_) if !(ctrl ^ alt) => Some(k),
        KeyCode::Enter | KeyCode::Tab if !ctrl && !alt => Some(k),
        KeyCode::Modifier(_) => Some(k),
        _ => None,
    }
}

/// The text of typed keys when they are a paste: a line break with more
/// text after it (a single Enter at the end is typed, not pasted).
fn pasted_text<'a>(keys: impl Iterator<Item = &'a KeyEvent>) -> Option<String> {
    let text: String = keys
        .filter(|k| k.kind != KeyEventKind::Release)
        .filter_map(|k| match k.code {
            KeyCode::Char(c) => Some(c),
            KeyCode::Enter => Some('\r'),
            KeyCode::Tab => Some('\t'),
            _ => None,
        })
        .collect();
    let i = text.find('\r')?;
    text[i + 1..].chars().any(|c| c != '\r').then_some(text)
}

/// Where a burst of typed keys is gathered as a paste.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Agent,
    Editor(usize),
}

/// Line breaks as Windows Terminal sends them (each as Enter, `\r`).
fn breaks_as_enter(text: &str) -> String {
    text.replace("\r\n", "\r").replace('\n', "\r")
}

impl App {
    /// Where the keys go, if that is a place a paste is gathered for.
    fn paste_target(&self) -> Option<Target> {
        if self.has_overlay() {
            return None;
        }
        match self.focus {
            Focus::Agent if self.agent_alive() => Some(Target::Agent),
            Focus::Panels => self.shown_editor().map(Target::Editor),
            _ => None,
        }
    }

    /// The messages that came with `first`; typing into the agent waits a
    /// moment for the rest of a burst.
    pub(super) fn take_messages(&self, first: AppMsg, rx: &Receiver<AppMsg>) -> Vec<AppMsg> {
        let mut batch = vec![first];
        while let Ok(m) = rx.try_recv() {
            batch.push(m);
        }
        if self.paste_target().is_some() && batch.iter().any(|m| typed(m).is_some()) {
            while batch.len() < BURST_MAX {
                match rx.recv_timeout(BURST_GAP) {
                    Ok(m) => batch.push(m),
                    Err(_) => break,
                }
            }
        }
        batch
    }

    /// Handles the messages in order; a run of typed keys that is a paste
    /// goes to the agent in one piece.
    pub(super) fn handle_batch(&mut self, batch: Vec<AppMsg>) {
        let mut run: Vec<AppMsg> = Vec::new();
        for msg in batch {
            if typed(&msg).is_some() {
                run.push(msg);
                continue;
            }
            self.flush_run(std::mem::take(&mut run));
            self.handle(msg);
        }
        self.flush_run(run);
    }

    fn flush_run(&mut self, run: Vec<AppMsg>) {
        if run.is_empty() {
            return;
        }
        if let Some(target) = self.paste_target()
            && let Some(text) = pasted_text(run.iter().filter_map(typed))
        {
            match target {
                Target::Agent => self.paste_to_agent(&text),
                Target::Editor(i) => self.paste_to_editor(i, &text),
            }
            return;
        }
        for msg in run {
            self.handle(msg);
        }
    }

    /// A paste in the editor: the clipboard's own text when it is what
    /// came (a column goes in as a column), else what came; one undo step.
    fn paste_to_editor(&mut self, i: usize, text: &str) {
        match self.clip_get_block() {
            Some((clip, vertical)) if breaks_as_enter(&clip) == text => {
                if vertical {
                    self.editors[i].paste_vertical(&clip);
                } else {
                    self.editors[i].insert_text(&clip);
                }
            }
            _ => self.editors[i].insert_text(text),
        }
        self.editors[i].scroll_to_cursor();
    }

    fn paste_to_agent(&mut self, text: &str) {
        let Some(agent) = &self.agent.pty else { return };
        agent.parser().screen_mut().set_scrollback(0);
        let bracketed = agent.parser().screen().bracketed_paste();
        let data = if bracketed {
            format!("\x1b[200~{text}\x1b[201~")
        } else {
            text.to_string()
        };
        let _ = agent.write(data.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(s: &str) -> Vec<KeyEvent> {
        s.chars()
            .map(|c| match c {
                '\n' => KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                c => KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
            })
            .collect()
    }

    #[test]
    fn clipboard_text_compares_as_windows_terminal_sends_it() {
        let came = pasted_text(keys("ab \ncd\n").iter()).unwrap();
        assert_eq!(breaks_as_enter("ab \r\ncd\r\n"), came);
        assert_eq!(breaks_as_enter("ab \ncd\n"), came);
    }

    #[test]
    fn line_breaks_inside_make_a_paste() {
        let k = keys("first line\nsecond\n");
        assert_eq!(
            pasted_text(k.iter()).as_deref(),
            Some("first line\rsecond\r")
        );
        // Typed (or caught up) text with one Enter at the end: as typed.
        assert_eq!(pasted_text(keys("yes\n").iter()), None);
        assert_eq!(pasted_text(keys("one line").iter()), None);
        // Releases are not text.
        let mut k = keys("a\nb");
        let mut release = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        k.insert(1, release);
        assert_eq!(pasted_text(k.iter()).as_deref(), Some("a\rb"));
    }
}
