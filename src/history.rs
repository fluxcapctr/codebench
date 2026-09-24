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

/// A task name short enough to remember the ask by: at most five words,
/// minus openers like "can you" and trailing punctuation.
pub fn task_name(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    let mut words: Vec<&str> = line.split_whitespace().collect();
    const OPENERS: &[&[&str]] = &[
        &["can", "you"], &["could", "you"], &["would", "you"], &["help", "me"],
        &["i", "want", "to"], &["i", "need", "to"], &["i'd", "like", "to"], &["let's"], &["please"], &["hey"],
    ];
    'strip: loop {
        for o in OPENERS {
            let hit = words.len() > o.len()
                && words.iter().zip(o.iter()).all(|(w, o)| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'').eq_ignore_ascii_case(o));
            if hit {
                words.drain(..o.len());
                continue 'strip;
            }
        }
        break;
    }
    words.truncate(5);
    let name = words.join(" ");
    let name = name.trim_end_matches(|c: char| !c.is_alphanumeric());
    let mut chars = name.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
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

/// Whether a task title is one Codebench made up ("task 3") rather than one
/// someone chose, so it may be replaced by a better one.
pub fn is_placeholder(title: &str) -> bool {
    title.strip_prefix("task ").is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// A title for a Claude conversation: the name it was given, else the one
/// Claude Code generated, else its first prompt.
pub fn claude_title(path: &Path) -> Option<String> {
    claude_named(path)
        .or_else(|| claude_first_prompt(&read_head(path, 256 * 1024)).map(|t| clean_prompt(&t)))
        .map(|t| shorten(&t))
}

/// The name a Claude conversation was given, or the one Claude Code made.
fn claude_named(path: &Path) -> Option<String> {
    let tail = read_tail(path, 64 * 1024);
    tail.lines().rev().find_map(|line| {
        let v: serde_json::Value = serde_json::from_str(line).ok()?;
        let custom = v["customTitle"].as_str().filter(|t| !is_placeholder(t));
        custom.or(v["aiTitle"].as_str()).map(str::to_string)
    })
}

/// A prompt without the "[Image #1]" and "[Pasted text #1 +20 lines]"
/// stand-ins agents put in place of attachments.
pub fn clean_prompt(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('[') {
        let tail = &rest[start..];
        let attachment = tail.starts_with("[Image") || tail.starts_with("[Pasted");
        match tail.find(']').filter(|_| attachment) {
            Some(end) => {
                out.push_str(&rest[..start]);
                rest = &tail[end + 1..];
            }
            None => {
                out.push_str(&rest[..=start]);
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A better name for a task that still has a made-up one ("task 3") or one
/// that is just the start of its first prompt: the name Claude gave it, else
/// a short summary of the ask. None when there is nothing to go on yet or
/// the task was named by hand.
pub fn auto_name(s: &crate::store::Session) -> Option<String> {
    let (named, prompt) = match s.agent.as_str() {
        "claude" => {
            let path = crate::agents::claude_transcript(&s.id)?;
            (claude_named(&path), claude_first_prompt(&read_head(&path, 256 * 1024)))
        }
        "codex" => {
            let path = crate::agents::codex_rollout(s)?;
            (None, codex_first_prompt(&read_head(&path, 256 * 1024)))
        }
        _ => return None,
    };
    let prompt = prompt?;
    let clean = clean_prompt(&prompt);
    let automatic = is_placeholder(&s.title) || s.title == task_name(&prompt) || s.title == task_name(&clean);
    if !automatic || clean.is_empty() {
        return None;
    }
    if let Some(name) = named {
        return Some(task_name(&name));
    }
    Some(summarize(&clean).unwrap_or_else(|| task_name(&clean)))
}

/// Asks a small, fast Claude model for a title of five words or fewer.
/// One at a time, so a burst of tasks does not start a crowd of them.
fn summarize(prompt: &str) -> Option<String> {
    use std::process::{Command, Stdio};
    static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _turn = ONE_AT_A_TIME.lock().ok()?;
    let ask: String = prompt.chars().take(2000).collect();
    let mut child = Command::new("claude")
        .args(["-p", "--model", "haiku", "--no-session-persistence", "--tools", "", "--strict-mcp-config", "--setting-sources", ""])
        .arg(format!(
            "Name this coding task in at most 5 words, like a short title that says what the task is about. \
             Reply with only the title, no quotes and no punctuation at the end.\n\nTask: {ask}"
        ))
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
    loop {
        match child.try_wait().ok()? {
            Some(status) if status.success() => break,
            Some(_) => return None,
            None if std::time::Instant::now() > deadline => {
                let _ = child.kill();
                return None;
            }
            None => std::thread::sleep(std::time::Duration::from_millis(200)),
        }
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    let name = task_name(out.trim().trim_matches(['"', '\'', '*']));
    (!name.is_empty()).then_some(name)
}

fn codex_first_prompt(head: &str) -> Option<String> {
    head.lines().find_map(|line| {
        let v: serde_json::Value = serde_json::from_str(line).ok()?;
        let p = &v["payload"];
        if v["type"] != "response_item" || p["role"] != "user" {
            return None;
        }
        let text = p["content"][0]["text"].as_str()?;
        let t = text.trim_start();
        // Skip the environment and AGENTS.md blocks Codex sends first.
        (!t.is_empty() && !t.starts_with('<') && !t.starts_with("# AGENTS.md")).then(|| text.to_string())
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
        let Some(title) = claude_title(&path) else { continue };
        out.push(Past { agent: "claude", id, title, updated: mtime(&path) });
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
            let prompt = codex_first_prompt(&head)?;
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

    #[test]
    fn only_made_up_titles_are_placeholders() {
        assert!(is_placeholder("task 3"));
        assert!(is_placeholder("task 12"));
        assert!(!is_placeholder("task"));
        assert!(!is_placeholder("task list"));
        assert!(!is_placeholder("fix task 3"));
    }

    #[test]
    fn task_names_are_five_words_at_most() {
        assert_eq!(task_name("Can you help me fix the login bug on the settings page?"), "Fix the login bug on");
        assert_eq!(task_name("please add dark mode."), "Add dark mode");
        assert_eq!(task_name("Chromium crash investigation"), "Chromium crash investigation");
        assert_eq!(task_name("hey"), "Hey");
    }

    #[test]
    fn attachments_leave_prompts() {
        assert_eq!(clean_prompt("[Image #1] look at this [Pasted text #2 +40 lines] sidebar"), "look at this sidebar");
        assert_eq!(clean_prompt("keep [x] and [links]"), "keep [x] and [links]");
    }
}
