use anyhow::Result;

use crate::config::{Config, Project};
use crate::tmux::{self, TmuxSession};

#[derive(Debug, Clone)]
pub enum ListItem {
    Project(Project),
    Session(TmuxSession),
}

#[derive(Debug, PartialEq)]
pub enum InputMode {
    Normal,
    AddProjectName,
    AddSessionName,
    ConfirmDelete,
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

    pub fn refresh_sessions(&mut self) {
        self.sessions = tmux::list_sessions().unwrap_or_default();
        self.rebuild_items();
    }

    pub fn rebuild_items(&mut self) {
        self.items.clear();
        for project in &self.config.projects {
            self.items.push(ListItem::Project(project.clone()));
            for session in &self.sessions {
                if session.project_name == project.name {
                    self.items.push(ListItem::Session(session.clone()));
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

    pub fn selected_project(&self) -> Option<&Project> {
        // Walk backwards from selected to find the parent project
        for i in (0..=self.selected).rev() {
            if let Some(ListItem::Project(p)) = self.items.get(i) {
                return Some(p);
            }
        }
        None
    }

    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.items.len() {
            self.selected += 1;
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

        self.pending_project_path = Some(cwd_str.clone());
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

    pub fn start_new_session(&mut self, use_worktree: bool) {
        if self.selected_project().is_some() {
            self.use_worktree = use_worktree;
            self.input_mode = InputMode::AddSessionName;
            self.input_buffer.clear();
            let next = tmux::next_session_number(
                &self.selected_project().unwrap().name,
                &self.sessions,
            );
            self.status_message = Some(format!(
                "Session name (default: {next}){}:",
                if use_worktree { " [worktree]" } else { "" }
            ));
        }
    }

    pub fn confirm_new_session(&mut self) {
        if let Some(project) = self.selected_project().cloned() {
            let session_name = if self.input_buffer.trim().is_empty() {
                tmux::next_session_number(&project.name, &self.sessions).to_string()
            } else {
                self.input_buffer.trim().to_string()
            };

            match tmux::create_session(
                &project.name,
                &project.path,
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
    }

    pub fn enter_selected(&mut self) {
        if let Some(ListItem::Session(session)) = self.selected_item().cloned() {
            self.should_attach = Some(session.name);
        }
    }

    pub fn start_delete(&mut self) {
        if let Some(ListItem::Session(_)) = self.selected_item() {
            self.input_mode = InputMode::ConfirmDelete;
            self.status_message = Some("Delete this session? (y/n)".into());
        }
    }

    pub fn confirm_delete(&mut self) {
        if let Some(ListItem::Session(session)) = self.selected_item().cloned() {
            let project_path = self.selected_project().map(|p| p.path.as_str());
            match tmux::kill_session(&session.name, project_path) {
                Ok(()) => {
                    self.status_message = Some(format!("Killed session {}", session.name));
                    self.refresh_sessions();
                }
                Err(e) => {
                    self.status_message = Some(format!("Error: {e}"));
                }
            }
        }
        self.input_mode = InputMode::Normal;
    }

    pub fn cancel_input(&mut self) {
        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();
        self.status_message = None;
    }
}
