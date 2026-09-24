//! Scheduled workflows while Codebench is closed: `codebench run-due`, run by
//! a systemd user timer that `codebench schedule on` installs.

use crate::agents;
use crate::store::{self, Session, State};
use crate::workflow::{self, Workflow};
use std::path::PathBuf;
use std::process::Command;

fn pid_file() -> PathBuf {
    store::cache_dir().join("app.pid")
}

/// Called by the app at startup; scheduled runs then happen inside it.
pub fn write_pid() {
    let _ = std::fs::create_dir_all(store::cache_dir());
    let _ = std::fs::write(pid_file(), std::process::id().to_string());
}

pub fn app_running() -> bool {
    let Some(pid) = std::fs::read_to_string(pid_file()).ok().and_then(|s| s.trim().parse::<u32>().ok()) else {
        return false;
    };
    std::fs::read_to_string(format!("/proc/{pid}/comm")).is_ok_and(|c| c.trim() == "codebench")
}

pub fn run_due() {
    if app_running() {
        return;
    }
    let now = store::now();
    let projects = State::load().projects;
    for (pid, wf) in workflow::due(&projects, now) {
        run(&pid, &wf, now);
    }
}

/// Runs one workflow to completion without a terminal, saves the agent's
/// final answer next to the notes, and records it as a resumable task.
fn run(pid: &str, wf: &Workflow, now: u64) {
    let Some(project) = State::load().project(pid).cloned() else { return };
    workflow::mark_ran(wf, &project, now);
    let session = Session {
        id: uuid::Uuid::new_v4().to_string(),
        agent: wf.agent.clone(),
        title: format!("{} · {}", wf.name, workflow::stamp(now)),
        created: now,
        launched: true,
        prompt: None,
        archived: false,
        workflow: Some(wf.path.clone()),
        worktree: None,
        codex_id: None,
    };
    let Some(argv) = agents::headless_argv(&session, &project, &wf.prompt) else {
        notify(&format!("{}: {}", project.name, wf.name), &format!("{} cannot run in the background", wf.agent));
        return;
    };
    let mut state = State::load();
    state.add_run(pid, session.clone());

    let output = Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(session.dir(&project))
        .stdin(std::process::Stdio::null())
        .output();
    let (ok, text) = match output {
        Ok(out) => (
            out.status.success(),
            String::from_utf8_lossy(if out.stdout.is_empty() { &out.stderr } else { &out.stdout }).into_owned(),
        ),
        Err(e) => (false, e.to_string()),
    };

    let runs = project.notes_dir().join("runs");
    let _ = std::fs::create_dir_all(&runs);
    let file = runs.join(format!("{}-{}.md", workflow::slug(&wf.name), workflow::stamp(now).replace([' ', ':'], "-")));
    let _ = std::fs::write(&file, format!("# {}\n\n{}\n", session.title, text.trim()));
    let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").chars().take(120).collect::<String>();
    let title = format!("{}: {}", project.name, wf.name);
    notify(&title, if ok { &first } else { "failed. see the run note" });
}

fn notify(title: &str, body: &str) {
    let _ = Command::new("notify-send").args(["-a", "Codebench", title, body]).status();
}

fn unit_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| store::home().join(".config"))
        .join("systemd/user")
}

pub fn timer_installed() -> bool {
    unit_dir().join("codebench-due.timer").is_file()
}

/// `codebench schedule on|off|status`.
pub fn schedule(arg: Option<&str>) -> i32 {
    let systemctl = |args: &[&str]| Command::new("systemctl").arg("--user").args(args).status().is_ok_and(|s| s.success());
    match arg {
        Some("on") => {
            let exe = std::env::current_exe().unwrap_or_else(|_| "codebench".into());
            let path = std::env::var("PATH").unwrap_or_default();
            let dir = unit_dir();
            let _ = std::fs::create_dir_all(&dir);
            let service = format!(
                "[Unit]\nDescription=Run due Codebench workflows\n\n[Service]\nType=oneshot\nEnvironment=\"PATH={path}\"\nExecStart=\"{}\" run-due\n",
                exe.display()
            );
            let timer = "[Unit]\nDescription=Check Codebench workflow schedules\n\n[Timer]\nOnCalendar=*:0/15\nPersistent=true\n\n[Install]\nWantedBy=timers.target\n";
            if std::fs::write(dir.join("codebench-due.service"), service).is_err()
                || std::fs::write(dir.join("codebench-due.timer"), timer).is_err()
            {
                eprintln!("could not write the systemd units in {}", dir.display());
                return 1;
            }
            if systemctl(&["daemon-reload"]) && systemctl(&["enable", "--now", "codebench-due.timer"]) {
                println!("scheduled workflows now run in the background every 15 minutes, even with Codebench closed");
                0
            } else {
                1
            }
        }
        Some("off") => {
            systemctl(&["disable", "--now", "codebench-due.timer"]);
            let _ = std::fs::remove_file(unit_dir().join("codebench-due.timer"));
            let _ = std::fs::remove_file(unit_dir().join("codebench-due.service"));
            systemctl(&["daemon-reload"]);
            println!("background runs are off. scheduled workflows run only while Codebench is open");
            0
        }
        _ => {
            println!(
                "background runs: {}\nusage: codebench schedule on|off",
                if timer_installed() { "on" } else { "off" }
            );
            0
        }
    }
}

/// `codebench status [--follow]`: a JSON line for the bar widget with the
/// tasks that are working or need you. With --follow, a new line each time
/// it changes.
pub fn status(follow: bool) {
    let mut last = String::new();
    loop {
        let line = status_line();
        if line != last {
            println!("{line}");
            use std::io::Write;
            if std::io::stdout().flush().is_err() {
                return;
            }
            last = line;
        }
        if !follow {
            return;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

fn status_line() -> String {
    if !app_running() {
        return serde_json::json!({ "running": false, "needs": 0, "working": 0, "tasks": [] }).to_string();
    }
    let live = crate::bus::read_snapshot();
    let state = State::load();
    let mut tasks = Vec::new();
    let (mut needs, mut working) = (0, 0);
    for p in &state.projects {
        for s in p.sessions.iter().filter(|s| !s.archived) {
            let Some(status) = live.get(&s.id) else { continue };
            let kind = match status.as_str() {
                "waiting on the user" | "done" => {
                    needs += 1;
                    "needs"
                }
                "working" => {
                    working += 1;
                    "working"
                }
                _ => continue,
            };
            tasks.push(serde_json::json!({
                "id": s.id, "project": p.name, "title": s.title, "agent": s.agent,
                "status": status, "kind": kind,
            }));
        }
    }
    let usage: Vec<serde_json::Value> = crate::limits::load()
        .into_iter()
        .map(|u| {
            let limits: Vec<String> = u.limits.iter().map(|l| format!("{} {}%", l.name, l.percent)).collect();
            serde_json::json!({ "agent": u.agent, "text": limits.join(" · "), "worst": u.worst().map_or(0, |l| l.percent) })
        })
        .collect();
    serde_json::json!({ "running": true, "needs": needs, "working": working, "tasks": tasks, "usage": usage }).to_string()
}
