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

fn app_running() -> bool {
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
    workflow::mark_ran(wf, now);
    let session = Session {
        id: uuid::Uuid::new_v4().to_string(),
        agent: wf.agent.clone(),
        title: format!("{} · {}", wf.name, workflow::stamp(now)),
        created: now,
        launched: true,
        prompt: None,
        archived: false,
        workflow: Some(wf.path.clone()),
    };
    let Some(project) = State::load().project(pid).cloned() else { return };
    let Some(argv) = agents::headless_argv(&session, &project, &wf.prompt) else {
        notify(&format!("{}: {}", project.name, wf.name), &format!("{} cannot run in the background", wf.agent));
        return;
    };
    let mut state = State::load();
    state.add_run(pid, session.clone());

    let output = Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(&project.path)
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
                "[Unit]\nDescription=Run due Codebench workflows\n\n[Service]\nType=oneshot\nEnvironment=PATH={path}\nExecStart={} run-due\n",
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
