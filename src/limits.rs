//! How much of each subscription's rate limits is used: Claude's from
//! `claude /usage`, Codex's from the rate limits it records in its newest
//! session file. Checks run the CLIs or read files, so call them off the
//! main thread.

use crate::store::{cache_dir, home};
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::{Duration, Instant, UNIX_EPOCH};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Limit {
    /// "session", "week (all models)", "5 hours", ...
    pub name: String,
    pub percent: u32,
    pub resets: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Usage {
    pub agent: String,
    pub limits: Vec<Limit>,
    /// When these numbers were true (Codex's come from its last session).
    pub as_of: u64,
}

impl Usage {
    /// The limit closest to running out.
    pub fn worst(&self) -> Option<&Limit> {
        self.limits.iter().max_by_key(|l| l.percent)
    }
}

/// Parses `claude /usage`: lines like
/// "Current week (all models): 50% used · resets Sep 26, 12:59pm (...)".
fn parse_claude(text: &str) -> Vec<Limit> {
    text.lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("Current ")?;
            let (name, value) = rest.split_once(':')?;
            let (pct, after) = value.trim().split_once('%')?;
            let percent = pct.trim().parse::<f64>().ok()?.round() as u32;
            let resets = after.split_once("resets ").map(|(_, r)| r.trim().to_string()).unwrap_or_default();
            // Drop the time zone in parentheses to keep it short.
            let resets = resets.split(" (").next().unwrap_or("").to_string();
            Some(Limit { name: name.trim().to_string(), percent, resets })
        })
        .collect()
}

pub fn claude() -> Option<Usage> {
    let dir = cache_dir();
    let _ = std::fs::create_dir_all(&dir);
    let mut child = std::process::Command::new("claude")
        .args(["-p", "--no-session-persistence", "/usage"])
        .current_dir(&dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    while child.try_wait().ok()?.is_none() {
        if started.elapsed() > Duration::from_secs(60) {
            let _ = child.kill();
            return None;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let mut text = String::new();
    child.stdout.take()?.read_to_string(&mut text).ok()?;
    let limits = parse_claude(&text);
    (!limits.is_empty()).then(|| Usage { agent: "claude".into(), limits, as_of: crate::store::now() })
}

fn find_key<'a>(v: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    match v {
        serde_json::Value::Object(map) => map.get(key).or_else(|| map.values().find_map(|x| find_key(x, key))),
        serde_json::Value::Array(items) => items.iter().find_map(|x| find_key(x, key)),
        _ => None,
    }
}

fn window_name(minutes: u64) -> String {
    match minutes {
        10080 => "week".into(),
        m if m % 1440 == 0 => format!("{} days", m / 1440),
        m if m % 60 == 0 => format!("{} hours", m / 60),
        m => format!("{m} min"),
    }
}

pub fn codex() -> Option<Usage> {
    let root = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".codex"))
        .join("sessions");
    // The newest session file holds the latest numbers.
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    let mut dirs = vec![root];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                dirs.push(path);
            } else if let Ok(modified) = meta.modified()
                && newest.as_ref().is_none_or(|(t, _)| modified > *t)
            {
                newest = Some((modified, path));
            }
        }
    }
    let (modified, path) = newest?;
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(512 * 1024))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let rl = text.lines().rev().filter(|l| l.contains("\"rate_limits\"")).find_map(|line| {
        let v: serde_json::Value = serde_json::from_str(line).ok()?;
        find_key(&v, "rate_limits").cloned()
    })?;
    let limits: Vec<Limit> = ["primary", "secondary"]
        .iter()
        .filter_map(|k| {
            let w = rl.get(*k)?;
            Some(Limit {
                name: window_name(w["window_minutes"].as_u64()?),
                percent: w["used_percent"].as_f64()?.round() as u32,
                resets: w["resets_at"].as_u64().map(crate::workflow::stamp).unwrap_or_default(),
            })
        })
        .collect();
    let as_of = modified.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    (!limits.is_empty()).then(|| Usage { agent: "codex".into(), limits, as_of })
}

fn file() -> PathBuf {
    cache_dir().join("usage.json")
}

pub fn save(all: &[Usage]) {
    if let Ok(json) = serde_json::to_vec(all) {
        let tmp = file().with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(tmp, file());
        }
    }
}

pub fn load() -> Vec<Usage> {
    std::fs::read(file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Both checks, in parallel.
pub fn check_all() -> Vec<Usage> {
    let c = std::thread::spawn(claude);
    let x = std::thread::spawn(codex);
    [c.join().ok().flatten(), x.join().ok().flatten()].into_iter().flatten().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_claude_usage() {
        let text = "You are currently using your subscription\n\n\
            Current session: 9% used · resets Sep 23, 6:49pm (America/Los_Angeles)\n\
            Current week (all models): 50% used · resets Sep 26, 12:59pm (America/Los_Angeles)\n\
            Last 24h · 443 requests\n";
        let limits = parse_claude(text);
        assert_eq!(limits.len(), 2);
        assert_eq!((limits[0].name.as_str(), limits[0].percent), ("session", 9));
        assert_eq!(limits[1].resets, "Sep 26, 12:59pm");
    }

    #[test]
    fn names_codex_windows() {
        assert_eq!(window_name(300), "5 hours");
        assert_eq!(window_name(10080), "week");
    }
}
