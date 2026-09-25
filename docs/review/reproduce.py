#!/usr/bin/env python3
"""Run review probes against a temporary source copy; never launch agent CLIs.

These assertions describe observed bugs, so PASS confirms a finding, not a fix.
Only disposable /tmp state is used. Requires the project's cached Cargo deps.
"""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

root = Path(__file__).resolve().parents[2]
scratch = Path(tempfile.mkdtemp(prefix="codebench-review-"))
shutil.copytree(root / "src", scratch / "src")
for name in ("Cargo.toml", "Cargo.lock"):
    shutil.copy2(root / name, scratch / name)

probes = r'''
#[cfg(test)]
mod review_probes {
    use crate::{agents, artifacts, notes, store, workflow};
    fn project(name: &str) -> store::Project {
        let path = store::data_dir().join(name);
        std::fs::create_dir_all(&path).unwrap();
        store::Project { id: name.into(), name: name.into(), path: path.clone(),
            notes: Some(path.join("notes")), sessions: vec![], browser: false }
    }
    #[test]
    fn review_agent_opening_prompts_are_dropped() {
        let p = project("prompts");
        for agent in ["gemini", "grok", "agy"] {
            let s = store::Session { id: "probe".into(), agent: agent.into(), title: "probe".into(),
                created: 0, launched: false, prompt: Some("UNIQUE_OPENING_PROMPT".into()),
                archived: false, workflow: None, worktree: None, codex_id: None };
            let argv = agents::argv(&s, &p, None);
            assert_eq!(argv.len(), 1, "recheck finding if prompt support is fixed");
            assert!(!argv.iter().any(|v| v.contains("UNIQUE_OPENING_PROMPT")));
        }
    }
    #[test]
    fn review_link_notes_omits_user_content() {
        let p = project("migration");
        let from = notes::ensure(&p);
        for rel in ["custom.md", "workflows/check.md", "runs/result.md", "artifacts/chart.html"] {
            let path = from.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "keep me").unwrap();
        }
        let to = p.path.join("new-notes");
        notes::carry_over(&from, &to);
        assert!(to.join("brief.md").exists());
        for rel in ["custom.md", "workflows/check.md", "runs/result.md", "artifacts/chart.html"] {
            assert!(!to.join(rel).exists(), "recheck finding if migration is fixed");
        }
    }
    #[test]
    fn review_dst_wall_clock_is_shifted() {
        // TZ is set by the parent before this process starts.
        unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            tm.tm_year = 126; tm.tm_mon = 2; tm.tm_mday = 8;
            tm.tm_hour = 12; tm.tm_isdst = -1;
            let noon = libc::mktime(&mut tm) as u64;
            let slot = workflow::latest_slot(&workflow::parse_schedule("daily 09:00").unwrap(), noon).unwrap();
            let t = slot as libc::time_t;
            libc::localtime_r(&t, &mut tm);
            assert_eq!(tm.tm_hour, 10, "recheck finding if DST scheduling is fixed");
        }
    }
    #[test]
    fn review_corrupt_state_is_silently_replaced() {
        store::make_private_dirs();
        let file = store::config_dir().join("state.json");
        std::fs::write(&file, "{broken but potentially recoverable").unwrap();
        let state = store::State::load();
        assert!(state.projects.is_empty());
        state.save();
        assert!(!std::fs::read_to_string(file).unwrap().contains("recoverable"));
    }
    #[test]
    fn review_artifact_copy_to_self_erases_content() {
        let p = project("self-copy");
        artifacts::save(&p, "chart", Some("html"), Some("<h1>keep me</h1>"), None).unwrap();
        let file = artifacts::dir(&p).join("chart.html");
        let result = artifacts::save(&p, "chart", None, None, Some(&file));
        assert!(result.is_ok());
        assert_eq!(std::fs::metadata(file).unwrap().len(), 0);
    }
    #[test]
    fn review_new_repository_cannot_create_worktree() {
        let p = project("empty-repo");
        crate::git::init(&p.path).unwrap();
        assert!(crate::git::is_repo(&p.path));
        assert!(crate::git::add_worktree(&p.path, &p.path.join("task-wt"), "cb/probe").is_err());
    }
}
'''
with (scratch / "src/main.rs").open("a") as f:
    f.write(probes)
env = os.environ.copy()
env.update(TZ="America/Los_Angeles", CARGO_TARGET_DIR=str(root / "target"))
for key, name in zip(("CODEBENCH_CONFIG_DIR", "CODEBENCH_CACHE_DIR", "CODEBENCH_DATA_DIR"),
                     ("config", "cache", "data")):
    env[key] = str(scratch / name)
print(f"Disposable review source and fixtures: {scratch}", flush=True)
result = subprocess.run(["cargo", "test", "--offline", "--manifest-path", str(scratch / "Cargo.toml"),
                         "review_", "--", "--test-threads=1", "--nocapture"], env=env)
raise SystemExit(result.returncode)
