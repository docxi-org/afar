//! The agent's permission prompts in afar (Claude Code's `PermissionRequest`
//! hook): instead of its question in the terminal, afar's dialog — allow,
//! deny, or leave it to the terminal. A shell command can be allowed by
//! afar itself after a few seconds (`[agent] allow_commands`), the seconds
//! ticking on the button; the user may stop that without denying.

use std::time::Instant;

use tokio::sync::oneshot;

use super::fileops::{Overlay, Purpose};
use super::{App, agent};
use crate::dialog::Dialog;
use crate::mcp::Reply;
use crate::tr;

/// How long a command afar allows by itself waits for the user to stop it.
const ALLOW_DELAY: std::time::Duration = std::time::Duration::from_secs(3);
/// Characters per line of a command in the dialog.
const LINE: usize = 72;

/// What the user answered.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Answer {
    Allow,
    Deny,
    /// Claude Code asks in its terminal as usual.
    Terminal,
}

impl App {
    /// `PermissionRequest`: the agent wants a tool it must ask for.
    pub(super) fn permission_request(&mut self, input: String, reply: oneshot::Sender<Reply>) {
        // An edit with the IDE protocol connected comes as its own question
        // (`openDiff`, the "Agent's edit" dialog): one is enough.
        let v: serde_json::Value = serde_json::from_str(&input).unwrap_or_default();
        if self.agent.ide_connected && is_edit(v["tool_name"].as_str().unwrap_or("")) {
            let _ = reply.send(Ok(String::new()));
            return;
        }
        self.agent_state(agent::AgentState::Waiting(String::new()));
        self.permission_dialog(input, Some(reply), true);
    }

    /// The question; `countdown`: afar may allow a shell command itself.
    pub(super) fn permission_dialog(
        &mut self,
        input: String,
        reply: Option<oneshot::Sender<Reply>>,
        countdown: bool,
    ) {
        let v: serde_json::Value = serde_json::from_str(&input).unwrap_or_default();
        let tool = v["tool_name"].as_str().unwrap_or("?").to_string();
        let args = &v["tool_input"];
        let shell = matches!(tool.as_str(), "Bash" | "PowerShell");
        let mut lines = Vec::new();
        match tool.as_str() {
            "Bash" | "PowerShell" => {
                for l in args["command"].as_str().unwrap_or("").lines() {
                    lines.extend(chunks(l, LINE));
                }
                if let Some(d) = args["description"].as_str().filter(|d| !d.is_empty()) {
                    lines.push("\u{1}".into());
                    lines.extend(chunks(d, LINE));
                }
            }
            "WebFetch" => {
                lines.extend(chunks(args["url"].as_str().unwrap_or(""), LINE));
            }
            _ => {
                let path = args["file_path"]
                    .as_str()
                    .or_else(|| args["path"].as_str())
                    .map(str::to_string);
                match path {
                    Some(p) => lines.extend(chunks(&p, LINE)),
                    None => lines.extend(chunks(&args.to_string(), LINE).into_iter().take(6)),
                }
            }
        }
        if let Some(cwd) = v["cwd"].as_str() {
            lines.push("\u{1}".into());
            lines.push(tr!("ask-folder", folder = cwd));
        }
        // A command or an edit afar takes itself after a countdown
        // (`allow_commands`, `accept_edits` — in the agent's folder).
        let edit = is_edit(&tool);
        let in_folder = args["file_path"]
            .as_str()
            .is_some_and(|p| self.in_agent_folder(std::path::Path::new(p)));
        let auto = countdown
            && (shell && self.config.agent.allow_commands
                || edit && in_folder && self.config.agent.accept_edits);
        let deadline = auto.then(|| Instant::now() + ALLOW_DELAY);
        let allow = if auto {
            tr!("ask-allow-in", sec = ALLOW_DELAY.as_secs())
        } else {
            tr!("ask-allow")
        };
        let mut buttons = vec![allow, tr!("ask-deny"), tr!("ask-terminal")];
        if shell || edit {
            buttons.push(if auto {
                tr!("ask-stop-auto")
            } else {
                tr!("ask-allow-all")
            });
        }
        let refs: Vec<&str> = buttons.iter().map(String::as_str).collect();
        let dialog = Dialog::message(
            &tr!("ask-title", tool = tool.as_str()),
            &lines,
            &refs,
            false,
        );
        self.overlays.push(Overlay::Dialog {
            dialog,
            purpose: Purpose::AgentPermission {
                input,
                reply,
                deadline,
            },
        });
        // The question needs the user even if they were in the agent's pane.
        if self.focus == super::Focus::Agent {
            self.say(tr!("ask-waits"));
        }
    }

    /// A button of the question (`None`: Esc — to the terminal).
    pub(super) fn permission_closed(
        &mut self,
        button: Option<usize>,
        input: String,
        reply: Option<oneshot::Sender<Reply>>,
        deadline: Option<Instant>,
    ) {
        let answer = match button {
            Some(0) => Answer::Allow,
            Some(1) => Answer::Deny,
            // During the countdown: afar stops allowing by itself, the
            // command is asked about as usual (not denied).
            Some(3) if deadline.is_some() => {
                self.auto_on(&input, false);
                self.permission_dialog(input, reply, false);
                return;
            }
            // "Allow all": this one and the next ones.
            Some(3) => {
                self.auto_on(&input, true);
                Answer::Allow
            }
            _ => Answer::Terminal,
        };
        self.permission_answer(reply, answer);
    }

    /// Tells Claude Code the answer; the agent works on unless it waits for
    /// the terminal.
    fn permission_answer(&mut self, reply: Option<oneshot::Sender<Reply>>, answer: Answer) {
        let body = match answer {
            Answer::Allow => serde_json::json!({"hookSpecificOutput": {
                "hookEventName": "PermissionRequest",
                "decision": {"behavior": "allow"},
            }})
            .to_string(),
            Answer::Deny => serde_json::json!({"hookSpecificOutput": {
                "hookEventName": "PermissionRequest",
                "decision": {"behavior": "deny", "message": "The user denied this in afar."},
            }})
            .to_string(),
            Answer::Terminal => String::new(),
        };
        if let Some(reply) = reply {
            let _ = reply.send(Ok(body));
        }
        if answer != Answer::Terminal {
            self.agent_state(agent::AgentState::Working);
        }
    }

    /// Allowing by itself on or off for the kind of `input`'s tool: edits
    /// (`accept_edits`) or commands (`allow_commands`).
    fn auto_on(&mut self, input: &str, on: bool) {
        let v: serde_json::Value = serde_json::from_str(input).unwrap_or_default();
        if is_edit(v["tool_name"].as_str().unwrap_or("")) {
            self.accept_edits_on(on);
        } else {
            self.allow_commands_on(on);
        }
    }

    /// `[agent] allow_commands` on or off (kept in config.toml).
    pub(super) fn allow_commands_on(&mut self, on: bool) {
        self.config.agent.allow_commands = on;
        let path = crate::config::config_path();
        if let Err(e) = self.config.save(&path) {
            self.say(tr!("settings-save-failed", error = e));
        }
        self.say(if on {
            tr!("ask-allow-commands-on")
        } else {
            tr!("ask-allow-commands-off")
        });
    }

    /// The commands afar allows by itself: the seconds left on the button;
    /// when they are over, allowed. A question whose hook gave up goes.
    pub(super) fn permission_tick(&mut self) {
        let now = Instant::now();
        let mut due = None;
        for (i, o) in self.overlays.iter_mut().enumerate() {
            if let Overlay::Dialog {
                dialog,
                purpose:
                    Purpose::AgentPermission {
                        deadline, reply, ..
                    },
            } = o
            {
                if reply.as_ref().is_some_and(|r| r.is_closed()) {
                    due = Some((i, false));
                    break;
                }
                let Some(t) = deadline else { continue };
                if now >= *t {
                    due = Some((i, true));
                    break;
                }
                let left = (*t - now).as_secs_f32().ceil() as u64;
                dialog.set_button_label(0, &tr!("ask-allow-in", sec = left));
            }
        }
        let Some((i, allow)) = due else {
            return;
        };
        if let Overlay::Dialog {
            purpose: Purpose::AgentPermission { reply, input, .. },
            ..
        } = self.overlays.remove(i)
        {
            if allow {
                self.permission_answer(reply, Answer::Allow);
                let v: serde_json::Value = serde_json::from_str(&input).unwrap_or_default();
                let tool = v["tool_name"].as_str().unwrap_or("?").to_string();
                self.say(tr!("ask-allowed-auto", tool = tool));
            } else {
                self.say(tr!("agent-request-abandoned"));
            }
        }
    }
}

/// A tool that edits files.
fn is_edit(tool: &str) -> bool {
    matches!(tool, "Edit" | "MultiEdit" | "Write" | "NotebookEdit")
}

/// `text` in pieces of at most `width` characters.
fn chunks(text: &str, width: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return vec![String::new()];
    }
    chars.chunks(width).map(|c| c.iter().collect()).collect()
}
