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
}

/// Shared state the UI thread writes to, the worker thread reads from.
pub struct WorkerHints {
    pub selection: Selection,
}

/// Data produced by the background worker for the UI to consume.
pub struct WorkerUpdate {
    pub sessions: Vec<TmuxSession>,
    pub statuses: HashMap<String, SessionStatus>,
    pub diff_stats: HashMap<String, DiffStats>,
    pub preview_content: Option<String>,
    pub task_diff: Option<DiffStats>,
}

pub struct Worker {
    pub hints: Arc<Mutex<WorkerHints>>,
    pub receiver: mpsc::Receiver<WorkerUpdate>,
}

impl Worker {
    pub fn spawn() -> Self {
        let hints = Arc::new(Mutex::new(WorkerHints {
            selection: Selection::None,
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

        // Refresh diff stats less frequently (~every 2 seconds)
        if tick % 4 == 0 {
            let session_names: Vec<String> =
                sessions.iter().map(|s| s.name.clone()).collect();
            diff_stats.retain(|k, _| session_names.contains(k));

            for session in &sessions {
                if let Some(stats) = tmux::get_diff_stats(&session.name) {
                    diff_stats.insert(session.name.clone(), stats);
                }
            }
        }

        // Handle selection-based content
        let selection = {
            let h = hints.lock().unwrap();
            h.selection.clone()
        };

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
                    PreviewMode::Output => tmux::capture_pane(name),
                    PreviewMode::Diff => diff_stats.get(name).map(|s| s.diff_output.clone()),
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
        };

        if tx.send(update).is_err() {
            break;
        }

        tick += 1;
        thread::sleep(Duration::from_millis(500));
    }
}
