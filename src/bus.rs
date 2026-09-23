//! Requests from agents (through `codebench mcp`) to the running app, passed
//! as small JSON files in a watched folder. Also works while the app is
//! closed: requests wait until it starts.

use crate::store::cache_dir;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Request {
    /// Deliver a message into another task's conversation.
    Send { from: String, to: String, text: String },
    /// Create a task in the sender's project and start it with a prompt.
    StartTask { from: String, title: String, agent: String, prompt: String },
}

pub fn dir() -> PathBuf {
    cache_dir().join("requests")
}

/// Writes through a temp name so the app never reads a half-written file.
pub fn post(req: &Request) -> std::io::Result<()> {
    let dir = dir();
    std::fs::create_dir_all(&dir)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let name = format!("{nanos}-{}", std::process::id());
    let tmp = dir.join(format!("{name}.tmp"));
    std::fs::write(&tmp, serde_json::to_vec(req)?)?;
    std::fs::rename(tmp, dir.join(format!("{name}.json")))
}

/// Removes and returns all waiting requests, oldest first.
pub fn take_all() -> Vec<Request> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir())
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    files
        .into_iter()
        .filter_map(|path| {
            let req = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok());
            let _ = std::fs::remove_file(&path);
            req
        })
        .collect()
}

/// Live task statuses, written by the app for the MCP server to read.
pub fn snapshot_path() -> PathBuf {
    cache_dir().join("tasks.json")
}

pub fn write_snapshot(statuses: &HashMap<String, &'static str>) {
    let path = snapshot_path();
    let tmp = path.with_extension("json.tmp");
    if let Ok(json) = serde_json::to_vec(statuses)
        && std::fs::write(&tmp, json).is_ok()
    {
        let _ = std::fs::rename(tmp, path);
    }
}

pub fn read_snapshot() -> HashMap<String, String> {
    std::fs::read(snapshot_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}
