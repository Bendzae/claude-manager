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
    /// File patterns to copy into new worktrees (e.g. [".env", "build/"])
    #[serde(default)]
    pub copy_patterns: Vec<String>,
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

/// Path to the shared task context file for a given project/branch.
pub fn task_context_path(project_name: &str, branch: &str) -> PathBuf {
    base_dir()
        .join("tasks")
        .join(crate::tmux::sanitize(project_name))
        .join(crate::tmux::sanitize(branch))
        .join("TASK_CONTEXT.md")
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
                copy_patterns: vec![],
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

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_config() -> Config {
        Config::default()
    }

    #[test]
    fn add_project_stores_it() {
        let mut cfg = empty_config();
        cfg.add_project("My App".into(), "/tmp/my-app".into());
        assert_eq!(cfg.projects.len(), 1);
        assert_eq!(cfg.projects[0].name, "My App");
        assert_eq!(cfg.projects[0].path, "/tmp/my-app");
    }

    #[test]
    fn add_project_deduplicates_by_path() {
        let mut cfg = empty_config();
        cfg.add_project("App".into(), "/tmp/app".into());
        cfg.add_project("App2".into(), "/tmp/app".into());
        assert_eq!(cfg.projects.len(), 1);
    }

    #[test]
    fn has_project_at() {
        let mut cfg = empty_config();
        assert!(!cfg.has_project_at("/tmp/app"));
        cfg.add_project("App".into(), "/tmp/app".into());
        assert!(cfg.has_project_at("/tmp/app"));
    }

    #[test]
    fn rename_project_success() {
        let mut cfg = empty_config();
        cfg.add_project("Old".into(), "/tmp/app".into());
        assert!(cfg.rename_project("Old", "New".into()));
        assert_eq!(cfg.projects[0].name, "New");
    }

    #[test]
    fn rename_project_not_found() {
        let mut cfg = empty_config();
        assert!(!cfg.rename_project("Missing", "New".into()));
    }

    #[test]
    fn add_task_to_project() {
        let mut cfg = empty_config();
        cfg.add_project("App".into(), "/tmp/app".into());
        assert!(cfg.add_task("App", "fix-bug".into(), "fix-bug-branch".into()));
        assert_eq!(cfg.projects[0].tasks.len(), 1);
        assert_eq!(cfg.projects[0].tasks[0].name, "fix-bug");
        assert_eq!(cfg.projects[0].tasks[0].branch, "fix-bug-branch");
    }

    #[test]
    fn add_task_deduplicates_by_name() {
        let mut cfg = empty_config();
        cfg.add_project("App".into(), "/tmp/app".into());
        cfg.add_task("App", "fix-bug".into(), "branch-1".into());
        assert!(!cfg.add_task("App", "fix-bug".into(), "branch-2".into()));
        assert_eq!(cfg.projects[0].tasks.len(), 1);
    }

    #[test]
    fn add_task_to_missing_project() {
        let mut cfg = empty_config();
        assert!(!cfg.add_task("Missing", "task".into(), "branch".into()));
    }

    #[test]
    fn remove_task() {
        let mut cfg = empty_config();
        cfg.add_project("App".into(), "/tmp/app".into());
        cfg.add_task("App", "t1".into(), "b1".into());
        cfg.add_task("App", "t2".into(), "b2".into());
        assert!(cfg.remove_task("App", "t1"));
        assert_eq!(cfg.projects[0].tasks.len(), 1);
        assert_eq!(cfg.projects[0].tasks[0].name, "t2");
    }

    #[test]
    fn remove_task_not_found() {
        let mut cfg = empty_config();
        cfg.add_project("App".into(), "/tmp/app".into());
        assert!(!cfg.remove_task("App", "nope"));
    }

    #[test]
    fn rename_task() {
        let mut cfg = empty_config();
        cfg.add_project("App".into(), "/tmp/app".into());
        cfg.add_task("App", "old".into(), "branch".into());
        assert!(cfg.rename_task("App", "old", "new".into()));
        assert_eq!(cfg.projects[0].tasks[0].name, "new");
    }

    #[test]
    fn find_task() {
        let mut cfg = empty_config();
        cfg.add_project("App".into(), "/tmp/app".into());
        cfg.add_task("App", "t1".into(), "b1".into());
        let task = cfg.find_task("App", "t1");
        assert!(task.is_some());
        assert_eq!(task.unwrap().branch, "b1");
        assert!(cfg.find_task("App", "missing").is_none());
        assert!(cfg.find_task("Missing", "t1").is_none());
    }

    #[test]
    fn remove_project() {
        let mut cfg = empty_config();
        cfg.add_project("App".into(), "/tmp/app".into());
        cfg.remove_project("/tmp/app");
        assert!(cfg.projects.is_empty());
    }

    #[test]
    fn roundtrip_serialization() {
        let mut cfg = empty_config();
        cfg.add_project("App".into(), "/tmp/app".into());
        cfg.add_task("App", "task1".into(), "branch1".into());

        let serialized = toml::to_string_pretty(&cfg).unwrap();
        let deserialized: Config = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized.projects.len(), 1);
        assert_eq!(deserialized.projects[0].tasks.len(), 1);
        assert_eq!(deserialized.projects[0].tasks[0].name, "task1");
    }
}
