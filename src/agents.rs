//! The agent CLIs Codebench can run, and how to launch each one so it reports
//! its status back.
//!
//! Every agent runs as its normal interactive TUI inside a real PTY. Status
//! comes back through the file named by `$CODEBENCH_STATUS`, which the agent's
//! own hook system writes to.

use crate::store::{Session, home};
use std::path::Path;

pub struct Agent {
    pub id: &'static str,
    pub label: &'static str,
    pub program: &'static str,
}

pub const AGENTS: &[Agent] = &[
    Agent { id: "claude", label: "Claude Code", program: "claude" },
    Agent { id: "codex", label: "Codex", program: "codex" },
    Agent { id: "gemini", label: "Gemini CLI", program: "gemini" },
    Agent { id: "grok", label: "Grok Build", program: "grok" },
    Agent { id: "opencode", label: "OpenCode", program: "opencode" },
    Agent { id: "agy", label: "Antigravity", program: "agy" },
    Agent { id: "shell", label: "Shell", program: "" },
];

pub fn get(id: &str) -> Option<&'static Agent> {
    AGENTS.iter().find(|a| a.id == id)
}

fn on_path(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&path).any(|dir| dir.join(program).is_file())
}

/// Agents whose CLI is installed, plus the plain shell.
pub fn installed() -> Vec<&'static Agent> {
    AGENTS
        .iter()
        .filter(|a| a.program.is_empty() || on_path(a.program))
        .collect()
}

pub fn shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into())
}

fn write_status(state: &str) -> String {
    format!("printf {state} > \"$CODEBENCH_STATUS\"")
}

/// Claude Code hooks layered on top of the user's own settings via
/// `--settings`. They only touch the status file.
fn claude_settings() -> String {
    let hook = |state: &str| serde_json::json!([{ "hooks": [{ "type": "command", "command": write_status(state) }] }]);
    let mut waiting = hook("waiting");
    waiting[0]["matcher"] = "permission_prompt|elicitation_dialog".into();
    serde_json::json!({
        "hooks": {
            "UserPromptSubmit": hook("working"),
            "PreToolUse": hook("working"),
            "Notification": waiting,
            "Stop": hook("done"),
        }
    })
    .to_string()
}

/// True when Claude Code has a transcript for this session id, meaning
/// `--resume` will find it.
fn claude_transcript_exists(id: &str) -> bool {
    let root = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(Into::into)
        .unwrap_or_else(|| home().join(".claude"))
        .join("projects");
    let file = format!("{id}.jsonl");
    std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .any(|dir| dir.path().join(&file).is_file())
}

/// Builds the argv for a session. Resumes the previous conversation when there
/// is one to resume.
pub fn argv(session: &Session) -> Vec<String> {
    let s = |v: &str| v.to_string();
    match session.agent.as_str() {
        "claude" => {
            let mut argv = vec![s("claude")];
            if claude_transcript_exists(&session.id) {
                argv.extend([s("--resume"), session.id.clone()]);
            } else {
                argv.extend([s("--session-id"), session.id.clone(), s("-n"), session.title.clone()]);
            }
            argv.extend([s("--settings"), claude_settings()]);
            argv
        }
        "codex" => {
            let notify = serde_json::json!(["sh", "-c", write_status("done")]);
            let mut argv = vec![s("codex"), s("-c"), format!("notify={notify}")];
            if session.launched {
                argv.push(s("resume"));
            }
            argv
        }
        "shell" => vec![shell()],
        other => vec![get(other).map(|a| a.program).unwrap_or(other).to_string()],
    }
}

/// Whether the agent reports "done" on its own, so Codebench can mark it as
/// working when you send it a prompt.
pub fn reports_done(agent: &str) -> bool {
    matches!(agent, "codex")
}

pub fn status_file(dir: &Path, session: &str) -> std::path::PathBuf {
    dir.join(session)
}
