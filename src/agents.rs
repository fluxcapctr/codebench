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
    Agent { id: "cursor", label: "Cursor", program: "cursor-agent" },
    // OpenCode pointed at the models Ollama serves on this machine.
    Agent { id: "local", label: "Local (Ollama)", program: "opencode" },
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
        .filter(|a| a.id != "local" || on_path("ollama"))
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

/// Chromium or Chrome, whichever is installed.
pub fn chromium() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    ["chromium", "chromium-browser", "google-chrome-stable", "google-chrome"]
        .iter()
        .find_map(|name| std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.is_file()))
}

/// The Playwright MCP server that gives a task a browser: a visible
/// Chromium window with its own profile, kept per task so logins survive
/// resumes and tasks never share a locked profile.
fn browser_server(session: &Session) -> (String, Vec<String>, serde_json::Map<String, serde_json::Value>) {
    let profile = store::data_dir().join("browser").join(&session.id);
    let shots = store::cache_dir().join("browser").join(&session.id);
    let mut args = vec![
        "-y".to_string(),
        "@playwright/mcp@latest".to_string(),
        "--browser".to_string(),
        "chromium".to_string(),
        "--user-data-dir".to_string(),
        profile.to_string_lossy().into_owned(),
        "--output-dir".to_string(),
        shots.to_string_lossy().into_owned(),
    ];
    if let Some(exe) = chromium() {
        args.extend(["--executable-path".to_string(), exe.to_string_lossy().into_owned()]);
    }
    let npx = real_program("npx");
    // npx starts node through `env`, so node's own folder goes first on PATH.
    let mut env = serde_json::Map::new();
    if let Some(dir) = npx.as_deref().and_then(|p| Path::new(p).parent()) {
        let rest = std::env::var("PATH").unwrap_or_default();
        env.insert("PATH".into(), format!("{}:{rest}", dir.display()).into());
    }
    (npx.unwrap_or_else(|| "npx".into()), args, env)
}

/// The first real executable named `program` on PATH, skipping mise shims,
/// which fail when an agent runs them from a folder without a mise config.
fn real_program(program: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|d| !d.to_string_lossy().contains("/mise/shims"))
        .map(|d| d.join(program))
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
}

/// Builds the argv for a session. Resumes the previous conversation when there
/// is one; otherwise starts fresh. The opening prompt is `extra` if given,
/// else the session's own (fresh starts only). The flag says whether the argv
/// carries a prompt, so the agent starts working without any typing.
pub fn argv(session: &Session, project: &Project, extra: Option<&str>) -> (Vec<String>, bool) {
    let s = |v: &str| v.to_string();
    let context = notes::agent_context(project);
    let (mcp_cmd, mcp_args, mcp_env) = mcp_server(session);
    let mut prompted = false;
    let argv = match session.agent.as_str() {
        "claude" => {
            let mut argv = vec![s("claude")];
            let resume = claude_transcript(&session.id).is_some();
            if resume {
                argv.extend([s("--resume"), session.id.clone()]);
            } else {
                argv.extend([s("--session-id"), session.id.clone()]);
                // A made-up "task 3" would stop Claude naming it itself.
                if !crate::history::is_placeholder(&session.title) {
                    argv.extend([s("-n"), session.title.clone()]);
                }
            }
            let mut mcp = serde_json::json!({
                "mcpServers": { "codebench": { "type": "stdio", "command": mcp_cmd, "args": mcp_args, "env": mcp_env } }
            });
            if project.browser {
                let (cmd, args, env) = browser_server(session);
                mcp["mcpServers"]["browser"] = serde_json::json!({ "type": "stdio", "command": cmd, "args": args, "env": env });
            }
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
            prompted = prompt.is_some();
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
            if project.browser {
                let (cmd, args, env) = browser_server(session);
                argv.extend([
                    s("-c"),
                    format!("mcp_servers.browser.command={}", serde_json::Value::String(cmd)),
                    s("-c"),
                    format!("mcp_servers.browser.args={}", serde_json::json!(args)),
                    s("-c"),
                    format!("mcp_servers.browser.env={}", toml_inline_table(&env)),
                ]);
            }
            // No `--add-dir` for Codex: under a read-only sandbox it refuses
            // the flag and exits. It can still read the notes folder.
            match codex_session(session) {
                Some(id) => argv.extend([s("resume"), id]),
                None => {
                    let prompt = extra.or(session.prompt.as_deref());
                    prompted = prompt.is_some();
                    argv.extend(prompt.map(str::to_string));
                }
            }
            argv
        }
        "opencode" | "local" => {
            // These start fresh each launch, so the task's own prompt is only
            // for the first.
            let prompt = extra.or(if session.launched { None } else { session.prompt.as_deref() });
            let config = opencode_config(session, project, &context, session.agent == "local");
            let mut argv = vec![s("env"), format!("OPENCODE_CONFIG_CONTENT={config}"), s("opencode")];
            prompted = prompt.is_some();
            if let Some(prompt) = prompt {
                argv.extend([s("--prompt"), prompt.to_string()]);
            }
            argv
        }
        "cursor" => {
            let prompt = extra.or(if session.launched { None } else { session.prompt.as_deref() });
            let mut argv = vec![s("cursor-agent"), s("--add-dir"), project.notes_dir().to_string_lossy().into_owned()];
            prompted = prompt.is_some();
            argv.extend(prompt.map(str::to_string));
            argv
        }
        "gemini" | "grok" | "agy" => {
            let prompt = extra.or(if session.launched { None } else { session.prompt.as_deref() });
            let mut argv = vec![get(&session.agent).map_or("", |a| a.program).to_string()];
            prompted = prompt.is_some();
            match (session.agent.as_str(), prompt) {
                (_, None) => {}
                ("grok", Some(p)) => argv.extend([s("--"), p.to_string()]),
                // The `=` form keeps a prompt starting with "-" a value.
                (_, Some(p)) => argv.push(format!("--prompt-interactive={p}")),
            }
            argv
        }
        "shell" => vec![shell()],
        other => vec![get(other).map(|a| a.program).unwrap_or(other).to_string()],
    };
    (argv, prompted)
}

/// OpenCode config layered on the user's own through
/// `OPENCODE_CONFIG_CONTENT`: the Codebench tools, the project context and,
/// for `local`, an Ollama provider serving every model Ollama has pulled.
fn opencode_config(session: &Session, project: &Project, context: &str, local: bool) -> String {
    let (cmd, args, env) = mcp_server(session);
    let mut command = vec![cmd];
    command.extend(args);
    let mut config = serde_json::json!({
        "mcp": { "codebench": { "type": "local", "command": command, "environment": env } },
    });
    if project.browser {
        let (cmd, args, env) = browser_server(session);
        let mut command = vec![cmd];
        command.extend(args);
        config["mcp"]["browser"] = serde_json::json!({ "type": "local", "command": command, "environment": env });
    }
    // OpenCode takes extra instructions as files, not text.
    let file = store::cache_dir().join("context").join(format!("{}.md", session.id));
    if std::fs::create_dir_all(file.parent().unwrap()).is_ok() && std::fs::write(&file, context).is_ok() {
        config["instructions"] = serde_json::json!([file]);
    }
    if local {
        let models = ollama_models();
        let entries: serde_json::Map<_, _> = models
            .iter()
            .map(|m| (m.clone(), serde_json::json!({ "name": m, "limit": { "context": 32768, "output": 8192 } })))
            .collect();
        config["provider"] = serde_json::json!({
            "ollama": {
                "npm": "@ai-sdk/openai-compatible",
                "name": "Ollama",
                "options": { "baseURL": format!("{}/v1", ollama_host()) },
                "models": entries,
            }
        });
        if let Some(model) = default_local_model(&models) {
            config["model"] = format!("ollama/{model}").into();
        }
    }
    config.to_string()
}

fn ollama_host() -> String {
    match std::env::var("OLLAMA_HOST") {
        Ok(h) if h.starts_with("http") => h.trim_end_matches('/').to_string(),
        Ok(h) if !h.is_empty() => format!("http://{h}"),
        _ => "http://localhost:11434".into(),
    }
}

/// The models Ollama has pulled, as `ollama list` names them.
pub fn ollama_models() -> Vec<String> {
    let Ok(out) = std::process::Command::new("ollama").arg("list").output() else { return Vec::new() };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .skip(1)
        .filter_map(|l| l.split_whitespace().next().map(str::to_string))
        .collect()
}

/// `CODEBENCH_LOCAL_MODEL` if set, else the first coding model, else the
/// first model. Others stay a `/models` away inside OpenCode.
fn default_local_model(models: &[String]) -> Option<String> {
    std::env::var("CODEBENCH_LOCAL_MODEL")
        .ok()
        .filter(|m| !m.is_empty())
        .or_else(|| models.iter().find(|m| m.contains("coder")).or(models.first()).cloned())
}

fn codex_marker(task: &str) -> String {
    format!("Codebench task id: {task}")
}

/// Finds this task's Codex conversation by the marker in its instructions,
/// looking only at session files written since the task was created.
/// The rollout file of this task's Codex conversation.
pub fn codex_rollout(session: &Session) -> Option<PathBuf> {
    let id = codex_session(session)?;
    let root = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".codex"))
        .join("sessions");
    let mut dirs = vec![root];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.file_name().is_some_and(|n| n.to_string_lossy().ends_with(&format!("{id}.jsonl"))) {
                return Some(path);
            }
        }
    }
    None
}

fn codex_session(session: &Session) -> Option<String> {
    use std::io::Read;
    if let Some(id) = &session.codex_id {
        return Some(id.clone());
    }
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
    matches!(agent, "claude" | "opencode" | "local" | "cursor" | "gemini" | "grok" | "agy")
}

/// Whether `argv` can start the agent with an opening prompt. Only the
/// shell cannot; starting one for a workflow or prompt would drop it.
pub fn takes_opening_prompt(agent: &str) -> bool {
    get(agent).is_some_and(|a| a.id != "shell")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_prompts_reach_every_agent() {
        let root = std::env::temp_dir().join(format!("cb-agents-{}", std::process::id()));
        let project = Project {
            id: "p".into(),
            name: "p".into(),
            path: root.clone(),
            sessions: Vec::new(),
            notes: Some(root.join("notes")),
            browser: false,
        };
        let session = |agent: &str, launched: bool| Session {
            id: "probe".into(),
            agent: agent.into(),
            title: "probe".into(),
            created: 0,
            launched,
            prompt: Some("-UNIQUE_OPENING_PROMPT".into()),
            archived: false,
            workflow: None,
            worktree: None,
            codex_id: None,
        };
        let count = |argv: &[String], text: &str| argv.iter().filter(|v| v.contains(text)).count();
        for (agent, program) in [("gemini", "gemini"), ("grok", "grok"), ("agy", "agy")] {
            let (argv, prompted) = argv(&session(agent, false), &project, None);
            assert!(prompted);
            assert_eq!(argv[0], program);
            assert_eq!(count(&argv, "UNIQUE_OPENING_PROMPT"), 1, "{argv:?}");
            let expected = if agent == "grok" { "-UNIQUE_OPENING_PROMPT" } else { "--prompt-interactive=-UNIQUE_OPENING_PROMPT" };
            assert_eq!(argv.last().unwrap(), expected);
            assert_eq!(super::argv(&session(agent, true), &project, None), (vec![program.to_string()], false));
            let (later, prompted) = super::argv(&session(agent, true), &project, Some("LATER"));
            assert!(prompted);
            assert_eq!(count(&later, "LATER"), 1);
            assert!(takes_opening_prompt(agent) && takes_prompt_on_resume(agent));
        }
        assert!(!takes_opening_prompt("shell") && !takes_opening_prompt("nope"));
        let _ = std::fs::remove_dir_all(root);
    }
}
