//! A task's conversation as a list of messages for the phone: what you
//! asked, what the agent said, and one line per tool it used. Read from
//! Claude Code transcripts and Codex rollouts.

use serde::Serialize;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Serialize, Clone, Debug)]
pub struct Entry {
    /// "user", "assistant" or "tool".
    pub role: &'static str,
    pub text: String,
}

const READ: u64 = 4 * 1024 * 1024;
const MAX_TEXT: usize = 6000;

fn tail(path: &Path) -> String {
    let mut buf = Vec::new();
    if let Ok(mut f) = std::fs::File::open(path) {
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if f.seek(SeekFrom::Start(len.saturating_sub(READ))).is_ok() {
            let _ = f.read_to_end(&mut buf);
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

fn clip(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() > MAX_TEXT {
        text.chars().take(MAX_TEXT).chain("…".chars()).collect()
    } else {
        text.to_string()
    }
}

/// A one-line summary of a tool call.
fn tool_line(name: &str, input: &serde_json::Value) -> String {
    let arg = ["command", "file_path", "path", "pattern", "url", "query", "description", "subject", "prompt"]
        .iter()
        .find_map(|k| input[*k].as_str())
        .unwrap_or("");
    let arg: String = arg.lines().next().unwrap_or("").chars().take(90).collect();
    if arg.is_empty() { name.to_string() } else { format!("{name}  {arg}") }
}

pub fn claude(path: &Path, max: usize) -> Vec<Entry> {
    let mut out = Vec::new();
    for line in tail(path).lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let role = v["type"].as_str().unwrap_or("");
        if !matches!(role, "user" | "assistant") || v["isSidechain"] == true || v["isMeta"] == true {
            continue;
        }
        match &v["message"]["content"] {
            serde_json::Value::String(s) if role == "user" && !s.trim_start().starts_with('<') => {
                out.push(Entry { role: "user", text: clip(s) });
            }
            serde_json::Value::Array(blocks) => {
                for b in blocks {
                    match b["type"].as_str() {
                        Some("text") => {
                            let text = b["text"].as_str().unwrap_or("");
                            if !text.trim().is_empty() && !text.trim_start().starts_with('<') {
                                out.push(Entry { role: if role == "user" { "user" } else { "assistant" }, text: clip(text) });
                            }
                        }
                        Some("tool_use") => out.push(Entry {
                            role: "tool",
                            text: tool_line(b["name"].as_str().unwrap_or("tool"), &b["input"]),
                        }),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    let skip = out.len().saturating_sub(max);
    out.split_off(skip)
}

pub fn codex(path: &Path, max: usize) -> Vec<Entry> {
    let mut out = Vec::new();
    for line in tail(path).lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if v["type"] != "response_item" {
            continue;
        }
        let p = &v["payload"];
        match p["type"].as_str() {
            Some("message") => {
                let role = match p["role"].as_str() {
                    Some("user") => "user",
                    Some("assistant") => "assistant",
                    _ => continue,
                };
                let text: Vec<&str> = p["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|c| c["text"].as_str())
                    .filter(|t| !t.trim_start().starts_with('<'))
                    .collect();
                let text = text.join("\n");
                if !text.trim().is_empty() {
                    out.push(Entry { role, text: clip(&text) });
                }
            }
            Some("function_call") => {
                let args: serde_json::Value = p["arguments"].as_str().and_then(|a| serde_json::from_str(a).ok()).unwrap_or_default();
                let name = p["name"].as_str().unwrap_or("tool");
                // Codex runs shell commands as an argv list.
                let text = match args["command"].as_array() {
                    Some(cmd) => {
                        let joined: Vec<&str> = cmd.iter().filter_map(|c| c.as_str()).collect();
                        format!("{name}  {}", joined.join(" ").chars().take(90).collect::<String>())
                    }
                    None => tool_line(name, &args),
                };
                out.push(Entry { role: "tool", text });
            }
            _ => {}
        }
    }
    let skip = out.len().saturating_sub(max);
    out.split_off(skip)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_messages_and_tools() {
        let dir = std::env::temp_dir().join(format!("cb-chat-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("t.jsonl");
        let lines = [
            serde_json::json!({ "type": "user", "message": { "content": "fix the bug" } }),
            serde_json::json!({ "type": "assistant", "message": { "content": [
                { "type": "text", "text": "Looking." },
                { "type": "tool_use", "name": "Bash", "input": { "command": "cargo test\nmore" } } ] } }),
            serde_json::json!({ "type": "user", "message": { "content": [ { "type": "tool_result", "content": "ok" } ] } }),
            serde_json::json!({ "type": "assistant", "isSidechain": true, "message": { "content": [ { "type": "text", "text": "hidden" } ] } }),
        ];
        std::fs::write(&file, lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n")).unwrap();
        let got: Vec<(&str, String)> = claude(&file, 10).into_iter().map(|e| (e.role, e.text)).collect();
        assert_eq!(
            got,
            [("user", "fix the bug".into()), ("assistant", "Looking.".into()), ("tool", "Bash  cargo test".into())]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
