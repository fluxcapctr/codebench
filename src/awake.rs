//! Keeps the machine awake while a task is working.
//!
//! Two locks, both let go a short while after the last task stops working:
//! a logind sleep/idle inhibitor, held by `systemd-inhibit ... cat` reading
//! from a pipe we own, so it ends by itself if Codebench dies; and Omarchy's
//! stay-awake switch (the bar's awake indicator), turned on only if it was
//! off, and turned off only if it still carries our owner token.

use std::io::ErrorKind;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// How long after the last task stops working before the machine may sleep.
const GRACE: Duration = Duration::from_secs(120);

const OWNER_PREFIX: &str = "codebench:";

fn stay_awake_file() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state/omarchy/indicators/stay-awake"))
}

fn toggle_idle(arg: &str) -> bool {
    Command::new("omarchy-toggle-idle")
        .arg(arg)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[derive(Default)]
pub struct Awake {
    inhibitor: Option<Child>,
    /// The token we wrote into the stay-awake file, if we turned it on.
    owner: Option<String>,
    idle_since: Option<Instant>,
}

impl Awake {
    /// Clears a stay-awake switch left on by a Codebench that is gone.
    pub fn new() -> Awake {
        if let Some(file) = stay_awake_file()
            && let Ok(text) = std::fs::read_to_string(&file)
            && let Some(pid) = text.trim().strip_prefix(OWNER_PREFIX).and_then(|r| r.split(':').next())
            && !std::fs::read_to_string(format!("/proc/{pid}/comm")).is_ok_and(|c| c.trim() == "codebench")
        {
            toggle_idle("allow-idle");
        }
        Awake::default()
    }

    /// Called every second with whether any task is working.
    pub fn update(&mut self, working: bool) {
        if working {
            self.idle_since = None;
            self.hold();
        } else if self.held() {
            let since = *self.idle_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= GRACE {
                self.release();
            }
        }
    }

    pub fn held(&self) -> bool {
        self.inhibitor.is_some() || self.owner.is_some()
    }

    fn hold(&mut self) {
        if let Some(child) = &mut self.inhibitor
            && matches!(child.try_wait(), Ok(Some(_)) | Err(_))
        {
            self.inhibitor = None;
        }
        if self.inhibitor.is_none() {
            self.inhibitor = Command::new("systemd-inhibit")
                .args([
                    "--what=sleep:idle",
                    "--who=Codebench",
                    "--why=A task is working",
                    "--mode=block",
                    "cat",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .ok();
        }
        if self.owner.is_none()
            && let Some(file) = stay_awake_file()
            && std::fs::metadata(&file).is_err_and(|e| e.kind() == ErrorKind::NotFound)
            && toggle_idle("stay-awake")
        {
            let token = format!("{OWNER_PREFIX}{}:{}", std::process::id(), uuid::Uuid::new_v4());
            if std::fs::write(&file, format!("{token}\n")).is_ok() {
                self.owner = Some(token);
            }
        }
    }

    pub fn release(&mut self) {
        self.idle_since = None;
        if let Some(mut child) = self.inhibitor.take() {
            // Closing its stdin ends `cat`, and with it the inhibitor.
            drop(child.stdin.take());
            let _ = child.wait();
        }
        if let Some(token) = self.owner.take()
            && let Some(file) = stay_awake_file()
            && std::fs::read_to_string(&file).is_ok_and(|t| t.trim() == token)
        {
            toggle_idle("allow-idle");
        }
    }
}

impl Drop for Awake {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore]
    fn live_hold_and_release() {
        let mut a = super::Awake::new();
        assert!(!super::stay_awake_file().unwrap().exists(), "stale switch cleared");
        a.update(true);
        std::thread::sleep(std::time::Duration::from_millis(500));
        let list = String::from_utf8(std::process::Command::new("systemd-inhibit").arg("--list").output().unwrap().stdout).unwrap();
        println!("HELD list:\n{list}");
        println!("file: {:?}", std::fs::read_to_string(super::stay_awake_file().unwrap()));
        assert!(list.contains("A task is working") && a.held());
        a.update(false);
        assert!(a.held(), "grace period keeps it");
        a.release();
        let list = String::from_utf8(std::process::Command::new("systemd-inhibit").arg("--list").output().unwrap().stdout).unwrap();
        assert!(!list.contains("A task is working"));
        println!("after file: {:?}", std::fs::read_to_string(super::stay_awake_file().unwrap()));
    }
}
