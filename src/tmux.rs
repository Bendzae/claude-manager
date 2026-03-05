use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Result};

const SESSION_SEP: &str = "__";
const PERMISSION_PROMPTS: &[&str] = &[
    "Do you want to",
    "Yes, allow all",
    "No, and tell Claude what to do differently",
];

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SessionStatus {
    Running,
    WaitingForInput,
    WaitingForPermission,
    Finished,
}

#[derive(Debug, Clone)]
pub struct TmuxSession {
    pub name: String,
    pub project_name: String,
    pub task_name: String,
    pub session_name: String,
}

impl TmuxSession {
    /// Parse a tmux session name like `cm__project__task__session`.
    pub fn from_tmux_name(name: &str) -> Option<Self> {
        let rest = name.strip_prefix("cm")?;
        let rest = rest.strip_prefix(SESSION_SEP)?;
        let (project_name, rest) = rest.split_once(SESSION_SEP)?;
        let (task_name, session_name) = rest.split_once(SESSION_SEP)?;
        Some(TmuxSession {
            name: name.to_string(),
            project_name: project_name.to_string(),
            task_name: task_name.to_string(),
            session_name: session_name.to_string(),
        })
    }

    /// Returns the worktree path if this session has one.
    pub fn worktree_path(&self) -> Option<PathBuf> {
        let path = worktree_dir(&self.project_name, &self.task_name, &self.session_name);
        if path.exists() { Some(path) } else { None }
    }
}

/// Sanitize a name for use in tmux session names.
/// Replaces problematic characters and ensures no double underscores.
pub fn sanitize(s: &str) -> String {
    let s: String = s
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    // Collapse multiple hyphens
    let mut result = String::new();
    let mut prev_hyphen = false;
    for c in s.chars() {
        if c == '-' {
            if !prev_hyphen {
                result.push(c);
            }
            prev_hyphen = true;
        } else {
            result.push(c);
            prev_hyphen = false;
        }
    }
    result
        .trim_matches('-')
        .replace("__", "_")
        .to_string()
}

/// Generate a branch name from a task name.
pub fn to_branch_name(task_name: &str) -> String {
    let s: String = task_name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let mut result = String::new();
    let mut prev_hyphen = true; // skip leading hyphens
    for c in s.chars() {
        if c == '-' {
            if !prev_hyphen {
                result.push(c);
            }
            prev_hyphen = true;
        } else {
            result.push(c);
            prev_hyphen = false;
        }
    }
    result.trim_end_matches('-').to_string()
}

fn build_tmux_name(project: &str, task: &str, session: &str) -> String {
    format!(
        "cm{sep}{}{sep}{}{sep}{}",
        sanitize(project),
        sanitize(task),
        sanitize(session),
        sep = SESSION_SEP
    )
}

pub fn worktree_dir(project_name: &str, task: &str, session: &str) -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("~/.local/share"))
        .join("claude-manager")
        .join("worktrees")
        .join(sanitize(project_name))
        .join(format!("{}-{}", sanitize(task), sanitize(session)))
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

/// Pull latest main and create a task branch from it.
pub fn create_task_branch(project_path: &str, branch_name: &str) -> Result<()> {
    // Try to fetch latest main from origin
    let _ = Command::new("git")
        .args(["-C", project_path, "fetch", "origin", "main"])
        .output();

    // Try creating from origin/main first, fall back to local main
    let status = Command::new("git")
        .args(["-C", project_path, "branch", branch_name, "origin/main"])
        .output()?;

    if !status.status.success() {
        let status = Command::new("git")
            .args(["-C", project_path, "branch", branch_name, "main"])
            .status()?;
        if !status.success() {
            bail!("Failed to create branch {branch_name}");
        }
    }

    Ok(())
}

pub fn create_session(
    project_name: &str,
    project_path: &str,
    task_name: &str,
    task_branch: &str,
    session_name: &str,
    use_worktree: bool,
) -> Result<String> {
    let tmux_name = build_tmux_name(project_name, task_name, session_name);

    let work_dir;
    let mut worktree_path_str = String::new();

    if use_worktree {
        let wt_path = worktree_dir(project_name, task_name, session_name);
        worktree_path_str = wt_path.to_string_lossy().to_string();

        // Create parent directories
        if let Some(parent) = wt_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Create worktree with a session-specific branch based on task branch
        let session_branch = format!("{task_branch}-{}", sanitize(session_name));
        let status = Command::new("git")
            .args([
                "-C",
                project_path,
                "worktree",
                "add",
                "-b",
                &session_branch,
                &worktree_path_str,
                task_branch,
            ])
            .output()?;

        if !status.status.success() {
            let stderr = String::from_utf8_lossy(&status.stderr);
            bail!("Failed to create worktree: {stderr}");
        }

        work_dir = worktree_path_str.clone();
    } else {
        work_dir = project_path.to_string();
    }

    let claude_cmd = "claude --dangerously-skip-permissions";

    let status = Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            &tmux_name,
            "-c",
            &work_dir,
            claude_cmd,
        ])
        .status()?;

    if !status.success() {
        bail!("Failed to create tmux session");
    }

    // Store metadata in tmux environment for cleanup
    let _ = Command::new("tmux")
        .args([
            "set-environment",
            "-t",
            &tmux_name,
            "CM_PROJECT_PATH",
            project_path,
        ])
        .status();

    if use_worktree {
        let _ = Command::new("tmux")
            .args([
                "set-environment",
                "-t",
                &tmux_name,
                "CM_WORKTREE_PATH",
                &worktree_path_str,
            ])
            .status();

        // Store the base commit so we can diff against it later
        if let Ok(output) = Command::new("git")
            .args(["-C", &worktree_path_str, "rev-parse", "HEAD"])
            .output()
        {
            if output.status.success() {
                let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
                let _ = Command::new("tmux")
                    .args([
                        "set-environment",
                        "-t",
                        &tmux_name,
                        "CM_BASE_COMMIT",
                        &sha,
                    ])
                    .status();
            }
        }
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

fn get_session_env(session_name: &str, var: &str) -> Option<String> {
    let output = Command::new("tmux")
        .args(["show-environment", "-t", session_name, var])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let line = String::from_utf8_lossy(&output.stdout);
    line.trim().split_once('=').map(|(_, v)| v.to_string())
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

pub fn kill_session(name: &str) -> Result<()> {
    let project_path = get_session_env(name, "CM_PROJECT_PATH");
    let worktree_path = get_session_env(name, "CM_WORKTREE_PATH");

    // Kill the tmux session
    let status = Command::new("tmux")
        .args(["kill-session", "-t", name])
        .status()?;

    if !status.success() {
        bail!("Failed to kill tmux session");
    }

    // Clean up worktree if applicable
    if let (Some(proj_path), Some(wt_path)) = (project_path, worktree_path) {
        if Path::new(&wt_path).exists() {
            let _ = Command::new("git")
                .args([
                    "-C",
                    &proj_path,
                    "worktree",
                    "remove",
                    "--force",
                    &wt_path,
                ])
                .status();
        }
    }

    Ok(())
}

pub fn capture_pane(session_name: &str) -> Option<String> {
    let output = Command::new("tmux")
        .args([
            "capture-pane",
            "-t",
            session_name,
            "-p",
            "-e",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    Some(String::from_utf8_lossy(&output.stdout).to_string())
}

#[derive(Debug, Clone, Default)]
pub struct DiffStats {
    pub added: usize,
    pub removed: usize,
    pub diff_output: String,
}

impl DiffStats {
    pub fn is_empty(&self) -> bool {
        self.added == 0 && self.removed == 0
    }
}

/// Compute diff stats for a session's worktree against its base commit.
pub fn get_diff_stats(session_name: &str) -> Option<DiffStats> {
    let worktree_path = get_session_env(session_name, "CM_WORKTREE_PATH")?;
    let base_commit = get_session_env(session_name, "CM_BASE_COMMIT")?;

    if !std::path::Path::new(&worktree_path).exists() {
        return None;
    }

    // Stage intent-to-add for untracked files so they show up in diff
    let _ = Command::new("git")
        .args(["-C", &worktree_path, "add", "-N", "."])
        .output();

    let output = Command::new("git")
        .args(["-C", &worktree_path, "--no-pager", "diff", &base_commit])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let diff_output = String::from_utf8_lossy(&output.stdout).to_string();

    let mut added = 0;
    let mut removed = 0;
    for line in diff_output.lines() {
        if line.starts_with('+') && !line.starts_with("+++") {
            added += 1;
        } else if line.starts_with('-') && !line.starts_with("---") {
            removed += 1;
        }
    }

    Some(DiffStats {
        added,
        removed,
        diff_output,
    })
}

/// Raw signals from a tmux session for status detection.
pub struct SessionProbe {
    pub claude_alive: bool,
    pub content_hash: u64,
    pub has_permission_prompt: bool,
}

/// Probe a session for raw status signals.
pub fn probe_session(session_name: &str) -> Option<SessionProbe> {
    // Check pane_pid and pane_dead
    let output = Command::new("tmux")
        .args([
            "display-message",
            "-t",
            session_name,
            "-p",
            "#{pane_pid} #{pane_dead}",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let info = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<&str> = info.trim().split(' ').collect();

    if parts.len() >= 2 && parts[1] == "1" {
        return None; // pane is dead
    }

    let pane_pid = parts.first().and_then(|p| p.parse::<u32>().ok())?;

    // Check if the pane process itself is claude, or if claude is a child
    let pane_comm = Command::new("ps")
        .args(["-o", "comm=", "-p", &pane_pid.to_string()])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    let claude_alive = pane_comm == "claude"
        || Command::new("pgrep")
            .args(["-P", &pane_pid.to_string(), "-x", "claude"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

    let content = capture_pane_plain(session_name).unwrap_or_default();
    let content_hash = hash_content(&content);
    let has_permission_prompt = PERMISSION_PROMPTS.iter().any(|p| content.contains(p));

    Some(SessionProbe {
        claude_alive,
        content_hash,
        has_permission_prompt,
    })
}

fn hash_content(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

fn capture_pane_plain(session_name: &str) -> Option<String> {
    let output = Command::new("tmux")
        .args(["capture-pane", "-t", session_name, "-p"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    Some(String::from_utf8_lossy(&output.stdout).to_string())
}

pub fn next_session_number(
    project_name: &str,
    task_name: &str,
    sessions: &[TmuxSession],
) -> u32 {
    let max = sessions
        .iter()
        .filter(|s| s.project_name == project_name && s.task_name == task_name)
        .filter_map(|s| s.session_name.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    max + 1
}

pub fn sessions_for_task(
    project_name: &str,
    task_name: &str,
    sessions: &[TmuxSession],
) -> Vec<TmuxSession> {
    sessions
        .iter()
        .filter(|s| {
            s.project_name == sanitize(project_name) && s.task_name == sanitize(task_name)
        })
        .cloned()
        .collect()
}
