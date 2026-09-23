//! Workflows: saved prompts kept as markdown in a project's notes folder
//! (`workflows/*.md`) or globally, optionally on a schedule.
//!
//! ```text
//! ---
//! agent: claude
//! schedule: weekdays 09:00
//! ---
//! Check for outdated dependencies and summarize what changed.
//! ```

use crate::store::{Project, config_dir};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub enum Schedule {
    /// Every so many seconds since the last run.
    Every(u64),
    /// At a local time of day on the given weekdays (bit 0 = Sunday).
    At { days: u8, minute: u32 },
}

#[derive(Clone, Debug)]
pub struct Workflow {
    pub name: String,
    pub path: PathBuf,
    pub agent: String,
    pub schedule: Option<Schedule>,
    /// The schedule line as written, for display.
    pub schedule_text: Option<String>,
    pub prompt: String,
    pub global: bool,
    /// The collection it came from, for workflows shared through git.
    pub collection: Option<String>,
    /// For global workflows: the projects a schedule runs it in, by name,
    /// or "all".
    pub projects: Vec<String>,
}

impl Workflow {
    /// Whether a global workflow's schedule covers this project.
    pub fn scheduled_for(&self, project: &Project) -> bool {
        !self.global || self.projects.iter().any(|p| p == "all" || p.eq_ignore_ascii_case(&project.name))
    }
}

pub const TEMPLATE: &str = "\
---
agent: claude
# schedule: weekdays 09:00    # or: daily 18:30, mon,thu 08:00, hourly, every 6h
---
Describe what the agent should do each time this workflow runs.
";

pub fn project_dir(project: &Project) -> PathBuf {
    project.notes_dir().join("workflows")
}

pub fn global_dir() -> PathBuf {
    config_dir().join("workflows")
}

/// Workflow sets cloned from git with `codebench workflows add`.
pub fn collections_dir() -> PathBuf {
    config_dir().join("collections")
}

const DAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

fn parse_time(s: &str) -> Option<u32> {
    let (h, m) = s.split_once(':')?;
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

pub fn parse_schedule(text: &str) -> Option<Schedule> {
    let text = text.trim().to_lowercase();
    let words: Vec<&str> = text.split_whitespace().collect();
    match words.as_slice() {
        ["hourly"] => Some(Schedule::Every(3600)),
        ["every", rest @ ..] if (1..=2).contains(&rest.len()) => {
            let joined = rest.concat();
            let split = joined.find(|c: char| !c.is_ascii_digit()).unwrap_or(joined.len());
            let (num, unit) = joined.split_at(split);
            let num: u64 = num.parse().ok()?;
            let secs = match unit {
                "h" | "hour" | "hours" => num * 3600,
                "m" | "min" | "mins" | "minutes" => num * 60,
                "d" | "day" | "days" => num * 86400,
                _ => return None,
            };
            // Anything tighter than 15 minutes is almost certainly a mistake.
            (secs >= 900).then_some(Schedule::Every(secs))
        }
        [days, time] => {
            let minute = parse_time(time)?;
            let days = match *days {
                "daily" | "everyday" => 0b111_1111,
                "weekdays" => 0b011_1110,
                "weekends" => 0b100_0001,
                list => list.split(',').try_fold(0u8, |mask, d| {
                    let d = d.trim();
                    let i = DAYS.iter().position(|name| d.starts_with(name))?;
                    Some(mask | 1 << i)
                })?,
            };
            (days != 0).then_some(Schedule::At { days, minute })
        }
        _ => None,
    }
}

/// Splits `---` frontmatter from the body. Keys are lowercased.
fn frontmatter(text: &str) -> (HashMap<String, String>, String) {
    let mut meta = HashMap::new();
    let Some(rest) = text.strip_prefix("---") else { return (meta, text.trim().to_string()) };
    let Some((head, body)) = rest.split_once("\n---") else { return (meta, text.trim().to_string()) };
    for line in head.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            // " # ..." after a value is a comment.
            let v = v.split(" #").next().unwrap_or("");
            meta.insert(k.trim().to_lowercase(), v.trim().to_string());
        }
    }
    let body = body.split_once('\n').map_or("", |(_, b)| b);
    (meta, body.trim().to_string())
}

pub fn parse(path: &Path, global: bool) -> Option<Workflow> {
    let text = std::fs::read_to_string(path).ok()?;
    let (meta, prompt) = frontmatter(&text);
    if prompt.is_empty() {
        return None;
    }
    let schedule_text = meta.get("schedule").filter(|s| !s.is_empty()).cloned();
    Some(Workflow {
        name: path.file_stem()?.to_string_lossy().replace(['-', '_'], " "),
        // Canonical, so runs and schedule records match however the notes
        // folder was reached.
        path: path.canonicalize().unwrap_or_else(|_| path.to_path_buf()),
        agent: meta.get("agent").cloned().unwrap_or_else(|| "claude".into()),
        schedule: schedule_text.as_deref().and_then(parse_schedule),
        schedule_text,
        prompt,
        global,
        collection: None,
        projects: meta
            .get("projects")
            .map(|p| p.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
            .unwrap_or_default(),
    })
}

fn list_dir(dir: &Path, global: bool) -> Vec<Workflow> {
    let mut out: Vec<Workflow> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .filter_map(|p| parse(&p, global))
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Workflows from every collection, marked with the collection's name. A
/// collection may keep them at its top level or in a `workflows/` folder.
fn list_collections() -> Vec<Workflow> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(collections_dir()).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let root = entry.path();
        let dir = if root.join("workflows").is_dir() { root.join("workflows") } else { root };
        out.extend(list_dir(&dir, true).into_iter().map(|mut w| {
            w.collection = Some(name.clone());
            w
        }));
    }
    out
}

/// Global workflows: your own, then collections'.
pub fn list_global() -> Vec<Workflow> {
    let mut out = list_dir(&global_dir(), true);
    out.extend(list_collections());
    out
}

/// The project's workflows, then global ones.
pub fn list(project: &Project) -> Vec<Workflow> {
    let mut out = list_dir(&project_dir(project), false);
    out.extend(list_global());
    out
}

pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

/// Creates `<name>.md` from the template in the project's workflow folder.
pub fn create(project: &Project, name: &str) -> PathBuf {
    let dir = project_dir(project);
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("{}.md", slug(name)));
    if !path.exists() {
        let _ = std::fs::write(&path, TEMPLATE);
    }
    path
}

// ── schedule bookkeeping ───────────────────────────────────────────────────

fn runs_file() -> PathBuf {
    config_dir().join("schedule.json")
}

/// When each scheduled workflow last ran (or was first seen), by path.
pub fn load_runs() -> HashMap<String, u64> {
    std::fs::read(runs_file())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save_runs(runs: &HashMap<String, u64>) {
    let file = runs_file();
    let _ = std::fs::create_dir_all(config_dir());
    let tmp = file.with_extension("json.tmp");
    if let Ok(json) = serde_json::to_vec_pretty(runs)
        && std::fs::write(&tmp, json).is_ok()
    {
        let _ = std::fs::rename(tmp, file);
    }
}

/// Local midnight `days_back` days before `now`, and that day's weekday.
fn local_day(now: u64, days_back: i64) -> Option<(u64, u32)> {
    unsafe {
        let t = now as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return None;
        }
        tm.tm_mday -= days_back as i32;
        tm.tm_hour = 0;
        tm.tm_min = 0;
        tm.tm_sec = 0;
        tm.tm_isdst = -1;
        let midnight = libc::mktime(&mut tm);
        (midnight >= 0).then_some((midnight as u64, tm.tm_wday as u32))
    }
}

/// The latest scheduled time at or before `now`.
pub fn latest_slot(schedule: &Schedule, now: u64) -> Option<u64> {
    let Schedule::At { days, minute } = schedule else { return None };
    (0..8).find_map(|back| {
        let (midnight, weekday) = local_day(now, back)?;
        let slot = midnight + *minute as u64 * 60;
        (days & (1 << weekday) != 0 && slot <= now).then_some(slot)
    })
}

/// Whether the workflow should run now, given when it last ran.
pub fn is_due(schedule: &Schedule, last: u64, now: u64) -> bool {
    match schedule {
        Schedule::Every(secs) => now >= last + secs,
        Schedule::At { .. } => latest_slot(schedule, now).is_some_and(|slot| slot > last),
    }
}

/// The scheduled workflows of all projects that are due now. A workflow seen
/// for the first time is recorded, not run, so adding one never fires it at
/// once.
pub fn due(projects: &[Project], now: u64) -> Vec<(String, Workflow)> {
    let mut runs = load_runs();
    let mut changed = false;
    let mut out = Vec::new();
    let global = list_global();
    for project in projects {
        let own = list_dir(&project_dir(project), false);
        let shared = global.iter().filter(|w| w.scheduled_for(project)).cloned();
        for wf in own.into_iter().chain(shared) {
            let Some(schedule) = &wf.schedule else { continue };
            let key = run_key(&wf, project);
            match runs.get(&key) {
                None => {
                    runs.insert(key, now);
                    changed = true;
                }
                Some(&last) if is_due(schedule, last, now) => out.push((project.id.clone(), wf)),
                Some(_) => {}
            }
        }
    }
    if changed {
        save_runs(&runs);
    }
    out
}

/// Schedule records are per workflow, and for global ones per project too.
fn run_key(wf: &Workflow, project: &Project) -> String {
    if wf.global {
        format!("{}#{}", wf.path.display(), project.id)
    } else {
        wf.path.to_string_lossy().into_owned()
    }
}

pub fn mark_ran(wf: &Workflow, project: &Project, now: u64) {
    let mut runs = load_runs();
    runs.insert(run_key(wf, project), now);
    save_runs(&runs);
}

// ── collections ────────────────────────────────────────────────────────────

fn git(args: &[&str], dir: Option<&Path>) -> Result<(), String> {
    let mut cmd = std::process::Command::new("git");
    if let Some(dir) = dir {
        cmd.arg("-C").arg(dir);
    }
    let out = cmd.args(args).env("GIT_TERMINAL_PROMPT", "0").output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// `codebench workflows add|update|list`.
pub fn cli(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("add") => {
            let Some(url) = args.get(1) else {
                eprintln!("usage: codebench workflows add <git url> [name]");
                return 2;
            };
            let name = args.get(2).cloned().unwrap_or_else(|| {
                url.trim_end_matches('/').trim_end_matches(".git").rsplit(['/', ':']).next().unwrap_or("collection").to_string()
            });
            let name = slug(&name);
            let dest = collections_dir().join(&name);
            if dest.exists() {
                eprintln!("a collection named {name} already exists. codebench workflows update refreshes it");
                return 1;
            }
            let _ = std::fs::create_dir_all(collections_dir());
            match git(&["clone", "--depth", "1", "--", url, &dest.to_string_lossy()], None) {
                Ok(()) => {
                    let n = list_collections().iter().filter(|w| w.collection.as_deref() == Some(&name)).count();
                    println!("added {name}: {n} workflows");
                    0
                }
                Err(e) => {
                    eprintln!("could not clone {url}: {e}");
                    1
                }
            }
        }
        Some("update") => {
            let mut failed = 0;
            for entry in std::fs::read_dir(collections_dir()).into_iter().flatten().flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                match git(&["pull", "--ff-only", "--quiet"], Some(&entry.path())) {
                    Ok(()) => println!("{name}: up to date"),
                    Err(e) => {
                        failed += 1;
                        eprintln!("{name}: {e}");
                    }
                }
            }
            i32::from(failed > 0)
        }
        Some("list") | None => {
            for wf in list_global() {
                let from = wf.collection.as_deref().unwrap_or("yours");
                let when = wf.schedule_text.as_deref().map(|s| format!("  [{s}]")).unwrap_or_default();
                println!("{:<28}{:<9}{from}{when}", wf.name, wf.agent);
            }
            println!("\nproject workflows live in each project's notes/workflows folder");
            0
        }
        _ => {
            eprintln!("usage: codebench workflows [list | add <git url> [name] | update]");
            2
        }
    }
}

/// Short local date and time for run titles, e.g. "09-24 09:00".
pub fn stamp(now: u64) -> String {
    unsafe {
        let t = now as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return String::new();
        }
        format!("{:02}-{:02} {:02}:{:02}", tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_schedules() {
        assert_eq!(parse_schedule("hourly"), Some(Schedule::Every(3600)));
        assert_eq!(parse_schedule("every 6h"), Some(Schedule::Every(6 * 3600)));
        assert_eq!(parse_schedule("every 30 min"), Some(Schedule::Every(1800)));
        assert_eq!(parse_schedule("every 5m"), None);
        assert_eq!(parse_schedule("daily 09:00"), Some(Schedule::At { days: 0b111_1111, minute: 540 }));
        assert_eq!(parse_schedule("weekdays 18:30"), Some(Schedule::At { days: 0b011_1110, minute: 1110 }));
        assert_eq!(parse_schedule("mon,thu 08:00"), Some(Schedule::At { days: 0b001_0010, minute: 480 }));
        assert_eq!(parse_schedule("daily 25:00"), None);
        assert_eq!(parse_schedule("sometimes"), None);
    }

    #[test]
    fn daily_slot_fires_once_per_day() {
        let daily = parse_schedule("daily 00:00").unwrap();
        let now = crate::store::now();
        let slot = latest_slot(&daily, now).unwrap();
        assert!(slot <= now && now - slot < 86400 + 3600);
        assert!(is_due(&daily, slot - 1, now));
        assert!(!is_due(&daily, slot, now));
        assert!(is_due(&Schedule::Every(3600), now - 3600, now));
        assert!(!is_due(&Schedule::Every(3600), now - 60, now));
    }

    #[test]
    fn global_schedules_name_their_projects() {
        let project = |name: &str| Project {
            id: "id".into(),
            name: name.into(),
            path: "/tmp".into(),
            sessions: Vec::new(),
            notes: None,
        };
        let dir = std::env::temp_dir().join(format!("cb-wf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("check.md");
        std::fs::write(&file, "---\nschedule: daily 09:00\nprojects: Compy, omaform\n---\nDo it.\n").unwrap();
        let wf = parse(&file, true).unwrap();
        assert!(wf.scheduled_for(&project("compy")));
        assert!(!wf.scheduled_for(&project("deep")));
        std::fs::write(&file, "---\nschedule: daily 09:00\n---\nDo it.\n").unwrap();
        assert!(!parse(&file, true).unwrap().scheduled_for(&project("compy")));
        std::fs::write(&file, "---\nschedule: daily 09:00\nprojects: all\n---\nDo it.\n").unwrap();
        assert!(parse(&file, true).unwrap().scheduled_for(&project("deep")));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reads_frontmatter() {
        let (meta, body) = frontmatter("---\nagent: codex\n# note\nschedule: daily 09:00   # every day\n---\nDo the thing.\n");
        assert_eq!(meta["agent"], "codex");
        assert_eq!(meta["schedule"], "daily 09:00");
        assert_eq!(body, "Do the thing.");
        let (meta, body) = frontmatter("Just a prompt");
        assert!(meta.is_empty());
        assert_eq!(body, "Just a prompt");
    }
}
