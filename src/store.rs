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
    /// The workflow file this task is a run of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<PathBuf>,
    /// Set when the task works in its own git worktree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<Worktree>,
    /// Codex's own id for an imported conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_id: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: String,
    /// The branch it was made from and merges back into.
    pub base: String,
}

impl Session {
    /// Where the task's agent runs.
    pub fn dir(&self, project: &Project) -> PathBuf {
        self.worktree.as_ref().map_or_else(|| project.path.clone(), |w| w.path.clone())
    }
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
    /// Give this project's Claude and Codex tasks a browser to drive.
    #[serde(default)]
    pub browser: bool,
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
    /// Why state.json could not be read. While set, saving is refused so the
    /// file is not replaced by this empty state.
    #[serde(skip)]
    pub load_error: Option<String>,
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

/// Creates Codebench's folders readable by you only. Anything that can
/// write to the request or status folders can prompt your agents.
pub fn make_private_dirs() {
    use std::os::unix::fs::PermissionsExt;
    for dir in [config_dir(), cache_dir(), data_dir()] {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

static SAVE_ERROR: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// The last failed `save`, if one has not been reported yet.
pub fn take_save_error() -> Option<String> {
    SAVE_ERROR.lock().unwrap_or_else(|p| p.into_inner()).take()
}

impl State {
    fn file() -> PathBuf {
        config_dir().join("state.json")
    }

    /// The saved state. A missing file is a fresh start; one that cannot be
    /// read or parsed is an error, after a copy of it has been kept.
    pub fn try_load() -> Result<State, String> {
        Self::read(&Self::file())
    }

    fn read(file: &Path) -> Result<State, String> {
        let bytes = match std::fs::read(file) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(State::default()),
            Err(e) => return Err(format!("could not read {}: {e}", file.display())),
        };
        serde_json::from_slice(&bytes).map_err(|e| {
            let err = format!("{} is not valid: {e}", file.display());
            match Self::keep_copy(file, &bytes) {
                Ok(copy) => format!("{err}. A copy is kept at {}", copy.display()),
                Err(e) => format!("{err}. Could not keep a copy: {e}"),
            }
        })
    }

    /// Like `try_load`, but an unreadable file gives an empty state that
    /// refuses to save over it; `load_error` says why.
    pub fn load() -> State {
        Self::try_load().unwrap_or_else(|e| State { load_error: Some(e), ..State::default() })
    }

    /// Copies unparseable state aside once per version of the file.
    fn keep_copy(file: &Path, bytes: &[u8]) -> Result<PathBuf, String> {
        let modified = std::fs::metadata(file)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(now(), |d| d.as_secs());
        let copy = file.with_extension(format!("json.corrupt-{modified}"));
        if std::fs::read(&copy).is_ok_and(|b| b == bytes) {
            return Ok(copy);
        }
        std::fs::write(&copy, bytes).map_err(|e| format!("{}: {e}", copy.display()))?;
        Ok(copy)
    }

    /// Accepts starting over after a failed load; the kept copy stays.
    pub fn start_fresh(&mut self) {
        self.load_error = None;
    }

    /// Writes through a temp file so a crash mid-write never truncates state.
    pub fn try_save(&self) -> Result<(), String> {
        self.write(&Self::file())
    }

    fn write(&self, file: &Path) -> Result<(), String> {
        if let Some(err) = &self.load_error {
            return Err(format!("not saving over state that failed to load: {err}"));
        }
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let tmp = file.with_extension(format!("json.{}.tmp", std::process::id()));
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let result = std::fs::write(&tmp, json).and_then(|_| std::fs::rename(&tmp, file));
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result.map_err(|e| format!("could not save {}: {e}", file.display()))
    }

    /// `try_save` for callers with nowhere to show an error; the window
    /// picks the error up with `take_save_error`.
    pub fn save(&self) {
        if let Err(e) = self.try_save() {
            eprintln!("codebench: {e}");
            *SAVE_ERROR.lock().unwrap_or_else(|p| p.into_inner()) = Some(e);
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

    /// Adds a run of `workflow` to a project, archiving its earlier runs so
    /// only the latest shows.
    pub fn add_run(&mut self, pid: &str, session: Session) {
        let Some(project) = self.project_mut(pid) else { return };
        let same = |p: &Option<PathBuf>| p.as_ref().map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()));
        let this = same(&session.workflow);
        for s in project.sessions.iter_mut() {
            if this.is_some() && same(&s.workflow) == this {
                s.archived = true;
            }
        }
        project.sessions.push(session);
        self.save();
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
            browser: false,
        });
        self.save();
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_state_is_kept_and_never_saved_over() {
        let dir = std::env::temp_dir().join(format!("cb-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("state.json");
        let state = State::read(&file).unwrap();
        assert!(state.projects.is_empty() && state.load_error.is_none());

        let broken = "{broken but potentially recoverable";
        std::fs::write(&file, broken).unwrap();
        assert!(State::read(&file).is_err());
        let mut state = State::read(&file).unwrap_or_else(|e| State { load_error: Some(e), ..State::default() });
        state.projects.push(Project {
            id: "p".into(),
            name: "p".into(),
            path: dir.clone(),
            sessions: Vec::new(),
            notes: None,
            browser: false,
        });
        assert!(state.write(&file).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), broken);
        let copies: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("state.json.corrupt-"))
            .collect();
        assert_eq!(copies.len(), 1, "one copy however often it is loaded");
        assert_eq!(std::fs::read_to_string(copies[0].path()).unwrap(), broken);

        state.start_fresh();
        state.write(&file).unwrap();
        assert_eq!(State::read(&file).unwrap().projects.len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unreadable_state_is_an_error() {
        let dir = std::env::temp_dir().join(format!("cb-state-dir-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("state.json")).unwrap();
        assert!(State::read(&dir.join("state.json")).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
