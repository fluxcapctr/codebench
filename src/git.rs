//! The little git Codebench needs: status counts for the sidebar, and
//! worktrees that give a task its own checkout and branch.

use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() {
        Ok(text)
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() { text } else { err })
    }
}

/// `git init` with a main branch.
pub fn init(dir: &Path) -> Result<(), String> {
    git(dir, &["init", "-q", "-b", "main"]).map(|_| ())
}

pub fn is_repo(dir: &Path) -> bool {
    git(dir, &["rev-parse", "--is-inside-work-tree"]).is_ok_and(|s| s == "true")
}

/// Whether HEAD points at a commit. A fresh `git init` has none yet, and a
/// worktree cannot be made until it does.
pub fn has_commit(dir: &Path) -> bool {
    git(dir, &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]).is_ok()
}

pub fn branch(dir: &Path) -> Option<String> {
    git(dir, &["branch", "--show-current"]).ok().filter(|b| !b.is_empty())
}

/// Files with uncommitted changes, including untracked ones.
pub fn changed_files(dir: &Path) -> Option<usize> {
    git(dir, &["status", "--porcelain"]).ok().map(|s| s.lines().count())
}

/// Commits on `branch` that `base` does not have, and files they touch.
pub fn ahead(repo: &Path, base: &str, branch: &str) -> Option<(usize, usize)> {
    let range = format!("{base}...{branch}");
    let commits = git(repo, &["rev-list", "--count", &format!("{base}..{branch}")]).ok()?.parse().ok()?;
    let files = git(repo, &["diff", "--name-only", &range]).ok()?.lines().count();
    Some((commits, files))
}

/// Creates a worktree at `dir` on a new branch from the current HEAD.
/// Returns the branch it started from.
pub fn add_worktree(repo: &Path, dir: &Path, branch: &str) -> Result<String, String> {
    let base = self::branch(repo).ok_or("the project is not on a branch")?;
    if !has_commit(repo) {
        return Err(format!("{base} has no commits yet. Commit something first to give tasks their own worktree"));
    }
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    git(repo, &["worktree", "add", "-b", branch, &dir.to_string_lossy(), &base])?;
    Ok(base)
}

/// Removes the worktree and its branch, discarding anything not merged.
pub fn remove_worktree(repo: &Path, dir: &Path, branch: &str) {
    let _ = git(repo, &["worktree", "remove", "--force", &dir.to_string_lossy()]);
    let _ = git(repo, &["branch", "-D", branch]);
}

/// Commits everything in `dir`. Returns false if there was nothing to commit.
pub fn commit_all(dir: &Path, message: &str) -> Result<bool, String> {
    if changed_files(dir) == Some(0) {
        return Ok(false);
    }
    git(dir, &["add", "-A"])?;
    git(dir, &["commit", "-m", message])?;
    Ok(true)
}

pub enum Merge {
    Merged,
    UpToDate,
    Conflicts,
}

/// Merges `branch` into whatever `repo` has checked out.
pub fn merge(repo: &Path, branch: &str, message: &str) -> Result<Merge, String> {
    match git(repo, &["merge", "--no-ff", "-m", message, branch]) {
        Ok(out) if out.contains("Already up to date") => Ok(Merge::UpToDate),
        Ok(_) => Ok(Merge::Merged),
        Err(err) => {
            let conflicted = git(repo, &["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default();
            if conflicted.is_empty() { Err(err) } else { Ok(Merge::Conflicts) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cb-git-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "t@t"],
            &["config", "user.name", "t"],
            &["commit", "-q", "--allow-empty", "-m", "start"],
        ] {
            git(&dir, args).unwrap();
        }
        dir
    }

    #[test]
    fn worktree_round_trip() {
        let repo = repo("round-trip");
        let wt = repo.with_extension("wt");
        assert_eq!(add_worktree(&repo, &wt, "cb/task").unwrap(), "main");
        std::fs::write(wt.join("a.txt"), "a").unwrap();
        assert_eq!(changed_files(&wt), Some(1));
        assert!(commit_all(&wt, "task work").unwrap());
        assert_eq!(ahead(&repo, "main", "cb/task"), Some((1, 1)));
        assert!(matches!(merge(&repo, "cb/task", "merge task").unwrap(), Merge::Merged));
        assert!(repo.join("a.txt").is_file());
        assert!(matches!(merge(&repo, "cb/task", "again").unwrap(), Merge::UpToDate));
        remove_worktree(&repo, &wt, "cb/task");
        assert!(!wt.exists());
        std::fs::remove_dir_all(repo).unwrap();
    }

    #[test]
    fn new_repositories_need_a_commit_for_worktrees() {
        let dir = std::env::temp_dir().join(format!("cb-git-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        init(&dir).unwrap();
        assert!(is_repo(&dir) && !has_commit(&dir));
        let err = add_worktree(&dir, &dir.with_extension("wt"), "cb/probe").unwrap_err();
        assert!(err.contains("no commits"), "{err}");
        assert!(!dir.with_extension("wt").exists());
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(!has_commit(&dir));
        let full = repo("has-commit");
        assert!(has_commit(&full));
        std::fs::remove_dir_all(full).unwrap();
    }

    #[test]
    fn merge_reports_conflicts() {
        let repo = repo("conflicts");
        let wt = repo.with_extension("wt");
        add_worktree(&repo, &wt, "cb/x").unwrap();
        std::fs::write(wt.join("f"), "task").unwrap();
        commit_all(&wt, "task").unwrap();
        std::fs::write(repo.join("f"), "main").unwrap();
        commit_all(&repo, "main").unwrap();
        assert!(matches!(merge(&repo, "cb/x", "m").unwrap(), Merge::Conflicts));
        remove_worktree(&repo, &wt, "cb/x");
        std::fs::remove_dir_all(repo).unwrap();
    }
}
