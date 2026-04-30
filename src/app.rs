use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;

use anyhow::Result;

use crate::config::{self, Config, KeyBindings, Project, Task};
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
    AdhocGroup {
        project_name: String,
        project_path: String,
        session_count: usize,
    },
    AdhocSession {
        project_name: String,
        project_path: String,
        session: TmuxSession,
    },
}

pub use worker::PreviewMode;

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum InputMode {
    Normal,
    ContextMenu,
    AddProjectName,
    AddTaskName,
    AddTaskBranch,
    AddTaskPrompt,
    AddSessionName,
    AddSessionPrompt,
    AddAdhocSessionName,
    ConfirmDelete,
    RenameProject,
    RenameTask,
    RenameSession,
    RenameAdhocSession,
    MergeCommitMessage,
    ConfirmCreatePr,
    AddDiffComment,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DiffSide {
    Added,
    Removed,
    Context,
}

#[derive(Debug, Clone)]
pub struct DiffComment {
    pub file: String,
    pub line: usize,
    pub side: DiffSide,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct PendingComment {
    pub session_name: String,
    pub file: String,
    pub line: usize,
    pub side: DiffSide,
}

#[derive(Debug, Clone)]
pub struct ContextMenuItem {
    pub key: char,
    pub label: &'static str,
    pub action: ContextAction,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ContextAction {
    AddTask,
    NewSession,
    NewSessionNoWorktree,
    NewAdhocSession,
    Delete,
    Rename,
    Merge,
    Update,
    Push,
    OpenPr,
    Checkout,
    CreateTerminal,
    KillTerminal,
    ToggleAutoContext,
}

pub struct App {
    pub config: Config,
    pub keybindings: KeyBindings,
    pub sessions: Vec<TmuxSession>,
    pub items: Vec<ListItem>,
    pub selected: usize,
    pub input_mode: InputMode,
    pub input_buffer: String,
    pub use_worktree: bool,
    pub status_message: Option<String>,
    pub should_quit: bool,
    pub should_attach: Option<String>,
    pub should_attach_window: Option<(String, usize)>,
    pub should_open_editor: Option<PathBuf>,
    pub pending_project_path: Option<String>,
    pub pending_task_name: Option<String>,
    pub pending_task_branch: Option<String>,
    pub pending_session_name: Option<String>,
    pub preview_content: Option<String>,
    pub preview_mode: PreviewMode,
    pub task_diff: Option<DiffStats>,
    pub task_context_content: Option<String>,
    pub collapsed: HashSet<String>,
    pub session_statuses: HashMap<String, SessionStatus>,
    pub diff_stats: HashMap<String, DiffStats>,
    pub merged_sessions: HashMap<String, bool>,
    pub task_diff_stats: HashMap<String, DiffStats>,
    pub preview_scroll: usize,
    /// Number of terminal windows per session (keyed by session tmux name)
    pub terminal_counts: HashMap<String, usize>,
    /// PR URLs keyed by branch name
    pub pr_urls: HashMap<String, String>,
    /// Current git branch for each project, keyed by project name
    pub project_branches: HashMap<String, String>,
    /// Number of in-flight async ops. UI stays interactive while ops run; the
    /// status bar shows a spinner when this is non-zero.
    pub op_count: usize,
    pub op_receiver: mpsc::Receiver<OpResult>,
    pub op_sender: mpsc::Sender<OpResult>,
    pub tick: usize,
    pub worker: Worker,
    pub context_menu_items: Vec<ContextMenuItem>,
    pub context_menu_selected: usize,
    /// Diff comments per session tmux name.
    pub diff_comments: HashMap<String, Vec<DiffComment>>,
    /// Diff cursor row (index into rendered diff content lines, excluding sticky header).
    pub diff_cursor: usize,
    /// In-flight comment location during AddDiffComment input mode.
    pub pending_comment: Option<PendingComment>,
}

pub struct OpResult {
    pub message: String,
    pub rebuild: bool,
    pub reload_config: bool,
}

fn project_key(name: &str) -> String {
    format!("p:{name}")
}

fn task_key(project: &str, task: &str) -> String {
    format!("t:{project}:{task}")
}

fn adhoc_group_key(project: &str) -> String {
    format!("a:{project}")
}

impl App {
    pub fn new() -> Result<Self> {
        let config = Config::load()?;
        let keybindings = KeyBindings::load();
        let mut sessions = tmux::list_sessions().unwrap_or_default();

        // Recreate any saved sessions that are no longer in tmux (e.g. tmux died)
        let saved = config::load_sessions();
        if !saved.is_empty() {
            let live_names: HashSet<_> = sessions.iter().map(|s| s.name.as_str()).collect();
            for (tmux_name, record) in &saved {
                if !live_names.contains(tmux_name.as_str()) {
                    let result = if tmux::is_adhoc_marker(&record.task_name) {
                        tmux::recreate_adhoc_session(tmux_name, record)
                    } else {
                        let auto_context = config
                            .find_task(&record.project_name, &record.task_name)
                            .map_or(true, |t| t.auto_context);
                        tmux::recreate_session(tmux_name, record, auto_context)
                    };
                    if result.is_err() {
                        // Could not recreate (e.g. worktree gone) — remove stale record
                        config::remove_session_record(tmux_name);
                    }
                }
            }
            // Re-list sessions after recreation
            sessions = tmux::list_sessions().unwrap_or_default();
        }
        let (tx, rx) = mpsc::channel();
        let mut app = App {
            config,
            keybindings,
            sessions,
            items: vec![],
            selected: 0,
            input_mode: InputMode::Normal,
            input_buffer: String::new(),
            use_worktree: true,
            status_message: None,
            should_quit: false,
            should_attach: None,
            should_attach_window: None,
            should_open_editor: None,
            pending_project_path: None,
            pending_task_name: None,
            pending_task_branch: None,
            pending_session_name: None,
            preview_content: None,
            preview_mode: PreviewMode::Output,
            task_diff: None,
            task_context_content: None,
            collapsed: HashSet::new(),
            session_statuses: HashMap::new(),
            diff_stats: HashMap::new(),
            merged_sessions: HashMap::new(),
            task_diff_stats: HashMap::new(),
            preview_scroll: 0,
            terminal_counts: HashMap::new(),
            pr_urls: HashMap::new(),
            project_branches: HashMap::new(),
            op_count: 0,
            op_receiver: rx,
            op_sender: tx,
            tick: 0,
            worker: Worker::spawn(),
            context_menu_items: vec![],
            context_menu_selected: 0,
            diff_comments: HashMap::new(),
            diff_cursor: 0,
            pending_comment: None,
        };
        // Start with all tasks collapsed, and projects with no tasks collapsed
        for project in &app.config.projects {
            if project.tasks.is_empty() {
                app.collapsed.insert(project_key(&project.name));
            }
            for task in &project.tasks {
                app.collapsed.insert(task_key(&project.name, &task.name));
            }
        }
        app.rebuild_items();
        app.check_cwd();
        Ok(app)
    }

    fn check_cwd(&mut self) {
        if let Ok(cwd) = std::env::current_dir() {
            let cwd_str = cwd.to_string_lossy().to_string();
            if cwd.join(".git").is_dir() && !self.config.has_project_at(&cwd_str) {
                self.pending_project_path = Some(cwd_str);
            }
        }
    }

    /// Apply any pending updates from the background worker.
    pub fn apply_worker_updates(&mut self) {
        let latest = self.worker.latest.lock().unwrap().take();
        if let Some(update) = latest {
            self.sessions = update.sessions;
            self.session_statuses = update.statuses;
            self.diff_stats = update.diff_stats;
            if !update.merged_sessions.is_empty() {
                self.merged_sessions = update.merged_sessions;
            }
            self.preview_content = update.preview_content;
            if update.task_diff.is_some() {
                self.task_diff = update.task_diff;
            }
            self.task_context_content = update.task_context_content;
            if !update.task_diff_stats.is_empty() {
                self.task_diff_stats = update.task_diff_stats;
            }
            if !update.pr_urls.is_empty() {
                self.pr_urls.extend(update.pr_urls);
            }
            if !update.project_branches.is_empty() {
                self.project_branches = update.project_branches;
            }
            if !update.terminal_counts.is_empty() {
                // Merge by taking the max of local and worker counts to avoid
                // stale worker data reverting a freshly created terminal.
                for (name, count) in &update.terminal_counts {
                    let local = self.terminal_counts.get(name).copied().unwrap_or(0);
                    self.terminal_counts
                        .insert(name.clone(), (*count).max(local));
                }
                // Remove sessions that the worker no longer knows about
                self.terminal_counts
                    .retain(|k, _| update.terminal_counts.contains_key(k));
                // If viewing a terminal that no longer exists, fall back
                if let PreviewMode::Terminal(idx) = self.preview_mode {
                    let count = self.selected_terminal_count();
                    if idx >= count {
                        self.preview_mode = if count > 0 {
                            PreviewMode::Terminal(count - 1)
                        } else {
                            PreviewMode::Output
                        };
                        self.preview_content = None;
                        self.sync_worker_hints();
                    }
                }
            }
            self.rebuild_items();
        }
    }

    /// Poll for completed background operations.
    pub fn apply_op_results(&mut self) {
        while let Ok(result) = self.op_receiver.try_recv() {
            self.op_count = self.op_count.saturating_sub(1);
            self.status_message = Some(result.message);
            if result.reload_config {
                if let Ok(config) = Config::load() {
                    self.config = config;
                }
            }
            if result.rebuild {
                self.rebuild_items();
            }
        }
    }

    fn start_op<F>(&mut self, loading_msg: &str, f: F)
    where
        F: FnOnce() -> OpResult + Send + 'static,
    {
        self.op_count += 1;
        self.status_message = Some(loading_msg.into());
        let tx = self.op_sender.clone();
        thread::spawn(move || {
            let result = f();
            let _ = tx.send(result);
        });
    }

    /// Tell the worker what is selected.
    pub fn sync_worker_hints(&self) {
        let selection = match self.selected_item() {
            Some(ListItem::Session { session, .. }) => Selection::Session {
                name: session.name.clone(),
                preview_mode: self.preview_mode,
            },
            Some(ListItem::AdhocSession { session, .. }) => Selection::Session {
                name: session.name.clone(),
                preview_mode: PreviewMode::Output,
            },
            Some(ListItem::Task {
                project_name,
                project_path,
                task,
                ..
            }) => Selection::Task {
                project_name: project_name.clone(),
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
                    project_name: p.name.clone(),
                    project_path: p.path.clone(),
                    branch: t.branch.clone(),
                })
            })
            .collect();

        let project_paths: Vec<(String, String)> = self
            .config
            .projects
            .iter()
            .map(|p| (p.name.clone(), p.path.clone()))
            .collect();

        if let Ok(mut hints) = self.worker.hints.lock() {
            hints.selection = selection;
            hints.tasks = tasks;
            hints.project_paths = project_paths;
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

            // Adhoc group: only rendered when the project has at least one adhoc session.
            let adhoc_sessions = tmux::adhoc_sessions_for_project(&project.name, &self.sessions);
            if !adhoc_sessions.is_empty() {
                self.items.push(ListItem::AdhocGroup {
                    project_name: project.name.clone(),
                    project_path: project.path.clone(),
                    session_count: adhoc_sessions.len(),
                });
                if !self.collapsed.contains(&adhoc_group_key(&project.name)) {
                    for session in adhoc_sessions {
                        self.items.push(ListItem::AdhocSession {
                            project_name: project.name.clone(),
                            project_path: project.path.clone(),
                            session,
                        });
                    }
                }
            }

            for task in &project.tasks {
                self.items.push(ListItem::Task {
                    project_name: project.name.clone(),
                    project_path: project.path.clone(),
                    task: task.clone(),
                });

                if self
                    .collapsed
                    .contains(&task_key(&project.name, &task.name))
                {
                    continue;
                }

                for session in tmux::sessions_for_task(&project.name, &task.name, &self.sessions) {
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
            ListItem::AdhocGroup {
                project_name,
                project_path,
                ..
            } => Some((project_name, project_path)),
            ListItem::AdhocSession {
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
        self.preview_scroll = 0;
        self.diff_cursor = 0;
        // Default to Context for tasks, Output for sessions
        if matches!(self.selected_item(), Some(ListItem::Task { .. })) {
            self.preview_mode = PreviewMode::Context;
        } else if matches!(
            self.selected_item(),
            Some(ListItem::Session { .. } | ListItem::AdhocSession { .. })
        ) {
            self.preview_mode = PreviewMode::Output;
        }
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
                project_name, task, ..
            }) => {
                let key = task_key(project_name, &task.name);
                if !self.collapsed.remove(&key) {
                    self.collapsed.insert(key);
                }
                self.rebuild_items();
            }
            Some(ListItem::AdhocGroup { project_name, .. }) => {
                let key = adhoc_group_key(project_name);
                if !self.collapsed.remove(&key) {
                    self.collapsed.insert(key);
                }
                self.rebuild_items();
            }
            _ => {}
        }
    }

    pub fn enter_selected(&mut self) {
        if let Some(ListItem::Task {
            project_name, task, ..
        }) = self.selected_item()
        {
            if self.preview_mode == PreviewMode::Context {
                let ctx_path = crate::config::task_context_path(&project_name, &task.branch);
                self.should_open_editor = Some(ctx_path);
                return;
            }
        }
        if let Some(ListItem::Session { session, .. }) = self.selected_item() {
            if let PreviewMode::Terminal(idx) = self.preview_mode {
                // Attach to specific terminal window
                self.should_attach_window = Some((session.name.clone(), idx + 1));
            } else {
                self.should_attach = Some(session.name.clone());
            }
        }
        if let Some(ListItem::AdhocSession { session, .. }) = self.selected_item() {
            self.should_attach = Some(session.name.clone());
        }
    }

    pub fn open_context_menu(&mut self) {
        let cm = self.keybindings.context_menu_keys.clone();
        let items = match self.selected_item() {
            Some(ListItem::Project { .. }) => vec![
                ContextMenuItem {
                    key: cm.add_task,
                    label: "Add task",
                    action: ContextAction::AddTask,
                },
                ContextMenuItem {
                    key: cm.new_adhoc_session,
                    label: "New adhoc session",
                    action: ContextAction::NewAdhocSession,
                },
                ContextMenuItem {
                    key: cm.rename,
                    label: "Rename",
                    action: ContextAction::Rename,
                },
                ContextMenuItem {
                    key: cm.delete,
                    label: "Delete",
                    action: ContextAction::Delete,
                },
            ],
            Some(ListItem::AdhocGroup { .. }) => vec![
                ContextMenuItem {
                    key: cm.new_adhoc_session,
                    label: "New adhoc session",
                    action: ContextAction::NewAdhocSession,
                },
            ],
            Some(ListItem::AdhocSession { .. }) => vec![
                ContextMenuItem {
                    key: cm.rename,
                    label: "Rename",
                    action: ContextAction::Rename,
                },
                ContextMenuItem {
                    key: cm.delete,
                    label: "Delete",
                    action: ContextAction::Delete,
                },
            ],
            Some(ListItem::Task { task, .. }) => {
                let ctx_label = if task.auto_context {
                    "Disable auto-context"
                } else {
                    "Enable auto-context"
                };
                vec![
                    ContextMenuItem {
                        key: cm.new_session,
                        label: "New session",
                        action: ContextAction::NewSession,
                    },
                    ContextMenuItem {
                        key: cm.new_session_no_worktree,
                        label: "New session (no worktree)",
                        action: ContextAction::NewSessionNoWorktree,
                    },
                    ContextMenuItem {
                        key: cm.toggle_auto_context,
                        label: ctx_label,
                        action: ContextAction::ToggleAutoContext,
                    },
                    ContextMenuItem {
                        key: cm.update,
                        label: "Update branch",
                        action: ContextAction::Update,
                    },
                    ContextMenuItem {
                        key: cm.push,
                        label: "Push",
                        action: ContextAction::Push,
                    },
                    ContextMenuItem {
                        key: cm.checkout,
                        label: "Checkout",
                        action: ContextAction::Checkout,
                    },
                    ContextMenuItem {
                        key: cm.open_pr,
                        label: "Open PR",
                        action: ContextAction::OpenPr,
                    },
                    ContextMenuItem {
                        key: cm.rename,
                        label: "Rename",
                        action: ContextAction::Rename,
                    },
                    ContextMenuItem {
                        key: cm.delete,
                        label: "Delete",
                        action: ContextAction::Delete,
                    },
                ]
            }
            Some(ListItem::Session { .. }) => {
                let mut items = vec![
                    ContextMenuItem {
                        key: cm.merge,
                        label: "Merge",
                        action: ContextAction::Merge,
                    },
                    ContextMenuItem {
                        key: cm.update,
                        label: "Update",
                        action: ContextAction::Update,
                    },
                    ContextMenuItem {
                        key: cm.create_terminal,
                        label: "Create terminal",
                        action: ContextAction::CreateTerminal,
                    },
                ];
                if let PreviewMode::Terminal(_) = self.preview_mode {
                    items.push(ContextMenuItem {
                        key: cm.kill_terminal,
                        label: "Kill terminal",
                        action: ContextAction::KillTerminal,
                    });
                }
                items.push(ContextMenuItem {
                    key: cm.rename,
                    label: "Rename",
                    action: ContextAction::Rename,
                });
                items.push(ContextMenuItem {
                    key: cm.delete,
                    label: "Delete",
                    action: ContextAction::Delete,
                });
                items
            }
            None => return,
        };
        self.context_menu_items = items;
        self.context_menu_selected = 0;
        self.input_mode = InputMode::ContextMenu;
    }

    pub fn execute_context_action(&mut self, action: ContextAction) {
        self.input_mode = InputMode::Normal;
        match action {
            ContextAction::AddTask => self.start_add_task(),
            ContextAction::NewSession => self.start_new_session(true),
            ContextAction::NewSessionNoWorktree => self.start_new_session(false),
            ContextAction::NewAdhocSession => self.start_new_adhoc_session(),
            ContextAction::Delete => self.start_delete(),
            ContextAction::Rename => self.start_rename(),
            ContextAction::Merge => self.start_merge(),
            ContextAction::Update => self.update_session(),
            ContextAction::Push => self.push_task_branch(),
            ContextAction::OpenPr => self.open_pr(),
            ContextAction::Checkout => self.checkout_task_branch(),
            ContextAction::CreateTerminal => self.create_terminal(),
            ContextAction::KillTerminal => self.kill_terminal(),
            ContextAction::ToggleAutoContext => self.toggle_auto_context(),
        }
    }

    pub fn toggle_auto_context(&mut self) {
        let (project_name, task_name, task_branch) = match self.selected_task_info() {
            Some((pn, _, t)) => (pn.to_string(), t.name.clone(), t.branch.clone()),
            None => return,
        };

        if let Some(new_state) = self.config.toggle_auto_context(&project_name, &task_name) {
            self.config.reload();
            let _ = self.config.save();

            // Update hooks for all existing sessions of this task
            let task_sessions = tmux::sessions_for_task(&project_name, &task_name, &self.sessions);
            for session in &task_sessions {
                if let Some(work_dir) = tmux::get_session_work_dir(&session.name) {
                    if new_state {
                        let context_path = config::task_context_path(&project_name, &task_branch);
                        tmux::setup_task_context(
                            &work_dir,
                            &task_name,
                            &task_branch,
                            &context_path,
                        );
                    } else {
                        tmux::remove_task_context_hooks(&work_dir);
                    }
                }
            }

            let label = if new_state { "enabled" } else { "disabled" };
            self.status_message = Some(format!("Auto-context {label} for '{task_name}'"));
            self.rebuild_items();
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
            self.status_message = Some("Error: current directory is not a git repository".into());
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
            self.config.reload();
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
            self.use_worktree = true;
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
        if tmux::is_adhoc_marker(&task_name) {
            self.status_message =
                Some("'adhoc' is reserved — pick a different task name".into());
            return;
        }

        self.pending_task_name = Some(task_name.clone());
        self.input_buffer = tmux::to_branch_name(&task_name);
        self.input_mode = InputMode::AddTaskBranch;
        self.status_message = Some("Branch name (existing or new): ".into());
    }

    pub fn confirm_add_task_branch(&mut self) {
        let branch = self.input_buffer.trim().to_string();
        if branch.is_empty() {
            self.cancel_input();
            return;
        }
        if branch == "main" || branch == "master" {
            self.status_message = Some("Cannot use 'main' or 'master' as a task branch".into());
            return;
        }

        if self.pending_task_name.is_none() {
            self.cancel_input();
            return;
        }

        self.pending_task_branch = Some(branch);
        self.input_buffer.clear();
        self.input_mode = InputMode::AddTaskPrompt;
        self.status_message = Some("Initial session prompt (empty to skip): ".into());
    }

    pub fn confirm_add_task_with_prompt(&mut self) {
        let task_name = match self.pending_task_name.take() {
            Some(n) => n,
            None => {
                self.cancel_input();
                return;
            }
        };
        let branch = match self.pending_task_branch.take() {
            Some(b) => b,
            None => {
                self.cancel_input();
                return;
            }
        };

        let (project_name, project_path) = match self.selected_project_info() {
            Some((name, path)) => (name.to_string(), path.to_string()),
            None => {
                self.cancel_input();
                return;
            }
        };

        let prompt = if self.input_buffer.trim().is_empty() {
            None
        } else {
            Some(self.input_buffer.trim().to_string())
        };

        self.collapsed.remove(&project_key(&project_name));
        self.input_buffer.clear();
        self.input_mode = InputMode::Normal;

        let use_worktree = self.use_worktree;
        let sessions = self.sessions.clone();
        let startup_skills = self.config.startup_skills.clone();
        let project = self.config.projects.iter().find(|p| p.name == project_name);
        let copy_patterns = project.map(|p| p.copy_patterns.clone()).unwrap_or_default();
        let setup_commands = project
            .map(|p| p.setup_commands.clone())
            .unwrap_or_default();

        self.start_op("Creating task...", move || {
            let branch_exists = tmux::branch_exists(&project_path, &branch);

            if !branch_exists {
                if let Err(e) = tmux::create_task_branch(&project_path, &branch) {
                    return OpResult {
                        message: format!("Error: {e}"),
                        rebuild: false,
                        reload_config: false,
                    };
                }
            }

            let task_name_for_modify = task_name.clone();
            let branch_for_modify = branch.clone();
            let project_name_for_modify = project_name.clone();
            let config = match Config::modify(move |c| {
                c.add_task(
                    &project_name_for_modify,
                    task_name_for_modify,
                    branch_for_modify,
                );
            }) {
                Ok(c) => c,
                Err(e) => {
                    return OpResult {
                        message: format!("Error saving config: {e}"),
                        rebuild: false,
                        reload_config: false,
                    };
                }
            };

            let auto_context = config
                .find_task(&project_name, &task_name)
                .map_or(true, |t| t.auto_context);

            let session_name =
                tmux::next_session_number(&project_name, &task_name, &sessions).to_string();

            match tmux::create_session(
                &project_name,
                &project_path,
                &task_name,
                &branch,
                &session_name,
                use_worktree,
                &copy_patterns,
                &setup_commands,
                prompt.as_deref(),
                auto_context,
                &startup_skills,
            ) {
                Ok(tmux_name) => {
                    config::add_session_record(
                        &tmux_name,
                        config::SessionRecord {
                            project_name: project_name.clone(),
                            project_path: project_path.clone(),
                            task_name: task_name.clone(),
                            task_branch: branch.clone(),
                            session_name: session_name.clone(),
                            use_worktree,
                        },
                    );
                    let task_msg = if branch_exists {
                        format!("task '{task_name}' on existing branch {branch}")
                    } else {
                        format!("task '{task_name}' on branch {branch}")
                    };
                    OpResult {
                        message: format!("Created {task_msg} and session {tmux_name}"),
                        rebuild: true,
                        reload_config: true,
                    }
                }
                Err(e) => OpResult {
                    message: format!("Task created but session failed: {e}"),
                    rebuild: true,
                    reload_config: true,
                },
            }
        });
    }

    pub fn start_new_adhoc_session(&mut self) {
        if self.selected_project_info().is_none() {
            self.status_message = Some("Select a project first".into());
            return;
        }
        self.input_mode = InputMode::AddAdhocSessionName;
        self.input_buffer.clear();
        self.status_message = Some("Adhoc session name: ".into());
    }

    pub fn confirm_new_adhoc_session(&mut self) {
        let name = self.input_buffer.trim().to_string();
        if name.is_empty() {
            self.cancel_input();
            return;
        }

        let (project_name, project_path) = match self.selected_project_info() {
            Some((n, p)) => (n.to_string(), p.to_string()),
            None => {
                self.cancel_input();
                return;
            }
        };

        let tmux_name_to_create = format!(
            "cm__{}__{}__{}",
            tmux::sanitize(&project_name),
            tmux::ADHOC_MARKER,
            tmux::sanitize(&name),
        );
        if self.sessions.iter().any(|s| s.name == tmux_name_to_create) {
            self.status_message =
                Some(format!("Adhoc session '{name}' already exists"));
            return;
        }

        self.collapsed.remove(&project_key(&project_name));
        self.collapsed.remove(&adhoc_group_key(&project_name));
        self.input_buffer.clear();
        self.input_mode = InputMode::Normal;

        let startup_skills = self.config.startup_skills.clone();
        let proj_name_for_op = project_name.clone();
        let proj_path_for_op = project_path.clone();
        let session_name = name.clone();

        self.start_op("Creating adhoc session...", move || {
            match tmux::create_adhoc_session(
                &proj_name_for_op,
                &proj_path_for_op,
                &session_name,
                &startup_skills,
            ) {
                Ok(tmux_name) => {
                    config::add_session_record(
                        &tmux_name,
                        config::SessionRecord {
                            project_name: proj_name_for_op.clone(),
                            project_path: proj_path_for_op.clone(),
                            task_name: tmux::ADHOC_MARKER.to_string(),
                            task_branch: String::new(),
                            session_name: session_name.clone(),
                            use_worktree: false,
                        },
                    );
                    OpResult {
                        message: format!("Created adhoc session {tmux_name}"),
                        rebuild: true,
                        reload_config: false,
                    }
                }
                Err(e) => OpResult {
                    message: format!("Error: {e}"),
                    rebuild: false,
                    reload_config: false,
                },
            }
        });
    }

    pub fn start_new_session(&mut self, use_worktree: bool) {
        let info = self
            .selected_task_info()
            .map(|(pn, _, t)| (pn.to_string(), t.name.clone()));

        if let Some((project_name, task_name)) = info {
            self.use_worktree = use_worktree;
            self.input_mode = InputMode::AddSessionName;
            self.input_buffer.clear();
            let next = tmux::next_session_number(&project_name, &task_name, &self.sessions);
            self.status_message = Some(format!(
                "Session name (default: {next}){}:",
                if use_worktree { " [worktree]" } else { "" }
            ));
        } else {
            self.status_message = Some("Select a task first to create a session".into());
        }
    }

    pub fn confirm_new_session(&mut self) {
        let (project_name, _, task) = match self.selected_task_info() {
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

        self.pending_session_name = Some(session_name);
        self.input_buffer.clear();
        self.input_mode = InputMode::AddSessionPrompt;
        self.status_message = Some("Initial prompt (empty to skip): ".into());
    }

    pub fn confirm_new_session_with_prompt(&mut self) {
        let (project_name, project_path, task) = match self.selected_task_info() {
            Some((pn, pp, t)) => (pn.to_string(), pp.to_string(), t.clone()),
            None => {
                self.cancel_input();
                return;
            }
        };

        let session_name = match self.pending_session_name.take() {
            Some(name) => name,
            None => {
                self.cancel_input();
                return;
            }
        };

        let prompt = if self.input_buffer.trim().is_empty() {
            None
        } else {
            Some(self.input_buffer.trim().to_string())
        };

        let use_worktree = self.use_worktree;
        let task_name = task.name.clone();
        let task_branch = task.branch.clone();
        let auto_context = task.auto_context;
        let project = self.config.projects.iter().find(|p| p.name == project_name);
        let copy_patterns = project.map(|p| p.copy_patterns.clone()).unwrap_or_default();
        let setup_commands = project
            .map(|p| p.setup_commands.clone())
            .unwrap_or_default();
        let startup_skills = self.config.startup_skills.clone();
        self.input_buffer.clear();
        self.input_mode = InputMode::Normal;

        self.start_op("Creating session...", move || {
            match tmux::create_session(
                &project_name,
                &project_path,
                &task_name,
                &task_branch,
                &session_name,
                use_worktree,
                &copy_patterns,
                &setup_commands,
                prompt.as_deref(),
                auto_context,
                &startup_skills,
            ) {
                Ok(tmux_name) => {
                    config::add_session_record(
                        &tmux_name,
                        config::SessionRecord {
                            project_name: project_name.clone(),
                            project_path: project_path.clone(),
                            task_name: task_name.clone(),
                            task_branch: task_branch.clone(),
                            session_name: session_name.clone(),
                            use_worktree,
                        },
                    );
                    OpResult {
                        message: format!("Created session {tmux_name}"),
                        rebuild: true,
                        reload_config: false,
                    }
                }
                Err(e) => OpResult {
                    message: format!("Error: {e}"),
                    rebuild: false,
                    reload_config: false,
                },
            }
        });
    }

    pub fn start_delete(&mut self) {
        match self.selected_item() {
            Some(ListItem::Project { project }) => {
                let session_count = self
                    .sessions
                    .iter()
                    .filter(|s| s.project_name == tmux::sanitize(&project.name))
                    .count();
                let task_count = project.tasks.len();
                self.input_mode = InputMode::ConfirmDelete;
                if session_count > 0 || task_count > 0 {
                    self.status_message = Some(format!(
                        "Delete project and all {} task(s), {} session(s)? (y/n)",
                        task_count, session_count
                    ));
                } else {
                    self.status_message = Some("Delete this project? (y/n)".into());
                }
            }
            Some(ListItem::Session { .. }) => {
                self.input_mode = InputMode::ConfirmDelete;
                self.status_message = Some("Delete this session? (y/n)".into());
            }
            Some(ListItem::AdhocSession { .. }) => {
                self.input_mode = InputMode::ConfirmDelete;
                self.status_message = Some("Delete this adhoc session? (y/n)".into());
            }
            Some(ListItem::Task {
                project_name, task, ..
            }) => {
                let active = tmux::sessions_for_task(project_name, &task.name, &self.sessions);
                self.input_mode = InputMode::ConfirmDelete;
                if active.is_empty() {
                    self.status_message = Some("Delete this task? (y/n)".into());
                } else {
                    self.status_message = Some(format!(
                        "Delete task and kill {} active session(s)? (y/n)",
                        active.len()
                    ));
                }
            }
            _ => {}
        }
    }

    pub fn confirm_delete(&mut self) {
        match self.selected_item().cloned() {
            Some(ListItem::Project { project }) => {
                let project_name = project.name.clone();
                let project_path = project.path.clone();
                let tasks: Vec<_> = project.tasks.clone();
                let sessions = self.sessions.clone();
                self.input_mode = InputMode::Normal;
                self.start_op("Deleting project...", move || {
                    let mut total_sessions = 0;
                    for task in &tasks {
                        let msg = tmux::delete_task(
                            &project_name,
                            &project_path,
                            &task.name,
                            &task.branch,
                            &sessions,
                        );
                        // Count sessions from message
                        if msg.contains("session(s)") {
                            total_sessions +=
                                tmux::sessions_for_task(&project_name, &task.name, &sessions).len();
                        }
                    }
                    let _ = total_sessions;
                    // Clean up leftover worktree and task config directories
                    tmux::cleanup_project_dirs(&project_name);
                    config::remove_project_session_records(&project_name);
                    OpResult {
                        message: format!("Deleted project '{}'", project_name),
                        rebuild: true,
                        reload_config: true,
                    }
                });
                // Remove project from config (done here so it's saved even if op thread is slow)
                self.config.reload();
                self.config.remove_project(&project.path);
                let _ = self.config.save();
                return;
            }
            Some(ListItem::Session { session, .. }) => {
                let name = session.name.clone();
                let display_name = session.session_name.clone();
                self.input_mode = InputMode::Normal;
                self.start_op("Deleting session...", move || {
                    // Load session record for fallback cleanup info
                    let fallback = config::load_sessions()
                        .remove(&name)
                        .filter(|r| r.use_worktree)
                        .map(|r| {
                            let wt =
                                tmux::worktree_dir(&r.project_name, &r.task_name, &r.session_name);
                            let branch = format!(
                                "{}-{}",
                                tmux::sanitize(&r.task_branch),
                                tmux::sanitize(&r.session_name),
                            );
                            tmux::SessionCleanupInfo {
                                project_path: r.project_path,
                                worktree_path: wt.to_string_lossy().to_string(),
                                branch_name: Some(branch),
                            }
                        });
                    match tmux::kill_session_with_fallback(&name, fallback) {
                        Ok(()) => {
                            config::remove_session_record(&name);
                            OpResult {
                                message: format!("Killed session {display_name}"),
                                rebuild: true,
                                reload_config: false,
                            }
                        }
                        Err(e) => OpResult {
                            message: format!("Error: {e}"),
                            rebuild: false,
                            reload_config: false,
                        },
                    }
                });
                return;
            }
            Some(ListItem::AdhocSession { session, .. }) => {
                let name = session.name.clone();
                let display_name = session.session_name.clone();
                self.input_mode = InputMode::Normal;
                self.start_op("Deleting adhoc session...", move || {
                    match tmux::kill_session_with_fallback(&name, None) {
                        Ok(()) => {
                            config::remove_session_record(&name);
                            OpResult {
                                message: format!("Killed adhoc session {display_name}"),
                                rebuild: true,
                                reload_config: false,
                            }
                        }
                        Err(e) => OpResult {
                            message: format!("Error: {e}"),
                            rebuild: false,
                            reload_config: false,
                        },
                    }
                });
                return;
            }
            Some(ListItem::Task {
                project_name,
                project_path,
                task,
            }) => {
                let task_name = task.name.clone();
                let task_branch = task.branch.clone();
                let pname = project_name.clone();
                let ppath = project_path.clone();
                let sessions = self.sessions.clone();
                self.input_mode = InputMode::Normal;
                self.start_op("Deleting task...", move || {
                    let msg =
                        tmux::delete_task(&pname, &ppath, &task_name, &task_branch, &sessions);
                    config::remove_task_session_records(&pname, &task_name);
                    OpResult {
                        message: msg,
                        rebuild: true,
                        reload_config: true,
                    }
                });
                // Remove task from config immediately
                self.config.reload();
                self.config.remove_task(&project_name, &task.name);
                let _ = self.config.save();
                return;
            }
            _ => {}
        }
        self.input_mode = InputMode::Normal;
    }

    pub fn start_rename(&mut self) {
        let (mode, name) = match self.selected_item() {
            Some(ListItem::Project { project }) => (InputMode::RenameProject, project.name.clone()),
            Some(ListItem::Task { task, .. }) => (InputMode::RenameTask, task.name.clone()),
            Some(ListItem::Session { session, .. }) => {
                (InputMode::RenameSession, session.session_name.clone())
            }
            Some(ListItem::AdhocSession { session, .. }) => (
                InputMode::RenameAdhocSession,
                session.session_name.clone(),
            ),
            _ => return,
        };
        let label = match mode {
            InputMode::RenameProject => "Rename project: ",
            InputMode::RenameTask => "Rename task: ",
            InputMode::RenameSession => "Rename session: ",
            InputMode::RenameAdhocSession => "Rename adhoc session: ",
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
                            config::rename_session_record(&session.name, &new_tmux);
                        }
                    }

                    self.config.reload();
                    self.config.rename_project(&old_name, new_name.clone());
                    let _ = self.config.save();
                    self.status_message = Some(format!("Renamed project to {new_name}"));
                }
            }
            InputMode::RenameTask => {
                if let Some(ListItem::Task {
                    project_name, task, ..
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
                            config::rename_session_record(&session.name, &new_tmux);
                        }
                    }

                    self.config.reload();
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
                            config::rename_session_record(&session.name, &new_tmux);
                            self.status_message = Some(format!("Renamed session to {new_name}"));
                        }
                        Err(e) => {
                            self.status_message = Some(format!("Error: {e}"));
                        }
                    }
                }
            }
            InputMode::RenameAdhocSession => {
                if let Some(ListItem::AdhocSession {
                    project_name,
                    session,
                    ..
                }) = self.selected_item().cloned()
                {
                    if session.session_name == new_name {
                        self.cancel_input();
                        return;
                    }

                    let new_tmux = format!(
                        "cm__{}__{}__{}",
                        tmux::sanitize(&project_name),
                        tmux::ADHOC_MARKER,
                        tmux::sanitize(&new_name),
                    );
                    match tmux::rename_session(&session.name, &new_tmux) {
                        Ok(()) => {
                            config::rename_session_record(&session.name, &new_tmux);
                            self.status_message =
                                Some(format!("Renamed adhoc session to {new_name}"));
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
                self.status_message = Some("Cannot merge: session has no worktree".into());
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
            self.do_merge(project_path, task.branch, session.session_name, wt_path);
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

        let task_branch = task.branch.clone();
        let session_display = session.session_name.clone();
        self.input_buffer.clear();
        self.input_mode = InputMode::Normal;

        self.start_op("Merging...", move || {
            if let Err(e) = tmux::commit_all(&wt_path, &msg) {
                return OpResult {
                    message: format!("Error committing: {e}"),
                    rebuild: false,
                    reload_config: false,
                };
            }
            match tmux::merge_session_to_task(
                &project_path,
                &task_branch,
                &session_display,
                &wt_path,
            ) {
                Ok(msg) => OpResult {
                    message: msg,
                    rebuild: false,
                    reload_config: false,
                },
                Err(e) => OpResult {
                    message: format!("Error: {e}"),
                    rebuild: false,
                    reload_config: false,
                },
            }
        });
    }

    fn do_merge(
        &mut self,
        project_path: String,
        task_branch: String,
        session_name: String,
        wt_path: String,
    ) {
        self.start_op("Merging...", move || {
            match tmux::merge_session_to_task(&project_path, &task_branch, &session_name, &wt_path)
            {
                Ok(msg) => OpResult {
                    message: msg,
                    rebuild: false,
                    reload_config: false,
                },
                Err(e) => OpResult {
                    message: format!("Error: {e}"),
                    rebuild: false,
                    reload_config: false,
                },
            }
        });
    }

    pub fn update_session(&mut self) {
        match self.selected_item().cloned() {
            Some(ListItem::Task {
                project_path, task, ..
            }) => {
                let branch = task.branch.clone();
                self.start_op(
                    "Updating task branch...",
                    move || match tmux::update_task_branch(&project_path, &branch) {
                        Ok(msg) => OpResult {
                            message: msg,
                            rebuild: false,
                            reload_config: false,
                        },
                        Err(e) => OpResult {
                            message: format!("Error: {e}"),
                            rebuild: false,
                            reload_config: false,
                        },
                    },
                );
            }
            Some(ListItem::Session {
                project_path,
                task,
                session,
                ..
            }) => {
                let wt_path = match session.worktree_path() {
                    Some(p) => p.to_string_lossy().to_string(),
                    None => {
                        self.status_message = Some("Cannot update: session has no worktree".into());
                        return;
                    }
                };
                let task_branch = task.branch.clone();
                self.start_op(
                    "Updating session...",
                    move || match tmux::rebase_session_on_task(
                        &project_path,
                        &task_branch,
                        &wt_path,
                    ) {
                        Ok(msg) => OpResult {
                            message: msg,
                            rebuild: false,
                            reload_config: false,
                        },
                        Err(e) => OpResult {
                            message: format!("Error: {e}"),
                            rebuild: false,
                            reload_config: false,
                        },
                    },
                );
            }
            _ => {
                self.status_message = Some("Select a session or task to update".into());
            }
        }
    }

    pub fn push_task_branch(&mut self) {
        let (project_path, task) = match self.selected_item().cloned() {
            Some(ListItem::Task {
                project_path, task, ..
            }) => (project_path, task),
            _ => {
                self.status_message = Some("Select a task to push".into());
                return;
            }
        };

        let branch = task.branch.clone();
        self.start_op("Pushing...", move || {
            match tmux::push_branch(&project_path, &branch) {
                Ok(msg) => OpResult {
                    message: msg,
                    rebuild: false,
                    reload_config: false,
                },
                Err(e) => OpResult {
                    message: format!("Error: {e}"),
                    rebuild: false,
                    reload_config: false,
                },
            }
        });
    }

    pub fn checkout_task_branch(&mut self) {
        let (project_path, task) = match self.selected_item().cloned() {
            Some(ListItem::Task {
                project_path, task, ..
            }) => (project_path, task),
            _ => {
                self.status_message = Some("Select a task to checkout".into());
                return;
            }
        };

        let branch = task.branch.clone();
        self.start_op("Checking out...", move || {
            let output = std::process::Command::new("git")
                .args(["-C", &project_path, "checkout", &branch])
                .output();

            match output {
                Ok(o) if o.status.success() => OpResult {
                    message: format!("Checked out {branch}"),
                    rebuild: false,
                    reload_config: false,
                },
                Ok(o) => {
                    let stderr = String::from_utf8_lossy(&o.stderr).trim().to_string();
                    OpResult {
                        message: format!("Error: {stderr}"),
                        rebuild: false,
                        reload_config: false,
                    }
                }
                Err(e) => OpResult {
                    message: format!("Error: {e}"),
                    rebuild: false,
                    reload_config: false,
                },
            }
        });
    }

    pub fn open_pr(&mut self) {
        if let Some(ListItem::Task { task, .. }) = self.selected_item() {
            if let Some(url) = self.pr_urls.get(&task.branch) {
                let _ = std::process::Command::new("open").arg(url).output();
            } else {
                self.input_mode = InputMode::ConfirmCreatePr;
                self.status_message = Some("No PR found. Create one? (y/n)".into());
            }
        }
    }

    pub fn confirm_create_pr(&mut self) {
        let (project_path, task) = match self.selected_item().cloned() {
            Some(ListItem::Task {
                project_path, task, ..
            }) => (project_path, task),
            _ => {
                self.cancel_input();
                return;
            }
        };

        let branch = task.branch.clone();
        let task_name = task.name.clone();
        self.input_mode = InputMode::Normal;

        self.start_op("Creating PR...", move || {
            // Push branch first
            if let Err(e) = tmux::push_branch(&project_path, &branch) {
                return OpResult {
                    message: format!("Error pushing: {e}"),
                    rebuild: false,
                    reload_config: false,
                };
            }

            let output = std::process::Command::new("gh")
                .args([
                    "pr", "create", "--draft", "--title", &task_name, "--body", "", "--head",
                    &branch,
                ])
                .current_dir(&project_path)
                .output();

            match output {
                Ok(o) if o.status.success() => {
                    let url = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    let _ = std::process::Command::new("open").arg(&url).output();
                    OpResult {
                        message: format!("Created PR: {url}"),
                        rebuild: false,
                        reload_config: false,
                    }
                }
                Ok(o) => {
                    let stderr = String::from_utf8_lossy(&o.stderr).trim().to_string();
                    OpResult {
                        message: format!("Error creating PR: {stderr}"),
                        rebuild: false,
                        reload_config: false,
                    }
                }
                Err(e) => OpResult {
                    message: format!("Error: {e}"),
                    rebuild: false,
                    reload_config: false,
                },
            }
        });
    }

    pub fn cancel_input(&mut self) {
        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();
        self.status_message = None;
        self.pending_task_name = None;
        self.pending_task_branch = None;
        self.pending_session_name = None;
        self.pending_comment = None;
    }

    pub fn toggle_preview_mode(&mut self) {
        // Adhoc sessions only have the agent tab — nothing to cycle through.
        if matches!(self.selected_item(), Some(ListItem::AdhocSession { .. })) {
            return;
        }
        let is_task = matches!(self.selected_item(), Some(ListItem::Task { .. }));
        if is_task {
            self.preview_mode = match self.preview_mode {
                PreviewMode::Context => PreviewMode::Diff,
                _ => PreviewMode::Context,
            };
        } else {
            let term_count = self.selected_terminal_count();
            self.preview_mode = match self.preview_mode {
                PreviewMode::Output => PreviewMode::Diff,
                PreviewMode::Diff => {
                    if term_count > 0 {
                        PreviewMode::Terminal(0)
                    } else {
                        PreviewMode::Output
                    }
                }
                PreviewMode::Context => PreviewMode::Output,
                PreviewMode::Terminal(idx) => {
                    if idx + 1 < term_count {
                        PreviewMode::Terminal(idx + 1)
                    } else {
                        PreviewMode::Output
                    }
                }
            };
        }
        self.preview_content = None;
        self.preview_scroll = 0;
        self.diff_cursor = 0;
        self.sync_worker_hints();
    }

    fn selected_terminal_count(&self) -> usize {
        if let Some(ListItem::Session { session, .. }) = self.selected_item() {
            self.terminal_counts
                .get(&session.name)
                .copied()
                .unwrap_or(0)
        } else {
            0
        }
    }

    pub fn create_terminal(&mut self) {
        if let Some(ListItem::Session { session, .. }) = self.selected_item() {
            let count = self.selected_terminal_count();
            if count >= 4 {
                self.status_message = Some("Maximum 4 terminals per session".into());
                return;
            }
            let session_name = session.name.clone();
            match tmux::create_terminal_window(&session_name) {
                Ok(_) => {
                    let new_count = tmux::count_terminal_windows(&session_name);
                    self.terminal_counts.insert(session_name, new_count);
                    // Switch to the new terminal tab
                    self.preview_mode = PreviewMode::Terminal(new_count.saturating_sub(1));
                    self.preview_content = None;
                    self.preview_scroll = 0;
                    self.sync_worker_hints();
                    self.status_message = Some("Created terminal".into());
                }
                Err(e) => {
                    self.status_message = Some(format!("Error: {e}"));
                }
            }
        }
    }

    pub fn kill_terminal(&mut self) {
        if let PreviewMode::Terminal(idx) = self.preview_mode {
            if let Some(ListItem::Session { session, .. }) = self.selected_item() {
                let session_name = session.name.clone();
                match tmux::kill_terminal_window(&session_name, idx) {
                    Ok(()) => {
                        let new_count = tmux::count_terminal_windows(&session_name);
                        self.terminal_counts.insert(session_name, new_count);
                        // Adjust preview mode
                        if new_count == 0 {
                            self.preview_mode = PreviewMode::Output;
                        } else if idx >= new_count {
                            self.preview_mode = PreviewMode::Terminal(new_count - 1);
                        }
                        self.preview_content = None;
                        self.preview_scroll = 0;
                        self.sync_worker_hints();
                        self.status_message = Some("Killed terminal".into());
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Error: {e}"));
                    }
                }
            }
        }
    }

    pub fn scroll_preview_down(&mut self) {
        self.preview_scroll = self.preview_scroll.saturating_add(3);
    }

    pub fn scroll_preview_up(&mut self) {
        self.preview_scroll = self.preview_scroll.saturating_sub(3);
    }

    /// True when the diff preview tab is active for the selected session.
    pub fn diff_focused(&self) -> bool {
        self.preview_mode == PreviewMode::Diff
            && matches!(self.selected_item(), Some(ListItem::Session { .. }))
    }

    fn selected_session_name(&self) -> Option<String> {
        if let Some(ListItem::Session { session, .. }) = self.selected_item() {
            Some(session.name.clone())
        } else {
            None
        }
    }

    fn diff_rows_for_selected(&self) -> Vec<crate::ui::DiffRow<'_>> {
        let session = match self.selected_session_name() {
            Some(s) => s,
            None => return Vec::new(),
        };
        let content = match self.preview_content.as_deref() {
            Some(c) => c,
            None => return Vec::new(),
        };
        let comments = self
            .diff_comments
            .get(&session)
            .cloned()
            .unwrap_or_default();
        crate::ui::parse_diff_rows(content, &comments)
    }

    fn code_row_indices(&self) -> Vec<usize> {
        self.diff_rows_for_selected()
            .iter()
            .enumerate()
            .filter_map(|(i, r)| {
                if matches!(r, crate::ui::DiffRow::Code { .. }) {
                    Some(i)
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn move_diff_cursor_down(&mut self) {
        let codes = self.code_row_indices();
        if codes.is_empty() {
            return;
        }
        // Find next code row strictly after current cursor.
        let next = codes.iter().find(|&&i| i > self.diff_cursor).copied();
        if let Some(n) = next {
            self.diff_cursor = n;
        } else {
            self.diff_cursor = *codes.last().unwrap();
        }
        self.ensure_cursor_visible();
    }

    pub fn move_diff_cursor_up(&mut self) {
        let codes = self.code_row_indices();
        if codes.is_empty() {
            return;
        }
        let prev = codes.iter().rev().find(|&&i| i < self.diff_cursor).copied();
        if let Some(p) = prev {
            self.diff_cursor = p;
        } else {
            self.diff_cursor = *codes.first().unwrap();
        }
        self.ensure_cursor_visible();
    }

    fn ensure_cursor_visible(&mut self) {
        // Approximate: keep cursor within ~3 lines of scroll viewport top.
        if self.diff_cursor < self.preview_scroll {
            self.preview_scroll = self.diff_cursor.saturating_sub(2);
        } else if self.diff_cursor > self.preview_scroll + 20 {
            self.preview_scroll = self.diff_cursor.saturating_sub(20);
        }
    }

    fn current_code_loc(&self) -> Option<crate::ui::DiffLineLoc> {
        let rows = self.diff_rows_for_selected();
        match rows.get(self.diff_cursor) {
            Some(crate::ui::DiffRow::Code { loc, .. }) => Some(loc.clone()),
            _ => {
                // Fall back to first code row if cursor isn't on one
                rows.iter().find_map(|r| match r {
                    crate::ui::DiffRow::Code { loc, .. } => Some(loc.clone()),
                    _ => None,
                })
            }
        }
    }

    pub fn start_add_diff_comment(&mut self) {
        let session_name = match self.selected_session_name() {
            Some(s) => s,
            None => return,
        };
        let loc = match self.current_code_loc() {
            Some(l) => l,
            None => {
                self.status_message = Some("No diff line under cursor".into());
                return;
            }
        };
        self.pending_comment = Some(PendingComment {
            session_name,
            file: loc.file.clone(),
            line: loc.line,
            side: loc.side,
        });
        self.input_buffer.clear();
        self.input_mode = InputMode::AddDiffComment;
        let side_label = match loc.side {
            DiffSide::Added => "+",
            DiffSide::Removed => "-",
            DiffSide::Context => " ",
        };
        self.status_message = Some(format!(
            "Comment on {}:{} ({}): ",
            loc.file, loc.line, side_label
        ));
    }

    pub fn confirm_add_diff_comment(&mut self) {
        let pending = match self.pending_comment.take() {
            Some(p) => p,
            None => {
                self.cancel_input();
                return;
            }
        };
        let text = self.input_buffer.trim().to_string();
        if text.is_empty() {
            self.cancel_input();
            return;
        }
        let entry = self.diff_comments.entry(pending.session_name).or_default();
        entry.push(DiffComment {
            file: pending.file,
            line: pending.line,
            side: pending.side,
            text,
        });
        self.input_buffer.clear();
        self.input_mode = InputMode::Normal;
        self.status_message = Some("Comment added".into());
    }

    pub fn delete_diff_comment(&mut self) {
        let session = match self.selected_session_name() {
            Some(s) => s,
            None => return,
        };
        let loc = match self.current_code_loc() {
            Some(l) => l,
            None => return,
        };
        if let Some(list) = self.diff_comments.get_mut(&session) {
            let before = list.len();
            list.retain(|c| !(c.file == loc.file && c.line == loc.line && c.side == loc.side));
            if list.len() < before {
                self.status_message = Some("Comment removed".into());
            }
            if list.is_empty() {
                self.diff_comments.remove(&session);
            }
        }
    }

    pub fn submit_diff_comments(&mut self, submit: bool) {
        let session = match self.selected_session_name() {
            Some(s) => s,
            None => return,
        };
        let comments = match self.diff_comments.get(&session) {
            Some(c) if !c.is_empty() => c.clone(),
            _ => {
                self.status_message = Some("No comments to submit".into());
                return;
            }
        };
        let prompt = build_comment_prompt(&comments);
        match tmux::send_text(&session, &prompt, submit) {
            Ok(()) => {
                self.diff_comments.remove(&session);
                self.status_message = Some(if submit {
                    format!("Sent {} comment(s) to agent", comments.len())
                } else {
                    format!(
                        "Pasted {} comment(s) — review and submit in agent",
                        comments.len()
                    )
                });
            }
            Err(e) => {
                self.status_message = Some(format!("Error sending comments: {e}"));
            }
        }
    }
}

fn build_comment_prompt(comments: &[DiffComment]) -> String {
    let mut s = String::from("Code review feedback on the current diff:\n\n");
    let mut by_file: std::collections::BTreeMap<&str, Vec<&DiffComment>> =
        std::collections::BTreeMap::new();
    for c in comments {
        by_file.entry(c.file.as_str()).or_default().push(c);
    }
    for (file, items) in by_file {
        s.push_str(&format!("## {file}\n"));
        let mut sorted = items;
        sorted.sort_by_key(|c| c.line);
        for c in sorted {
            let side = match c.side {
                DiffSide::Added => "added",
                DiffSide::Removed => "removed",
                DiffSide::Context => "context",
            };
            s.push_str(&format!("- L{} ({}): {}\n", c.line, side, c.text));
        }
        s.push('\n');
    }
    s.push_str("Please address each point.");
    s
}
