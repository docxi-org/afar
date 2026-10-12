//! The agent's folder profile at its start (`crate::agentprofile`): the
//! user's `[[agent.folders]]`, and the project's `afar.toml` once the user
//! trusts its text. The answer is asked in a dialog and kept (history.db's
//! `meta`) until the file's text changes, as `direnv allow` does.

use std::path::{Path, PathBuf};

use super::App;
use super::agent::Launch;
use super::fileops::{Overlay, Purpose};
use crate::agentprofile::{self, Profile};
use crate::dialog::Dialog;
use crate::tr;

/// A start of the agent held until the user's word on a project file.
pub(super) struct TrustAsk {
    pub file: PathBuf,
    pub text: String,
    pub cwd: PathBuf,
    pub launch: Launch,
    pub extra: Vec<String>,
    pub cols: u16,
    pub rows: u16,
}

/// The project file with no answer yet: its path, text and profile.
pub(super) struct Untrusted {
    pub file: PathBuf,
    pub text: String,
    pub project: Profile,
}

fn trust_key(file: &Path) -> String {
    format!("agent-trust:{}", super::folder_key(file))
}

impl App {
    /// What an agent starting in `cwd` gets beyond `[agent]`; `Err` while
    /// the project file waits for the user's answer.
    pub(super) fn agent_profile(&mut self, cwd: &Path) -> Result<Profile, Untrusted> {
        let mut profile = agentprofile::for_folder(&self.config.agent.folders, cwd);
        let Some(file) = agentprofile::find(cwd) else {
            return Ok(profile);
        };
        let parsed = std::fs::read_to_string(&file)
            .map_err(|e| e.to_string())
            .and_then(|text| agentprofile::parse(&text).map(|p| (text, p)));
        let (text, project) = match parsed {
            Ok(found) => found,
            Err(error) => {
                self.say(tr!(
                    "agent-project-broken",
                    file = file.display().to_string(),
                    error = error
                ));
                return Ok(profile);
            }
        };
        if project.is_empty() {
            return Ok(profile);
        }
        match self.store.meta(&trust_key(&file)) {
            Some(v) if v == format!("allow\n{text}") => profile.add(&project),
            Some(v) if v == format!("deny\n{text}") => {}
            _ => {
                return Err(Untrusted {
                    file,
                    text,
                    project,
                });
            }
        }
        Ok(profile)
    }

    /// Asks whether to use the project file; the start waits for it.
    pub(super) fn ask_agent_trust(&mut self, found: Untrusted, ask: TrustAsk) {
        let p = &found.project;
        let dir = found
            .file
            .parent()
            .map_or_else(String::new, |d| d.display().to_string());
        let mut lines = vec![tr!("agent-project-intro", dir = dir), String::new()];
        if !p.channels.is_empty() {
            lines.push(tr!("agent-project-channels", list = p.channels().join(" ")));
        }
        if !p.env.is_empty() {
            let list: Vec<String> = p.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
            lines.push(tr!("agent-project-env", list = list.join(" ")));
        }
        if !p.args.is_empty() {
            lines.push(tr!("agent-project-args", list = p.args.join(" ")));
        }
        let dangers = p.dangers();
        if !dangers.is_empty() {
            lines.push(String::new());
            lines.push(tr!("agent-project-dangers", list = dangers.join(" ")));
        }
        lines.push(String::new());
        lines.push(tr!("agent-project-question"));
        let dialog = Dialog::message(
            &tr!("agent-project-title"),
            &lines,
            &[
                &tr!("agent-project-apply"),
                &tr!("agent-project-skip"),
                &tr!("MCancel"),
            ],
            !dangers.is_empty(),
        );
        self.overlays.push(Overlay::Dialog {
            dialog,
            purpose: Purpose::AgentTrust(Box::new(ask)),
        });
    }

    /// The answer: kept for this text of the file, and the agent starts —
    /// with the file or without it. Cancel: no agent.
    pub(super) fn agent_trust_answered(&mut self, ask: TrustAsk, button: Option<usize>) {
        let word = match button {
            Some(0) => "allow",
            Some(1) => "deny",
            _ => return,
        };
        self.store
            .set_meta(&trust_key(&ask.file), &format!("{word}\n{}", ask.text));
        self.launch_agent(ask.cwd, ask.launch, &ask.extra, ask.cols, ask.rows);
    }
}
