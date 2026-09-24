mod accounts;
mod artifacts;
mod awake;
mod agents;
mod bus;
mod chat;
mod git;
mod headless;
mod history;
mod mcp;
mod limits;
mod notes;
mod processes;
mod progress;
mod push;
mod remote;
mod store;
mod theme;
mod ui;
mod usage;
mod workflow;

/// Markers a parent Claude Code session sets. Inherited by the agents we
/// spawn, they make Claude treat itself as a subagent and stop saving
/// transcripts, which breaks resume.
const PARENT_SESSION_VARS: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
];

fn main() -> gtk::glib::ExitCode {
    store::make_private_dirs();

    // `codebench mcp --session <id>`: the per-task MCP server agents launch.
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("mcp") {
        let session = args
            .iter()
            .position(|a| a == "--session")
            .and_then(|i| args.get(i + 1))
            .cloned()
            .unwrap_or_default();
        mcp::serve(session);
        std::process::exit(0);
    }
    match args.get(1).map(String::as_str) {
        Some("run-due") => {
            headless::run_due();
            std::process::exit(0);
        }
        Some("status") => {
            headless::status(args.iter().any(|a| a == "--follow"));
            std::process::exit(0);
        }
        Some("workflows") => std::process::exit(workflow::cli(&args[2..])),
        Some("accounts") => {
            for a in accounts::check_all() {
                let update = a.update.map(|(c, l)| format!("   (update {c} -> {l})")).unwrap_or_default();
                match a.login {
                    _ if !a.installed => println!("{:<10}not installed", a.agent),
                    accounts::Login::In(d) => println!("{:<10}signed in  {d}{update}", a.agent),
                    accounts::Login::Out => println!("{:<10}signed out{update}", a.agent),
                    accounts::Login::Unknown(w) => println!("{:<10}unknown    {w}{update}", a.agent),
                }
            }
            for u in limits::check_all() {
                let parts: Vec<String> = u.limits.iter().map(|l| format!("{} {}% (resets {})", l.name, l.percent, l.resets)).collect();
                println!("{:<10}{}", u.agent, parts.join(", "));
            }
            std::process::exit(0);
        }
        Some("schedule") => std::process::exit(headless::schedule(args.get(2).map(String::as_str))),
        _ => {}
    }

    for var in PARENT_SESSION_VARS {
        // SAFETY: runs before GTK or any other thread starts.
        unsafe { std::env::remove_var(var) };
    }
    ui::run()
}
