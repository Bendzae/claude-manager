use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub name: String,
    pub branch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub tasks: Vec<Task>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub projects: Vec<Project>,
}

/// Root directory for all claude-manager data: ~/.claude-manager
pub fn base_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".claude-manager")
}

impl Config {
    pub fn config_path() -> PathBuf {
        base_dir().join("config.toml")
    }

    pub fn load() -> Result<Self> {
        let path = Self::config_path();
        if !path.exists() {
            return Ok(Config::default());
        }
        let content = fs::read_to_string(&path).context("Failed to read config file")?;
        toml::from_str(&content).context("Failed to parse config file")
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).context("Failed to create config directory")?;
        }
        let content = toml::to_string_pretty(self).context("Failed to serialize config")?;
        fs::write(&path, content).context("Failed to write config file")
    }

    pub fn add_project(&mut self, name: String, path: String) {
        if !self.projects.iter().any(|p| p.path == path) {
            self.projects.push(Project {
                name,
                path,
                tasks: vec![],
            });
        }
    }

    pub fn has_project_at(&self, path: &str) -> bool {
        self.projects.iter().any(|p| p.path == path)
    }

    pub fn rename_project(&mut self, old_name: &str, new_name: String) -> bool {
        if let Some(project) = self.projects.iter_mut().find(|p| p.name == old_name) {
            project.name = new_name;
            true
        } else {
            false
        }
    }

    pub fn add_task(&mut self, project_name: &str, task_name: String, branch: String) -> bool {
        if let Some(project) = self.projects.iter_mut().find(|p| p.name == project_name) {
            if !project.tasks.iter().any(|t| t.name == task_name) {
                project.tasks.push(Task {
                    name: task_name,
                    branch,
                });
                return true;
            }
        }
        false
    }

    pub fn remove_task(&mut self, project_name: &str, task_name: &str) -> bool {
        if let Some(project) = self.projects.iter_mut().find(|p| p.name == project_name) {
            let before = project.tasks.len();
            project.tasks.retain(|t| t.name != task_name);
            return project.tasks.len() < before;
        }
        false
    }

    pub fn rename_task(
        &mut self,
        project_name: &str,
        old_task_name: &str,
        new_task_name: String,
    ) -> bool {
        if let Some(project) = self.projects.iter_mut().find(|p| p.name == project_name) {
            if let Some(task) = project.tasks.iter_mut().find(|t| t.name == old_task_name) {
                task.name = new_task_name;
                return true;
            }
        }
        false
    }

    #[allow(dead_code)]
    pub fn find_task(&self, project_name: &str, task_name: &str) -> Option<&Task> {
        self.projects
            .iter()
            .find(|p| p.name == project_name)?
            .tasks
            .iter()
            .find(|t| t.name == task_name)
    }

    #[allow(dead_code)]
    pub fn remove_project(&mut self, path: &str) {
        self.projects.retain(|p| p.path != path);
    }
}
