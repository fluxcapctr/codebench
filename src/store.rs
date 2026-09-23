//! Projects and their task sessions, persisted as JSON under
//! `$XDG_CONFIG_HOME/codebench/state.json`.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Session {
    /// UUID. Doubles as the Claude Code session id, so `--resume` finds it.
    pub id: String,
    pub agent: String,
    pub title: String,
    pub created: u64,
    /// Set once the agent has been started at least once, so later launches
    /// resume instead of starting fresh.
    #[serde(default)]
    pub launched: bool,
    /// Sent as the opening prompt on the first launch only (used by handoff).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Handed off to a newer task; hidden from the sidebar unless shown.
    #[serde(default)]
    pub archived: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    #[serde(default)]
    pub sessions: Vec<Session>,
    /// Where the brief and handoffs live. Defaults to Codebench's data dir;
    /// can point into an Obsidian vault.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<PathBuf>,
}

impl Project {
    pub fn notes_dir(&self) -> PathBuf {
        self.notes.clone().unwrap_or_else(|| {
            let short: String = self.id.chars().take(8).collect();
            data_dir().join("notes").join(format!("{}-{short}", self.name))
        })
    }
}

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct State {
    #[serde(default)]
    pub projects: Vec<Project>,
}

/// Agents can hand MCP servers a trimmed environment, so `codebench mcp`
/// receives Codebench's folders explicitly through these variables.
pub const DIR_VARS: [&str; 3] = ["CODEBENCH_CONFIG_DIR", "CODEBENCH_CACHE_DIR", "CODEBENCH_DATA_DIR"];

pub fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(DIR_VARS[0]) {
        return dir.into();
    }
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
        .join("codebench")
}

pub fn cache_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(DIR_VARS[1]) {
        return dir.into();
    }
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".cache"))
        .join("codebench")
}

pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(DIR_VARS[2]) {
        return dir.into();
    }
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/share"))
        .join("codebench")
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/".into())
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl State {
    fn file() -> PathBuf {
        config_dir().join("state.json")
    }

    pub fn load() -> State {
        std::fs::read_to_string(Self::file())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Writes through a temp file so a crash mid-write never truncates state.
    pub fn save(&self) {
        let file = Self::file();
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = file.with_extension("json.tmp");
        if let Ok(json) = serde_json::to_string_pretty(self)
            && std::fs::write(&tmp, json).is_ok()
        {
            let _ = std::fs::rename(&tmp, &file);
        }
    }

    pub fn project(&self, id: &str) -> Option<&Project> {
        self.projects.iter().find(|p| p.id == id)
    }

    pub fn project_mut(&mut self, id: &str) -> Option<&mut Project> {
        self.projects.iter_mut().find(|p| p.id == id)
    }

    /// Returns the project owning a session along with the session.
    pub fn session(&self, sid: &str) -> Option<(&Project, &Session)> {
        self.projects
            .iter()
            .find_map(|p| p.sessions.iter().find(|s| s.id == sid).map(|s| (p, s)))
    }

    pub fn session_mut(&mut self, sid: &str) -> Option<&mut Session> {
        self.projects
            .iter_mut()
            .find_map(|p| p.sessions.iter_mut().find(|s| s.id == sid))
    }

    /// Adds a project for `path`, or returns the existing one's id.
    pub fn add_project(&mut self, path: &Path) -> String {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if let Some(p) = self.projects.iter().find(|p| p.path == path) {
            return p.id.clone();
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let id = uuid::Uuid::new_v4().to_string();
        self.projects.push(Project {
            id: id.clone(),
            name,
            path,
            sessions: Vec::new(),
            notes: None,
        });
        self.save();
        id
    }
}
