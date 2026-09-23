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
