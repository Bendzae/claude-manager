use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::tmux::{self, DiffStats, SessionStatus, TmuxSession};

/// What the UI has selected.
#[derive(Clone)]
pub enum Selection {
    None,
    Task {
        project_path: String,
        branch: String,
    },
    Session {
        name: String,
        preview_mode: PreviewMode,
    },
}

#[derive(Clone, Copy, PartialEq)]
pub enum PreviewMode {
    Output,
    Diff,
    Terminal(usize), // 0-indexed terminal number
}

/// Task info for computing branch diffs.
#[derive(Clone)]
pub struct TaskInfo {
    pub project_path: String,
    pub branch: String,
}

/// Shared state the UI thread writes to, the worker thread reads from.
pub struct WorkerHints {
    pub selection: Selection,
    pub tasks: Vec<TaskInfo>,
}

/// Data produced by the background worker for the UI to consume.
pub struct WorkerUpdate {
    pub sessions: Vec<TmuxSession>,
    pub statuses: HashMap<String, SessionStatus>,
    pub diff_stats: HashMap<String, DiffStats>,
    pub preview_content: Option<String>,
    pub task_diff: Option<DiffStats>,
    /// Keyed by branch name.
    pub task_diff_stats: HashMap<String, DiffStats>,
    /// Keyed by session tmux name.
    pub terminal_counts: HashMap<String, usize>,
    /// PR URLs keyed by branch name.
    pub pr_urls: HashMap<String, String>,
}

pub struct Worker {
    pub hints: Arc<Mutex<WorkerHints>>,
    pub receiver: mpsc::Receiver<WorkerUpdate>,
}

impl Worker {
    pub fn spawn() -> Self {
        let hints = Arc::new(Mutex::new(WorkerHints {
            selection: Selection::None,
            tasks: Vec::new(),
        }));
        let (tx, rx) = mpsc::channel();

        let hints_clone = hints.clone();
        thread::spawn(move || worker_loop(hints_clone, tx));

        Worker {
            hints,
            receiver: rx,
        }
    }
}

fn worker_loop(hints: Arc<Mutex<WorkerHints>>, tx: mpsc::Sender<WorkerUpdate>) {
    let mut content_hashes: HashMap<String, u64> = HashMap::new();
    let mut stable_ticks: HashMap<String, u32> = HashMap::new();
    let mut diff_stats: HashMap<String, DiffStats> = HashMap::new();
    let mut terminal_counts: HashMap<String, usize> = HashMap::new();
    let mut pr_urls: HashMap<String, String> = HashMap::new();
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
                Some(probe) if !probe.claude_alive => {
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

        // Refresh diff stats and terminal counts less frequently (~every 2 seconds)
        if tick % 4 == 0 {
            let session_names: Vec<String> =
                sessions.iter().map(|s| s.name.clone()).collect();
            diff_stats.retain(|k, _| session_names.contains(k));
            terminal_counts.retain(|k, _| session_names.contains(k));

            for session in &sessions {
                if let Some(stats) = tmux::get_diff_stats(&session.name) {
                    diff_stats.insert(session.name.clone(), stats);
                }
                let count = tmux::count_terminal_windows(&session.name);
                terminal_counts.insert(session.name.clone(), count);
            }
        }

        // Handle selection-based content
        let (selection, tasks) = {
            let h = hints.lock().unwrap();
            (h.selection.clone(), h.tasks.clone())
        };

        // Compute task branch diffs (less frequently)
        let mut task_diff_stats: HashMap<String, DiffStats> = HashMap::new();
        if tick % 4 == 0 {
            for task in &tasks {
                if let Some(stats) = tmux::get_branch_diff(&task.project_path, &task.branch) {
                    task_diff_stats.insert(task.branch.clone(), stats);
                }
            }
        }

        // Check for PRs (infrequently, ~every 10 seconds)
        if tick % 20 == 0 {
            for task in &tasks {
                if !pr_urls.contains_key(&task.branch) {
                    if let Some(url) = tmux::get_pr_url(&task.project_path, &task.branch) {
                        pr_urls.insert(task.branch.clone(), url);
                    }
                }
            }
        }

        let (preview_content, task_diff) = match &selection {
            Selection::None => (None, None),
            Selection::Task {
                project_path,
                branch,
            } => {
                let diff = if tick % 4 == 0 {
                    tmux::get_branch_diff(project_path, branch)
                } else {
                    None
                };
                (None, diff)
            }
            Selection::Session { name, preview_mode } => {
                let content = match preview_mode {
                    PreviewMode::Output => tmux::capture_pane(&format!("{name}:0")),
                    PreviewMode::Diff => diff_stats.get(name).map(|s| s.diff_output.clone()),
                    PreviewMode::Terminal(idx) => {
                        // Terminal windows are 1-indexed (window 0 is claude)
                        let target = format!("{name}:{}", idx + 1);
                        tmux::capture_pane(&target)
                    }
                };
                (content, None)
            }
        };

        let update = WorkerUpdate {
            sessions,
            statuses,
            diff_stats: diff_stats.clone(),
            preview_content,
            task_diff,
            task_diff_stats,
            terminal_counts: terminal_counts.clone(),
            pr_urls: pr_urls.clone(),
        };

        if tx.send(update).is_err() {
            break;
        }

        tick += 1;
        thread::sleep(Duration::from_millis(500));
    }
}
