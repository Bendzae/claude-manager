use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

use anyhow::Result;

use crate::config::{self, Config, KeyBindings, Project, Task};
use crate::tmux::{self, DiffStats, SessionStatus, TmuxSession};
use crate::worker::{TaskInfo, Worker};

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
    SetBaseBranch,
    Search,
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
    ToggleAutoContext,
    CopyWorktreePath,
    SetBaseBranch,
    Archive,
    Unarchive,
    ToggleStacked,
    Review,
    Terminal,
}

/// Extract the review-comment block difit prints to stdout on exit. Returns
/// `None` when the session left no comments (difit prints nothing).
pub fn extract_difit_comments(stdout: &str) -> Option<String> {
    const MARKER: &str = "Comments from review session:";
    let idx = stdout.find(MARKER)?;
    // Back up to the start of the marker's line so the header is included.
    let line_start = stdout[..idx].rfind('\n').map(|n| n + 1).unwrap_or(0);
    Some(stdout[line_start..].trim_end().to_string())
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
    /// Attach to a specific (session, window index) — used for terminals.
    pub should_attach_window: Option<(String, usize)>,
    pub should_open_editor: Option<PathBuf>,
    pub pending_project_path: Option<String>,
    pub pending_task_name: Option<String>,
    pub pending_task_branch: Option<String>,
    pub pending_session_name: Option<String>,
    pub collapsed: HashSet<String>,
    pub session_statuses: HashMap<String, SessionStatus>,
    pub diff_stats: HashMap<String, DiffStats>,
    pub session_branches: HashMap<String, String>,
    pub task_diff_stats: HashMap<String, DiffStats>,
    /// PR URLs keyed by branch name
    pub pr_urls: HashMap<String, String>,
    /// Stacked tasks' PRs (bottom→top `(url, title)`), keyed by branch name
    pub stack_prs: HashMap<String, Vec<(String, String)>>,
    /// Current git branch for each project, keyed by project name
    pub project_branches: HashMap<String, String>,
    /// Last-seen modification time of config.toml, used to detect external edits
    /// (e.g. `claude-manager set-stacked` run by the stacked-pr skill).
    pub config_mtime: Option<std::time::SystemTime>,
    /// Number of in-flight async ops. UI stays interactive while ops run; the
    /// status bar shows a spinner when this is non-zero.
    pub op_count: usize,
    pub op_receiver: mpsc::Receiver<OpResult>,
    pub op_sender: mpsc::Sender<OpResult>,
    pub tick: usize,
    pub worker: Worker,
    pub context_menu_items: Vec<ContextMenuItem>,
    pub context_menu_selected: usize,
    /// When true, the task list shows only archived tasks instead of active ones.
    pub view_archived: bool,
    /// Active filter substring; tasks/projects/sessions are matched case-insensitively.
    pub search_query: String,
    /// Index into `theme::THEMES` of the active color theme.
    pub theme_index: usize,
    /// Screen row (relative to the list area top) of the selected item, recorded
    /// during rendering so popups can anchor to it. Interior-mutable since draw
    /// only borrows `&App`.
    pub selected_row: std::cell::Cell<u16>,
}

pub struct OpResult {
    pub message: String,
    pub rebuild: bool,
    pub reload_config: bool,
}

/// Modification time of config.toml, if it exists.
fn config_file_mtime() -> Option<std::time::SystemTime> {
    std::fs::metadata(Config::config_path())
        .ok()
        .and_then(|m| m.modified().ok())
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
                if record.archived {
                    continue;
                }
                // Only act on records whose tmux session is gone. The decision
                // to recreate vs. prune is based on whether the task still
                // exists in config — NOT on tmux liveness — so legitimate
                // sessions are recovered after claude-manager/tmux restarts,
                // while sessions whose task was deleted/renamed-away are reaped
                // instead of being resurrected on every startup.
                if live_names.contains(tmux_name.as_str()) {
                    continue;
                }

                if tmux::is_adhoc_marker(&record.task_name) {
                    // Adhoc sessions are project-scoped. Recreate while the
                    // project exists; otherwise the project is gone — prune.
                    if !config.project_exists(&record.project_path) {
                        config::remove_session_record(tmux_name);
                    } else if tmux::recreate_adhoc_session(tmux_name, record).is_err() {
                        config::remove_session_record(tmux_name);
                    }
                    continue;
                }

                // Task-scoped session: match by branch (+ project path), which
                // is stable across renames, unlike the display-name fields.
                match config.find_task_by_branch(&record.project_path, &record.task_branch) {
                    Some(task) => {
                        if tmux::recreate_session(tmux_name, record, task.auto_context).is_err() {
                            // Could not recreate (e.g. worktree gone) — remove stale record
                            config::remove_session_record(tmux_name);
                        }
                    }
                    None => {
                        // The task no longer exists in config. Reap the orphan
                        // (worktree + cached context + record) so it isn't
                        // resurrected on every startup. The git branch is kept,
                        // preserving any committed work.
                        tmux::cleanup_orphan_session(record);
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
            collapsed: HashSet::new(),
            session_statuses: HashMap::new(),
            diff_stats: HashMap::new(),
            session_branches: HashMap::new(),
            task_diff_stats: HashMap::new(),
            pr_urls: HashMap::new(),
            stack_prs: HashMap::new(),
            project_branches: HashMap::new(),
            config_mtime: config_file_mtime(),
            op_count: 0,
            op_receiver: rx,
            op_sender: tx,
            tick: 0,
            worker: Worker::spawn(),
            context_menu_items: vec![],
            context_menu_selected: 0,
            view_archived: false,
            search_query: String::new(),
            theme_index: config::load_theme()
                .map(|n| crate::theme::by_name(&n))
                .unwrap_or(0),
            selected_row: std::cell::Cell::new(0),
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
            if !update.session_branches.is_empty() {
                self.session_branches = update.session_branches;
            }
            if !update.task_diff_stats.is_empty() {
                self.task_diff_stats = update.task_diff_stats;
            }
            if !update.pr_urls.is_empty() {
                self.pr_urls.extend(update.pr_urls);
            }
            if !update.stack_prs.is_empty() {
                self.stack_prs.extend(update.stack_prs);
            }
            if !update.project_branches.is_empty() {
                self.project_branches = update.project_branches;
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

    /// Pick up config edits made by another process (e.g. `claude-manager
    /// set-stacked` from the stacked-pr skill). Only reloads when idle (no
    /// in-flight op, Normal mode) and the on-disk content actually differs from
    /// memory — so the app's own saves never trigger a spurious rebuild.
    pub fn maybe_reload_config(&mut self) {
        if self.op_count != 0 || self.input_mode != InputMode::Normal {
            return;
        }
        let mtime = config_file_mtime();
        if mtime == self.config_mtime {
            return;
        }
        self.config_mtime = mtime;
        let Ok(disk) = std::fs::read_to_string(Config::config_path()) else {
            return;
        };
        // Skip if disk matches our in-memory state (our own write).
        if toml::to_string_pretty(&self.config)
            .map(|s| s == disk)
            .unwrap_or(false)
        {
            return;
        }
        if let Ok(cfg) = toml::from_str::<Config>(&disk) {
            self.config = cfg;
            self.rebuild_items();
            self.sync_worker_hints();
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
        let tasks: Vec<TaskInfo> = self
            .config
            .projects
            .iter()
            .flat_map(|p| {
                p.tasks.iter().map(|t| TaskInfo {
                    project_name: p.name.clone(),
                    project_path: p.path.clone(),
                    branch: t.branch.clone(),
                    base_branch: t.base_branch().to_string(),
                    stacked: t.stacked,
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
            hints.tasks = tasks;
            hints.project_paths = project_paths;
        }
    }

    pub fn rebuild_items(&mut self) {
        self.items.clear();
        let needle = self.search_query.to_lowercase();
        let needle = needle.trim();
        let want_archived = self.view_archived;
        for project in &self.config.projects {
            // Determine which tasks of this project match the current view + filter.
            let visible_tasks: Vec<&Task> = project
                .tasks
                .iter()
                .filter(|t| t.archived == want_archived)
                .filter(|t| {
                    needle.is_empty()
                        || project.name.to_lowercase().contains(needle)
                        || t.name.to_lowercase().contains(needle)
                        || t.branch.to_lowercase().contains(needle)
                })
                .collect();

            // Hide the project entirely when filtering and nothing matches under it.
            // Without a filter we still show empty projects so the user can add tasks.
            if !needle.is_empty() && visible_tasks.is_empty() {
                continue;
            }

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

            for task in visible_tasks {
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

                // Archived tasks have no live tmux sessions; skip session rendering.
                if task.archived {
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
        match self.selected_item() {
            // Enter on a task opens its shared context file in $EDITOR.
            Some(ListItem::Task {
                project_name, task, ..
            }) => {
                let ctx_path = crate::config::task_context_path(&project_name, &task.branch);
                self.should_open_editor = Some(ctx_path);
            }
            // Enter on a session attaches to it.
            Some(ListItem::Session { session, .. })
            | Some(ListItem::AdhocSession { session, .. }) => {
                self.should_attach = Some(session.name.clone());
            }
            _ => {}
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
            Some(ListItem::AdhocGroup { .. }) => vec![ContextMenuItem {
                key: cm.new_adhoc_session,
                label: "New adhoc session",
                action: ContextAction::NewAdhocSession,
            }],
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
                if task.archived {
                    vec![
                        ContextMenuItem {
                            key: cm.archive,
                            label: "Unarchive",
                            action: ContextAction::Unarchive,
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
                } else {
                    let ctx_label = if task.auto_context {
                        "Disable auto-context"
                    } else {
                        "Enable auto-context"
                    };
                    let stacked_label = if task.stacked {
                        "Disable stacked PRs"
                    } else {
                        "Enable stacked PRs"
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
                            key: cm.review,
                            label: "Review diff (difit)",
                            action: ContextAction::Review,
                        },
                        ContextMenuItem {
                            key: cm.toggle_auto_context,
                            label: ctx_label,
                            action: ContextAction::ToggleAutoContext,
                        },
                        ContextMenuItem {
                            key: cm.toggle_stacked,
                            label: stacked_label,
                            action: ContextAction::ToggleStacked,
                        },
                        ContextMenuItem {
                            key: cm.update,
                            label: "Update branch",
                            action: ContextAction::Update,
                        },
                        ContextMenuItem {
                            key: cm.set_base_branch,
                            label: "Set base branch",
                            action: ContextAction::SetBaseBranch,
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
                            key: cm.archive,
                            label: "Archive",
                            action: ContextAction::Archive,
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
            }
            Some(ListItem::Session { .. }) => {
                let mut items = vec![
                    ContextMenuItem {
                        key: cm.review,
                        label: "Review diff (difit)",
                        action: ContextAction::Review,
                    },
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
                        key: cm.terminal,
                        label: "Terminal",
                        action: ContextAction::Terminal,
                    },
                ];
                items.push(ContextMenuItem {
                    key: cm.copy_path,
                    label: "Copy worktree path",
                    action: ContextAction::CopyWorktreePath,
                });
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
            ContextAction::ToggleAutoContext => self.toggle_auto_context(),
            ContextAction::ToggleStacked => self.toggle_stacked(),
            ContextAction::CopyWorktreePath => self.copy_worktree_path(),
            ContextAction::SetBaseBranch => self.start_set_base_branch(),
            ContextAction::Archive => self.archive_task(),
            ContextAction::Unarchive => self.unarchive_task(),
            ContextAction::Review => self.start_review(),
            ContextAction::Terminal => self.open_terminal(),
        }
    }

    pub fn archive_task(&mut self) {
        let (project_name, task_name) = match self.selected_task_info() {
            Some((pn, _, t)) => (pn.to_string(), t.name.clone()),
            None => {
                self.status_message = Some("Select a task to archive".into());
                return;
            }
        };

        let task_sessions = tmux::sessions_for_task(&project_name, &task_name, &self.sessions);
        let live_names: Vec<String> = task_sessions.iter().map(|s| s.name.clone()).collect();
        let session_count = live_names.len();

        // Persist archived state on the task and its session records.
        self.config.reload();
        self.config
            .set_task_archived(&project_name, &task_name, true);
        let _ = self.config.save();
        config::set_task_session_records_archived(&project_name, &task_name, true);

        // Collapse so the archived task hides cleanly when the user toggles back to active view.
        self.collapsed.insert(task_key(&project_name, &task_name));

        self.start_op("Archiving task...", move || {
            for name in &live_names {
                let _ = tmux::kill_session_only(name);
            }
            OpResult {
                message: format!(
                    "Archived task '{task_name}' ({} session{} suspended)",
                    session_count,
                    if session_count == 1 { "" } else { "s" }
                ),
                rebuild: true,
                reload_config: true,
            }
        });
    }

    pub fn unarchive_task(&mut self) {
        let (project_name, task_name, task_branch, auto_context) = match self.selected_task_info() {
            Some((pn, _, t)) => (
                pn.to_string(),
                t.name.clone(),
                t.branch.clone(),
                t.auto_context,
            ),
            None => {
                self.status_message = Some("Select a task to unarchive".into());
                return;
            }
        };

        self.config.reload();
        self.config
            .set_task_archived(&project_name, &task_name, false);
        let _ = self.config.save();
        config::set_task_session_records_archived(&project_name, &task_name, false);

        let _ = task_branch;
        // Switch back to the active view so the unarchived task is visible.
        if self.view_archived {
            self.view_archived = false;
        }

        self.start_op("Unarchiving task...", move || {
            let records = config::load_sessions();
            let mut recreated = 0;
            let mut failed = 0;
            for (tmux_name, record) in &records {
                if record.project_name == project_name && record.task_name == task_name {
                    match tmux::recreate_session(tmux_name, record, auto_context) {
                        Ok(_) => recreated += 1,
                        Err(_) => {
                            failed += 1;
                            // Stale record (e.g. worktree removed externally) — drop it.
                            config::remove_session_record(tmux_name);
                        }
                    }
                }
            }
            let msg = if failed > 0 {
                format!(
                    "Unarchived '{task_name}' — {recreated} session(s) restored, {failed} dropped"
                )
            } else {
                format!("Unarchived '{task_name}' — {recreated} session(s) restored")
            };
            OpResult {
                message: msg,
                rebuild: true,
                reload_config: true,
            }
        });
    }

    pub fn toggle_archive_view(&mut self) {
        self.view_archived = !self.view_archived;
        self.search_query.clear();
        self.selected = 0;
        self.rebuild_items();
        self.status_message = Some(if self.view_archived {
            "Showing archived tasks".into()
        } else {
            "Showing active tasks".into()
        });
        self.sync_worker_hints();
    }

    pub fn cycle_theme(&mut self) {
        self.theme_index = (self.theme_index + 1) % crate::theme::THEMES.len();
        let name = crate::theme::THEMES[self.theme_index].name;
        crate::config::save_theme(name);
        self.status_message = Some(format!("Theme: {name}"));
    }

    /// Launch difit to review a task (branch vs base) or a session (uncommitted
    /// changes). Runs in the background so the TUI stays interactive; on exit any
    /// review comments are forwarded to the agent session as a new prompt.
    pub fn start_review(&mut self) {
        // (cwd, difit args, session to forward to, description)
        let (cwd, args, session, description) = match self.selected_item().cloned() {
            Some(ListItem::Task {
                project_name,
                project_path,
                task,
            }) => {
                let base = task
                    .base_branch
                    .clone()
                    .unwrap_or_else(|| "main".to_string());
                let base_ref = tmux::resolve_base_ref(&project_path, &base);
                let session = tmux::sessions_for_task(&project_name, &task.name, &self.sessions)
                    .first()
                    .map(|s| s.name.clone());
                // `difit <target> <base>`: the SECOND positional is the base
                // (old side). `--merge-base` resolves it to merge-base(branch,
                // base) so we see only the branch's changes — the GitHub PR diff,
                // excluding main's commits since the fork point.
                (
                    project_path,
                    vec![task.branch.clone(), base_ref, "--merge-base".to_string()],
                    session,
                    format!("{} vs {base}", task.branch),
                )
            }
            Some(ListItem::Session {
                project_path,
                session,
                ..
            }) => {
                let cwd = session
                    .worktree_path()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or(project_path);
                (
                    cwd,
                    vec![".".to_string(), "--include-untracked".to_string()],
                    Some(session.name.clone()),
                    format!("uncommitted changes in {}", session.session_name),
                )
            }
            _ => {
                self.status_message = Some("Select a task or session to review".into());
                return;
            }
        };

        self.start_op(&format!("Reviewing {description} in difit…"), move || {
            // `.output()` captures difit's stdout/stderr (keeping them off the
            // TUI) and blocks this background thread until the browser closes.
            let message = match std::process::Command::new("difit")
                .args(&args)
                .current_dir(&cwd)
                .output()
            {
                Err(e) => format!("difit failed to launch: {e}"),
                Ok(out) => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    match extract_difit_comments(&stdout) {
                        Some(comments) => match &session {
                            Some(s) => {
                                let prompt = format!(
                                    "The following code review comments were left in difit. \
                                     Please address them:\n\n{comments}"
                                );
                                match tmux::send_text(s, &prompt, true) {
                                    Ok(()) => format!("Forwarded review comments to {s}"),
                                    Err(e) => format!("Failed to forward comments: {e}"),
                                }
                            }
                            None => "Review finished; no session to forward comments to".into(),
                        },
                        None => "Review closed with no comments".into(),
                    }
                }
            };
            OpResult {
                message,
                rebuild: false,
                reload_config: false,
            }
        });
    }

    /// Open a terminal in the session's worktree: create one if none exists,
    /// then attach to it (attaches directly if one already exists).
    pub fn open_terminal(&mut self) {
        let (name, cwd) = match self.selected_item().cloned() {
            Some(ListItem::Session {
                project_path,
                session,
                ..
            })
            | Some(ListItem::AdhocSession {
                project_path,
                session,
                ..
            }) => {
                let cwd = session
                    .worktree_path()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or(project_path);
                (session.name.clone(), cwd)
            }
            _ => {
                self.status_message = Some("Select a session to open a terminal".into());
                return;
            }
        };
        if tmux::count_terminal_windows(&name) == 0 {
            if let Err(e) = tmux::create_terminal_window(&name, &cwd) {
                self.status_message = Some(format!("Error: {e}"));
                return;
            }
        }
        // Window 0 is the agent; the first terminal is window 1.
        self.should_attach_window = Some((name, 1));
    }

    pub fn start_search(&mut self) {
        self.input_mode = InputMode::Search;
        self.input_buffer = self.search_query.clone();
        self.status_message = Some("Filter (Esc to clear): ".into());
    }

    pub fn update_search(&mut self) {
        self.search_query = self.input_buffer.clone();
        self.selected = 0;
        self.rebuild_items();
    }

    pub fn confirm_search(&mut self) {
        self.search_query = self.input_buffer.trim().to_string();
        self.input_buffer.clear();
        self.input_mode = InputMode::Normal;
        self.status_message = if self.search_query.is_empty() {
            None
        } else {
            Some(format!("Filter: {}", self.search_query))
        };
        self.selected = 0;
        self.rebuild_items();
        self.sync_worker_hints();
    }

    pub fn cancel_search(&mut self) {
        self.search_query.clear();
        self.input_buffer.clear();
        self.input_mode = InputMode::Normal;
        self.status_message = None;
        self.selected = 0;
        self.rebuild_items();
        self.sync_worker_hints();
    }

    pub fn start_set_base_branch(&mut self) {
        let task = match self.selected_item() {
            Some(ListItem::Task { task, .. }) => task.clone(),
            _ => {
                self.status_message = Some("Select a task to set its base branch".into());
                return;
            }
        };
        self.input_mode = InputMode::SetBaseBranch;
        self.input_buffer = task
            .base_branch
            .clone()
            .unwrap_or_else(|| task.base_branch().to_string());
        self.status_message = Some("Base branch (empty for main): ".into());
    }

    pub fn confirm_set_base_branch(&mut self) {
        let (project_name, task_name) = match self.selected_item() {
            Some(ListItem::Task {
                project_name, task, ..
            }) => (project_name.clone(), task.name.clone()),
            _ => {
                self.cancel_input();
                return;
            }
        };

        let raw = self.input_buffer.trim().to_string();
        let new_base = if raw.is_empty() { None } else { Some(raw) };

        self.config.reload();
        self.config
            .set_task_base_branch(&project_name, &task_name, new_base.clone());
        let _ = self.config.save();

        let label = new_base.as_deref().unwrap_or("main");
        self.status_message = Some(format!("Base branch for '{task_name}' set to {label}"));
        self.input_buffer.clear();
        self.input_mode = InputMode::Normal;
        self.rebuild_items();
        self.sync_worker_hints();
    }

    pub fn copy_worktree_path(&mut self) {
        let session = match self.selected_item() {
            Some(ListItem::Session { session, .. }) => session,
            _ => {
                self.status_message = Some("Select a session to copy its worktree path".into());
                return;
            }
        };
        let path = match session.worktree_path() {
            Some(p) => p.to_string_lossy().to_string(),
            None => {
                self.status_message = Some("Session has no worktree".into());
                return;
            }
        };
        match copy_to_clipboard(&path) {
            Ok(()) => self.status_message = Some(format!("Copied to clipboard: {path}")),
            Err(e) => self.status_message = Some(format!("Copy failed: {e}")),
        }
    }

    pub fn toggle_auto_context(&mut self) {
        let (project_name, task_name, task_branch) = match self.selected_task_info() {
            Some((pn, _, t)) => (pn.to_string(), t.name.clone(), t.branch.clone()),
            None => return,
        };

        self.config.reload();
        if let Some(new_state) = self.config.toggle_auto_context(&project_name, &task_name) {
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

    pub fn toggle_stacked(&mut self) {
        let (project_name, task_name) = match self.selected_task_info() {
            Some((pn, _, t)) => (pn.to_string(), t.name.clone()),
            None => return,
        };

        self.config.reload();
        if let Some(new_state) = self.config.toggle_stacked(&project_name, &task_name) {
            let _ = self.config.save();
            let label = if new_state { "enabled" } else { "disabled" };
            self.status_message = Some(format!("Stacked PRs {label} for '{task_name}'"));
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
            self.status_message = Some("'adhoc' is reserved — pick a different task name".into());
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
                            archived: false,
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
            self.status_message = Some(format!("Adhoc session '{name}' already exists"));
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

        self.start_op(
            "Creating adhoc session...",
            move || match tmux::create_adhoc_session(
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
                            archived: false,
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
            },
        );
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
                            archived: false,
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
            Some(ListItem::AdhocSession { session, .. }) => {
                (InputMode::RenameAdhocSession, session.session_name.clone())
            }
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
                project_name,
                project_path,
                task,
            }) => {
                let branch = task.branch.clone();
                let base_branch = task.base_branch().to_string();
                // Stacked task: re-publish the stack (rebases onto trunk + refreshes PRs).
                if task.stacked {
                    self.run_stack_update(
                        project_name,
                        project_path,
                        branch,
                        "Syncing stack...",
                        false,
                    );
                    return;
                }
                self.start_op(
                    "Updating task branch...",
                    move || match tmux::update_task_branch(&project_path, &branch, &base_branch) {
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

    /// Run `git spr update` for a stacked task: publishes/refreshes one PR per commit,
    /// caches the resulting stack for the worker/UI, and (optionally) opens the bottom PR.
    fn run_stack_update(
        &mut self,
        project_name: String,
        project_path: String,
        branch: String,
        label: &'static str,
        open_bottom: bool,
    ) {
        self.start_op(label, move || {
            match tmux::spr_update(&project_path, &branch) {
                Ok(prs) => {
                    // Cache the stack so the background worker (and UI) can read it
                    // without touching git. Bottom PR feeds the PR icon / "Open PR".
                    config::write_stack_cache(&project_name, &branch, &prs);
                    if open_bottom {
                        if let Some((url, _)) = prs.first() {
                            let _ = std::process::Command::new("open").arg(url).output();
                        }
                    }
                    OpResult {
                        message: format!("Stack updated: {} PR(s)", prs.len()),
                        rebuild: true,
                        reload_config: false,
                    }
                }
                Err(e) => OpResult {
                    message: format!("Stack error: {e}"),
                    rebuild: false,
                    reload_config: false,
                },
            }
        });
    }

    pub fn confirm_create_pr(&mut self) {
        let (project_path, project_name, task) = match self.selected_item().cloned() {
            Some(ListItem::Task {
                project_path,
                project_name,
                task,
            }) => (project_path, project_name, task),
            _ => {
                self.cancel_input();
                return;
            }
        };

        let branch = task.branch.clone();
        let task_name = task.name.clone();
        self.input_mode = InputMode::Normal;

        // Stacked task: publish each commit as its own PR via `git spr`, not a single PR.
        if task.stacked {
            self.run_stack_update(
                project_name,
                project_path,
                branch,
                "Publishing stack...",
                true,
            );
            return;
        }

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
    }
}

/// Copy `text` to the system clipboard. Tries `pbcopy` (macOS), `wl-copy`
/// (Wayland), then `xclip -selection clipboard` (X11).
fn copy_to_clipboard(text: &str) -> std::result::Result<(), String> {
    let candidates: &[(&str, &[&str])] = &[
        ("pbcopy", &[]),
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
    ];
    let mut last_err = String::from("no clipboard tool found (pbcopy / wl-copy / xclip)");
    for (cmd, args) in candidates {
        match Command::new(cmd)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                if let Some(mut stdin) = child.stdin.take() {
                    if let Err(e) = stdin.write_all(text.as_bytes()) {
                        last_err = format!("{cmd}: write failed: {e}");
                        let _ = child.wait();
                        continue;
                    }
                }
                match child.wait() {
                    Ok(status) if status.success() => return Ok(()),
                    Ok(status) => last_err = format!("{cmd} exited with {status}"),
                    Err(e) => last_err = format!("{cmd}: {e}"),
                }
            }
            Err(_) => continue,
        }
    }
    Err(last_err)
}
