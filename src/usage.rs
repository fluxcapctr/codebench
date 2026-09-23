//! How much context a Claude Code session is carrying, read from the usage
//! numbers on the last assistant message in its transcript.

use crate::agents::claude_transcript;
use std::io::{Read, Seek, SeekFrom};

const TAIL: u64 = 512 * 1024;

pub fn claude_context_tokens(session_id: &str) -> Option<u64> {
    let mut file = std::fs::File::open(claude_transcript(session_id)?).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);

    text.lines().rev().find_map(|line| {
        if !line.contains("\"usage\"") {
            return None;
        }
        let v: serde_json::Value = serde_json::from_str(line).ok()?;
        if v.get("type")?.as_str()? != "assistant" || v.get("isSidechain").and_then(|s| s.as_bool()) == Some(true) {
            return None;
        }
        let usage = v.get("message")?.get("usage")?;
        let n = |k: &str| usage.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
        Some(
            n("input_tokens")
                + n("cache_creation_input_tokens")
                + n("cache_read_input_tokens")
                + n("output_tokens"),
        )
    })
}

pub fn short(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else {
        format!("{}k", (tokens + 500) / 1000)
    }
}

/// The last `n` user and assistant text messages of a Claude transcript,
/// skipping tool calls, tool results and subagent traffic.
pub fn recent_messages(path: &std::path::Path, n: usize) -> Vec<String> {
    const MAX_CHARS: usize = 1500;
    let Ok(mut file) = std::fs::File::open(path) else { return Vec::new() };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if file.seek(SeekFrom::Start(len.saturating_sub(4 * TAIL))).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    if file.read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&buf);

    let mut out = Vec::new();
    for line in text.lines().rev() {
        if out.len() >= n {
            break;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let role = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let flagged = |k: &str| v.get(k).and_then(|x| x.as_bool()) == Some(true);
        if !matches!(role, "user" | "assistant") || flagged("isSidechain") || flagged("isMeta") {
            continue;
        }
        let Some(content) = v.get("message").and_then(|m| m.get("content")) else { continue };
        let body = match content {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Array(blocks) => blocks
                .iter()
                .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => continue,
        };
        let body = body.trim();
        if body.is_empty() || body.starts_with('<') {
            continue;
        }
        let body: String = if body.chars().count() > MAX_CHARS {
            body.chars().take(MAX_CHARS).chain("…".chars()).collect()
        } else {
            body.to_string()
        };
        out.push(format!("{role}: {body}"));
    }
    out.reverse();
    out
}
