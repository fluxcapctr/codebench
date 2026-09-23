//! Each task's plan and latest word, read from the agent's own transcript:
//! Claude Code's TaskCreate/TaskUpdate (or older TodoWrite) calls and
//! Codex's update_plan calls.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StepState {
    Done,
    Active,
    Pending,
}

#[derive(Clone, Debug)]
pub struct Step {
    pub text: String,
    pub state: StepState,
}

#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub steps: Vec<Step>,
    pub last: Option<String>,
}

const MAX_READ: u64 = 8 * 1024 * 1024;

/// The file, or its last 8 MB when it is bigger than that.
fn read(path: &Path) -> String {
    let mut buf = Vec::new();
    if let Ok(mut f) = std::fs::File::open(path) {
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if f.seek(SeekFrom::Start(len.saturating_sub(MAX_READ))).is_ok() {
            let _ = f.read_to_end(&mut buf);
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

fn state(s: &str) -> Option<StepState> {
    match s {
        "completed" | "done" => Some(StepState::Done),
        "in_progress" | "active" => Some(StepState::Active),
        "pending" => Some(StepState::Pending),
        _ => None,
    }
}

pub fn claude(transcript: &Path) -> Progress {
    let text = read(transcript);
    let mut steps: Vec<Option<Step>> = Vec::new();
    for line in text.lines() {
        if !(line.contains("\"TaskCreate\"") || line.contains("\"TaskUpdate\"") || line.contains("\"TodoWrite\"")) {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if v["isSidechain"] == true {
            continue;
        }
        let Some(blocks) = v["message"]["content"].as_array() else { continue };
        for b in blocks.iter().filter(|b| b["type"] == "tool_use") {
            let input = &b["input"];
            match b["name"].as_str() {
                // Task ids count up from 1 within a session.
                Some("TaskCreate") => steps.push(input["subject"].as_str().map(|s| Step {
                    text: s.to_string(),
                    state: StepState::Pending,
                })),
                Some("TaskUpdate") => {
                    let idx = input["taskId"].as_str().and_then(|i| i.parse::<usize>().ok()).and_then(|i| i.checked_sub(1));
                    if let Some(slot) = idx.and_then(|i| steps.get_mut(i)) {
                        match input["status"].as_str() {
                            Some("deleted") => *slot = None,
                            Some(s) => {
                                if let (Some(step), Some(st)) = (slot.as_mut(), state(s)) {
                                    step.state = st;
                                }
                            }
                            None => {}
                        }
                        if let (Some(step), Some(subject)) = (slot.as_mut(), input["subject"].as_str()) {
                            step.text = subject.to_string();
                        }
                    }
                }
                // The older tool replaces the whole list each time.
                Some("TodoWrite") => {
                    steps = input["todos"]
                        .as_array()
                        .map(|todos| {
                            todos
                                .iter()
                                .map(|t| {
                                    Some(Step {
                                        text: t["content"].as_str().unwrap_or("").to_string(),
                                        state: t["status"].as_str().and_then(state).unwrap_or(StepState::Pending),
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                }
                _ => {}
            }
        }
    }
    Progress {
        steps: steps.into_iter().flatten().collect(),
        last: crate::usage::recent_messages(transcript, 1)
            .into_iter()
            .find(|m| m.starts_with("assistant: "))
            .map(|m| m.trim_start_matches("assistant: ").to_string()),
    }
}

pub fn codex(rollout: &Path) -> Progress {
    let text = read(rollout);
    let mut progress = Progress::default();
    for line in text.lines() {
        let plan = line.contains("\"update_plan\"");
        let said = line.contains("\"agent_message\"");
        if !plan && !said {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let p = &v["payload"];
        if plan && p["type"] == "function_call" && p["name"] == "update_plan" {
            let args: serde_json::Value = p["arguments"].as_str().and_then(|a| serde_json::from_str(a).ok()).unwrap_or_default();
            if let Some(items) = args["plan"].as_array() {
                progress.steps = items
                    .iter()
                    .map(|i| Step {
                        text: i["step"].as_str().unwrap_or("").to_string(),
                        state: i["status"].as_str().and_then(state).unwrap_or(StepState::Pending),
                    })
                    .collect();
            }
        } else if said && p["type"] == "agent_message" {
            progress.last = p["message"].as_str().map(str::to_string);
        }
    }
    progress
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_tasks_follow_updates() {
        let dir = std::env::temp_dir().join(format!("cb-progress-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("t.jsonl");
        let call = |name: &str, input: serde_json::Value| {
            serde_json::json!({ "type": "assistant", "message": { "content": [
                { "type": "tool_use", "name": name, "input": input } ] } })
            .to_string()
        };
        let lines = [
            call("TaskCreate", serde_json::json!({ "subject": "one" })),
            call("TaskCreate", serde_json::json!({ "subject": "two" })),
            call("TaskCreate", serde_json::json!({ "subject": "three" })),
            call("TaskUpdate", serde_json::json!({ "taskId": "1", "status": "completed" })),
            call("TaskUpdate", serde_json::json!({ "taskId": "2", "status": "in_progress" })),
            call("TaskUpdate", serde_json::json!({ "taskId": "3", "status": "deleted" })),
        ];
        std::fs::write(&file, lines.join("\n")).unwrap();
        let p = claude(&file);
        let got: Vec<_> = p.steps.iter().map(|s| (s.text.as_str(), s.state)).collect();
        assert_eq!(got, [("one", StepState::Done), ("two", StepState::Active)]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
