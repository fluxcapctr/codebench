//! Servers running in a project: processes of yours whose working folder is
//! inside the project (or one of its worktrees) and that listen on a TCP
//! port, like the dev server an agent started and forgot about.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Server {
    pub pid: i32,
    pub command: String,
    pub ports: Vec<u16>,
    pub cwd: PathBuf,
}

/// Listening sockets from /proc/net/tcp and tcp6: inode -> port.
fn listening() -> HashMap<u64, u16> {
    let mut out = HashMap::new();
    for file in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(text) = std::fs::read_to_string(file) else { continue };
        out.extend(parse_listening(&text));
    }
    out
}

fn parse_listening(text: &str) -> Vec<(u64, u16)> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let cols: Vec<&str> = line.split_whitespace().collect();
            // local_address is "ADDR:PORT" in hex; state 0A is LISTEN.
            if cols.get(3) != Some(&"0A") {
                return None;
            }
            let port = u16::from_str_radix(cols.get(1)?.rsplit(':').next()?, 16).ok()?;
            let inode = cols.get(9)?.parse().ok()?;
            Some((inode, port))
        })
        .collect()
}

/// Servers whose working folder is under one of `roots`. Codebench itself
/// is left out.
pub fn in_folders(roots: &[PathBuf]) -> Vec<Server> {
    let sockets = listening();
    if sockets.is_empty() {
        return Vec::new();
    }
    let me = std::process::id() as i32;
    let mut out = Vec::new();
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<i32>().ok()) else { continue };
        if pid == me {
            continue;
        }
        let proc_dir = entry.path();
        let Ok(cwd) = std::fs::read_link(proc_dir.join("cwd")) else { continue };
        if !roots.iter().any(|r| cwd.starts_with(r)) {
            continue;
        }
        let mut ports: Vec<u16> = std::fs::read_dir(proc_dir.join("fd"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|fd| {
                let target = std::fs::read_link(fd.path()).ok()?;
                let inode = target.to_str()?.strip_prefix("socket:[")?.strip_suffix(']')?.parse().ok()?;
                sockets.get(&inode).copied()
            })
            .collect();
        if ports.is_empty() {
            continue;
        }
        ports.sort_unstable();
        ports.dedup();
        let command = std::fs::read(proc_dir.join("cmdline"))
            .map(|b| b.split(|c| *c == 0).filter(|p| !p.is_empty()).map(|p| String::from_utf8_lossy(p).into_owned()).collect::<Vec<_>>().join(" "))
            .unwrap_or_default();
        out.push(Server { pid, command, ports, cwd });
    }
    out.sort_by_key(|s| s.ports.first().copied().unwrap_or(0));
    out
}

pub fn stop(pid: i32) {
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
}

/// A short form of a long command line.
pub fn short_command(command: &str, cwd: &Path) -> String {
    let mut parts: Vec<String> = command
        .split_whitespace()
        .map(|p| p.strip_prefix(&format!("{}/", cwd.display())).unwrap_or(p).to_string())
        .collect();
    if let Some(first) = parts.first_mut() {
        *first = first.rsplit('/').next().unwrap_or(first).to_string();
    }
    let joined = parts.join(" ");
    if joined.chars().count() > 70 { joined.chars().take(69).chain(['…']).collect() } else { joined }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_listening_sockets() {
        let text = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
           0: 0100007F:1435 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 55501 1 0\n\
           1: 0100007F:9C40 0100007F:D2A2 01 00000000:00000000 00:00000000 00000000  1000        0 55502 1 0\n";
        assert_eq!(parse_listening(text), vec![(55501, 5173)]);
    }

    #[test]
    fn shortens_commands() {
        assert_eq!(short_command("/usr/bin/node /p/node_modules/.bin/vite --port 5173", Path::new("/p")), "node node_modules/.bin/vite --port 5173");
    }
}
