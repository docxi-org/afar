//! What the agent gets in a folder beyond `[agent]`: channels, environment
//! and arguments from the user's `[[agent.folders]]` (config.toml) and from
//! the project's own `afar.toml` (its `[agent]` section, the nearest one up
//! from the agent's folder). A project file comes with the project, so it
//! is used only when the user trusts that very text (`App::agent_profile`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::AgentFolder;

/// The project file's name.
pub const FILE: &str = "afar.toml";

/// What a folder adds to the agent's start.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    /// Development channels (`server:name`; a bare name is a server).
    pub channels: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// Arguments of `claude` (after `[agent] args`).
    pub args: Vec<String>,
}

#[derive(Deserialize)]
struct ProjectFile {
    #[serde(default)]
    agent: Profile,
}

impl Profile {
    pub fn is_empty(&self) -> bool {
        self.channels.is_empty() && self.env.is_empty() && self.args.is_empty()
    }

    /// Adds `other`: its channels and arguments after these, its variables
    /// over these.
    pub fn add(&mut self, other: &Profile) {
        for c in other.channels() {
            if !self.channels().contains(&c) {
                self.channels.push(c);
            }
        }
        self.env
            .extend(other.env.iter().map(|(k, v)| (k.clone(), v.clone())));
        self.args.extend(other.args.iter().cloned());
    }

    /// The channels as `claude` takes them.
    pub fn channels(&self) -> Vec<String> {
        self.channels
            .iter()
            .map(|c| c.trim())
            .filter(|c| !c.is_empty())
            .map(|c| {
                if c.contains(':') {
                    c.to_string()
                } else {
                    format!("server:{c}")
                }
            })
            .collect()
    }

    /// What deserves a warning: arguments that loosen the agent's
    /// permissions, variables that change which programs run or where the
    /// requests go.
    pub fn dangers(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .args
            .iter()
            .filter(|a| {
                let a = a.to_ascii_lowercase();
                a.contains("dangerously") || a.contains("bypasspermissions")
            })
            .cloned()
            .collect();
        out.extend(
            self.env
                .keys()
                .filter(|k| {
                    let k = k.to_ascii_uppercase();
                    ["PATH", "PATHEXT", "COMSPEC", "NODE_OPTIONS", "NODE_PATH"]
                        .contains(&k.as_str())
                        || k.starts_with("ANTHROPIC_")
                        || k.starts_with("CLAUDE_")
                })
                .cloned(),
        );
        out
    }
}

/// The `[agent]` section of a project file's text.
pub fn parse(text: &str) -> Result<Profile, String> {
    toml::from_str::<ProjectFile>(text)
        .map(|f| f.agent)
        .map_err(|e| e.to_string())
}

/// The nearest project file: in `dir` or above it.
pub fn find(dir: &Path) -> Option<PathBuf> {
    dir.ancestors().map(|d| d.join(FILE)).find(|p| p.is_file())
}

/// The user's profiles for `dir`: those of its folder and the folders
/// above it, the outer ones first.
pub fn for_folder(folders: &[AgentFolder], dir: &Path) -> Profile {
    let mut matching: Vec<&AgentFolder> = folders
        .iter()
        .filter(|f| !f.path.as_os_str().is_empty() && is_within(dir, &f.path))
        .collect();
    matching.sort_by_key(|f| f.path.components().count());
    let mut out = Profile::default();
    for f in matching {
        out.add(&f.profile);
    }
    out
}

/// `dir` is `root` or inside it (letter case aside on Windows).
fn is_within(dir: &Path, root: &Path) -> bool {
    let norm = |p: &Path| {
        let s = p.to_string_lossy().replace('/', "\\");
        let s = s.trim_end_matches('\\').to_string();
        if cfg!(windows) { s.to_lowercase() } else { s }
    };
    let (d, r) = (norm(dir), norm(root));
    d == r || d.starts_with(&format!("{r}\\"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_of_a_folder() {
        let p = parse(
            "[agent]\nchannels = [\"astro-events\"]\nenv = { MCP_PROTOCOL_NEGOTIATION = \"legacy\" }\n",
        )
        .unwrap();
        assert_eq!(p.channels(), vec!["server:astro-events"]);
        assert_eq!(p.env["MCP_PROTOCOL_NEGOTIATION"], "legacy");
        assert!(p.dangers().is_empty());
        assert!(parse("# nothing for the agent\n").unwrap().is_empty());
        assert!(parse("[agent]\nchannels = 1\n").is_err());

        let folder = |path: &str, arg: &str| AgentFolder {
            path: PathBuf::from(path),
            profile: Profile {
                args: vec![arg.into()],
                channels: vec!["server:x".into()],
                ..Default::default()
            },
        };
        let folders = [
            folder(r"F:\AGI\astronomer\apps", "inner"),
            folder(r"f:\agi\Astronomer", "outer"),
        ];
        let p = for_folder(&folders, Path::new(r"F:\AGI\astronomer\apps\channel"));
        assert_eq!(p.args, vec!["outer", "inner"]);
        assert_eq!(p.channels(), vec!["server:x"]);
        assert!(for_folder(&folders, Path::new(r"F:\AGI\astronomer2")).is_empty());

        let risky = Profile {
            args: vec!["--dangerously-skip-permissions".into()],
            env: [("NODE_OPTIONS".to_string(), "x".to_string())].into(),
            ..Default::default()
        };
        assert_eq!(risky.dangers().len(), 2);
    }
}
