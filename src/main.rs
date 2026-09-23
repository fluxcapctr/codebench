mod agents;
mod bus;
mod mcp;
mod notes;
mod store;
mod theme;
mod ui;
mod usage;

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

    for var in PARENT_SESSION_VARS {
        // SAFETY: runs before GTK or any other thread starts.
        unsafe { std::env::remove_var(var) };
    }
    ui::run()
}
