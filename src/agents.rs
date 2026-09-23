//! The agent CLIs Codebench can run, and how to launch each one so it reports
//! its status back.
//!
//! Every agent runs as its normal interactive TUI inside a real PTY. Status
//! comes back through the file named by `$CODEBENCH_STATUS`, which the agent's
//! own hook system writes to.

use crate::notes;
use crate::store::{self, Project, Session, home};
use std::path::{Path, PathBuf};

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

pub fn on_path(program: &str) -> bool {
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

/// Claude Code's transcript for this session id, if it has written one.
/// Its presence means `--resume` will find the conversation.
pub fn claude_transcript(id: &str) -> Option<PathBuf> {
    let root = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(Into::into)
        .unwrap_or_else(|| home().join(".claude"))
        .join("projects");
    let file = format!("{id}.jsonl");
    std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|dir| dir.path().join(&file))
        .find(|path| path.is_file())
}

/// How an agent should launch `codebench mcp` for this session: the
/// command, its arguments, and Codebench's folders as environment.
fn mcp_server(session: &Session) -> (String, Vec<String>, serde_json::Map<String, serde_json::Value>) {
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "codebench".into());
    let args = vec!["mcp".to_string(), "--session".to_string(), session.id.clone()];
    let dirs = [store::config_dir(), store::cache_dir(), store::data_dir()];
    let env = store::DIR_VARS
        .iter()
        .zip(dirs)
        .map(|(k, v)| (k.to_string(), v.to_string_lossy().into_owned().into()))
        .collect();
    (exe, args, env)
}

/// Builds the argv for a session. Resumes the previous conversation when there
/// is one; otherwise starts fresh. The opening prompt is `extra` if given,
/// else the session's own (fresh starts only).
pub fn argv(session: &Session, project: &Project, extra: Option<&str>) -> Vec<String> {
    let s = |v: &str| v.to_string();
    let context = notes::agent_context(project);
    let (mcp_cmd, mcp_args, mcp_env) = mcp_server(session);
    match session.agent.as_str() {
        "claude" => {
            let mut argv = vec![s("claude")];
            let resume = claude_transcript(&session.id).is_some();
            if resume {
                argv.extend([s("--resume"), session.id.clone()]);
            } else {
                argv.extend([s("--session-id"), session.id.clone(), s("-n"), session.title.clone()]);
            }
            let mcp = serde_json::json!({
                "mcpServers": { "codebench": { "type": "stdio", "command": mcp_cmd, "args": mcp_args, "env": mcp_env } }
            });
            argv.extend([
                s("--settings"),
                claude_settings(),
                s("--mcp-config"),
                mcp.to_string(),
                s("--add-dir"),
                project.notes_dir().to_string_lossy().into_owned(),
                s("--append-system-prompt"),
                context,
            ]);
            let prompt = extra.or(if resume { None } else { session.prompt.as_deref() });
            if let Some(prompt) = prompt {
                // `--add-dir` and `--mcp-config` take several values, so end
                // options first.
                argv.extend([s("--"), prompt.to_string()]);
            }
            argv
        }
        "codex" => {
            let notify = serde_json::json!(["sh", "-c", write_status("done")]);
            let mut argv = vec![
                s("codex"),
                s("-c"),
                format!("notify={notify}"),
                s("-c"),
                // The task id doubles as a marker to find this conversation
                // again in Codex's session files.
                format!(
                    "developer_instructions={}",
                    serde_json::Value::String(format!("{context}\n\n{}", codex_marker(&session.id)))
                ),
                s("-c"),
                format!("mcp_servers.codebench.command={}", serde_json::Value::String(mcp_cmd)),
                s("-c"),
                format!("mcp_servers.codebench.args={}", serde_json::json!(mcp_args)),
                s("-c"),
                format!("mcp_servers.codebench.env={}", toml_inline_table(&mcp_env)),
            ];
            // No `--add-dir` for Codex: under a read-only sandbox it refuses
            // the flag and exits. It can still read the notes folder.
            match codex_session(session) {
                Some(id) => argv.extend([s("resume"), id]),
                None => argv.extend(extra.or(session.prompt.as_deref()).map(str::to_string)),
            }
            argv
        }
        "shell" => vec![shell()],
        other => vec![get(other).map(|a| a.program).unwrap_or(other).to_string()],
    }
}

fn codex_marker(task: &str) -> String {
    format!("Codebench task id: {task}")
}

/// Finds this task's Codex conversation by the marker in its instructions,
/// looking only at session files written since the task was created.
fn codex_session(session: &Session) -> Option<String> {
    use std::io::Read;
    let root = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".codex"))
        .join("sessions");
    let since = std::time::UNIX_EPOCH + std::time::Duration::from_secs(session.created.saturating_sub(60));
    let mut files = Vec::new();
    let mut dirs = vec![root];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "jsonl")
                && let Ok(modified) = meta.modified()
                && modified >= since
            {
                files.push((modified, path));
            }
        }
    }
    files.sort_by(|a, b| b.0.cmp(&a.0));
    let marker = codex_marker(&session.id);
    files.into_iter().find_map(|(_, path)| {
        let mut head = Vec::new();
        std::fs::File::open(&path).ok()?.take(512 * 1024).read_to_end(&mut head).ok()?;
        let head = String::from_utf8_lossy(&head);
        if !head.contains(&marker) {
            return None;
        }
        let first: serde_json::Value = serde_json::from_str(head.lines().next()?).ok()?;
        first.get("payload")?.get("id")?.as_str().map(str::to_string)
    })
}

/// A one-shot, non-interactive run of `prompt` that still records a
/// conversation the task can resume later. None for agents without one.
pub fn headless_argv(session: &Session, project: &Project, prompt: &str) -> Option<Vec<String>> {
    let s = |v: &str| v.to_string();
    let context = notes::agent_context(project);
    match session.agent.as_str() {
        "claude" => Some(vec![
            s("claude"),
            s("-p"),
            s("--session-id"),
            session.id.clone(),
            s("--add-dir"),
            project.notes_dir().to_string_lossy().into_owned(),
            s("--append-system-prompt"),
            context,
            s("--"),
            prompt.to_string(),
        ]),
        "codex" => Some(vec![
            s("codex"),
            s("-c"),
            format!(
                "developer_instructions={}",
                serde_json::Value::String(format!("{context}\n\n{}", codex_marker(&session.id)))
            ),
            s("exec"),
            prompt.to_string(),
        ]),
        _ => None,
    }
}

/// `{ KEY = "value", ... }`: a TOML inline table for `codex -c`.
fn toml_inline_table(map: &serde_json::Map<String, serde_json::Value>) -> String {
    let pairs: Vec<String> = map.iter().map(|(k, v)| format!("{k} = {v}")).collect();
    format!("{{ {} }}", pairs.join(", "))
}

/// Whether a stopped task can be started with a message as its prompt, even
/// when resuming.
pub fn takes_prompt_on_resume(agent: &str) -> bool {
    agent == "claude"
}

/// Agents that can write a handoff note and start from an opening prompt.
pub fn supports_handoff(agent: &str) -> bool {
    matches!(agent, "claude" | "codex")
}

/// Whether the agent reports "done" on its own, so Codebench can mark it as
/// working when you send it a prompt.
pub fn reports_done(agent: &str) -> bool {
    matches!(agent, "codex")
}

pub fn status_file(dir: &Path, session: &str) -> PathBuf {
    dir.join(session)
}
