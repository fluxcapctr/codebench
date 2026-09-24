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

/// Copies the brief and handoffs into a newly linked folder, without
/// overwriting anything already there.
pub fn carry_over(from: &Path, to: &Path) {
    let _ = std::fs::create_dir_all(to.join("handoffs"));
    for rel in ["brief.md"] {
        let (src, dst) = (from.join(rel), to.join(rel));
        if src.is_file() && !dst.exists() {
            let _ = std::fs::copy(src, dst);
        }
    }
    for entry in std::fs::read_dir(from.join("handoffs")).into_iter().flatten().flatten() {
        let dst = to.join("handoffs").join(entry.file_name());
        if !dst.exists() {
            let _ = std::fs::copy(entry.path(), dst);
        }
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
        std::fs::create_dir_all(from.join("handoffs")).unwrap();
        std::fs::write(from.join("brief.md"), "brief").unwrap();
        std::fs::write(from.join("handoffs/a.md"), "a").unwrap();

        carry_over(&from, &to);
        assert_eq!(std::fs::read_to_string(to.join("brief.md")).unwrap(), "brief");
        assert!(to.join("handoffs/a.md").is_file());
        assert_eq!(vault_root(&to), Some(vault));
        assert_eq!(vault_root(&from), None);
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
