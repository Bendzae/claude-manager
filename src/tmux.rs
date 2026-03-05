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
        // tmux returns error when no server is running
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

pub fn kill_session(name: &str) -> Result<()> {
    let status = Command::new("tmux")
        .args(["kill-session", "-t", name])
        .status()?;

    if !status.success() {
        bail!("Failed to kill tmux session");
    }

    Ok(())
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
