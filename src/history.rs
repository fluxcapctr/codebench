//! Past Claude Code and Codex conversations for a project folder, so they can
//! be brought into Codebench as resumable tasks.

use crate::store::home;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

#[derive(Clone, Debug)]
pub struct Past {
    pub agent: &'static str,
    /// Claude session id, or Codex session id.
    pub id: String,
    pub title: String,
    pub updated: u64,
}

fn read_head(path: &Path, bytes: u64) -> String {
    let mut buf = Vec::new();
    if let Ok(f) = std::fs::File::open(path) {
        let _ = f.take(bytes).read_to_end(&mut buf);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

fn read_tail(path: &Path, bytes: u64) -> String {
    let mut buf = Vec::new();
    if let Ok(mut f) = std::fs::File::open(path) {
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if f.seek(SeekFrom::Start(len.saturating_sub(bytes))).is_ok() {
            let _ = f.read_to_end(&mut buf);
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

fn mtime(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

fn shorten(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    if line.chars().count() > 60 {
        line.chars().take(59).chain(['…']).collect()
    } else {
        line.to_string()
    }
}

/// Claude Code keeps a project's transcripts in a folder named after its
/// path with every other character turned into '-'.
fn claude_dir(project: &Path) -> PathBuf {
    let name: String = project
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".claude"))
        .join("projects")
        .join(name)
}

fn claude_first_prompt(head: &str) -> Option<String> {
    head.lines().find_map(|line| {
        let v: serde_json::Value = serde_json::from_str(line).ok()?;
        if v["type"] != "user" || v["isMeta"] == true || v["isSidechain"] == true {
            return None;
        }
        let text = match &v["message"]["content"] {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Array(blocks) => blocks
                .iter()
                .find(|b| b["type"] == "text")
                .and_then(|b| b["text"].as_str())?
                .to_string(),
            _ => return None,
        };
        (!text.trim().is_empty() && !text.trim_start().starts_with('<')).then_some(text)
    })
}

pub fn claude(project: &Path) -> Vec<Past> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(claude_dir(project)).into_iter().flatten().flatten() {
        let path = entry.path();
        let Some(id) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else { continue };
        if path.extension().is_none_or(|e| e != "jsonl") || id.starts_with("agent-") {
            continue;
        }
        let tail = read_tail(&path, 64 * 1024);
        let named = tail.lines().rev().find_map(|line| {
            let v: serde_json::Value = serde_json::from_str(line).ok()?;
            v["customTitle"].as_str().or(v["aiTitle"].as_str()).map(str::to_string)
        });
        let Some(title) = named.or_else(|| claude_first_prompt(&read_head(&path, 256 * 1024))) else { continue };
        out.push(Past { agent: "claude", id, title: shorten(&title), updated: mtime(&path) });
    }
    out
}

pub fn codex(project: &Path) -> Vec<Past> {
    let root = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".codex"))
        .join("sessions");
    let mut files = Vec::new();
    let mut dirs = vec![root];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "jsonl") {
                files.push((mtime(&path), path));
            }
        }
    }
    files.sort_by(|a, b| b.0.cmp(&a.0));
    let want = project.to_string_lossy();
    files
        .into_iter()
        .take(400)
        .filter_map(|(updated, path)| {
            let head = read_head(&path, 256 * 1024);
            let meta: serde_json::Value = serde_json::from_str(head.lines().next()?).ok()?;
            // Conversations Codebench started are tasks already.
            if meta["payload"]["cwd"].as_str()? != want || head.contains("Codebench task id: ") {
                return None;
            }
            let id = meta["payload"]["id"].as_str()?.to_string();
            let prompt = head.lines().find_map(|line| {
                let v: serde_json::Value = serde_json::from_str(line).ok()?;
                let p = &v["payload"];
                if v["type"] != "response_item" || p["role"] != "user" {
                    return None;
                }
                let text = p["content"][0]["text"].as_str()?;
                (!text.trim_start().starts_with('<')).then(|| text.to_string())
            })?;
            Some(Past { agent: "codex", id, title: shorten(&prompt), updated })
        })
        .collect()
}

/// Past conversations for a project, newest first, minus those listed in
/// `known`.
pub fn find(project: &Path, known: &[String]) -> Vec<Past> {
    let mut all = claude(project);
    all.extend(codex(project));
    all.retain(|p| !known.contains(&p.id));
    all.sort_by(|a, b| b.updated.cmp(&a.updated));
    all.truncate(60);
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_folder_names_match_claude_code() {
        let dir = claude_dir(Path::new("/home/e/code/my.app"));
        assert!(dir.ends_with("projects/-home-e-code-my-app"));
    }

    #[test]
    fn titles_are_one_short_line() {
        assert_eq!(shorten("\n  fix the login bug\nmore"), "fix the login bug");
        assert_eq!(shorten(&"x".repeat(80)).chars().count(), 60);
    }
}
