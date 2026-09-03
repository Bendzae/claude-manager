use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::tmux::{self, DiffStats, PrInfo, PrState, SessionStatus, TmuxSession};

/// Task info for computing branch diffs.
#[derive(Clone)]
pub struct TaskInfo {
    pub project_name: String,
    pub project_path: String,
    pub branch: String,
    pub base_branch: String,
}

/// Shared state the UI thread writes to, the worker thread reads from.
pub struct WorkerHints {
    pub tasks: Vec<TaskInfo>,
    /// Project name → project path, so the worker can query the current branch.
    pub project_paths: Vec<(String, String)>,
}

/// Data produced by the background worker for the UI to consume.
pub struct WorkerUpdate {
    pub sessions: Vec<TmuxSession>,
    pub statuses: HashMap<String, SessionStatus>,
    pub diff_stats: HashMap<String, DiffStats>,
    /// Keyed by branch name.
    pub task_diff_stats: HashMap<String, DiffStats>,
    /// Branch checked out in each session's worktree, keyed by session tmux name.
    pub session_branches: HashMap<String, String>,
    /// Agent harness id per session, keyed by session tmux name.
    pub session_agents: HashMap<String, String>,
    /// PR details keyed by branch name.
    pub prs: HashMap<String, PrInfo>,
    /// Current git branch for each project, keyed by project name.
    pub project_branches: HashMap<String, String>,
    /// Live "Run" sessions, keyed by tmux name (`cmrun-*`); value is true while
    /// the command is still executing, false once it dropped to a shell.
    pub run_sessions: HashMap<String, bool>,
}

pub struct Worker {
    pub hints: Arc<Mutex<WorkerHints>>,
    /// Single-slot handoff: the worker overwrites this each tick; the UI takes
    /// it. Using a mutex instead of an unbounded channel prevents updates from
    /// piling up while the main thread is blocked (e.g. during `tmux attach`).
    pub latest: Arc<Mutex<Option<WorkerUpdate>>>,
}

impl Worker {
    pub fn spawn() -> Self {
        let hints = Arc::new(Mutex::new(WorkerHints {
            tasks: Vec::new(),
            project_paths: Vec::new(),
        }));
        let latest = Arc::new(Mutex::new(None));

        let hints_clone = hints.clone();
        let latest_clone = latest.clone();
        thread::spawn(move || worker_loop(hints_clone, latest_clone));

        Worker { hints, latest }
    }
}

fn worker_loop(hints: Arc<Mutex<WorkerHints>>, latest: Arc<Mutex<Option<WorkerUpdate>>>) {
    let mut content_hashes: HashMap<String, u64> = HashMap::new();
    let mut stable_ticks: HashMap<String, u32> = HashMap::new();
    let mut diff_stats: HashMap<String, DiffStats> = HashMap::new();
    let mut session_branches: HashMap<String, String> = HashMap::new();
    let mut session_agents: HashMap<String, String> = HashMap::new();
    let mut prs: HashMap<String, PrInfo> = HashMap::new();
    let mut task_diff_stats: HashMap<String, DiffStats> = HashMap::new();
    let mut project_branches: HashMap<String, String> = HashMap::new();
    let mut tick: u64 = 0;

    loop {
        let sessions = tmux::list_sessions().unwrap_or_default();

        // Compute statuses
        let mut statuses = HashMap::new();
        const STABLE_THRESHOLD: u32 = 3;

        for session in &sessions {
            let probe = tmux::probe_session(&session.name);

            let status = match probe {
                None => {
                    content_hashes.remove(&session.name);
                    stable_ticks.remove(&session.name);
                    SessionStatus::Finished
                }
                Some(probe) if !probe.agent_alive => {
                    content_hashes.remove(&session.name);
                    stable_ticks.remove(&session.name);
                    SessionStatus::Finished
                }
                Some(probe) => {
                    let prev_hash = content_hashes.get(&session.name).copied();
                    let content_changed = prev_hash.is_some_and(|h| h != probe.content_hash);

                    content_hashes.insert(session.name.clone(), probe.content_hash);

                    if content_changed {
                        stable_ticks.insert(session.name.clone(), 0);
                        SessionStatus::Running
                    } else {
                        let ticks = stable_ticks.entry(session.name.clone()).or_insert(0);
                        *ticks = ticks.saturating_add(1);

                        if *ticks < STABLE_THRESHOLD {
                            SessionStatus::Running
                        } else if probe.has_permission_prompt {
                            SessionStatus::WaitingForPermission
                        } else {
                            SessionStatus::WaitingForInput
                        }
                    }
                }
            };

            statuses.insert(session.name.clone(), status);
        }

        // Publish statuses right away with the previously computed maps — the
        // git work below can take many seconds on a cold start, and statuses
        // are the most time-sensitive signal.
        *latest.lock().unwrap() = Some(WorkerUpdate {
            sessions: sessions.clone(),
            statuses: statuses.clone(),
            diff_stats: diff_stats.clone(),
            session_branches: session_branches.clone(),
            session_agents: session_agents.clone(),
            task_diff_stats: task_diff_stats.clone(),
            prs: prs.clone(),
            project_branches: project_branches.clone(),
            run_sessions: tmux::list_run_sessions(),
        });

        // Refresh diff stats and terminal counts less frequently (~every 2 seconds)
        if tick % 4 == 0 {
            let session_names: Vec<String> = sessions.iter().map(|s| s.name.clone()).collect();
            diff_stats.retain(|k, _| session_names.contains(k));
            session_branches.retain(|k, _| session_names.contains(k));
            session_agents.retain(|k, _| session_names.contains(k));

            for session in &sessions {
                if let Some(stats) = tmux::get_diff_stats(&session.name) {
                    diff_stats.insert(session.name.clone(), stats);
                }
                if let Some(branch) = tmux::get_session_branch(&session.name) {
                    session_branches.insert(session.name.clone(), branch);
                }
                session_agents
                    .entry(session.name.clone())
                    .or_insert_with(|| tmux::session_agent(&session.name).id().to_string());
            }
        }

        let (tasks, project_paths) = {
            let h = hints.lock().unwrap();
            (h.tasks.clone(), h.project_paths.clone())
        };

        // Compute task branch diffs (less frequently). Values persist across
        // ticks so consumers always see the latest computed stats.
        if tick % 4 == 0 {
            task_diff_stats.retain(|k, _| tasks.iter().any(|t| t.branch == *k));
            for task in &tasks {
                if let Some(stats) =
                    tmux::get_branch_diff(&task.project_path, &task.branch, &task.base_branch)
                {
                    task_diff_stats.insert(task.branch.clone(), stats);
                }
            }
        }

        // Look for new PRs every ~10 seconds; refresh known ones (review and
        // CI state change over time) every ~60 seconds. Merged PRs are final.
        if tick % 20 == 0 {
            prs.retain(|k, _| tasks.iter().any(|t| t.branch == *k));
            for task in tasks.iter() {
                let known = prs.get(&task.branch);
                let refresh = tick % 120 == 0 && known.is_some_and(|p| p.state != PrState::Merged);
                if known.is_some() && !refresh {
                    continue;
                }
                if let Some(info) = tmux::get_pr_info(&task.project_path, &task.branch) {
                    if known.is_none_or(|p| p.url != info.url) {
                        // Write PR URL to file so hooks can read it without network calls
                        let pr_path = crate::config::pr_url_path(&task.project_name, &task.branch);
                        if let Some(parent) = pr_path.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        let _ = std::fs::write(&pr_path, &info.url);
                    }
                    prs.insert(task.branch.clone(), info);
                }
            }
        }

        // Compute current branch for each project (less frequently, ~every 2 seconds)
        if tick % 4 == 0 {
            project_branches.retain(|k, _| project_paths.iter().any(|(n, _)| n == k));
            for (name, path) in &project_paths {
                if let Some(branch) = get_current_branch(path) {
                    project_branches.insert(name.clone(), branch);
                }
            }
        }

        let update = WorkerUpdate {
            sessions,
            statuses,
            diff_stats: diff_stats.clone(),
            session_branches: session_branches.clone(),
            session_agents: session_agents.clone(),
            task_diff_stats: task_diff_stats.clone(),
            prs: prs.clone(),
            project_branches: project_branches.clone(),
            run_sessions: tmux::list_run_sessions(),
        };

        *latest.lock().unwrap() = Some(update);

        tick += 1;
        thread::sleep(Duration::from_millis(500));
    }
}

fn get_current_branch(project_path: &str) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["-C", project_path, "rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}
