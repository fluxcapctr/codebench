//! Per-project notes: a short brief every task reads, and handoff notes that
//! carry a task into a fresh session. Plain markdown, so the folder can live
//! inside an Obsidian vault.

use crate::store::{Project, Session};
use std::path::{Path, PathBuf};

const BRIEF_TEMPLATE: &str = "\
# {name}

What this project is, the stack, conventions, and current goals.
Every Codebench task on this project reads this brief when it starts,
so keep it short and current.
";

fn template(name: &str) -> String {
    BRIEF_TEMPLATE.replace("{name}", name)
}

/// Creates the notes folder with a starter brief if it does not exist yet.
pub fn ensure(project: &Project) -> PathBuf {
    let dir = project.notes_dir();
    let _ = std::fs::create_dir_all(dir.join("handoffs"));
    let brief = dir.join("brief.md");
    if !brief.exists() {
        let _ = std::fs::write(&brief, template(&project.name));
    }
    dir
}

/// The brief, unless it is still the untouched template.
pub fn brief(project: &Project) -> Option<String> {
    let text = std::fs::read_to_string(project.notes_dir().join("brief.md")).ok()?;
    let text = text.trim();
    (!text.is_empty() && text != template(&project.name).trim()).then(|| text.to_string())
}

/// Extra instructions given to every agent that supports them.
pub fn agent_context(project: &Project) -> String {
    let dir = project.notes_dir();
    let mut text = format!(
        "You are running as a task inside Codebench. Project notes live in {}: \
         brief.md is the project brief, handoffs/ holds notes from earlier tasks. \
         When you learn something every future task on this project should know, \
         add it to brief.md and keep that file short. To show the user something \
         visual (a page, chart, diagram or document), use the codebench \
         show_artifact tool.",
        dir.display()
    );
    if let Some(brief) = brief(project) {
        text.push_str("\n\nProject brief:\n\n");
        text.push_str(&brief);
    }
    text
}

fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').chars().take(48).collect()
}

pub fn handoff_path(project: &Project, session: &Session) -> PathBuf {
    let short: String = session.id.chars().take(8).collect();
    project
        .notes_dir()
        .join("handoffs")
        .join(format!("{}-{short}.md", slug(&session.title)))
}

pub fn handoff_request(path: &Path) -> String {
    format!(
        "Write a handoff note to {} so a fresh session can pick up this task: the goal, \
         what is done, the current state, decisions made, open problems, the exact next \
         steps, and the key files. Keep it under 80 lines. When it is written, reply only \
         with: handoff written.",
        path.display()
    )
}

pub fn handoff_resume(path: &Path) -> String {
    format!(
        "Continue the task described in the handoff note {}. Read it first, then carry on \
         with the next steps.",
        path.display()
    )
}

/// "fix login" -> "fix login (2)", "fix login (2)" -> "fix login (3)".
pub fn next_title(title: &str) -> String {
    if let Some(open) = title.rfind(" (")
        && let Some(n) = title[open + 2..].strip_suffix(')').and_then(|n| n.parse::<u32>().ok())
    {
        return format!("{} ({})", &title[..open], n + 1);
    }
    format!("{title} (2)")
}

/// The vault Obsidian last had open, from its own config.
pub fn default_vault() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::store::home().join(".config"))
        .join("obsidian/obsidian.json");
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(config).ok()?).ok()?;
    v["vaults"]
        .as_object()?
        .values()
        .max_by_key(|vault| (vault["open"] == true, vault["ts"].as_u64().unwrap_or(0)))
        .and_then(|vault| vault["path"].as_str())
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
}

/// The Obsidian vault containing `dir`, if any.
pub fn vault_root(dir: &Path) -> Option<PathBuf> {
    dir.ancestors().find(|d| d.join(".obsidian").is_dir()).map(Path::to_path_buf)
}

fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn obsidian_uri(file: &Path) -> String {
    format!("obsidian://open?path={}", url_encode(&file.to_string_lossy()))
}

/// Points the project's notes at `to`, first copying everything from the
/// current folder across and moving workflow run records along. Leaves the
/// project unchanged if the copy fails. Returns what was left behind, as in
/// `carry_over`.
pub fn relink(project: &mut Project, to: &Path) -> Result<Vec<PathBuf>, String> {
    let from = project.notes_dir();
    let left = carry_over(&from, to)?;
    let (old, new) = (from.join("workflows"), to.join("workflows"));
    crate::workflow::rebase_runs(&old, &new);
    for s in project.sessions.iter_mut() {
        if let Some(rel) = s.workflow.as_ref().and_then(|w| w.strip_prefix(&old).ok()) {
            s.workflow = Some(new.join(rel));
        }
    }
    project.notes = Some(to.to_path_buf());
    Ok(left)
}

/// Copies the whole notes folder into a newly linked one without replacing
/// anything already there. Returns the paths, relative to the folder, left
/// behind: names the new folder already uses for something different, and
/// linked folders.
pub fn carry_over(from: &Path, to: &Path) -> Result<Vec<PathBuf>, String> {
    let err = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    std::fs::create_dir_all(to.join("handoffs")).map_err(|e| err(to, e))?;
    let target = to.canonicalize().map_err(|e| err(to, e))?;
    if from.canonicalize().is_ok_and(|f| f == target) {
        return Ok(Vec::new());
    }
    let mut left = Vec::new();
    let mut dirs = vec![PathBuf::new()];
    while let Some(dir) = dirs.pop() {
        let entries = match std::fs::read_dir(from.join(&dir)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && dir.as_os_str().is_empty() => break,
            other => other.map_err(|e| err(&from.join(&dir), e))?,
        };
        for entry in entries {
            let entry = entry.map_err(|e| err(&from.join(&dir), e))?;
            let rel = dir.join(entry.file_name());
            let (src, dst) = (from.join(&rel), to.join(&rel));
            let link = entry.file_type().map_err(|e| err(&src, e))?.is_symlink();
            let Ok(meta) = std::fs::metadata(&src) else {
                left.push(rel);
                continue;
            };
            if meta.is_dir() {
                if link {
                    left.push(rel);
                } else if src.canonicalize().is_ok_and(|c| c != target) {
                    std::fs::create_dir_all(&dst).map_err(|e| err(&dst, e))?;
                    dirs.push(rel);
                }
            } else if dst.symlink_metadata().is_ok() {
                if !same_contents(&src, &dst) {
                    left.push(rel);
                }
            } else {
                crate::artifacts::copy_file_safely(&src, &dst)?;
                if let Ok(modified) = meta.modified()
                    && let Ok(file) = std::fs::File::options().write(true).open(&dst)
                {
                    let _ = file.set_modified(modified);
                }
            }
        }
    }
    left.sort();
    Ok(left)
}

fn same_contents(a: &Path, b: &Path) -> bool {
    match (std::fs::read(a), std::fs::read(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_count_up() {
        assert_eq!(next_title("fix login"), "fix login (2)");
        assert_eq!(next_title("fix login (2)"), "fix login (3)");
        assert_eq!(next_title("a (b)"), "a (b) (2)");
    }

    #[test]
    fn slugs_are_path_safe() {
        assert_eq!(slug("Fix the login/export bug!"), "fix-the-login-export-bug");
    }

    #[test]
    fn finds_vault_and_carries_notes_over() {
        let root = std::env::temp_dir().join(format!("cb-notes-{}", std::process::id()));
        let vault = root.join("vault");
        let (from, to) = (root.join("old"), vault.join("Projects/app"));
        std::fs::create_dir_all(vault.join(".obsidian")).unwrap();
        let files = ["brief.md", "handoffs/a.md", "custom.md", "workflows/check.md", "runs/result.md", "artifacts/chart.html"];
        for rel in files {
            std::fs::create_dir_all(from.join(rel).parent().unwrap()).unwrap();
            std::fs::write(from.join(rel), format!("old {rel}")).unwrap();
        }
        std::fs::create_dir_all(to.join("runs")).unwrap();
        std::fs::write(to.join("runs/result.md"), "theirs").unwrap();
        std::fs::write(to.join("custom.md"), "old custom.md").unwrap();

        assert_eq!(carry_over(&from, &to).unwrap(), vec![PathBuf::from("runs/result.md")]);
        for rel in files.iter().filter(|r| **r != "runs/result.md") {
            assert_eq!(std::fs::read_to_string(to.join(rel)).unwrap(), format!("old {rel}"));
        }
        assert_eq!(std::fs::read_to_string(to.join("runs/result.md")).unwrap(), "theirs");
        assert_eq!(std::fs::read_to_string(from.join("brief.md")).unwrap(), "old brief.md");
        assert_eq!(vault_root(&to), Some(vault));
        assert_eq!(vault_root(&from), None);

        // A new folder inside the old one is not copied into itself.
        let inner = from.join("inner");
        assert!(carry_over(&from, &inner).unwrap().is_empty());
        assert!(inner.join("workflows/check.md").is_file() && !inner.join("inner").exists());
        assert!(carry_over(&from, &from).unwrap().is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn relinking_keeps_workflow_runs() {
        let root = std::env::temp_dir().join(format!("cb-relink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (from, to) = (root.join("old"), root.join("new"));
        std::fs::create_dir_all(from.join("workflows")).unwrap();
        std::fs::write(from.join("workflows/check.md"), "Do it.").unwrap();
        let mut project = Project {
            id: "p".into(),
            name: "p".into(),
            path: root.clone(),
            sessions: vec![Session {
                id: "s".into(),
                agent: "claude".into(),
                title: "check".into(),
                created: 0,
                launched: true,
                prompt: None,
                archived: false,
                workflow: Some(from.join("workflows/check.md")),
                worktree: None,
                codex_id: None,
            }],
            notes: Some(from.clone()),
            browser: false,
        };
        relink(&mut project, &to).unwrap();
        assert_eq!(project.notes_dir(), to);
        assert_eq!(project.sessions[0].workflow.as_deref(), Some(to.join("workflows/check.md").as_path()));
        assert!(to.join("workflows/check.md").is_file());

        // A failed copy leaves the project where it was.
        let blocked = root.join("file");
        std::fs::write(&blocked, "").unwrap();
        assert!(relink(&mut project, &blocked.join("notes")).is_err());
        assert_eq!(project.notes_dir(), to);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn uri_encodes_spaces() {
        assert_eq!(
            obsidian_uri(Path::new("/home/e/My Vault/a.md")),
            "obsidian://open?path=/home/e/My%20Vault/a.md"
        );
    }
}
