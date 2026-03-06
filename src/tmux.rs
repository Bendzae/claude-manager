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
        .args(["-C", project_path, "rev-parse", "--verify", &format!("refs/heads/{branch}")])
        .output()
        .is_ok_and(|o| o.status.success())
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
        let output = Command::new("git")
            .args(["-C", project_path, "branch", branch_name, "main"])
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
    initial_prompt: Option<&str>,
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

        work_dir = worktree_path_str.clone();
    } else {
        work_dir = project_path.to_string();
    }

    let context_path = crate::config::task_context_path(project_name, task_branch);
    let context_path_str = context_path.to_string_lossy().to_string();

    // Set up shared task context with hooks BEFORE starting Claude so it picks up settings
    setup_task_context(&work_dir, task_name, task_branch, &context_path);

    let system_prompt = format!(
        "SHARED TASK CONTEXT: You are one of potentially multiple agents working on the same task. \
         A shared context file at {context_path_str} is automatically injected into every prompt."
    );

    let mut claude_cmd = format!(
        "claude --dangerously-skip-permissions --append-system-prompt {}",
        shell_escape(&system_prompt)
    );
    if let Some(prompt) = initial_prompt {
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

pub fn kill_session(name: &str) -> Result<()> {
    let project_path = get_session_env(name, "CM_PROJECT_PATH");
    let worktree_path = get_session_env(name, "CM_WORKTREE_PATH");

    // Kill the tmux session
    let output = Command::new("tmux")
        .args(["kill-session", "-t", name])
        .output()?;

    if !output.status.success() {
        bail!("Failed to kill tmux session");
    }

    // Clean up worktree and its branch if applicable
    if let (Some(proj_path), Some(wt_path)) = (project_path, worktree_path) {
        // Get the branch name before removing the worktree
        let branch = Command::new("git")
            .args(["-C", &wt_path, "rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

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

/// Set up shared task context for a session.
/// Creates the context file if it doesn't exist and writes hooks into the work directory.
fn setup_task_context(
    work_dir: &str,
    task_name: &str,
    task_branch: &str,
    context_path: &Path,
) {
    let context_path_str = context_path.to_string_lossy().to_string();

    // Create context file with initial content if it doesn't exist
    if !context_path.exists() {
        if let Some(parent) = context_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let initial = format!(
            "# {task_name}\nBranch: {task_branch}\n"
        );
        let _ = fs::write(&context_path, initial);
    }

    // Write .claude/settings.local.json with hooks
    let claude_dir = Path::new(work_dir).join(".claude");
    let _ = fs::create_dir_all(&claude_dir);

    // Stop hook: reads JSON input from stdin to check stop_hook_active.
    // If already re-running from a stop hook, allow stopping.
    // Otherwise, output JSON with decision:block to force context update.
    // Write stop hook as a script that runs claude -p in the background to update context.
    let hook_dir = context_path.parent().unwrap_or(context_path);
    let hook_script_path = hook_dir.join("stop-hook.sh");
    let stop_script = format!(
        r#"#!/bin/bash
CONTEXT_FILE='{context}'
INPUT=$(cat)
MSG=$(echo "$INPUT" | jq -r '.last_assistant_message // empty')
SUMMARY=$(echo "$INPUT" | jq -r '.transcript_summary // empty')
[ -z "$MSG" ] && exit 0

TMPFILE=$(mktemp)
cat > "$TMPFILE" <<PROMPT_END
You maintain a shared task context file. Below is the current file content between <current> tags, a summary of the full conversation between <summary> tags, and the latest agent message between <message> tags.

<current>
$(cat "$CONTEXT_FILE" 2>/dev/null || echo '(empty)')
</current>

<summary>
$SUMMARY
</summary>

<message>
$MSG
</message>

Update the file based on the conversation summary and latest message. Maintain a clear, evolving summary of what the task is trying to achieve, what has been done, and what is known. Include anything useful for other agents picking up this task. Remove outdated info. Keep it concise. Output ONLY the raw file content, no wrapping, no fences, no delimiters.
PROMPT_END

unset CLAUDECODE CLAUDE_CODE_ENTRYPOINT CLAUDE_BASH_MAINTAIN_PROJECT_WORKING_DIR CLAUDE_PROJECT_DIR
cd /tmp
claude -p < "$TMPFILE" > "$CONTEXT_FILE.tmp" 2>/dev/null && mv "$CONTEXT_FILE.tmp" "$CONTEXT_FILE"
rm -f "$TMPFILE" "$CONTEXT_FILE.tmp"
exit 0"#,
        context = context_path_str
    );
    let _ = fs::write(&hook_script_path, &stop_script);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&hook_script_path, fs::Permissions::from_mode(0o755));
    }
    let hook_script_str = hook_script_path.to_string_lossy().to_string();

    let settings = serde_json::json!({
        "hooks": {
            "UserPromptSubmit": [{
                "hooks": [{
                    "type": "command",
                    "command": format!("echo '--- SHARED TASK CONTEXT (other agents working on this task update this file) ---' && cat '{}' 2>/dev/null || true", context_path_str)
                }]
            }],
            "Stop": [{
                "hooks": [{
                    "type": "command",
                    "command": hook_script_str
                }]
            }]
        }
    });

    let settings_path = claude_dir.join("settings.local.json");

    // Merge with existing settings if present
    let mut existing: serde_json::Value = fs::read_to_string(&settings_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));

    if let Some(obj) = existing.as_object_mut() {
        obj.insert("hooks".to_string(), settings["hooks"].clone());
    }

    let _ = fs::write(&settings_path, serde_json::to_string_pretty(&existing).unwrap_or_default());
}

/// Check if a worktree has uncommitted changes.
pub fn worktree_is_dirty(worktree_path: &str) -> bool {
    Command::new("git")
        .args(["-C", worktree_path, "status", "--porcelain"])
        .output()
        .map(|o| {
            o.status.success()
                && !String::from_utf8_lossy(&o.stdout).trim().is_empty()
        })
        .unwrap_or(false)
}

/// Generate a default commit message: "<session_name>-<N>" where N increments.
pub fn next_commit_message(worktree_path: &str, session_name: &str) -> String {
    let count = Command::new("git")
        .args(["-C", worktree_path, "rev-list", "--count", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<u32>().ok())
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
    let output = Command::new("git")
        .args(["-C", project_path, "push", "--force-with-lease", "-u", "origin", branch])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Push failed: {stderr}");
    }

    Ok(format!("Pushed {branch} to origin"))
}

pub fn update_task_branch(project_path: &str, branch: &str) -> Result<String> {
    // Fetch latest main
    let _ = Command::new("git")
        .args(["-C", project_path, "fetch", "origin", "main"])
        .output();

    // Find worktree with this branch, or use project path
    let rebase_dir = find_worktree_for_branch(project_path, branch)
        .unwrap_or_else(|| project_path.to_string());

    let output = Command::new("git")
        .args(["-C", &rebase_dir, "rebase", "origin/main"])
        .output()?;

    if !output.status.success() {
        let _ = Command::new("git")
            .args(["-C", &rebase_dir, "rebase", "--abort"])
            .output();
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Rebase failed, aborted. Resolve manually.\n{stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.contains("is up to date") {
        Ok(format!("Branch {branch} is already up to date with main"))
    } else {
        Ok(format!("Rebased {branch} onto latest main"))
    }
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
        return Ok(format!("{session_branch} is already up to date with {task_branch}"));
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
            .args([
                "-C",
                &task_wt_path,
                "merge",
                "--ff-only",
                &session_branch,
            ])
            .output()?;

        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            return Ok(format!("Merged {session_branch} into {task_branch} (ff)\n{}", stdout.trim()));
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

/// Count the number of terminal windows (non-claude windows) in a session.
/// Window 0 is always claude; terminals are windows 1+.
pub fn count_terminal_windows(session_name: &str) -> usize {
    let output = Command::new("tmux")
        .args(["list-windows", "-t", session_name, "-F", "#{window_index}"])
        .output();
    match output {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            // Count windows with index > 0
            stdout
                .lines()
                .filter(|line| line.trim().parse::<usize>().is_ok_and(|i| i > 0))
                .count()
        }
        _ => 0,
    }
}

/// Create a new terminal window in the session. Returns the window index.
pub fn create_terminal_window(session_name: &str) -> Result<usize> {
    // Get the working directory from window 0 (claude)
    let dir_output = Command::new("tmux")
        .args([
            "display-message",
            "-t",
            &format!("{session_name}:0"),
            "-p",
            "#{pane_current_path}",
        ])
        .output()?;
    let work_dir = String::from_utf8_lossy(&dir_output.stdout).trim().to_string();

    let output = Command::new("tmux")
        .args([
            "new-window",
            "-t",
            session_name,
            "-c",
            &work_dir,
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

/// Kill a terminal window by its 0-indexed terminal number (window index = terminal_idx + 1).
pub fn kill_terminal_window(session_name: &str, terminal_idx: usize) -> Result<()> {
    let window_idx = terminal_idx + 1;
    let output = Command::new("tmux")
        .args([
            "kill-window",
            "-t",
            &format!("{session_name}:{window_idx}"),
        ])
        .output()?;

    if !output.status.success() {
        bail!("Failed to kill terminal window");
    }
    Ok(())
}

/// Attach to a specific window in a session.
pub fn attach_session_window(session_name: &str, window_idx: usize) -> Result<()> {
    // Select the window first, then attach
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

    // Diff working tree against the task branch (includes committed + uncommitted changes)
    let output = Command::new("git")
        .args(["-C", &worktree_path, "--no-pager", "diff", &diff_target])
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

/// Compute diff stats for a task branch against main.
pub fn get_branch_diff(project_path: &str, branch: &str) -> Option<DiffStats> {
    // Try origin/main first, fall back to main
    let base = if Command::new("git")
        .args(["-C", project_path, "rev-parse", "--verify", "origin/main"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        "origin/main"
    } else {
        "main"
    };

    let output = Command::new("git")
        .args(["-C", project_path, "--no-pager", "diff", &format!("{base}...{branch}")])
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

/// Get the PR URL for a branch using the `gh` CLI.
pub fn get_pr_url(project_path: &str, branch: &str) -> Option<String> {
    let output = Command::new("gh")
        .args([
            "pr", "view", branch,
            "--json", "url",
            "-q", ".url",
        ])
        .current_dir(project_path)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if url.is_empty() { None } else { Some(url) }
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

/// Name for the tmux session that hosts the vim context editor.
pub fn context_session_name(project_name: &str, task_branch: &str) -> String {
    format!(
        "cm-ctx__{}_{}",
        sanitize(project_name),
        sanitize(task_branch)
    )
}

/// Ensure a tmux session running vim on the context file exists.
/// Returns the session name. Creates it if it doesn't already exist.
pub fn ensure_context_session(project_name: &str, task_branch: &str) -> String {
    let name = context_session_name(project_name, task_branch);
    let context_path = crate::config::task_context_path(project_name, task_branch);
    let context_str = context_path.to_string_lossy().to_string();

    // Check if session already exists
    let exists = Command::new("tmux")
        .args(["has-session", "-t", &name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !exists {
        // Ensure context file exists
        if !context_path.exists() {
            if let Some(parent) = context_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::write(&context_path, format!("# {task_branch}\n"));
        }

        let _ = Command::new("tmux")
            .args([
                "new-session",
                "-d",
                "-s",
                &name,
                "-x",
                "200",
                "-y",
                "50",
                "nvim",
                &context_str,
            ])
            .output();
    }

    name
}

/// Send keys to a tmux session.
pub fn send_keys(session_name: &str, keys: &str) {
    let _ = Command::new("tmux")
        .args(["send-keys", "-t", session_name, keys])
        .output();
}

/// Kill the context vim session for a task.
pub fn kill_context_session(project_name: &str, task_branch: &str) {
    let name = context_session_name(project_name, task_branch);
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", &name])
        .output();
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn diff_stats_empty() {
        let stats = DiffStats { added: 0, removed: 0, diff_output: String::new() };
        assert!(stats.is_empty());
    }

    #[test]
    fn diff_stats_not_empty() {
        let stats = DiffStats { added: 5, removed: 3, diff_output: "some diff".into() };
        assert!(!stats.is_empty());
    }
}
