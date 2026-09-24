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
    pub installed: bool,
    /// (current, latest) when a newer version is out.
    pub update: Option<(String, String)>,
}

/// The mise tool name for each agent, when installed through mise.
fn mise_tool(agent: &str) -> Option<&'static str> {
    match agent {
        "claude" => Some("claude"),
        "codex" => Some("codex"),
        "gemini" => Some("gemini"),
        "opencode" => Some("opencode"),
        "grok" => Some("npm:@xai-official/grok"),
        _ => None,
    }
}

fn mise_managed() -> Vec<String> {
    run("mise", &["ls", "--current", "--json"])
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.as_object().map(|o| o.keys().cloned().collect()))
        .unwrap_or_default()
}

/// Newer versions of mise-managed tools: tool -> (current, latest).
fn mise_outdated() -> std::collections::HashMap<String, (String, String)> {
    run("mise", &["outdated", "--json"])
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| {
            v.as_object().map(|o| {
                o.iter()
                    .filter_map(|(k, t)| Some((k.clone(), (t["current"].as_str()?.to_string(), t["latest"].as_str()?.to_string()))))
                    .collect()
            })
        })
        .unwrap_or_default()
}

/// Shell command that updates the agent's CLI, if Codebench knows one.
pub fn update_command(agent: &str) -> Option<String> {
    if let Some(tool) = mise_tool(agent).filter(|t| mise_managed().iter().any(|m| m == t)) {
        return Some(format!("mise upgrade '{tool}'"));
    }
    match agent {
        "claude" => Some("claude update".into()),
        "codex" => Some("codex update".into()),
        "grok" => Some("grok update".into()),
        "opencode" => Some("opencode upgrade".into()),
        "agy" => Some("agy update".into()),
        "cursor" => Some("cursor-agent update".into()),
        _ => None,
    }
}

/// Shell command that installs a missing agent's CLI through mise.
pub fn install_command(agent: &str) -> Option<String> {
    let tool = mise_tool(agent)?;
    agents::on_path("mise").then(|| format!("mise use -g '{tool}@latest'"))
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
        "cursor" => match run("cursor-agent", &["status"]) {
            Some(t) if t.to_lowercase().contains("not logged in") => Login::Out,
            Some(t) if t.to_lowercase().contains("logged in") => Login::In(
                t.lines().find(|l| l.to_lowercase().contains("logged in")).unwrap_or("").trim().to_string(),
            ),
            _ => Login::Unknown("cursor-agent status failed".into()),
        },
        "local" => match agents::ollama_models().len() {
            0 => Login::Unknown("no Ollama models pulled".into()),
            n => Login::In(format!("{n} Ollama models")),
        },
        "agy" => match run("agy", &["models"]) {
            Some(t) if t.lines().any(|l| l.contains('\t')) => Login::In("Google account".into()),
            Some(t) if t.to_lowercase().contains("sign in") || t.to_lowercase().contains("log in") => Login::Out,
            _ => Login::Unknown("agy models failed".into()),
        },
        _ => Login::Unknown(String::new()),
    };
    Account { agent, login, installed: true, update: None }
}

/// Every known agent: installed ones checked in parallel, with any update
/// mise knows about; missing ones listed as not installed.
pub fn check_all() -> Vec<Account> {
    let outdated = std::thread::spawn(mise_outdated);
    let installed: Vec<&str> = agents::installed().iter().map(|a| a.id).collect();
    let handles: Vec<_> = agents::AGENTS
        .iter()
        .filter(|a| a.id != "shell")
        .map(|a| {
            let present = installed.contains(&a.id);
            std::thread::spawn(move || {
                if present {
                    check(a.id)
                } else {
                    Account { agent: a.id, login: Login::Unknown("not installed".into()), installed: false, update: None }
                }
            })
        })
        .collect();
    let outdated = outdated.join().unwrap_or_default();
    handles
        .into_iter()
        .filter_map(|h| h.join().ok())
        .map(|mut a| {
            a.update = mise_tool(a.agent).and_then(|t| outdated.get(t).cloned());
            a
        })
        .collect()
}

/// Shell command that signs in (or, with `switch`, signs out first).
pub fn login_command(agent: &str, switch: bool) -> Option<String> {
    let (login, logout) = match agent {
        "claude" => ("claude auth login", Some("claude auth logout")),
        "codex" => ("codex login", Some("codex logout")),
        "grok" => ("grok --no-auto-update login", Some("grok --no-auto-update logout")),
        "opencode" => ("opencode auth login", Some("opencode auth logout")),
        "cursor" => ("cursor-agent login", Some("cursor-agent logout")),
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
