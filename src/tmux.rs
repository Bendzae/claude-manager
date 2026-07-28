use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Result, bail};

const SESSION_SEP: &str = "__";
/// Markers for dialogs that need the user's attention: permission prompts and
/// question dialogs (AskUserQuestion renders a "❯ 1." option selector).
const PERMISSION_PROMPTS: &[&str] = &[
    "Do you want to",
    "Yes, allow all",
    "No, and tell Claude what to do differently",
    "❯ 1.",
];

/// Sentinel placed in the task slot of a tmux session name to mark an adhoc session.
/// Adhoc sessions belong to a project but no task; they run in the project dir.
pub const ADHOC_MARKER: &str = "adhoc";

pub fn is_adhoc_marker(s: &str) -> bool {
    sanitize(s).eq_ignore_ascii_case(ADHOC_MARKER)
}

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
    result.trim_matches('-').replace("__", "_").to_string()
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

fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
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
    crate::config::base_dir()
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

pub fn branch_exists(project_path: &str, branch: &str) -> bool {
    Command::new("git")
        .args([
            "-C",
            project_path,
            "rev-parse",
            "--verify",
            &format!("refs/heads/{branch}"),
        ])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// List checkout-able branches for `project_path`: local branches first, then
/// remote-tracking branches reduced to their short name (so `git checkout <name>`
/// creates a local tracking branch). Remote duplicates of local branches and
/// the symbolic `origin/HEAD` are skipped. Ordering otherwise follows git's
/// most-recently-committed-first.
pub fn list_branches(project_path: &str) -> Vec<String> {
    let mut branches: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Local branches, most recent commit first.
    if let Ok(out) = Command::new("git")
        .args([
            "-C",
            project_path,
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname:short)",
            "refs/heads",
        ])
        .output()
    {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let name = line.trim();
            if !name.is_empty() && seen.insert(name.to_string()) {
                branches.push(name.to_string());
            }
        }
    }

    // Remote branches, reduced to their short name (origin/foo -> foo).
    if let Ok(out) = Command::new("git")
        .args([
            "-C",
            project_path,
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname:short)",
            "refs/remotes",
        ])
        .output()
    {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let full = line.trim();
            // Skip symbolic refs like `origin/HEAD`.
            if full.is_empty() || full.ends_with("/HEAD") {
                continue;
            }
            let short = full.split_once('/').map(|(_, rest)| rest).unwrap_or(full);
            if !short.is_empty() && seen.insert(short.to_string()) {
                branches.push(short.to_string());
            }
        }
    }

    branches
}

/// Fetch every remote (pruning deleted branches), then fast-forward the
/// currently checked-out branch. The fetch updates all remote-tracking refs;
/// the pull is best-effort (a detached HEAD, missing upstream, or diverged
/// branch leaves the fetch intact and is reported, not treated as failure).
pub fn fetch_pull_all(project_path: &str) -> Result<String> {
    let fetch = Command::new("git")
        .args(["-C", project_path, "fetch", "--all", "--prune"])
        .output()?;
    if !fetch.status.success() {
        let stderr = String::from_utf8_lossy(&fetch.stderr);
        bail!("Fetch failed: {}", stderr.trim());
    }

    let pull = Command::new("git")
        .args(["-C", project_path, "pull", "--ff-only"])
        .output();
    match pull {
        Ok(o) if o.status.success() => {
            Ok("Fetched all remotes; fast-forwarded current branch".to_string())
        }
        _ => Ok("Fetched all remotes (current branch not fast-forwarded)".to_string()),
    }
}

/// Pull latest main and create a task branch from it.
pub fn create_task_branch(project_path: &str, branch_name: &str) -> Result<()> {
    // Try to fetch latest main from origin
    let _ = Command::new("git")
        .args(["-C", project_path, "fetch", "origin", "main"])
        .output();

    // Try creating from origin/main first, fall back to local main.
    // `--no-track` prevents inheriting origin/main as the upstream — once the
    // branch is pushed, `push -u` will set it to track origin/<branch_name>.
    let status = Command::new("git")
        .args([
            "-C",
            project_path,
            "branch",
            "--no-track",
            branch_name,
            "origin/main",
        ])
        .output()?;

    if !status.status.success() {
        let output = Command::new("git")
            .args([
                "-C",
                project_path,
                "branch",
                "--no-track",
                branch_name,
                "main",
            ])
            .output()?;
        if !output.status.success() {
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
    copy_patterns: &[String],
    setup_commands: &[String],
    initial_prompt: Option<&str>,
    startup_skills: &[String],
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

        // Always copy .claude/ folder, plus any configured patterns (sync, before hooks setup)
        let mut all_patterns = vec![".claude/***".to_string()];
        all_patterns.extend_from_slice(copy_patterns);
        copy_patterns_to_worktree(project_path, &worktree_path_str, &all_patterns);

        // Run setup commands in the new worktree if configured
        for cmd in setup_commands {
            let output = Command::new("sh")
                .args(["-c", cmd])
                .current_dir(&worktree_path_str)
                .output()?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                bail!("Setup command failed: {stderr}\nCommand: {cmd}");
            }
        }

        work_dir = worktree_path_str.clone();
    } else {
        work_dir = project_path.to_string();
    }

    // Always install the claude-manager plugin (skills + plugin enable)
    install_claude_manager_plugin(&work_dir);

    let session_branch = if use_worktree {
        let branch = format!("{task_branch}-{}", sanitize(session_name));
        Some(branch)
    } else {
        None
    };

    let system_prompt =
        build_base_system_prompt(project_name, task_branch, session_branch.as_deref());

    let mut claude_cmd = String::from("claude --dangerously-skip-permissions");
    claude_cmd.push_str(&format!(
        " --plugin-dir {}",
        shell_escape(&claude_manager_plugin_path(&work_dir))
    ));
    claude_cmd.push_str(&format!(
        " --append-system-prompt {}",
        shell_escape(&system_prompt)
    ));

    let combined_prompt = build_initial_prompt(startup_skills, initial_prompt);
    if let Some(prompt) = &combined_prompt {
        claude_cmd.push(' ');
        claude_cmd.push_str(&shell_escape(prompt));
    }

    let output = Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            &tmux_name,
            "-c",
            &work_dir,
            &claude_cmd,
        ])
        .output()?;

    if !output.status.success() {
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
        .output();

    // Store the task branch so we can diff against it later
    let _ = Command::new("tmux")
        .args([
            "set-environment",
            "-t",
            &tmux_name,
            "CM_TASK_BRANCH",
            task_branch,
        ])
        .output();

    if use_worktree {
        let _ = Command::new("tmux")
            .args([
                "set-environment",
                "-t",
                &tmux_name,
                "CM_WORKTREE_PATH",
                &worktree_path_str,
            ])
            .output();
    }

    Ok(tmux_name)
}

/// Create an adhoc session: tmux session running Claude in the project directory
/// on whatever branch is currently checked out, with no task or worktree.
/// Applies `startup_skills` if any, but sends no user prompt.
pub fn create_adhoc_session(
    project_name: &str,
    project_path: &str,
    session_name: &str,
    startup_skills: &[String],
) -> Result<String> {
    let tmux_name = build_tmux_name(project_name, ADHOC_MARKER, session_name);

    let mut claude_cmd = String::from("claude --dangerously-skip-permissions");
    if let Some(prompt) = build_initial_prompt(startup_skills, None) {
        claude_cmd.push(' ');
        claude_cmd.push_str(&shell_escape(&prompt));
    }

    let output = Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            &tmux_name,
            "-c",
            project_path,
            &claude_cmd,
        ])
        .output()?;

    if !output.status.success() {
        bail!("Failed to create tmux session");
    }

    let _ = Command::new("tmux")
        .args([
            "set-environment",
            "-t",
            &tmux_name,
            "CM_PROJECT_PATH",
            project_path,
        ])
        .output();

    Ok(tmux_name)
}

/// Recreate an adhoc tmux session from a saved record (e.g. after tmux dies).
pub fn recreate_adhoc_session(
    tmux_name: &str,
    record: &crate::config::SessionRecord,
) -> Result<String> {
    let claude_cmd = String::from("claude --dangerously-skip-permissions --continue");

    let output = Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            tmux_name,
            "-c",
            &record.project_path,
            &claude_cmd,
        ])
        .output()?;

    if !output.status.success() {
        bail!("Failed to recreate adhoc tmux session");
    }

    let _ = Command::new("tmux")
        .args([
            "set-environment",
            "-t",
            tmux_name,
            "CM_PROJECT_PATH",
            &record.project_path,
        ])
        .output();

    Ok(tmux_name.to_string())
}

/// Recreate a tmux session from a saved record (e.g. after tmux dies).
/// Reuses the existing worktree if present; does NOT send an initial prompt.
/// `tmux_name` is the expected session name (which may differ from what
/// build_tmux_name would produce if the session was renamed).
pub fn recreate_session(tmux_name: &str, record: &crate::config::SessionRecord) -> Result<String> {
    let work_dir = if record.use_worktree {
        let wt_path = worktree_dir(
            &record.project_name,
            &record.task_name,
            &record.session_name,
        );
        if wt_path.exists() {
            wt_path.to_string_lossy().to_string()
        } else {
            // Worktree is gone — cannot recreate this session
            bail!(
                "Worktree no longer exists for session {}",
                record.session_name
            );
        }
    } else {
        record.project_path.clone()
    };

    // Always install plugin
    install_claude_manager_plugin(&work_dir);

    let session_branch = if record.use_worktree {
        Some(format!(
            "{}-{}",
            record.task_branch,
            sanitize(&record.session_name)
        ))
    } else {
        None
    };

    let system_prompt = build_base_system_prompt(
        &record.project_name,
        &record.task_branch,
        session_branch.as_deref(),
    );

    let mut claude_cmd = String::from("claude --dangerously-skip-permissions --continue");
    claude_cmd.push_str(&format!(
        " --plugin-dir {}",
        shell_escape(&claude_manager_plugin_path(&work_dir))
    ));
    claude_cmd.push_str(&format!(
        " --append-system-prompt {}",
        shell_escape(&system_prompt)
    ));

    let output = Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            tmux_name,
            "-c",
            &work_dir,
            &claude_cmd,
        ])
        .output()?;

    if !output.status.success() {
        bail!("Failed to create tmux session for recreation");
    }

    // Restore environment variables
    let _ = Command::new("tmux")
        .args([
            "set-environment",
            "-t",
            tmux_name,
            "CM_PROJECT_PATH",
            &record.project_path,
        ])
        .output();

    let _ = Command::new("tmux")
        .args([
            "set-environment",
            "-t",
            tmux_name,
            "CM_TASK_BRANCH",
            &record.task_branch,
        ])
        .output();

    if record.use_worktree {
        let _ = Command::new("tmux")
            .args([
                "set-environment",
                "-t",
                tmux_name,
                "CM_WORKTREE_PATH",
                &work_dir,
            ])
            .output();
    }

    Ok(tmux_name.to_string())
}

/// Insert text into the claude pane's input buffer as a bracketed paste.
/// If `submit` is true, also presses Enter afterwards.
pub fn send_text(session_name: &str, text: &str, submit: bool) -> Result<()> {
    let target = format!("{session_name}:0");
    let buf_name = "cm_comment_paste";

    let mut child = Command::new("tmux")
        .args(["load-buffer", "-b", buf_name, "-"])
        .stdin(Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes())?;
    }
    let status = child.wait()?;
    if !status.success() {
        bail!("tmux load-buffer failed");
    }

    // -p: bracketed paste, -d: delete buffer after pasting.
    let out = Command::new("tmux")
        .args(["paste-buffer", "-d", "-p", "-b", buf_name, "-t", &target])
        .output()?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("tmux paste-buffer failed: {}", stderr.trim());
    }

    if submit {
        let out = Command::new("tmux")
            .args(["send-keys", "-t", &target, "Enter"])
            .output()?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            bail!("tmux send-keys Enter failed: {}", stderr.trim());
        }
    }
    Ok(())
}

/// Capture the last `lines` lines (including scrollback) of a session's Claude
/// pane (window 0) with ANSI escape sequences, plus the pane width in columns.
pub fn capture_output(session_name: &str, lines: usize) -> Option<(String, usize)> {
    let target = format!("{session_name}:0");
    let output = Command::new("tmux")
        .args([
            "capture-pane",
            "-p",
            "-e",
            "-t",
            &target,
            "-S",
            &format!("-{lines}"),
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let width = Command::new("tmux")
        .args(["display-message", "-p", "-t", &target, "#{pane_width}"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
        .unwrap_or(80);

    Some((String::from_utf8_lossy(&output.stdout).to_string(), width))
}

/// Send a single tmux key name (e.g. "Enter", "Escape", "Up", "1") to the
/// Claude pane.
pub fn send_key(session_name: &str, key: &str) -> Result<()> {
    let target = format!("{session_name}:0");
    let out = Command::new("tmux")
        .args(["send-keys", "-t", &target, key])
        .output()?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("tmux send-keys failed: {}", stderr.trim());
    }
    Ok(())
}

pub fn attach_session(name: &str) -> Result<()> {
    // Select window 0 (claude) before attaching
    let _ = Command::new("tmux")
        .args(["select-window", "-t", &format!("{name}:0")])
        .output();

    let status = Command::new("tmux")
        .args(["attach-session", "-t", name])
        .status()?;

    if !status.success() {
        bail!("Failed to attach to tmux session");
    }

    Ok(())
}

/// Attach to a specific window of a session (selects it first).
pub fn attach_session_window(session_name: &str, window_idx: usize) -> Result<()> {
    let _ = Command::new("tmux")
        .args([
            "select-window",
            "-t",
            &format!("{session_name}:{window_idx}"),
        ])
        .output();

    let status = Command::new("tmux")
        .args(["attach-session", "-t", session_name])
        .status()?;

    if !status.success() {
        bail!("Failed to attach to tmux session");
    }
    Ok(())
}

/// Number of terminal windows in a session (windows past index 0, the agent).
pub fn count_terminal_windows(session_name: &str) -> usize {
    let output = Command::new("tmux")
        .args(["list-windows", "-t", session_name, "-F", "#{window_index}"])
        .output();
    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter(|line| line.trim().parse::<usize>().is_ok_and(|i| i > 0))
            .count(),
        _ => 0,
    }
}

/// Create a terminal window in the session rooted at `work_dir`. Returns its
/// window index.
pub fn create_terminal_window(session_name: &str, work_dir: &str) -> Result<usize> {
    let output = Command::new("tmux")
        .args([
            "new-window",
            "-t",
            session_name,
            "-c",
            work_dir,
            "-P",
            "-F",
            "#{window_index}",
        ])
        .output()?;

    if !output.status.success() {
        bail!("Failed to create terminal window");
    }

    let idx = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<usize>()
        .unwrap_or(1);
    Ok(idx)
}

/// Launch `command` in a dedicated, detached tmux session rooted at `work_dir`
/// and return its tmux session name. Any prior run session with the same name
/// is replaced so "Run" always starts fresh. The shell stays alive after the
/// command exits so its output (and any error) remains visible on attach. The
/// `cmrun-` name prefix is deliberately unparseable by `from_tmux_name`, so run
/// sessions never appear in the managed session list.
/// tmux session name hosting the run command for `label`. Shared by the
/// launcher and the UI so an item can find its own run session.
pub fn run_session_name(label: &str) -> String {
    format!("cmrun-{}", sanitize(label))
}

/// Live run sessions (`cmrun-*`) mapped to whether their command is still
/// executing (`true`) versus having dropped to an interactive shell (`false`,
/// i.e. the command finished). Uses one `list-panes` call across all sessions.
pub fn list_run_sessions() -> HashMap<String, bool> {
    let mut map = HashMap::new();
    let output = Command::new("tmux")
        .args([
            "list-panes",
            "-a",
            "-F",
            "#{session_name}\t#{pane_current_command}",
        ])
        .output();
    if let Ok(o) = output
        && o.status.success()
    {
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            if let Some((name, cmd)) = line.split_once('\t')
                && name.starts_with("cmrun-")
            {
                let active = !is_shell_command(cmd);
                map.entry(name.to_string())
                    .and_modify(|v| *v |= active)
                    .or_insert(active);
            }
        }
    }
    map
}

/// Whether `cmd` (a tmux `pane_current_command`) is an interactive shell, i.e.
/// the run command has finished and left the keep-alive shell in the foreground.
fn is_shell_command(cmd: &str) -> bool {
    matches!(
        cmd.trim_start_matches('-'),
        "sh" | "bash" | "zsh" | "fish" | "dash" | "ksh" | "tcsh" | "csh"
    )
}

pub fn run_command_session(label: &str, work_dir: &str, command: &str) -> Result<String> {
    let tmux_name = run_session_name(label);

    // Replace any existing run session so re-running restarts the command.
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", &tmux_name])
        .output();

    let shell_cmd = format!("{command}; exec \"${{SHELL:-/bin/sh}}\"");
    let output = Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            &tmux_name,
            "-c",
            work_dir,
            "sh",
            "-c",
            &shell_cmd,
        ])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to start run session: {}", stderr.trim());
    }

    Ok(tmux_name)
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
    let output = Command::new("tmux")
        .args(["rename-session", "-t", old_name, new_name])
        .output()?;

    if !output.status.success() {
        bail!("Failed to rename tmux session from {old_name} to {new_name}");
    }

    Ok(())
}

/// Fallback paths for cleaning up a session when the tmux session is already dead
/// and environment variables are unavailable.
pub struct SessionCleanupInfo {
    pub project_path: String,
    pub worktree_path: String,
    /// The branch checked out in the worktree (e.g. "task-branch-session-name").
    /// If not provided, it will be derived from the worktree before removal.
    pub branch_name: Option<String>,
}

pub fn kill_session(name: &str) -> Result<()> {
    kill_session_with_fallback(name, None)
}

/// Kill the tmux session only — leave worktrees, branches, and session records intact.
/// Used when archiving a task so the session can be recreated later.
pub fn kill_session_only(name: &str) -> Result<()> {
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", name])
        .output();
    Ok(())
}

pub fn kill_session_with_fallback(name: &str, fallback: Option<SessionCleanupInfo>) -> Result<()> {
    // Try to get paths from tmux env vars first, fall back to provided info
    let project_path = get_session_env(name, "CM_PROJECT_PATH")
        .or_else(|| fallback.as_ref().map(|f| f.project_path.clone()));
    let worktree_path = get_session_env(name, "CM_WORKTREE_PATH")
        .or_else(|| fallback.as_ref().map(|f| f.worktree_path.clone()));

    // Kill the tmux session (ignore errors — it may already be dead)
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", name])
        .output();

    // Clean up worktree and its branch if applicable
    if let (Some(proj_path), Some(wt_path)) = (project_path, worktree_path) {
        // Get the branch name before removing the worktree
        let branch = Command::new("git")
            .args(["-C", &wt_path, "rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .or_else(|| fallback.and_then(|f| f.branch_name));

        if Path::new(&wt_path).exists() {
            let _ = Command::new("git")
                .args(["-C", &proj_path, "worktree", "remove", "--force", &wt_path])
                .output();
        }

        // Prune stale worktree references so git no longer considers the branch checked out
        let _ = Command::new("git")
            .args(["-C", &proj_path, "worktree", "prune"])
            .output();

        // Delete the worktree branch
        if let Some(branch_name) = branch {
            if !branch_name.is_empty() && branch_name != "main" && branch_name != "master" {
                let _ = Command::new("git")
                    .args(["-C", &proj_path, "branch", "-D", &branch_name])
                    .output();
            }
        }
    }

    Ok(())
}

/// Copy specific file patterns from the project into a new worktree.
/// Patterns can be files (`.env`) or directories (`build/`).
fn copy_patterns_to_worktree(project_path: &str, worktree_path: &str, patterns: &[String]) {
    let src = if project_path.ends_with('/') {
        project_path.to_string()
    } else {
        format!("{project_path}/")
    };

    let dst = if worktree_path.ends_with('/') {
        worktree_path.to_string()
    } else {
        format!("{worktree_path}/")
    };

    let mut args = vec!["-a".to_string()];
    for pattern in patterns {
        args.push("--include".to_string());
        args.push(pattern.to_string());
    }
    // Exclude everything not matched
    args.push("--exclude".to_string());
    args.push("*".to_string());
    args.push(src);
    args.push(dst);

    let _ = Command::new("rsync")
        .args(&args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
}

/// Build the base system prompt that all sessions receive.
fn build_base_system_prompt(
    project_name: &str,
    task_branch: &str,
    worktree_branch: Option<&str>,
) -> String {
    let mut prompt = format!(
        "You have been spawned as a session agent by Claude Manager, a multi-agent task management tool.\n\
         \n\
         - Project: {project_name}\n\
         - Task branch: {task_branch}\n"
    );
    if let Some(wt_branch) = worktree_branch {
        prompt.push_str(&format!("- Worktree branch: {wt_branch}\n"));
    }
    prompt.push_str(&format!(
        "- PRs should always be opened from the task branch: {task_branch}\n\
         - Other agents may be working on the same task in parallel\n\
         - NEVER push the worktree branch unless explicitly told to do so"
    ));
    prompt
}

/// Build the combined initial prompt from startup skills and optional user prompt.
/// Returns `None` if both are empty.
fn build_initial_prompt(startup_skills: &[String], user_prompt: Option<&str>) -> Option<String> {
    let has_skills = !startup_skills.is_empty();
    let has_prompt = user_prompt.is_some_and(|p| !p.is_empty());

    if !has_skills && !has_prompt {
        return None;
    }

    if !has_skills {
        return user_prompt.map(String::from);
    }

    let skills_list: String = startup_skills
        .iter()
        .enumerate()
        .map(|(i, s)| format!("{}. {s}", i + 1))
        .collect::<Vec<_>>()
        .join("\n");

    if !has_prompt {
        return Some(format!(
            "Run these startup skills first (one at a time, using the Skill tool):\n{skills_list}"
        ));
    }

    Some(format!(
        "Run these startup skills first (one at a time, using the Skill tool), then proceed with the task below:\n\
         {skills_list}\n\n\
         Task: {}",
        user_prompt.unwrap()
    ))
}

// Embedded claude-manager plugin files (see claude-manager-plugin/ at repo root).
const PLUGIN_MANIFEST: &str = include_str!("../claude-manager-plugin/.claude-plugin/plugin.json");
const PLUGIN_SKILL_COMMIT_PUSH_TASK: &str =
    include_str!("../claude-manager-plugin/skills/commit-push-task/SKILL.md");
const PLUGIN_SKILL_STACKED_PR: &str =
    include_str!("../claude-manager-plugin/skills/stacked-pr/SKILL.md");

/// Filesystem path to the installed claude-manager plugin directory inside `work_dir`.
/// This is the path passed to `claude --plugin-dir`.
fn claude_manager_plugin_path(work_dir: &str) -> String {
    Path::new(work_dir)
        .join(".claude")
        .join("plugins")
        .join("claude-manager")
        .to_string_lossy()
        .to_string()
}

/// Install the bundled claude-manager plugin into the work directory's
/// `.claude/plugins/claude-manager/`. The plugin is loaded at session start via
/// `claude --plugin-dir <path>` (see `claude_manager_plugin_path`).
fn install_claude_manager_plugin(work_dir: &str) {
    // Remove the update-task-context skill that older versions installed
    // (standalone under `.claude/skills/` and inside the plugin) — the shared
    // task context concept no longer exists.
    let legacy_skill_dir = Path::new(work_dir)
        .join(".claude")
        .join("skills")
        .join("update-task-context");
    let _ = fs::remove_dir_all(&legacy_skill_dir);

    let plugin_dir = PathBuf::from(claude_manager_plugin_path(work_dir));
    let _ = fs::remove_dir_all(plugin_dir.join("skills").join("update-task-context"));

    let _ = fs::create_dir_all(plugin_dir.join(".claude-plugin"));
    let _ = fs::create_dir_all(plugin_dir.join("skills").join("commit-push-task"));
    let _ = fs::create_dir_all(plugin_dir.join("skills").join("stacked-pr"));

    let _ = fs::write(
        plugin_dir.join(".claude-plugin").join("plugin.json"),
        PLUGIN_MANIFEST,
    );
    let _ = fs::write(
        plugin_dir
            .join("skills")
            .join("commit-push-task")
            .join("SKILL.md"),
        PLUGIN_SKILL_COMMIT_PUSH_TASK,
    );
    let _ = fs::write(
        plugin_dir
            .join("skills")
            .join("stacked-pr")
            .join("SKILL.md"),
        PLUGIN_SKILL_STACKED_PR,
    );

    // Git-ignore the locally installed plugin via .git/info/exclude.
    let exclude_entries = [".claude/plugins/claude-manager/"];
    let git_dir = Path::new(work_dir).join(".git");
    let real_git_dir = if git_dir.is_file() {
        fs::read_to_string(&git_dir).ok().and_then(|content| {
            content
                .strip_prefix("gitdir: ")
                .map(|p| PathBuf::from(p.trim()))
        })
    } else if git_dir.is_dir() {
        Some(git_dir)
    } else {
        None
    };
    if let Some(gd) = real_git_dir {
        let info_dir = gd.join("info");
        let _ = fs::create_dir_all(&info_dir);
        let exclude_path = info_dir.join("exclude");
        let mut content = fs::read_to_string(&exclude_path).unwrap_or_default();
        let mut changed = false;
        for entry in exclude_entries {
            if !content.lines().any(|l| l.trim() == entry) {
                if !content.ends_with('\n') && !content.is_empty() {
                    content.push('\n');
                }
                content.push_str(entry);
                content.push('\n');
                changed = true;
            }
        }
        if changed {
            let _ = fs::write(&exclude_path, content);
        }
    }
}

/// Check if a worktree has uncommitted changes.
pub fn worktree_is_dirty(worktree_path: &str) -> bool {
    Command::new("git")
        .args(["-C", worktree_path, "status", "--porcelain"])
        .output()
        .map(|o| o.status.success() && !String::from_utf8_lossy(&o.stdout).trim().is_empty())
        .unwrap_or(false)
}

/// Generate a default commit message: "<session_name>-<N>" where N increments.
pub fn next_commit_message(worktree_path: &str, session_name: &str) -> String {
    let count = Command::new("git")
        .args(["-C", worktree_path, "rev-list", "--count", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse::<u32>()
                .ok()
        })
        .unwrap_or(0);

    format!("{session_name}-{count}")
}

/// Stage all changes and commit.
pub fn commit_all(worktree_path: &str, message: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["-C", worktree_path, "add", "-A"])
        .output()?;
    if !output.status.success() {
        bail!("Failed to stage changes");
    }

    let output = Command::new("git")
        .args(["-C", worktree_path, "commit", "-m", message])
        .output()?;
    if !output.status.success() {
        bail!("Failed to commit");
    }

    Ok(())
}

/// Rebase a session's worktree branch onto the task branch to pull in latest changes.
/// Pull latest main and rebase the task branch onto it.
pub fn push_branch(project_path: &str, branch: &str) -> Result<String> {
    if branch.is_empty() || branch == "main" || branch == "master" {
        bail!("Refusing to push protected branch '{branch}'");
    }

    let output = Command::new("git")
        .args([
            "-C",
            project_path,
            "push",
            "--force-with-lease",
            "-u",
            "origin",
            branch,
        ])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Push failed: {stderr}");
    }

    Ok(format!("Pushed {branch} to origin"))
}

pub fn update_task_branch(project_path: &str, branch: &str, base_branch: &str) -> Result<String> {
    // Fetch latest base branch from origin (always updates origin/<base>).
    let _ = Command::new("git")
        .args(["-C", project_path, "fetch", "origin", base_branch])
        .output();

    // Pick the rebase target: prefer origin/<base> if it resolves, else local <base>.
    let remote_ref = format!("origin/{base_branch}");
    let has_remote = Command::new("git")
        .args(["-C", project_path, "rev-parse", "--verify", &remote_ref])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    // Fast-forward the local <base> ref to origin/<base>. Worktrees share refs, so
    // this keeps every worktree's view of the base branch (e.g. `main`) current.
    if has_remote {
        update_local_base_branch(project_path, base_branch, &remote_ref);
    }

    let target = if has_remote {
        remote_ref
    } else {
        base_branch.to_string()
    };

    // Remember current branch to restore after rebase
    let head = Command::new("git")
        .args(["-C", project_path, "rev-parse", "--abbrev-ref", "HEAD"])
        .output()?;
    let original_branch = String::from_utf8_lossy(&head.stdout).trim().to_string();

    // Rebase the task branch onto target (checks out branch, rebases, leaves it checked out)
    let output = Command::new("git")
        .args(["-C", project_path, "rebase", &target, branch])
        .output()?;

    if !output.status.success() {
        // Leave the branch checked out so the user can resolve conflicts
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "Rebase has conflicts. Resolve them in {project_path} then run `git rebase --continue`.\n{stderr}"
        );
    }

    // Restore original branch only on success
    let _ = Command::new("git")
        .args(["-C", project_path, "checkout", &original_branch])
        .output();

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.contains("is up to date") {
        Ok(format!(
            "Branch {branch} is already up to date with {base_branch}"
        ))
    } else {
        Ok(format!("Rebased {branch} onto latest {base_branch}"))
    }
}

/// Whether the `git spr` subcommand is available.
pub fn spr_installed() -> bool {
    Command::new("git")
        .args(["spr", "version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Ensure `git spr` never blocks on the interactive "enjoying git spr?" star prompt
/// by writing `stargazer: true` to `~/.spr.yml` (idempotent).
fn ensure_spr_stargazer() {
    let Some(home) = dirs::home_dir() else { return };
    let path = home.join(".spr.yml");
    let already = fs::read_to_string(&path)
        .map(|c| c.lines().any(|l| l.trim_start().starts_with("stargazer:")))
        .unwrap_or(false);
    if already {
        return;
    }
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "stargazer: true");
    }
}

/// One pull request in a stack: its URL and title (commit subject).
pub type StackPr = (String, String);

/// Parse `git spr status --text` output (`<url> : <title>` per line) into PRs.
/// spr lists top→bottom; we reverse to bottom→top (merge order).
fn parse_spr_status(stdout: &str) -> Vec<StackPr> {
    let mut prs: Vec<StackPr> = stdout
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let (url, title) = line.split_once(" : ")?;
            let url = url.trim();
            if !url.starts_with("http") {
                return None;
            }
            Some((url.to_string(), title.trim().to_string()))
        })
        .collect();
    prs.reverse();
    prs
}

/// Current branch name, or `None` if detached/unknown.
fn current_branch(project_path: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["-C", project_path, "rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if name.is_empty() || name == "HEAD" {
        None
    } else {
        Some(name)
    }
}

fn checkout_branch(project_path: &str, branch: &str) -> Result<()> {
    let out = Command::new("git")
        .args(["-C", project_path, "checkout", branch])
        .output()?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("Could not checkout '{branch}' in {project_path}: {stderr}");
    }
    Ok(())
}

/// Publish/update a task's commits as a stack of dependent PRs via `git spr update`.
/// Each commit on `branch` (relative to trunk) becomes one PR. Returns the stack's
/// PRs bottom→top. spr rewrites `branch` history (rebase onto trunk + `commit-id`
/// trailers); pre-existing session worktrees re-sync on their next push (rebase-then-ff).
pub fn spr_update(project_path: &str, branch: &str) -> Result<Vec<StackPr>> {
    run_spr_command(project_path, branch, "update")
}

/// Reconcile the stack after PRs merge or trunk moves (`git spr sync`): fetch trunk,
/// rebase the remaining stack onto it, update PRs. Returns the stack's PRs bottom→top.
pub fn spr_sync(project_path: &str, branch: &str) -> Result<Vec<StackPr>> {
    run_spr_command(project_path, branch, "sync")
}

/// Run `git spr <subcommand>` on `branch` in `project_path`: check the branch out,
/// run the command, read the resulting stack via `spr status --text`, restore HEAD.
/// On a rebase conflict spr leaves `branch` mid-rebase (checked out) for the user to
/// resolve; the error surfaces stdout+stderr.
fn run_spr_command(project_path: &str, branch: &str, subcommand: &str) -> Result<Vec<StackPr>> {
    if branch.is_empty() || branch == "main" || branch == "master" {
        bail!("Refusing to run `git spr {subcommand}` on protected branch '{branch}'");
    }
    if !spr_installed() {
        bail!("`git spr` not found. Install with `brew install ejoffe/tap/spr`.");
    }
    ensure_spr_stargazer();

    let original = current_branch(project_path);
    if original.as_deref() != Some(branch) {
        checkout_branch(project_path, branch)?;
    }

    let output = Command::new("git")
        .args(["-C", project_path, "spr", subcommand])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!("`git spr {subcommand}` failed:\n{stdout}{stderr}");
    }

    // Read the resulting stack while `branch` is still checked out, then restore HEAD.
    let status = Command::new("git")
        .args(["-C", project_path, "spr", "status", "--text"])
        .output();
    if let Some(orig) = &original {
        if orig != branch {
            let _ = checkout_branch(project_path, orig);
        }
    }
    let prs = match status {
        Ok(o) if o.status.success() => parse_spr_status(&String::from_utf8_lossy(&o.stdout)),
        _ => Vec::new(),
    };
    Ok(prs)
}

pub fn rebase_session_on_task(
    project_path: &str,
    task_branch: &str,
    worktree_path: &str,
) -> Result<String> {
    // Check for uncommitted changes
    if worktree_is_dirty(worktree_path) {
        bail!("Worktree has uncommitted changes. Commit or stash first.");
    }

    // Get the session branch name
    let output = Command::new("git")
        .args(["-C", worktree_path, "rev-parse", "--abbrev-ref", "HEAD"])
        .output()?;
    if !output.status.success() {
        bail!("Failed to determine worktree branch");
    }
    let session_branch = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // Check if already up to date
    let is_ancestor = Command::new("git")
        .args([
            "-C",
            project_path,
            "merge-base",
            "--is-ancestor",
            task_branch,
            &session_branch,
        ])
        .output()?
        .status
        .success();

    if is_ancestor {
        return Ok(format!(
            "{session_branch} is already up to date with {task_branch}"
        ));
    }

    // Rebase onto task branch
    let output = Command::new("git")
        .args(["-C", worktree_path, "rebase", task_branch])
        .output()?;

    if !output.status.success() {
        let _ = Command::new("git")
            .args(["-C", worktree_path, "rebase", "--abort"])
            .output();
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Rebase conflict. Aborted. Resolve manually.\n{stderr}");
    }

    Ok(format!("Rebased {session_branch} onto {task_branch}"))
}

/// Merge a session's worktree branch into the task branch.
pub fn merge_session_to_task(
    project_path: &str,
    task_branch: &str,
    _session_name: &str,
    worktree_path: &str,
) -> Result<String> {
    // Get the session branch name from the worktree
    let output = Command::new("git")
        .args(["-C", worktree_path, "rev-parse", "--abbrev-ref", "HEAD"])
        .output()?;
    if !output.status.success() {
        bail!("Failed to determine worktree branch");
    }
    let session_branch = String::from_utf8_lossy(&output.stdout).trim().to_string();

    if session_branch.is_empty() {
        bail!("Could not determine session branch");
    }

    // Find a worktree that has the task branch checked out
    let task_wt = find_worktree_for_branch(project_path, task_branch);

    if let Some(task_wt_path) = task_wt {
        // Merge in the worktree that has the task branch — this naturally updates
        // its index and working tree, and respects uncommitted changes.
        let output = Command::new("git")
            .args(["-C", &task_wt_path, "merge", "--ff-only", &session_branch])
            .output()?;

        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            return Ok(format!(
                "Merged {session_branch} into {task_branch} (ff)\n{}",
                stdout.trim()
            ));
        }

        // ff-only failed — try a real merge
        let output = Command::new("git")
            .args([
                "-C",
                &task_wt_path,
                "merge",
                &session_branch,
                "-m",
                &format!("Merge {session_branch} into {task_branch}"),
            ])
            .output()?;

        if !output.status.success() {
            let _ = Command::new("git")
                .args(["-C", &task_wt_path, "merge", "--abort"])
                .output();
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("Merge conflict. Aborted. Resolve manually.\n{stderr}");
        }

        Ok(format!("Merged {session_branch} into {task_branch}"))
    } else {
        // No worktree has the task branch — safe to use update-ref
        let is_ancestor = Command::new("git")
            .args([
                "-C",
                project_path,
                "merge-base",
                "--is-ancestor",
                task_branch,
                &session_branch,
            ])
            .output()?
            .status
            .success();

        if is_ancestor {
            let output = Command::new("git")
                .args(["-C", project_path, "rev-parse", &session_branch])
                .output()?;
            if !output.status.success() {
                bail!("Failed to resolve {session_branch}");
            }
            let session_sha = String::from_utf8_lossy(&output.stdout).trim().to_string();

            let output = Command::new("git")
                .args([
                    "-C",
                    project_path,
                    "rev-list",
                    "--count",
                    &format!("{task_branch}..{session_branch}"),
                ])
                .output()?;
            let count = String::from_utf8_lossy(&output.stdout).trim().to_string();

            let output = Command::new("git")
                .args([
                    "-C",
                    project_path,
                    "update-ref",
                    &format!("refs/heads/{task_branch}"),
                    &session_sha,
                ])
                .output()?;
            if !output.status.success() {
                bail!("Failed to fast-forward {task_branch}");
            }

            Ok(format!(
                "Fast-forwarded {task_branch} ({count} commit(s) from {session_branch})"
            ))
        } else {
            // Non-ff merge without a worktree: do it in the session worktree temporarily
            let output = Command::new("git")
                .args(["-C", worktree_path, "checkout", task_branch])
                .output()?;
            if !output.status.success() {
                bail!("Failed to checkout {task_branch} in worktree");
            }

            let output = Command::new("git")
                .args([
                    "-C",
                    worktree_path,
                    "merge",
                    &session_branch,
                    "-m",
                    &format!("Merge {session_branch} into {task_branch}"),
                ])
                .output()?;

            if !output.status.success() {
                let _ = Command::new("git")
                    .args(["-C", worktree_path, "merge", "--abort"])
                    .output();
                let _ = Command::new("git")
                    .args(["-C", worktree_path, "checkout", &session_branch])
                    .output();
                let stderr = String::from_utf8_lossy(&output.stderr);
                bail!("Merge conflict. Aborted. Resolve manually.\n{stderr}");
            }

            let _ = Command::new("git")
                .args(["-C", worktree_path, "checkout", &session_branch])
                .output();

            Ok(format!("Merged {session_branch} into {task_branch}"))
        }
    }
}

/// Find a worktree path that has the given branch checked out.
/// Fast-forward the local `base_branch` ref to `remote_ref` (origin/<base>) so that
/// every worktree — which share a single ref store — sees the latest base branch.
///
/// Git refuses to update a checked-out branch via `fetch origin base:base`, so when
/// the base branch is checked out somewhere we fast-forward it in that worktree
/// instead. If it isn't checked out anywhere, the ref is updated directly. All steps
/// are best-effort and no-ops when already up to date.
fn update_local_base_branch(project_path: &str, base_branch: &str, remote_ref: &str) {
    match find_worktree_for_branch(project_path, base_branch) {
        Some(wt) => {
            // Checked out — fast-forward its working tree. Skip if dirty so we never
            // touch uncommitted work; a non-ff history is left untouched by --ff-only.
            if !worktree_is_dirty(&wt) {
                let _ = Command::new("git")
                    .args(["-C", &wt, "merge", "--ff-only", remote_ref])
                    .output();
            }
        }
        None => {
            // Not checked out anywhere — safe to update the local ref directly.
            let _ = Command::new("git")
                .args([
                    "-C",
                    project_path,
                    "fetch",
                    "origin",
                    &format!("{base_branch}:{base_branch}"),
                ])
                .output();
        }
    }
}

fn find_worktree_for_branch(project_path: &str, branch: &str) -> Option<String> {
    // Check main repo first
    let output = Command::new("git")
        .args(["-C", project_path, "rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if output.status.success() {
        let current = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if current == branch {
            return Some(project_path.to_string());
        }
    }

    // Check worktrees
    let output = Command::new("git")
        .args(["-C", project_path, "worktree", "list", "--porcelain"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut current_path = None;

    for line in stdout.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current_path = Some(path.to_string());
        } else if let Some(b) = line.strip_prefix("branch refs/heads/") {
            if b == branch {
                return current_path;
            }
        } else if line.is_empty() {
            current_path = None;
        }
    }

    None
}

#[derive(Debug, Clone, Default)]
pub struct DiffStats {
    pub added: usize,
    pub removed: usize,
}

impl DiffStats {
    pub fn is_empty(&self) -> bool {
        self.added == 0 && self.removed == 0
    }
}

/// The branch currently checked out in the session's worktree (or the project
/// directory for no-worktree sessions).
pub fn get_session_branch(session_name: &str) -> Option<String> {
    let path = get_session_env(session_name, "CM_WORKTREE_PATH")
        .or_else(|| get_session_env(session_name, "CM_PROJECT_PATH"))?;
    current_branch(&path)
}

fn count_diff(diff: &str) -> DiffStats {
    let mut added = 0;
    let mut removed = 0;
    for line in diff.lines() {
        if line.starts_with('+') && !line.starts_with("+++") {
            added += 1;
        } else if line.starts_with('-') && !line.starts_with("---") {
            removed += 1;
        }
    }
    DiffStats { added, removed }
}

/// Full unified diff of a session's worktree against its task branch
/// (includes committed + uncommitted changes).
pub fn get_session_diff_text(session_name: &str) -> Option<String> {
    let worktree_path = get_session_env(session_name, "CM_WORKTREE_PATH")
        .or_else(|| get_session_env(session_name, "CM_PROJECT_PATH"))?;

    // Try task branch first, fall back to base commit for older sessions
    let diff_target = get_session_env(session_name, "CM_TASK_BRANCH")
        .or_else(|| get_session_env(session_name, "CM_BASE_COMMIT"))?;

    if !std::path::Path::new(&worktree_path).exists() {
        return None;
    }

    // Stage intent-to-add for untracked files so they show up in diff
    let _ = Command::new("git")
        .args(["-C", &worktree_path, "add", "-N", "."])
        .output();

    let output = Command::new("git")
        .args(["-C", &worktree_path, "--no-pager", "diff", &diff_target])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    Some(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Compute diff stats for a session's worktree against its base commit.
pub fn get_diff_stats(session_name: &str) -> Option<DiffStats> {
    Some(count_diff(&get_session_diff_text(session_name)?))
}

/// Compute diff stats for a task branch against its base branch.
/// Resolve a diff base ref, preferring `origin/<base>` when it exists (matching
/// `get_branch_diff`), else the local branch name.
pub fn resolve_base_ref(project_path: &str, base_branch: &str) -> String {
    let remote = format!("origin/{base_branch}");
    let has_remote = Command::new("git")
        .args([
            "-C",
            project_path,
            "rev-parse",
            "--verify",
            "--quiet",
            &remote,
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if has_remote {
        remote
    } else {
        base_branch.to_string()
    }
}

/// Full unified diff of a task branch against its base branch.
pub fn get_branch_diff_text(project_path: &str, branch: &str, base_branch: &str) -> Option<String> {
    let base = resolve_base_ref(project_path, base_branch);

    let output = Command::new("git")
        .args([
            "-C",
            project_path,
            "--no-pager",
            "diff",
            &format!("{base}...{branch}"),
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    Some(String::from_utf8_lossy(&output.stdout).to_string())
}

pub fn get_branch_diff(project_path: &str, branch: &str, base_branch: &str) -> Option<DiffStats> {
    Some(count_diff(&get_branch_diff_text(
        project_path,
        branch,
        base_branch,
    )?))
}

/// Raw signals from a tmux session for status detection.
pub struct SessionProbe {
    pub claude_alive: bool,
    pub content_hash: u64,
    pub has_permission_prompt: bool,
}

/// Probe a session for raw status signals.
pub fn probe_session(session_name: &str) -> Option<SessionProbe> {
    let target = format!("{session_name}:0");
    // Check pane_pid and pane_dead
    let output = Command::new("tmux")
        .args([
            "display-message",
            "-t",
            &target,
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

    let content = capture_pane_plain(&target).unwrap_or_default();
    let content_hash = hash_content(&content);
    let has_permission_prompt = detect_attention_dialog(&content);

    Some(SessionProbe {
        claude_alive,
        content_hash,
        has_permission_prompt,
    })
}

/// Whether the pane shows an active dialog needing the user (permission
/// prompt or question selector).
///
/// Two guards against false positives from dialog text merely echoed in the
/// transcript: markers must appear near the bottom of the pane (active
/// dialogs render there), and the pane must not currently show the regular
/// input prompt — a bare `❯`/`>` line — which Claude Code replaces while a
/// dialog is open.
fn detect_attention_dialog(content: &str) -> bool {
    let nonempty: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();

    let at_input_prompt = nonempty[nonempty.len().saturating_sub(8)..]
        .iter()
        .any(|l| matches!(l.trim(), "❯" | ">"));
    if at_input_prompt {
        return false;
    }

    let tail = nonempty[nonempty.len().saturating_sub(12)..].join("\n");
    PERMISSION_PROMPTS.iter().any(|p| tail.contains(p))
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

/// Get the PR URL for a branch using the `gh` CLI.
pub fn get_pr_url(project_path: &str, branch: &str) -> Option<String> {
    let output = Command::new("gh")
        .args(["pr", "view", branch, "--json", "url", "-q", ".url"])
        .current_dir(project_path)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if url.is_empty() { None } else { Some(url) }
}

pub fn next_session_number(project_name: &str, task_name: &str, sessions: &[TmuxSession]) -> u32 {
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
        .filter(|s| s.project_name == sanitize(project_name) && s.task_name == sanitize(task_name))
        .cloned()
        .collect()
}

/// All adhoc sessions belonging to a project.
pub fn adhoc_sessions_for_project(
    project_name: &str,
    sessions: &[TmuxSession],
) -> Vec<TmuxSession> {
    sessions
        .iter()
        .filter(|s| s.project_name == sanitize(project_name) && is_adhoc_marker(&s.task_name))
        .cloned()
        .collect()
}

/// Delete a task and all its sessions, worktrees, branches, and config files.
/// Returns a description of what was cleaned up.
pub fn delete_task(
    project_name: &str,
    project_path: &str,
    task_name: &str,
    task_branch: &str,
    sessions: &[TmuxSession],
) -> String {
    let task_sessions = sessions_for_task(project_name, task_name, sessions);
    let session_count = task_sessions.len();

    // Collect tmux names of live sessions so we can identify orphaned records
    let live_names: std::collections::HashSet<&str> =
        task_sessions.iter().map(|s| s.name.as_str()).collect();

    // Kill all live tmux sessions (this also removes worktrees + session branches)
    for session in &task_sessions {
        let _ = kill_session(&session.name);
    }

    // Also clean up any orphaned session records (tmux session already dead)
    let records = crate::config::load_sessions();
    for (tmux_name, record) in &records {
        if record.project_name == sanitize(project_name)
            && record.task_name == sanitize(task_name)
            && !live_names.contains(tmux_name.as_str())
        {
            // This record's tmux session is dead — clean up its worktree and branch
            let wt_path = worktree_dir(
                &record.project_name,
                &record.task_name,
                &record.session_name,
            );
            if record.use_worktree {
                let session_branch = format!(
                    "{}-{}",
                    sanitize(task_branch),
                    sanitize(&record.session_name)
                );
                let _ = kill_session_with_fallback(
                    tmux_name,
                    Some(SessionCleanupInfo {
                        project_path: record.project_path.clone(),
                        worktree_path: wt_path.to_string_lossy().to_string(),
                        branch_name: Some(session_branch),
                    }),
                );
            }
        }
    }

    // Clean up any remaining worktree directories for this task that weren't covered above
    // (e.g. if session records were also lost)
    let task_wt_prefix = format!("{}-", sanitize(task_name));
    let project_wt_dir = crate::config::base_dir()
        .join("worktrees")
        .join(sanitize(project_name));
    if project_wt_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&project_wt_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with(&task_wt_prefix) && entry.path().is_dir() {
                    // Derive branch name from worktree before removing
                    let wt_path_str = entry.path().to_string_lossy().to_string();
                    let branch = Command::new("git")
                        .args(["-C", &wt_path_str, "rev-parse", "--abbrev-ref", "HEAD"])
                        .output()
                        .ok()
                        .filter(|o| o.status.success())
                        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

                    let _ = Command::new("git")
                        .args([
                            "-C",
                            project_path,
                            "worktree",
                            "remove",
                            "--force",
                            &wt_path_str,
                        ])
                        .output();

                    if let Some(branch_name) = branch {
                        if !branch_name.is_empty()
                            && branch_name != "main"
                            && branch_name != "master"
                        {
                            let _ = Command::new("git")
                                .args(["-C", project_path, "branch", "-D", &branch_name])
                                .output();
                        }
                    }
                }
            }
        }
    }

    // Prune any stale worktree references
    let _ = Command::new("git")
        .args(["-C", project_path, "worktree", "prune"])
        .output();

    // Delete cached task files (pr_url.txt, stack.json)
    let _ = std::fs::remove_dir_all(crate::config::task_dir(project_name, task_branch));

    // Delete the task branch itself (session branches are already cleaned up above)
    if !task_branch.is_empty() && task_branch != "main" && task_branch != "master" {
        let _ = Command::new("git")
            .args(["-C", project_path, "branch", "-D", task_branch])
            .output();
    }

    if session_count > 0 {
        format!(
            "Deleted task '{}' and {} session(s)",
            task_name, session_count
        )
    } else {
        format!("Deleted task '{}'", task_name)
    }
}

/// Reap an orphaned session record whose task no longer exists in config.
///
/// Removes the worktree directory and cached task files, but deliberately
/// PRESERVES the git branch so any committed work stays recoverable. This is
/// meant for automatic startup reconciliation, where silently deleting branches
/// would be unsafe. Explicit, user-initiated deletion still goes through
/// [`delete_task`], which also removes the branch.
pub fn cleanup_orphan_session(record: &crate::config::SessionRecord) {
    // Remove the worktree directory if this session used one (committed work
    // remains on the branch, which we keep).
    if record.use_worktree {
        let wt_path = worktree_dir(
            &record.project_name,
            &record.task_name,
            &record.session_name,
        );
        let wt_str = wt_path.to_string_lossy().to_string();
        if wt_path.exists() {
            let _ = Command::new("git")
                .args([
                    "-C",
                    &record.project_path,
                    "worktree",
                    "remove",
                    "--force",
                    &wt_str,
                ])
                .output();
        }
        // Prune stale worktree references regardless, in case the dir was
        // already removed but git still tracks it.
        let _ = Command::new("git")
            .args(["-C", &record.project_path, "worktree", "prune"])
            .output();
    }

    // Remove the cached task directory (pr_url.txt, stack.json). The dir is
    // shared by all sessions of a task; since orphan status is per-task, every
    // session of this task is being reaped together.
    let _ = std::fs::remove_dir_all(crate::config::task_dir(
        &record.project_name,
        &record.task_branch,
    ));
}

/// Clean up worktree and task config directories for a project.
pub fn cleanup_project_dirs(project_name: &str) {
    let sanitized = sanitize(project_name);
    let base = crate::config::base_dir();

    // Remove worktree directory for this project
    let wt_dir = base.join("worktrees").join(&sanitized);
    if wt_dir.exists() {
        let _ = std::fs::remove_dir_all(&wt_dir);
    }

    // Remove task config directory for this project
    let task_dir = base.join("tasks").join(&sanitized);
    if task_dir.exists() {
        let _ = std::fs::remove_dir_all(&task_dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- spr status parsing ---

    #[test]
    fn parse_spr_status_reverses_to_bottom_top() {
        // spr lists top→bottom; we return bottom→top (merge order).
        let out = "\
https://github.com/o/r/pull/14 : Add dashboard
https://github.com/o/r/pull/13 : Add API
https://github.com/o/r/pull/12 : Add model
";
        let prs = parse_spr_status(out);
        assert_eq!(prs.len(), 3);
        assert_eq!(prs[0].0, "https://github.com/o/r/pull/12");
        assert_eq!(prs[0].1, "Add model");
        assert_eq!(prs[2].1, "Add dashboard");
    }

    #[test]
    fn parse_spr_status_ignores_non_pr_lines() {
        let out = "warming up\nhttps://github.com/o/r/pull/1 : Title\n\n";
        let prs = parse_spr_status(out);
        assert_eq!(prs.len(), 1);
        assert_eq!(prs[0].1, "Title");
    }

    // --- sanitize ---

    #[test]
    fn sanitize_alphanumeric_unchanged() {
        assert_eq!(sanitize("hello123"), "hello123");
    }

    #[test]
    fn sanitize_replaces_special_chars() {
        assert_eq!(sanitize("hello world!"), "hello-world");
    }

    #[test]
    fn sanitize_collapses_hyphens() {
        assert_eq!(sanitize("a--b---c"), "a-b-c");
    }

    #[test]
    fn sanitize_trims_leading_trailing_hyphens() {
        assert_eq!(sanitize("-hello-"), "hello");
    }

    #[test]
    fn sanitize_replaces_dots_and_slashes() {
        assert_eq!(sanitize("my.project/path"), "my-project-path");
    }

    #[test]
    fn sanitize_replaces_underscores_with_hyphens() {
        // Underscores are not alphanumeric or '-', so they become hyphens
        assert_eq!(sanitize("a__b"), "a-b");
    }

    // --- run sessions ---

    #[test]
    fn run_session_name_is_prefixed_and_sanitized() {
        assert_eq!(run_session_name("My App"), "cmrun-My-App");
        // The prefix is intentionally unparseable as a managed session.
        assert!(TmuxSession::from_tmux_name(&run_session_name("App")).is_none());
    }

    #[test]
    fn is_shell_command_detects_shells_and_processes() {
        assert!(is_shell_command("zsh"));
        assert!(is_shell_command("bash"));
        assert!(is_shell_command("-zsh")); // login shell form
        assert!(!is_shell_command("node"));
        assert!(!is_shell_command("npm"));
        assert!(!is_shell_command("cargo"));
    }

    // --- to_branch_name ---

    #[test]
    fn branch_name_lowercases() {
        assert_eq!(to_branch_name("Fix Bug"), "fix-bug");
    }

    #[test]
    fn branch_name_strips_special_chars() {
        assert_eq!(to_branch_name("Add feature #123!"), "add-feature-123");
    }

    #[test]
    fn branch_name_collapses_hyphens() {
        assert_eq!(to_branch_name("a   b"), "a-b");
    }

    #[test]
    fn branch_name_trims_edges() {
        assert_eq!(to_branch_name(" hello "), "hello");
    }

    // --- TmuxSession::from_tmux_name ---

    #[test]
    fn parse_valid_session_name() {
        let session = TmuxSession::from_tmux_name("cm__myproject__mytask__mysession").unwrap();
        assert_eq!(session.project_name, "myproject");
        assert_eq!(session.task_name, "mytask");
        assert_eq!(session.session_name, "mysession");
        assert_eq!(session.name, "cm__myproject__mytask__mysession");
    }

    #[test]
    fn parse_session_with_hyphens() {
        let session = TmuxSession::from_tmux_name("cm__my-project__my-task__my-session").unwrap();
        assert_eq!(session.project_name, "my-project");
        assert_eq!(session.task_name, "my-task");
        assert_eq!(session.session_name, "my-session");
    }

    #[test]
    fn parse_rejects_no_prefix() {
        assert!(TmuxSession::from_tmux_name("myproject__task__session").is_none());
    }

    #[test]
    fn parse_rejects_too_few_parts() {
        assert!(TmuxSession::from_tmux_name("cm__project__task").is_none());
    }

    #[test]
    fn parse_rejects_unrelated_session() {
        assert!(TmuxSession::from_tmux_name("random-session").is_none());
    }

    // --- build_tmux_name ---

    #[test]
    fn build_tmux_name_basic() {
        assert_eq!(
            build_tmux_name("proj", "task", "sess"),
            "cm__proj__task__sess"
        );
    }

    #[test]
    fn build_tmux_name_sanitizes_parts() {
        let name = build_tmux_name("my project", "my task", "my session");
        assert_eq!(name, "cm__my-project__my-task__my-session");
    }

    #[test]
    fn build_tmux_name_roundtrips() {
        let name = build_tmux_name("proj", "task", "sess");
        let parsed = TmuxSession::from_tmux_name(&name).unwrap();
        assert_eq!(parsed.project_name, "proj");
        assert_eq!(parsed.task_name, "task");
        assert_eq!(parsed.session_name, "sess");
    }

    // --- adhoc helpers ---

    #[test]
    fn adhoc_marker_recognises_canonical() {
        assert!(is_adhoc_marker("adhoc"));
        assert!(is_adhoc_marker("Adhoc"));
        assert!(is_adhoc_marker("ADHOC"));
    }

    #[test]
    fn adhoc_marker_rejects_other_names() {
        assert!(!is_adhoc_marker("ad-hoc"));
        assert!(!is_adhoc_marker("adhocs"));
        assert!(!is_adhoc_marker("explore"));
    }

    #[test]
    fn adhoc_tmux_name_uses_marker_slot() {
        let name = build_tmux_name("proj", ADHOC_MARKER, "explore");
        assert_eq!(name, "cm__proj__adhoc__explore");
        let parsed = TmuxSession::from_tmux_name(&name).unwrap();
        assert!(is_adhoc_marker(&parsed.task_name));
    }

    #[test]
    fn adhoc_sessions_for_project_filters() {
        let sessions = vec![
            TmuxSession::from_tmux_name("cm__proj__adhoc__a").unwrap(),
            TmuxSession::from_tmux_name("cm__proj__task1__1").unwrap(),
            TmuxSession::from_tmux_name("cm__other__adhoc__a").unwrap(),
        ];
        let adhoc = adhoc_sessions_for_project("proj", &sessions);
        assert_eq!(adhoc.len(), 1);
        assert_eq!(adhoc[0].session_name, "a");
    }

    // --- shell_escape ---

    #[test]
    fn shell_escape_simple() {
        assert_eq!(shell_escape("hello"), "'hello'");
    }

    #[test]
    fn shell_escape_with_single_quotes() {
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
    }

    #[test]
    fn shell_escape_with_spaces() {
        assert_eq!(shell_escape("hello world"), "'hello world'");
    }

    // --- DiffStats ---

    // --- build_initial_prompt ---

    #[test]
    fn initial_prompt_none_when_empty() {
        assert!(build_initial_prompt(&[], None).is_none());
        assert!(build_initial_prompt(&[], Some("")).is_none());
    }

    #[test]
    fn initial_prompt_passthrough_no_skills() {
        assert_eq!(
            build_initial_prompt(&[], Some("do stuff")),
            Some("do stuff".into())
        );
    }

    #[test]
    fn initial_prompt_skills_only() {
        let result = build_initial_prompt(&["/prime".into()], None).unwrap();
        assert!(result.contains("/prime"));
        assert!(!result.contains("Task:"));
    }

    #[test]
    fn initial_prompt_skills_and_prompt() {
        let result =
            build_initial_prompt(&["/prime".into(), "/caveman ultra".into()], Some("fix bug"))
                .unwrap();
        assert!(result.contains("1. /prime"));
        assert!(result.contains("2. /caveman ultra"));
        assert!(result.contains("Task: fix bug"));
    }

    // --- DiffStats ---

    #[test]
    fn diff_stats_empty() {
        let stats = DiffStats {
            added: 0,
            removed: 0,
        };
        assert!(stats.is_empty());
    }

    #[test]
    fn diff_stats_not_empty() {
        let stats = DiffStats {
            added: 5,
            removed: 3,
        };
        assert!(!stats.is_empty());
    }

    // --- detect_attention_dialog ---

    #[test]
    fn attention_for_active_question_dialog() {
        let pane = "⏺ Some earlier output\n\n\
                    Which approach should we take?\n\
                    ❯ 1. Option A\n  \
                    2. Option B\n  \
                    3. Option C\n\n  \
                    Enter to confirm";
        assert!(detect_attention_dialog(pane));
    }

    #[test]
    fn attention_for_active_permission_prompt() {
        let pane = "⏺ Bash(rm -rf build)\n\n\
                    Do you want to proceed?\n\
                    ❯ 1. Yes\n  \
                    2. Yes, allow all edits during this session\n  \
                    3. No, and tell Claude what to do differently";
        assert!(detect_attention_dialog(pane));
    }

    #[test]
    fn no_attention_when_idle_at_input_prompt() {
        // Dialog-like text echoed in the transcript above an idle input box
        // (bare ❯ line) must not count as an active dialog.
        let pane = "⏺ Added \"❯ 1.\" to the marker list\n\n\
                    Do you want to test this?\n\
                    ────────────\n\
                    ❯ \n\
                    ────────────\n  \
                    -- INSERT -- ⏵⏵ bypass permissions on";
        assert!(!detect_attention_dialog(pane));
    }

    #[test]
    fn no_attention_when_marker_scrolled_far_up() {
        let mut pane = String::from("❯ 1. Old answered dialog\n");
        for i in 0..20 {
            pane.push_str(&format!("output line {i}\n"));
        }
        assert!(!detect_attention_dialog(&pane));
    }

    #[test]
    fn no_attention_on_plain_output() {
        assert!(!detect_attention_dialog("⏺ Done. All tests pass.\n"));
    }
}
