//! Which model subscriptions each agent CLI is logged into, and the commands
//! to log in or switch accounts. The checks run the CLIs, so call them off
//! the main thread.

use crate::agents;
use crate::store::home;
use std::process::Command;

#[derive(Clone, Debug)]
pub enum Login {
    In(String),
    Out,
    Unknown(String),
}

#[derive(Clone, Debug)]
pub struct Account {
    pub agent: &'static str,
    pub login: Login,
}

fn run(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program)
        .args(args)
        .current_dir(home())
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Some(text)
}

fn env_set(vars: &[&str]) -> Option<String> {
    vars.iter()
        .find(|v| std::env::var_os(v).is_some_and(|x| !x.is_empty()))
        .map(|v| format!("API key from ${v}"))
}

pub fn check(agent: &'static str) -> Account {
    let login = match agent {
        "claude" => match run("claude", &["auth", "status"]).map(|t| (serde_json::from_str::<serde_json::Value>(&t).ok(), t)) {
            Some((Some(v), _)) if v["loggedIn"].as_bool() == Some(true) => {
                let plan = v["subscriptionType"].as_str().map(|p| format!("{p} plan")).unwrap_or_default();
                let method = v["authMethod"].as_str().unwrap_or("");
                let who = v["orgName"].as_str().unwrap_or("");
                Login::In([plan.as_str(), method, who].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" · "))
            }
            Some((Some(_), _)) => Login::Out,
            Some((None, text)) => Login::Unknown(text.lines().find(|l| !l.trim().is_empty()).unwrap_or("no output").chars().take(70).collect()),
            None => Login::Unknown("claude is not runnable".into()),
        },
        "codex" => match run("codex", &["login", "status"]) {
            Some(t) if t.contains("Logged in") => Login::In(t.lines().find(|l| l.contains("Logged in")).unwrap_or("").trim().replace("Logged in using ", "")),
            Some(t) if t.to_lowercase().contains("not logged in") => Login::Out,
            Some(t) => Login::Unknown(t.lines().next().unwrap_or("").to_string()),
            None => Login::Unknown("codex login status failed".into()),
        },
        "grok" => match run("grok", &["--no-auto-update", "models"]) {
            Some(t) if t.contains("not authenticated") => env_set(&["XAI_API_KEY"]).map_or(Login::Out, Login::In),
            Some(_) => Login::In("xAI account".into()),
            None => Login::Unknown("grok models failed".into()),
        },
        "gemini" => {
            let creds = home().join(".gemini/oauth_creds.json").is_file();
            let who = std::fs::read_to_string(home().join(".gemini/google_accounts.json"))
                .ok()
                .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                .and_then(|v| v["active"].as_str().map(str::to_string));
            match (creds, env_set(&["GEMINI_API_KEY", "GOOGLE_API_KEY"])) {
                (true, _) => Login::In(who.map_or("Google account".into(), |w| format!("Google · {w}"))),
                (false, Some(key)) => Login::In(key),
                _ => Login::Out,
            }
        }
        "opencode" => match run("opencode", &["auth", "list"]) {
            Some(t) if t.contains(" 0 credentials") => Login::Out,
            Some(t) => {
                let n = t.split_whitespace().collect::<Vec<_>>().windows(2).find(|w| w[1] == "credentials").map(|w| w[0].to_string());
                Login::In(n.map_or("credentials set".into(), |n| format!("{n} providers")))
            }
            None => Login::Unknown("opencode auth list failed".into()),
        },
        "agy" => match run("agy", &["models"]) {
            Some(t) if t.lines().any(|l| l.contains('\t')) => Login::In("Google account".into()),
            Some(t) if t.to_lowercase().contains("sign in") || t.to_lowercase().contains("log in") => Login::Out,
            _ => Login::Unknown("agy models failed".into()),
        },
        _ => Login::Unknown(String::new()),
    };
    Account { agent, login }
}

/// All installed agents, checked in parallel.
pub fn check_all() -> Vec<Account> {
    let handles: Vec<_> = agents::installed()
        .into_iter()
        .filter(|a| a.id != "shell")
        .map(|a| std::thread::spawn(move || check(a.id)))
        .collect();
    handles.into_iter().filter_map(|h| h.join().ok()).collect()
}

/// Shell command that signs in (or, with `switch`, signs out first).
pub fn login_command(agent: &str, switch: bool) -> Option<String> {
    let (login, logout) = match agent {
        "claude" => ("claude auth login", Some("claude auth logout")),
        "codex" => ("codex login", Some("codex logout")),
        "grok" => ("grok --no-auto-update login", Some("grok --no-auto-update logout")),
        "opencode" => ("opencode auth login", Some("opencode auth logout")),
        // These sign in from inside their own TUI (/auth, /logout).
        "gemini" => ("echo 'Use /auth inside Gemini to sign in or switch accounts.'; gemini", None),
        "agy" => ("echo 'Use /logout, then sign in again, inside Antigravity.'; agy", None),
        _ => return None,
    };
    Some(match (switch, logout) {
        (true, Some(out)) => format!("{out}; {login}"),
        _ => login.to_string(),
    })
}
