use std::path::Path;
use std::process::Command;

use anyhow::{bail, Result};

const SESSION_PREFIX: &str = "cm-";

#[derive(Debug, Clone)]
pub struct TmuxSession {
    pub name: String,
    pub project_name: String,
    pub session_name: String,
}

impl TmuxSession {
    pub fn from_tmux_name(name: &str) -> Option<Self> {
        let rest = name.strip_prefix(SESSION_PREFIX)?;
        let (project_name, session_name) = rest.split_once('-')?;
        Some(TmuxSession {
            name: name.to_string(),
            project_name: project_name.to_string(),
            session_name: session_name.to_string(),
        })
    }
}

pub fn list_sessions() -> Result<Vec<TmuxSession>> {
    let output = Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}"])
        .output();

    let output = match output {
        Ok(o) => o,
        Err(_) => return Ok(vec![]),
    };

    if !output.status.success() {
        return Ok(vec![]);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .filter_map(TmuxSession::from_tmux_name)
        .collect())
}

pub fn create_session(
    project_name: &str,
    project_path: &str,
    session_name: &str,
    use_worktree: bool,
) -> Result<String> {
    let tmux_name = format!("{SESSION_PREFIX}{project_name}-{session_name}");

    let mut claude_cmd = String::from("claude --dangerously-skip-permissions");
    if use_worktree {
        claude_cmd.push_str(" --worktree");
    }

    let status = Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            &tmux_name,
            "-c",
            project_path,
            &claude_cmd,
        ])
        .status()?;

    if !status.success() {
        bail!("Failed to create tmux session");
    }

    // Store worktree flag and project path in tmux environment for cleanup
    if use_worktree {
        let _ = Command::new("tmux")
            .args(["set-environment", "-t", &tmux_name, "CM_WORKTREE", "1"])
            .status();
    }
    let _ = Command::new("tmux")
        .args([
            "set-environment",
            "-t",
            &tmux_name,
            "CM_PROJECT_PATH",
            project_path,
        ])
        .status();

    Ok(tmux_name)
}

pub fn attach_session(name: &str) -> Result<()> {
    let status = Command::new("tmux")
        .args(["attach-session", "-t", name])
        .status()?;

    if !status.success() {
        bail!("Failed to attach to tmux session");
    }

    Ok(())
}

/// Get a tmux environment variable for a session.
fn get_session_env(session_name: &str, var: &str) -> Option<String> {
    let output = Command::new("tmux")
        .args(["show-environment", "-t", session_name, var])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let line = String::from_utf8_lossy(&output.stdout);
    // Format is "VAR=value\n"
    line.trim().split_once('=').map(|(_, v)| v.to_string())
}

/// Get the current working directory of the first pane in the session.
fn get_pane_cwd(session_name: &str) -> Option<String> {
    let output = Command::new("tmux")
        .args([
            "display-message",
            "-t",
            session_name,
            "-p",
            "#{pane_current_path}",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() { None } else { Some(path) }
}

/// Check if a path is a git worktree (has a .git file pointing to the main repo).
fn is_git_worktree(path: &str) -> bool {
    let git_path = Path::new(path).join(".git");
    git_path.is_file() // worktrees have a .git *file*, not a directory
}

/// Remove a git worktree directory.
fn remove_worktree(project_path: &str, worktree_path: &str) -> Result<()> {
    let status = Command::new("git")
        .args(["-C", project_path, "worktree", "remove", "--force", worktree_path])
        .status()?;

    if !status.success() {
        bail!("Failed to remove worktree at {worktree_path}");
    }

    Ok(())
}

/// List all worktree paths for a project (excluding the main working tree).
fn list_worktrees(project_path: &str) -> Vec<String> {
    let output = Command::new("git")
        .args(["-C", project_path, "worktree", "list", "--porcelain"])
        .output();

    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return vec![],
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut worktrees = Vec::new();
    let mut current_path = None;
    let mut is_bare = false;

    for line in stdout.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current_path = Some(path.to_string());
            is_bare = false;
        } else if line == "bare" {
            is_bare = true;
        } else if line.is_empty() {
            if let Some(path) = current_path.take() {
                if !is_bare && path != project_path {
                    worktrees.push(path);
                }
            }
        }
    }
    // Handle last entry
    if let Some(path) = current_path {
        if !is_bare && path != project_path {
            worktrees.push(path);
        }
    }

    worktrees
}

pub fn kill_session(name: &str, project_path: Option<&str>) -> Result<()> {
    // Gather worktree info before killing
    let was_worktree = get_session_env(name, "CM_WORKTREE").is_some();
    let session_project_path =
        get_session_env(name, "CM_PROJECT_PATH").or_else(|| project_path.map(String::from));
    let pane_cwd = get_pane_cwd(name);

    // Collect worktrees before kill so we can diff after
    let worktrees_before: Vec<String> = session_project_path
        .as_deref()
        .map(list_worktrees)
        .unwrap_or_default();

    // Kill the tmux session
    let status = Command::new("tmux")
        .args(["kill-session", "-t", name])
        .status()?;

    if !status.success() {
        bail!("Failed to kill tmux session");
    }

    // Clean up worktree if applicable
    if was_worktree {
        if let Some(ref proj_path) = session_project_path {
            // Strategy 1: Try the pane's cwd if it's a worktree
            if let Some(ref cwd) = pane_cwd {
                if is_git_worktree(cwd) {
                    let _ = remove_worktree(proj_path, cwd);
                    return Ok(());
                }
            }

            // Strategy 2: Find worktrees that still exist after kill and remove orphans.
            // Claude Code's worktree might still be around if the process was killed.
            let worktrees_after = list_worktrees(proj_path);
            // Any worktree that existed before and still exists might be orphaned.
            // We check each one - if no tmux session is using it, clean it up.
            let active_sessions = list_sessions().unwrap_or_default();
            for wt in &worktrees_before {
                if !worktrees_after.contains(wt) {
                    continue; // already cleaned up
                }
                // Check if any active session is using this worktree
                let in_use = active_sessions.iter().any(|s| {
                    get_pane_cwd(&s.name)
                        .is_some_and(|cwd| cwd.starts_with(wt.as_str()))
                });
                if !in_use {
                    let _ = remove_worktree(proj_path, wt);
                }
            }
        }
    }

    Ok(())
}

pub fn rename_session(old_name: &str, new_name: &str) -> Result<()> {
    let status = Command::new("tmux")
        .args(["rename-session", "-t", old_name, new_name])
        .status()?;

    if !status.success() {
        bail!("Failed to rename tmux session from {old_name} to {new_name}");
    }

    Ok(())
}

/// Capture the visible content of a tmux session's pane.
pub fn capture_pane(session_name: &str) -> Option<String> {
    let output = Command::new("tmux")
        .args([
            "capture-pane",
            "-t",
            session_name,
            "-p", // print to stdout
            "-e", // include escape sequences (for colors, though we strip them)
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    Some(String::from_utf8_lossy(&output.stdout).to_string())
}

pub fn next_session_number(project_name: &str, sessions: &[TmuxSession]) -> u32 {
    let max = sessions
        .iter()
        .filter(|s| s.project_name == project_name)
        .filter_map(|s| s.session_name.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    max + 1
}
