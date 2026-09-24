//! `codebench mcp --session <id>`: a small stdio MCP server each agent gets,
//! so tasks on the same project can see, read, message and start each other.

use crate::agents;
use crate::bus::{self, Request};
use crate::store::{Project, Session, State};
use crate::usage;
use serde_json::{Value, json};
use std::io::{BufRead, Write};

const INSTRUCTIONS: &str = "Codebench runs several agent tasks side by side on this project, each its own \
conversation. Use these tools to see the other tasks, read what one has been doing, send one a message, \
or start a new task for separate work instead of growing this conversation.";

pub fn serve(session: String) {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        let Some(id) = msg.get("id").cloned() else { continue }; // notifications
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let reply = match method {
            "initialize" => Ok(json!({
                "protocolVersion": params.get("protocolVersion").cloned().unwrap_or(json!("2025-06-18")),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "codebench", "version": env!("CARGO_PKG_VERSION") },
                "instructions": INSTRUCTIONS,
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools() })),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                let (text, is_error) = match call(&session, name, &args) {
                    Ok(text) => (text, false),
                    Err(text) => (text, true),
                };
                Ok(json!({ "content": [{ "type": "text", "text": text }], "isError": is_error }))
            }
            _ => Err(json!({ "code": -32601, "message": format!("unknown method {method}") })),
        };
        let out = match reply {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
        };
        if writeln!(stdout, "{out}").and_then(|_| stdout.flush()).is_err() {
            break;
        }
    }
}

fn tools() -> Value {
    let task = json!({ "type": "string", "description": "Task title (or id prefix) from list_tasks" });
    json!([
        {
            "name": "list_tasks",
            "description": "List the tasks on this project: title, agent, status, context size.",
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "read_task",
            "description": "Read the latest messages of another Claude task, or its handoff note.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task": task,
                    "messages": { "type": "integer", "description": "How many recent messages (default 6)" },
                },
                "required": ["task"],
            },
        },
        {
            "name": "send_to_task",
            "description": "Send a message into another task's conversation. It is delivered when that agent is free; a stopped task is started.",
            "inputSchema": {
                "type": "object",
                "properties": { "task": task, "message": { "type": "string" } },
                "required": ["task", "message"],
            },
        },
        {
            "name": "show_artifact",
            "description": "Show the user something visual in a viewer window next to Codebench: an HTML page (self-contained, scripts allowed), an SVG, a Mermaid diagram, a markdown document, or an image or PDF you made. Pass content with a kind, or the path of a file you wrote. Showing the same title again updates it, and the viewer reloads.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "Short name; also its file name" },
                    "kind": { "type": "string", "description": "html, svg, mermaid, markdown, png, jpg, gif, webp or pdf" },
                    "content": { "type": "string", "description": "The artifact's text (html, svg, mermaid, markdown)" },
                    "path": { "type": "string", "description": "Instead of content: a file to show (it is copied)" },
                },
                "required": ["title"],
            },
        },
        {
            "name": "start_task",
            "description": "Start a new task on this project with its own fresh conversation.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": { "type": "string" },
                    "prompt": { "type": "string", "description": "Opening instructions for the new task" },
                    "agent": {
                        "type": "string",
                        "description": "claude, codex, gemini, grok, opencode or agy (default claude)",
                    },
                },
                "required": ["title", "prompt"],
            },
        },
    ])
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("missing {key}"))
}

fn call(me: &str, tool: &str, args: &Value) -> Result<String, String> {
    let state = State::load();
    let (project, me_session) = state
        .session(me)
        .ok_or("this task is not known to Codebench")?;
    match tool {
        "list_tasks" => Ok(list_tasks(project, me_session)),
        "read_task" => {
            let target = find(project, me, str_arg(args, "task")?)?;
            let n = args.get("messages").and_then(Value::as_u64).unwrap_or(6).clamp(1, 30) as usize;
            read_task(project, target, n)
        }
        "send_to_task" => {
            let target = find(project, me, str_arg(args, "task")?)?;
            let text = str_arg(args, "message")?.to_string();
            bus::post(&Request::Send { from: me.to_string(), to: target.id.clone(), text })
                .map_err(|e| e.to_string())?;
            Ok(format!("sent to \"{}\"", target.title))
        }
        "start_task" => {
            let agent = args.get("agent").and_then(Value::as_str).unwrap_or("claude");
            if agents::get(agent).is_none_or(|a| a.id == "shell") {
                return Err(format!("unknown agent {agent}"));
            }
            let title = str_arg(args, "title")?.to_string();
            let prompt = str_arg(args, "prompt")?.to_string();
            bus::post(&Request::StartTask {
                from: me.to_string(),
                title: title.clone(),
                agent: agent.to_string(),
                prompt,
            })
            .map_err(|e| e.to_string())?;
            Ok(format!("started \"{title}\" ({agent})"))
        }
        "show_artifact" => {
            let title = str_arg(args, "title")?;
            let content = args.get("content").and_then(Value::as_str);
            let path = args.get("path").and_then(Value::as_str).map(|p| {
                let p = std::path::PathBuf::from(p);
                if p.is_relative() { me_session.dir(project).join(p) } else { p }
            });
            let kind = args.get("kind").and_then(Value::as_str);
            let file = crate::artifacts::save(project, title, kind, content, path.as_deref())?;
            bus::post(&Request::ShowArtifact { from: me.to_string(), file: file.clone() }).map_err(|e| e.to_string())?;
            Ok(format!("showing \"{title}\" ({file}). It updates when you show the same title again."))
        }
        _ => Err(format!("unknown tool {tool}")),
    }
}

/// Finds another task in the project by exact title, id prefix, or partial title.
fn find<'a>(project: &'a Project, me: &str, query: &str) -> Result<&'a Session, String> {
    let q = query.trim().trim_matches('"').to_lowercase();
    let others = || project.sessions.iter().filter(|s| s.id != me);
    others()
        .find(|s| s.title.to_lowercase() == q)
        .or_else(|| others().find(|s| q.len() >= 4 && s.id.starts_with(&q)))
        .or_else(|| others().find(|s| !s.archived && s.title.to_lowercase().contains(&q)))
        .ok_or_else(|| format!("no other task matches \"{query}\". Use list_tasks to see them."))
}

fn list_tasks(project: &Project, me: &Session) -> String {
    let live = bus::read_snapshot();
    let mut out = format!(
        "project: {} ({})\nnotes: {} (brief.md, handoffs/)\nyou: \"{}\" ({})\n\ntasks:\n",
        project.name,
        project.path.display(),
        project.notes_dir().display(),
        me.title,
        me.agent
    );
    for s in project.sessions.iter().filter(|s| s.id != me.id) {
        let status = live.get(&s.id).map(String::as_str).unwrap_or("not running");
        let context = (s.agent == "claude")
            .then(|| usage::claude_context_tokens(&s.id))
            .flatten()
            .map(|n| format!(", {} context", usage::short(n)))
            .unwrap_or_default();
        let archived = if s.archived { ", handed off" } else { "" };
        out.push_str(&format!("- \"{}\" ({}): {status}{context}{archived}\n", s.title, s.agent));
    }
    if project.sessions.len() == 1 {
        out.push_str("(no other tasks)\n");
    }
    out
}

fn read_task(project: &Project, target: &Session, n: usize) -> Result<String, String> {
    let handoff = crate::notes::handoff_path(project, target);
    if target.agent == "claude"
        && let Some(path) = agents::claude_transcript(&target.id)
    {
        let messages = usage::recent_messages(&path, n);
        if !messages.is_empty() {
            return Ok(format!("latest messages of \"{}\":\n\n{}", target.title, messages.join("\n\n")));
        }
    }
    if handoff.is_file() {
        return std::fs::read_to_string(&handoff).map_err(|e| e.to_string());
    }
    Err(format!(
        "\"{}\" has nothing readable yet (only Claude conversations and handoff notes can be read)",
        target.title
    ))
}
