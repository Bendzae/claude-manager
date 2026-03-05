use std::collections::{HashMap, HashSet};

use anyhow::Result;

use crate::config::{Config, Project, Task};
use crate::tmux::{self, DiffStats, SessionStatus, TmuxSession};
use crate::worker::{self, Selection, TaskInfo, Worker};

#[derive(Debug, Clone)]
pub enum ListItem {
    Project {
        project: Project,
    },
    Task {
        project_name: String,
        project_path: String,
        task: Task,
    },
    Session {
        project_name: String,
        project_path: String,
        task: Task,
        session: TmuxSession,
    },
}

pub use worker::PreviewMode;

#[derive(Debug, PartialEq)]
pub enum InputMode {
    Normal,
    AddProjectName,
    AddTaskName,
    AddSessionName,
    ConfirmDelete,
    RenameProject,
    RenameTask,
    RenameSession,
    MergeCommitMessage,
}

pub struct App {
    pub config: Config,
    pub sessions: Vec<TmuxSession>,
    pub items: Vec<ListItem>,
    pub selected: usize,
    pub input_mode: InputMode,
    pub input_buffer: String,
    pub use_worktree: bool,
    pub status_message: Option<String>,
    pub should_quit: bool,
    pub should_attach: Option<String>,
    pub pending_project_path: Option<String>,
    pub preview_content: Option<String>,
    pub preview_mode: PreviewMode,
    pub task_diff: Option<DiffStats>,
    pub collapsed: HashSet<String>,
    pub session_statuses: HashMap<String, SessionStatus>,
    pub diff_stats: HashMap<String, DiffStats>,
    pub task_diff_stats: HashMap<String, DiffStats>,
    pub tick: usize,
    pub worker: Worker,
}

fn project_key(name: &str) -> String {
    format!("p:{name}")
}

fn task_key(project: &str, task: &str) -> String {
    format!("t:{project}:{task}")
}

impl App {
    pub fn new() -> Result<Self> {
        let config = Config::load()?;
        let sessions = tmux::list_sessions().unwrap_or_default();
        let mut app = App {
            config,
            sessions,
            items: vec![],
            selected: 0,
            input_mode: InputMode::Normal,
            input_buffer: String::new(),
            use_worktree: true,
            status_message: None,
            should_quit: false,
            should_attach: None,
            pending_project_path: None,
            preview_content: None,
            preview_mode: PreviewMode::Output,
            task_diff: None,
            collapsed: HashSet::new(),
            session_statuses: HashMap::new(),
            diff_stats: HashMap::new(),
            task_diff_stats: HashMap::new(),
            tick: 0,
            worker: Worker::spawn(),
        };
        app.rebuild_items();
        app.check_cwd();
        Ok(app)
    }

    fn check_cwd(&mut self) {
        if let Ok(cwd) = std::env::current_dir() {
            let cwd_str = cwd.to_string_lossy().to_string();
            if cwd.join(".git").is_dir() && !self.config.has_project_at(&cwd_str) {
                self.pending_project_path = Some(cwd_str);
                self.status_message = Some(
                    "Current directory is a git repo but not registered. Press 'a' to add it."
                        .into(),
                );
            }
        }
    }

    /// Apply any pending updates from the background worker.
    pub fn apply_worker_updates(&mut self) {
        // Drain all pending updates, keep only the latest
        let mut latest = None;
        while let Ok(update) = self.worker.receiver.try_recv() {
            latest = Some(update);
        }
        if let Some(update) = latest {
            self.sessions = update.sessions;
            self.session_statuses = update.statuses;
            self.diff_stats = update.diff_stats;
            self.preview_content = update.preview_content;
            if update.task_diff.is_some() {
                self.task_diff = update.task_diff;
            }
            if !update.task_diff_stats.is_empty() {
                self.task_diff_stats = update.task_diff_stats;
            }
            self.rebuild_items();
        }
    }

    /// Tell the worker what is selected.
    pub fn sync_worker_hints(&self) {
        let selection = match self.selected_item() {
            Some(ListItem::Session { session, .. }) => Selection::Session {
                name: session.name.clone(),
                preview_mode: self.preview_mode,
            },
            Some(ListItem::Task {
                project_path,
                task,
                ..
            }) => Selection::Task {
                project_path: project_path.clone(),
                branch: task.branch.clone(),
            },
            _ => Selection::None,
        };
        let tasks: Vec<TaskInfo> = self
            .config
            .projects
            .iter()
            .flat_map(|p| {
                p.tasks.iter().map(|t| TaskInfo {
                    project_path: p.path.clone(),
                    branch: t.branch.clone(),
                })
            })
            .collect();

        if let Ok(mut hints) = self.worker.hints.lock() {
            hints.selection = selection;
            hints.tasks = tasks;
        }
    }

    pub fn rebuild_items(&mut self) {
        self.items.clear();
        for project in &self.config.projects {
            self.items.push(ListItem::Project {
                project: project.clone(),
            });

            if self.collapsed.contains(&project_key(&project.name)) {
                continue;
            }

            for task in &project.tasks {
                self.items.push(ListItem::Task {
                    project_name: project.name.clone(),
                    project_path: project.path.clone(),
                    task: task.clone(),
                });

                if self.collapsed.contains(&task_key(&project.name, &task.name)) {
                    continue;
                }

                for session in
                    tmux::sessions_for_task(&project.name, &task.name, &self.sessions)
                {
                    self.items.push(ListItem::Session {
                        project_name: project.name.clone(),
                        project_path: project.path.clone(),
                        task: task.clone(),
                        session,
                    });
                }
            }
        }
        if self.selected >= self.items.len() && !self.items.is_empty() {
            self.selected = self.items.len() - 1;
        }
    }

    pub fn selected_item(&self) -> Option<&ListItem> {
        self.items.get(self.selected)
    }

    /// Get the project context for the currently selected item.
    fn selected_project_info(&self) -> Option<(&str, &str)> {
        match self.selected_item()? {
            ListItem::Project { project } => Some((&project.name, &project.path)),
            ListItem::Task {
                project_name,
                project_path,
                ..
            } => Some((project_name, project_path)),
            ListItem::Session {
                project_name,
                project_path,
                ..
            } => Some((project_name, project_path)),
        }
    }

    /// Get the task context for the currently selected item.
    fn selected_task_info(&self) -> Option<(&str, &str, &Task)> {
        match self.selected_item()? {
            ListItem::Task {
                project_name,
                project_path,
                task,
            } => Some((project_name, project_path, task)),
            ListItem::Session {
                project_name,
                project_path,
                task,
                ..
            } => Some((project_name, project_path, task)),
            _ => None,
        }
    }

    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            self.on_selection_changed();
        }
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.items.len() {
            self.selected += 1;
            self.on_selection_changed();
        }
    }

    fn on_selection_changed(&mut self) {
        self.preview_content = None;
        self.task_diff = None;
        self.sync_worker_hints();
    }

    pub fn toggle_collapse(&mut self) {
        match self.selected_item() {
            Some(ListItem::Project { project }) => {
                let key = project_key(&project.name);
                if !self.collapsed.remove(&key) {
                    self.collapsed.insert(key);
                }
                self.rebuild_items();
            }
            Some(ListItem::Task {
                project_name,
                task,
                ..
            }) => {
                let key = task_key(project_name, &task.name);
                if !self.collapsed.remove(&key) {
                    self.collapsed.insert(key);
                }
                self.rebuild_items();
            }
            _ => {}
        }
    }

    pub fn enter_selected(&mut self) {
        if let Some(ListItem::Session { session, .. }) = self.selected_item() {
            self.should_attach = Some(session.name.clone());
        }
    }

    pub fn start_add_project(&mut self) {
        let cwd = match std::env::current_dir() {
            Ok(cwd) => cwd,
            Err(_) => {
                self.status_message = Some("Error: cannot determine current directory".into());
                return;
            }
        };
        let cwd_str = cwd.to_string_lossy().to_string();

        if !cwd.join(".git").is_dir() {
            self.status_message =
                Some("Error: current directory is not a git repository".into());
            return;
        }
        if self.config.has_project_at(&cwd_str) {
            self.status_message = Some("Project already registered".into());
            return;
        }

        self.pending_project_path = Some(cwd_str);
        self.input_mode = InputMode::AddProjectName;
        let default_name = cwd
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        self.input_buffer.clear();
        self.status_message = Some(format!("Enter project name (default: {default_name}): "));
    }

    pub fn confirm_add_project(&mut self) {
        if let Some(path) = self.pending_project_path.take() {
            let name = if self.input_buffer.trim().is_empty() {
                std::path::Path::new(&path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "unknown".into())
            } else {
                self.input_buffer.trim().to_string()
            };
            self.config.add_project(name, path);
            let _ = self.config.save();
            self.input_buffer.clear();
            self.input_mode = InputMode::Normal;
            self.status_message = None;
            self.rebuild_items();
        }
    }

    pub fn start_add_task(&mut self) {
        if self.selected_project_info().is_some() {
            self.input_mode = InputMode::AddTaskName;
            self.input_buffer.clear();
            self.status_message = Some("Task name: ".into());
        }
    }

    pub fn confirm_add_task(&mut self) {
        let task_name = self.input_buffer.trim().to_string();
        if task_name.is_empty() {
            self.cancel_input();
            return;
        }

        let (project_name, project_path) = match self.selected_project_info() {
            Some((name, path)) => (name.to_string(), path.to_string()),
            None => {
                self.cancel_input();
                return;
            }
        };

        let branch = tmux::to_branch_name(&task_name);

        match tmux::create_task_branch(&project_path, &branch) {
            Ok(()) => {
                self.config
                    .add_task(&project_name, task_name.clone(), branch.clone());
                let _ = self.config.save();
                self.status_message = Some(format!("Created task '{task_name}' on branch {branch}"));
                // Expand the project so the new task is visible
                self.collapsed.remove(&project_key(&project_name));
                self.rebuild_items();
            }
            Err(e) => {
                self.status_message = Some(format!("Error: {e}"));
            }
        }

        self.input_buffer.clear();
        self.input_mode = InputMode::Normal;
    }

    pub fn start_new_session(&mut self, use_worktree: bool) {
        let info = self
            .selected_task_info()
            .map(|(pn, _, t)| (pn.to_string(), t.name.clone()));

        if let Some((project_name, task_name)) = info {
            self.use_worktree = use_worktree;
            self.input_mode = InputMode::AddSessionName;
            self.input_buffer.clear();
            let next =
                tmux::next_session_number(&project_name, &task_name, &self.sessions);
            self.status_message = Some(format!(
                "Session name (default: {next}){}:",
                if use_worktree { " [worktree]" } else { "" }
            ));
        } else {
            self.status_message = Some("Select a task first to create a session".into());
        }
    }

    pub fn confirm_new_session(&mut self) {
        let (project_name, project_path, task) = match self.selected_task_info() {
            Some((pn, pp, t)) => (pn.to_string(), pp.to_string(), t.clone()),
            None => {
                self.cancel_input();
                return;
            }
        };

        let session_name = if self.input_buffer.trim().is_empty() {
            tmux::next_session_number(&project_name, &task.name, &self.sessions).to_string()
        } else {
            self.input_buffer.trim().to_string()
        };

        match tmux::create_session(
            &project_name,
            &project_path,
            &task.name,
            &task.branch,
            &session_name,
            self.use_worktree,
        ) {
            Ok(tmux_name) => {
                self.should_attach = Some(tmux_name);
            }
            Err(e) => {
                self.status_message = Some(format!("Error: {e}"));
            }
        }

        self.input_buffer.clear();
        self.input_mode = InputMode::Normal;
    }

    pub fn start_delete(&mut self) {
        match self.selected_item() {
            Some(ListItem::Session { .. }) => {
                self.input_mode = InputMode::ConfirmDelete;
                self.status_message = Some("Delete this session? (y/n)".into());
            }
            Some(ListItem::Task {
                project_name,
                task,
                ..
            }) => {
                let active = tmux::sessions_for_task(project_name, &task.name, &self.sessions);
                if active.is_empty() {
                    self.input_mode = InputMode::ConfirmDelete;
                    self.status_message = Some("Delete this task? (y/n)".into());
                } else {
                    self.status_message = Some(format!(
                        "Cannot delete task with {} active session(s). Delete sessions first.",
                        active.len()
                    ));
                }
            }
            _ => {}
        }
    }

    pub fn confirm_delete(&mut self) {
        match self.selected_item().cloned() {
            Some(ListItem::Session { session, .. }) => {
                match tmux::kill_session(&session.name) {
                    Ok(()) => {
                        self.status_message =
                            Some(format!("Killed session {}", session.session_name));
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Error: {e}"));
                    }
                }
            }
            Some(ListItem::Task {
                project_name,
                task,
                ..
            }) => {
                self.config.remove_task(&project_name, &task.name);
                let _ = self.config.save();
                self.status_message = Some(format!("Removed task '{}'", task.name));
                self.rebuild_items();
            }
            _ => {}
        }
        self.input_mode = InputMode::Normal;
    }

    pub fn start_rename(&mut self) {
        let (mode, name) = match self.selected_item() {
            Some(ListItem::Project { project }) => {
                (InputMode::RenameProject, project.name.clone())
            }
            Some(ListItem::Task { task, .. }) => (InputMode::RenameTask, task.name.clone()),
            Some(ListItem::Session { session, .. }) => {
                (InputMode::RenameSession, session.session_name.clone())
            }
            None => return,
        };
        let label = match mode {
            InputMode::RenameProject => "Rename project: ",
            InputMode::RenameTask => "Rename task: ",
            InputMode::RenameSession => "Rename session: ",
            _ => unreachable!(),
        };
        self.input_mode = mode;
        self.input_buffer = name;
        self.status_message = Some(label.into());
    }

    pub fn confirm_rename(&mut self) {
        let new_name = self.input_buffer.trim().to_string();
        if new_name.is_empty() {
            self.cancel_input();
            return;
        }

        match self.input_mode {
            InputMode::RenameProject => {
                if let Some(ListItem::Project { project }) = self.selected_item().cloned() {
                    let old_name = project.name.clone();
                    if old_name == new_name {
                        self.cancel_input();
                        return;
                    }

                    // Rename all tmux sessions for this project
                    let old_san = tmux::sanitize(&old_name);
                    let new_san = tmux::sanitize(&new_name);
                    for session in &self.sessions {
                        if session.project_name == old_san {
                            let new_tmux = session.name.replacen(&old_san, &new_san, 1);
                            let _ = tmux::rename_session(&session.name, &new_tmux);
                        }
                    }

                    self.config.rename_project(&old_name, new_name.clone());
                    let _ = self.config.save();
                    self.status_message = Some(format!("Renamed project to {new_name}"));
                }
            }
            InputMode::RenameTask => {
                if let Some(ListItem::Task {
                    project_name,
                    task,
                    ..
                }) = self.selected_item().cloned()
                {
                    if task.name == new_name {
                        self.cancel_input();
                        return;
                    }

                    let old_san = tmux::sanitize(&task.name);
                    let new_san = tmux::sanitize(&new_name);
                    for session in &self.sessions {
                        if session.project_name == tmux::sanitize(&project_name)
                            && session.task_name == old_san
                        {
                            let new_tmux = session.name.replacen(&old_san, &new_san, 1);
                            let _ = tmux::rename_session(&session.name, &new_tmux);
                        }
                    }

                    self.config
                        .rename_task(&project_name, &task.name, new_name.clone());
                    let _ = self.config.save();
                    self.status_message = Some(format!("Renamed task to {new_name}"));
                }
            }
            InputMode::RenameSession => {
                if let Some(ListItem::Session {
                    project_name,
                    task,
                    session,
                    ..
                }) = self.selected_item().cloned()
                {
                    if session.session_name == new_name {
                        self.cancel_input();
                        return;
                    }

                    let new_tmux = format!(
                        "cm__{}__{}__{new_name}",
                        tmux::sanitize(&project_name),
                        tmux::sanitize(&task.name),
                    );
                    match tmux::rename_session(&session.name, &new_tmux) {
                        Ok(()) => {
                            self.status_message =
                                Some(format!("Renamed session to {new_name}"));
                        }
                        Err(e) => {
                            self.status_message = Some(format!("Error: {e}"));
                        }
                    }
                }
            }
            _ => {}
        }

        self.input_buffer.clear();
        self.input_mode = InputMode::Normal;
    }

    pub fn start_merge(&mut self) {
        let (project_path, task, session) = match self.selected_item().cloned() {
            Some(ListItem::Session {
                project_path,
                task,
                session,
                ..
            }) => (project_path, task, session),
            _ => {
                self.status_message = Some("Select a session to merge".into());
                return;
            }
        };

        let wt_path = match session.worktree_path() {
            Some(p) => p.to_string_lossy().to_string(),
            None => {
                self.status_message =
                    Some("Cannot merge: session has no worktree".into());
                return;
            }
        };

        // Check if worktree has uncommitted changes
        if tmux::worktree_is_dirty(&wt_path) {
            self.input_mode = InputMode::MergeCommitMessage;
            self.input_buffer.clear();
            let default_msg = tmux::next_commit_message(&wt_path, &session.session_name);
            self.status_message = Some(format!("Commit message (default: {default_msg}): "));
        } else {
            self.do_merge(&project_path, &task.branch, &session.session_name, &wt_path);
        }
    }

    pub fn confirm_merge_commit(&mut self) {
        let (project_path, task, session) = match self.selected_item().cloned() {
            Some(ListItem::Session {
                project_path,
                task,
                session,
                ..
            }) => (project_path, task, session),
            _ => {
                self.cancel_input();
                return;
            }
        };

        let wt_path = match session.worktree_path() {
            Some(p) => p.to_string_lossy().to_string(),
            None => {
                self.cancel_input();
                return;
            }
        };

        let msg = if self.input_buffer.trim().is_empty() {
            tmux::next_commit_message(&wt_path, &session.session_name)
        } else {
            self.input_buffer.trim().to_string()
        };

        // Commit all changes
        match tmux::commit_all(&wt_path, &msg) {
            Ok(()) => {
                self.do_merge(&project_path, &task.branch, &session.session_name, &wt_path);
            }
            Err(e) => {
                self.status_message = Some(format!("Error committing: {e}"));
            }
        }

        self.input_buffer.clear();
        self.input_mode = InputMode::Normal;
    }

    fn do_merge(&mut self, project_path: &str, task_branch: &str, session_name: &str, wt_path: &str) {
        match tmux::merge_session_to_task(project_path, task_branch, session_name, wt_path) {
            Ok(msg) => self.status_message = Some(msg),
            Err(e) => self.status_message = Some(format!("Error: {e}")),
        }
    }

    pub fn update_session(&mut self) {
        let (project_path, task, session) = match self.selected_item().cloned() {
            Some(ListItem::Session {
                project_path,
                task,
                session,
                ..
            }) => (project_path, task, session),
            _ => {
                self.status_message = Some("Select a session to update".into());
                return;
            }
        };

        let wt_path = match session.worktree_path() {
            Some(p) => p.to_string_lossy().to_string(),
            None => {
                self.status_message =
                    Some("Cannot update: session has no worktree".into());
                return;
            }
        };

        match tmux::rebase_session_on_task(&project_path, &task.branch, &wt_path) {
            Ok(msg) => self.status_message = Some(msg),
            Err(e) => self.status_message = Some(format!("Error: {e}")),
        }
    }

    pub fn cancel_input(&mut self) {
        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();
        self.status_message = None;
    }

    pub fn toggle_preview_mode(&mut self) {
        self.preview_mode = match self.preview_mode {
            PreviewMode::Output => PreviewMode::Diff,
            PreviewMode::Diff => PreviewMode::Output,
        };
        self.preview_content = None; // Clear stale content from the other mode
        self.sync_worker_hints();
    }
}
