//! The agent's test tools (`[agent] test_tools`): `afar_test_input`
//! presses keys, types text and clicks in afar as the user would — one
//! action per frame, through the same path as real input — and answers
//! with the screen; `afar_test_screen` gives the last frame as text with
//! colors or as a PNG picture. Off by default: the input bypasses the
//! agent's permissions (afar takes it as the user's).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crossterm::event::{
    Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton,
    MouseEvent, MouseEventKind,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Position;
use tokio::sync::oneshot;

use super::{App, AppMsg, Focus};
use crate::journal::{Actor, Event};
use crate::keymap::{Chord, Key};
use crate::mcp::Reply;
use crate::tr;

/// What the screen answer carries: `text`, `png`, `both` or `none`.
pub(super) type ScreenFormat = String;

/// After the last action, the screen is taken this much later (dialogs,
/// operations and redraws settle).
const SETTLE: Duration = Duration::from_millis(250);

enum Action {
    Event(TermEvent),
    Wait(Duration),
}

pub(super) struct TestRun {
    actions: VecDeque<Action>,
    wait_until: Option<Instant>,
    screen: ScreenFormat,
    reply: Option<oneshot::Sender<Reply>>,
}

/// The marker of an answer with a picture: `\0png:<base64>\0<text>`.
pub const PNG_MARK: &str = "\0png:";

fn key_event(code: KeyCode, modifiers: KeyModifiers) -> TermEvent {
    TermEvent::Key(KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    })
}

fn mouse(kind: MouseEventKind, x: u16, y: u16, modifiers: KeyModifiers) -> TermEvent {
    TermEvent::Mouse(MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers,
    })
}

fn numbers(s: &str, n: usize) -> Result<Vec<i32>, String> {
    let v: Vec<i32> = s
        .split(',')
        .map(|p| p.trim().parse::<i32>())
        .collect::<Result<_, _>>()
        .map_err(|_| format!("expected {n} numbers: {s:?}"))?;
    if v.len() != n {
        return Err(format!("expected {n} numbers: {s:?}"));
    }
    Ok(v)
}

/// One action: a key in the key map's notation (`F4`, `Ctrl+Z`,
/// `Shift+F2`, `Gray+`), `text:…`, `click:x,y`, `rclick:x,y`,
/// `dclick:x,y`, `drag:x1,y1,x2,y2`, `wheel:x,y,n` (n > 0: down) or
/// `wait:ms`.
fn parse(action: &str) -> Result<Vec<Action>, String> {
    let ev = |e: TermEvent| Action::Event(e);
    let none = KeyModifiers::NONE;
    if let Some(text) = action.strip_prefix("text:") {
        return Ok(text
            .chars()
            .map(|c| match c {
                '\n' => ev(key_event(KeyCode::Enter, none)),
                '\t' => ev(key_event(KeyCode::Tab, none)),
                c => ev(key_event(KeyCode::Char(c), none)),
            })
            .collect());
    }
    if let Some(ms) = action.strip_prefix("wait:") {
        let ms: u64 = ms
            .trim()
            .parse()
            .map_err(|_| format!("bad wait: {action:?}"))?;
        return Ok(vec![Action::Wait(Duration::from_millis(ms.min(10_000)))]);
    }
    let click = |kind_down: MouseButton, x: i32, y: i32| -> Vec<Action> {
        let (x, y) = (x.max(0) as u16, y.max(0) as u16);
        vec![
            ev(mouse(MouseEventKind::Down(kind_down), x, y, none)),
            ev(mouse(MouseEventKind::Up(kind_down), x, y, none)),
        ]
    };
    if let Some(rest) = action.strip_prefix("click:") {
        let v = numbers(rest, 2)?;
        return Ok(click(MouseButton::Left, v[0], v[1]));
    }
    if let Some(rest) = action.strip_prefix("rclick:") {
        let v = numbers(rest, 2)?;
        return Ok(click(MouseButton::Right, v[0], v[1]));
    }
    if let Some(rest) = action.strip_prefix("dclick:") {
        let v = numbers(rest, 2)?;
        let mut a = click(MouseButton::Left, v[0], v[1]);
        a.extend(click(MouseButton::Left, v[0], v[1]));
        return Ok(a);
    }
    if let Some(rest) = action.strip_prefix("drag:") {
        let v = numbers(rest, 4)?;
        let p = |i: usize| v[i].max(0) as u16;
        return Ok(vec![
            ev(mouse(
                MouseEventKind::Down(MouseButton::Left),
                p(0),
                p(1),
                none,
            )),
            ev(mouse(
                MouseEventKind::Drag(MouseButton::Left),
                p(2),
                p(3),
                none,
            )),
            ev(mouse(
                MouseEventKind::Up(MouseButton::Left),
                p(2),
                p(3),
                none,
            )),
        ]);
    }
    if let Some(rest) = action.strip_prefix("wheel:") {
        let v = numbers(rest, 3)?;
        let kind = if v[2] > 0 {
            MouseEventKind::ScrollDown
        } else {
            MouseEventKind::ScrollUp
        };
        return Ok((0..v[2].unsigned_abs().max(1))
            .map(|_| ev(mouse(kind, v[0].max(0) as u16, v[1].max(0) as u16, none)))
            .collect());
    }
    let chord = Chord::parse(action).ok_or_else(|| {
        format!(
            "unknown action {action:?}: a key (F4, Ctrl+Z, Shift+F2, Enter, Esc, Up, …), text:…, \
             click:x,y, rclick:x,y, dclick:x,y, drag:x1,y1,x2,y2, wheel:x,y,n or wait:ms"
        )
    })?;
    let mut m = KeyModifiers::NONE;
    if chord.ctrl {
        m |= KeyModifiers::CONTROL;
    }
    if chord.alt {
        m |= KeyModifiers::ALT;
    }
    if chord.shift {
        m |= KeyModifiers::SHIFT;
    }
    let mut state = KeyEventState::empty();
    let code = match chord.key {
        Key::Char(c) if chord.shift && c.is_ascii_alphabetic() => {
            KeyCode::Char(c.to_ascii_uppercase())
        }
        Key::Char(c) => KeyCode::Char(c),
        Key::Gray(c) => {
            state = KeyEventState::KEYPAD;
            KeyCode::Char(c)
        }
        Key::F(n) => KeyCode::F(n),
        Key::Up => KeyCode::Up,
        Key::Down => KeyCode::Down,
        Key::Left => KeyCode::Left,
        Key::Right => KeyCode::Right,
        Key::PgUp => KeyCode::PageUp,
        Key::PgDn => KeyCode::PageDown,
        Key::Home => KeyCode::Home,
        Key::End => KeyCode::End,
        Key::Ins => KeyCode::Insert,
        Key::Del => KeyCode::Delete,
        Key::Enter => KeyCode::Enter,
        Key::Tab if chord.shift => KeyCode::BackTab,
        Key::Tab => KeyCode::Tab,
        Key::Esc => KeyCode::Esc,
        Key::Bs => KeyCode::Backspace,
        Key::Space => KeyCode::Char(' '),
    };
    Ok(vec![Action::Event(TermEvent::Key(KeyEvent {
        code,
        modifiers: m,
        kind: KeyEventKind::Press,
        state,
    }))])
}

/// The screen answer of the last frame.
pub(super) fn screen_answer(frame: Option<&(Buffer, Option<Position>)>, format: &str) -> Reply {
    let Some((buf, cursor)) = frame else {
        return Err("no frame drawn yet".into());
    };
    let text = || crate::shot::text(buf, *cursor);
    match format {
        "none" => Ok("done".into()),
        "png" | "both" => {
            let png = crate::shot::png(buf, *cursor)?;
            let rest = if format == "both" {
                text()
            } else {
                String::new()
            };
            Ok(format!("{PNG_MARK}{}\0{rest}", crate::shot::base64(&png)))
        }
        _ => Ok(text()),
    }
}

impl App {
    /// `afar_test_input`: the actions are queued and played in the main
    /// loop; the answer (with the screen) comes after the last one.
    pub(super) fn test_input(
        &mut self,
        actions: Vec<String>,
        screen: Option<String>,
        reply: oneshot::Sender<Reply>,
    ) {
        if !self.config.agent.test_tools {
            let _ = reply.send(Err("the test tools are off ([agent] test_tools)".into()));
            return;
        }
        if self.test_run.is_some() {
            let _ = reply.send(Err("another afar_test_input is still running".into()));
            return;
        }
        let mut queue = VecDeque::new();
        for a in &actions {
            match parse(a) {
                Ok(list) => queue.extend(list),
                Err(e) => {
                    let _ = reply.send(Err(e));
                    return;
                }
            }
        }
        queue.push_back(Action::Wait(SETTLE));
        self.journal.push(
            Actor::Agent,
            Event::TestInput {
                actions: actions.iter().take(40).cloned().collect(),
            },
        );
        let shown: Vec<&str> = actions.iter().take(6).map(String::as_str).collect();
        self.say(tr!("test-input", what = shown.join(", ")));
        // The keys are for afar, not for the agent's own pane.
        if self.focus == Focus::Agent {
            self.focus = Focus::Panels;
        }
        self.test_run = Some(TestRun {
            actions: queue,
            wait_until: None,
            screen: screen.unwrap_or_else(|| "text".into()),
            reply: Some(reply),
        });
    }

    /// After a frame: the next queued action, or the answer once all are
    /// played. `true` while a run goes on (the loop should come back soon).
    pub(super) fn test_step(&mut self) -> bool {
        let Some(run) = &mut self.test_run else {
            return false;
        };
        if let Some(t) = run.wait_until {
            if Instant::now() < t {
                return true;
            }
            run.wait_until = None;
        }
        match run.actions.pop_front() {
            Some(Action::Wait(d)) => {
                run.wait_until = Some(Instant::now() + d);
                true
            }
            Some(Action::Event(ev)) => {
                self.handle(AppMsg::Input(ev));
                true
            }
            None => {
                let run = self.test_run.take().expect("checked above");
                let answer = screen_answer(self.last_frame.as_ref(), &run.screen);
                if let Some(reply) = run.reply {
                    let _ = reply.send(answer);
                }
                false
            }
        }
    }
}
