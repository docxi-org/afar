//! What the agent may do through afar (`[agent.permissions]` in
//! config.toml; docs/02-architecture.md, "Политика разрешений"): allow,
//! confirm in a dialog, or deny. The agent's own tools (Bash, Edit) are
//! Claude Code's business.

use tokio::sync::oneshot;

use super::App;
use crate::config::Level;
use crate::journal::Actor;
use crate::mcp::Reply;
use crate::tr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AgentAction {
    Navigate,
    MkDir,
    Copy,
    Move,
    Delete,
    DeletePermanent,
    /// Changing the text of a file open in afar's editor (the buffer; the
    /// user saves it).
    EditBuffer,
}

impl AgentAction {
    /// The setting's name, for the agent's error message.
    fn key(self) -> &'static str {
        match self {
            AgentAction::Navigate => "navigate",
            AgentAction::MkDir => "mkdir",
            AgentAction::Copy => "copy",
            AgentAction::Move => "move",
            AgentAction::Delete => "delete",
            AgentAction::DeletePermanent => "delete_permanent",
            AgentAction::EditBuffer => "edit_buffer",
        }
    }
}

impl App {
    pub(super) fn permission(&self, action: AgentAction) -> Level {
        let p = &self.config.agent.permissions;
        let level = match action {
            AgentAction::Navigate => p.navigate,
            AgentAction::MkDir => p.mkdir,
            AgentAction::Copy => p.copy,
            AgentAction::Move => p.move_,
            AgentAction::Delete => p.delete,
            AgentAction::DeletePermanent => p.delete_permanent,
            AgentAction::EditBuffer => p.edit_buffer,
        };
        // Showing things needs no question, and the buffer is not the disk
        // (the user saves it): "confirm" means allow.
        if matches!(action, AgentAction::Navigate | AgentAction::EditBuffer)
            && level == Level::Confirm
        {
            Level::Allow
        } else {
            level
        }
    }

    /// The agent's answer when `action` is denied.
    pub(super) fn denied(action: AgentAction) -> String {
        format!(
            "denied by the user's afar settings (agent.permissions.{} = \"deny\"); ask the user \
             to do it or to change the setting",
            action.key()
        )
    }

    /// `afar_mkdir`: created at once, or after the user confirms the make-
    /// folder dialog filled in with the agent's names.
    pub(super) fn agent_mkdir_request(
        &mut self,
        side: usize,
        names: Vec<String>,
        reply: oneshot::Sender<Reply>,
    ) {
        match self.permission(AgentAction::MkDir) {
            Level::Deny => {
                let _ = reply.send(Err(Self::denied(AgentAction::MkDir)));
            }
            Level::Allow => {
                let _ = reply.send(self.agent_mkdir(side, &names));
            }
            Level::Confirm => {
                self.set_panels_visible(true);
                self.say(tr!("agent-asks", what = tr!("MMakeFolderTitle")));
                self.open_mkdir_dialog(side, &names, Actor::Agent, Some(reply));
            }
        }
    }
}
