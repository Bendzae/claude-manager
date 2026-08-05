use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde::de::Deserializer;
use serde::{Deserialize, Serialize, Serializer};

/// Serializes config / sessions.json reads-modify-writes across threads so
/// concurrent ops (e.g. two async session creations) don't clobber each other.
static IO_LOCK: Mutex<()> = Mutex::new(());

fn kb_quit() -> char {
    'q'
}
fn kb_move_up() -> char {
    'k'
}
fn kb_move_down() -> char {
    'j'
}
fn kb_toggle_collapse() -> char {
    ' '
}
fn kb_context_menu() -> char {
    'a'
}
fn kb_add_project() -> char {
    'p'
}

// Context menu action key defaults
fn cm_add_task() -> char {
    't'
}
fn cm_new_session() -> char {
    'n'
}
fn cm_new_session_no_worktree() -> char {
    'N'
}
fn cm_new_adhoc_session() -> char {
    'A'
}
fn cm_update() -> char {
    'u'
}
fn cm_push() -> char {
    'P'
}
fn cm_checkout() -> char {
    'b'
}
fn cm_open_pr() -> char {
    'o'
}
fn cm_delete() -> char {
    'd'
}
fn cm_merge() -> char {
    'm'
}
fn cm_copy_path() -> char {
    'y'
}
fn cm_set_base_branch() -> char {
    'B'
}
fn cm_archive() -> char {
    'A'
}

fn cm_review() -> char {
    'r'
}

fn cm_terminal() -> char {
    't'
}
fn cm_fetch_pull() -> char {
    'f'
}
fn cm_run() -> char {
    'x'
}
fn kb_toggle_archive_view() -> char {
    'Z'
}
fn kb_search() -> char {
    '/'
}

fn kb_cycle_theme() -> char {
    't'
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Keybindings for context menu actions. All fields are single characters.
/// Configured under `[context_menu]` in `~/.claude-manager/keybindings.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextMenuKeyBindings {
    /// Add task to project (default: t)
    #[serde(default = "cm_add_task")]
    pub add_task: char,
    /// New session with worktree (default: n)
    #[serde(default = "cm_new_session")]
    pub new_session: char,
    /// New session without worktree (default: N)
    #[serde(default = "cm_new_session_no_worktree")]
    pub new_session_no_worktree: char,
    /// New adhoc session on project (default: A)
    #[serde(default = "cm_new_adhoc_session")]
    pub new_adhoc_session: char,
    /// Update branch (default: u)
    #[serde(default = "cm_update")]
    pub update: char,
    /// Push branch (default: P)
    #[serde(default = "cm_push")]
    pub push: char,
    /// Checkout branch (default: b)
    #[serde(default = "cm_checkout")]
    pub checkout: char,
    /// Open PR (default: o)
    #[serde(default = "cm_open_pr")]
    pub open_pr: char,
    /// Delete item (default: d)
    #[serde(default = "cm_delete")]
    pub delete: char,
    /// Merge session (default: m)
    #[serde(default = "cm_merge")]
    pub merge: char,
    /// Copy session worktree path to clipboard (default: y)
    #[serde(default = "cm_copy_path")]
    pub copy_path: char,
    /// Set task base branch (default: B)
    #[serde(default = "cm_set_base_branch")]
    pub set_base_branch: char,
    /// Archive / unarchive task (default: A)
    #[serde(default = "cm_archive")]
    pub archive: char,
    /// Review diff in the configured review tool (default: r)
    #[serde(default = "cm_review")]
    pub review: char,
    /// Open/attach a terminal in the session worktree (default: t)
    #[serde(default = "cm_terminal")]
    pub terminal: char,
    /// Fetch & pull all branches for a project (default: f)
    #[serde(default = "cm_fetch_pull")]
    pub fetch_pull: char,
    /// Run the project's configured run command (default: x)
    #[serde(default = "cm_run")]
    pub run: char,
}

impl Default for ContextMenuKeyBindings {
    fn default() -> Self {
        ContextMenuKeyBindings {
            add_task: cm_add_task(),
            new_session: cm_new_session(),
            new_session_no_worktree: cm_new_session_no_worktree(),
            new_adhoc_session: cm_new_adhoc_session(),
            update: cm_update(),
            push: cm_push(),
            checkout: cm_checkout(),
            open_pr: cm_open_pr(),
            delete: cm_delete(),
            merge: cm_merge(),
            copy_path: cm_copy_path(),
            set_base_branch: cm_set_base_branch(),
            archive: cm_archive(),
            review: cm_review(),
            terminal: cm_terminal(),
            fetch_pull: cm_fetch_pull(),
            run: cm_run(),
        }
    }
}

/// Keybindings for Normal mode. All fields are single characters.
/// Arrow keys, Enter, Esc, and Tab are not configurable.
/// Loaded from `~/.claude-manager/keybindings.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyBindings {
    /// Quit the application (default: q)
    #[serde(default = "kb_quit")]
    pub quit: char,
    /// Move selection up (default: k)
    #[serde(default = "kb_move_up")]
    pub move_up: char,
    /// Move selection down (default: j)
    #[serde(default = "kb_move_down")]
    pub move_down: char,
    /// Toggle collapse of selected item (default: space)
    #[serde(default = "kb_toggle_collapse")]
    pub toggle_collapse: char,
    /// Open context menu (default: a)
    #[serde(default = "kb_context_menu")]
    pub context_menu: char,
    /// Add project from current directory (default: p)
    #[serde(default = "kb_add_project")]
    pub add_project: char,
    /// Toggle archived task view (default: Z)
    #[serde(default = "kb_toggle_archive_view")]
    pub toggle_archive_view: char,
    /// Filter tasks by substring (default: /)
    #[serde(default = "kb_search")]
    pub search: char,
    /// Cycle the color theme (default: t)
    #[serde(default = "kb_cycle_theme")]
    pub cycle_theme: char,
    /// Context menu action keybindings
    #[serde(default)]
    pub context_menu_keys: ContextMenuKeyBindings,
}

impl Default for KeyBindings {
    fn default() -> Self {
        KeyBindings {
            quit: kb_quit(),
            move_up: kb_move_up(),
            move_down: kb_move_down(),
            toggle_collapse: kb_toggle_collapse(),
            context_menu: kb_context_menu(),
            add_project: kb_add_project(),
            toggle_archive_view: kb_toggle_archive_view(),
            search: kb_search(),
            cycle_theme: kb_cycle_theme(),
            context_menu_keys: ContextMenuKeyBindings::default(),
        }
    }
}

/// Path to the keybindings config file.
pub fn keybindings_path() -> PathBuf {
    base_dir().join("keybindings.toml")
}

impl KeyBindings {
    pub fn load() -> Self {
        let path = keybindings_path();
        if !path.exists() {
            return KeyBindings::default();
        }
        fs::read_to_string(&path)
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub name: String,
    pub branch: String,
    /// Base branch for `update` (rebase target) and diff stats.
    /// `None` means default to "main".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_branch: Option<String>,
    /// Archived: hidden from default view, sessions killed but worktrees/branches/context preserved.
    #[serde(default, skip_serializing_if = "is_false")]
    pub archived: bool,
}

impl Task {
    /// Effective base branch for rebase/diff. Defaults to "main".
    pub fn base_branch(&self) -> &str {
        self.base_branch.as_deref().unwrap_or("main")
    }
}

/// Deserialize `setup_commands` from either a single string or an array of strings.
fn deserialize_setup_commands<'de, D>(deserializer: D) -> std::result::Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }

    Ok(match Option::<OneOrMany>::deserialize(deserializer)? {
        None => vec![],
        Some(OneOrMany::One(s)) => vec![s],
        Some(OneOrMany::Many(v)) => v,
    })
}

/// Serialize `setup_commands`: skip if empty, single string if one element, array otherwise.
fn serialize_setup_commands<S>(
    commands: &[String],
    serializer: S,
) -> std::result::Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match commands.len() {
        0 => serializer.serialize_none(),
        1 => serializer.serialize_str(&commands[0]),
        _ => {
            use serde::ser::SerializeSeq;
            let mut seq = serializer.serialize_seq(Some(commands.len()))?;
            for cmd in commands {
                seq.serialize_element(cmd)?;
            }
            seq.end()
        }
    }
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
    /// Commands to run in the worktree after creation (e.g. "./gradlew configureGitHooks")
    /// Accepts a single string or an array of strings in the config.
    #[serde(
        default,
        deserialize_with = "deserialize_setup_commands",
        serialize_with = "serialize_setup_commands"
    )]
    pub setup_commands: Vec<String>,
    /// Command run by the "Run" action (e.g. "npm run dev"). Prompted for and
    /// saved on first use; executed in a dedicated tmux session in the selected
    /// item's working directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_command: Option<String>,
}

/// Which diff review tool the review action launches.
///
/// - `Hunk` (default) — a terminal diff viewer ([modem-dev/hunk]) run in the
///   foreground, suspending the TUI for the duration of the review.
/// - `Difit` — a browser-based diff viewer run in the background; any comments
///   left on exit are forwarded to the agent session.
///
/// [modem-dev/hunk]: https://github.com/modem-dev/hunk
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewTool {
    #[default]
    Hunk,
    Difit,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    /// Diff review tool launched by the review action (default: hunk).
    /// Declared first so it serializes before the `[[projects]]` tables (a
    /// scalar emitted after a table would be invalid TOML).
    #[serde(default)]
    pub review_tool: ReviewTool,
    #[serde(default)]
    pub projects: Vec<Project>,
    /// Startup skills/commands to run before the initial prompt (e.g. ["/prime", "/caveman ultra"])
    #[serde(
        default,
        deserialize_with = "deserialize_setup_commands",
        serialize_with = "serialize_setup_commands"
    )]
    pub startup_skills: Vec<String>,
}

/// Root directory for all claude-manager data: ~/.claude-manager
pub fn base_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".claude-manager")
}

/// Path to the persisted UI theme name.
pub fn theme_path() -> PathBuf {
    base_dir().join("theme")
}

/// Load the persisted theme name, if any.
pub fn load_theme() -> Option<String> {
    fs::read_to_string(theme_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Persist the selected theme name.
pub fn save_theme(name: &str) {
    let _ = fs::write(theme_path(), name);
}

/// Directory holding cached per-task files (pr_url.txt) for a project/branch.
pub fn task_dir(project_name: &str, branch: &str) -> PathBuf {
    base_dir()
        .join("tasks")
        .join(crate::tmux::sanitize(project_name))
        .join(crate::tmux::sanitize(branch))
}

/// Path to the cached PR URL file for a given project/branch.
pub fn pr_url_path(project_name: &str, branch: &str) -> PathBuf {
    task_dir(project_name, branch).join("pr_url.txt")
}

/// Metadata needed to recreate a tmux session after tmux dies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub project_name: String,
    pub project_path: String,
    pub task_name: String,
    pub task_branch: String,
    pub session_name: String,
    pub use_worktree: bool,
    /// Session belongs to an archived task. Skipped during startup recreation.
    #[serde(default, skip_serializing_if = "is_false")]
    pub archived: bool,
}

/// Path to the persisted sessions file.
pub fn sessions_path() -> PathBuf {
    base_dir().join("sessions.json")
}

/// Load all saved session records, keyed by tmux session name.
pub fn load_sessions() -> HashMap<String, SessionRecord> {
    let path = sessions_path();
    fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Save all session records to disk.
fn save_sessions(sessions: &HashMap<String, SessionRecord>) -> Result<()> {
    let path = sessions_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(sessions)?;
    fs::write(&path, content).context("Failed to write sessions file")
}

/// Add a session record and persist.
pub fn add_session_record(tmux_name: &str, record: SessionRecord) {
    let _g = IO_LOCK.lock().unwrap();
    let mut sessions = load_sessions();
    sessions.insert(tmux_name.to_string(), record);
    let _ = save_sessions(&sessions);
}

/// Remove a session record by tmux name and persist.
pub fn remove_session_record(tmux_name: &str) {
    let _g = IO_LOCK.lock().unwrap();
    let mut sessions = load_sessions();
    if sessions.remove(tmux_name).is_some() {
        let _ = save_sessions(&sessions);
    }
}

/// Remove all session records matching a project+task and persist.
pub fn remove_task_session_records(project_name: &str, task_name: &str) {
    let _g = IO_LOCK.lock().unwrap();
    let mut sessions = load_sessions();
    let before = sessions.len();
    sessions.retain(|_, r| !(r.project_name == project_name && r.task_name == task_name));
    if sessions.len() < before {
        let _ = save_sessions(&sessions);
    }
}

/// Mark all session records for a project+task as archived (or not) and persist.
/// Returns true if any record was updated.
pub fn set_task_session_records_archived(
    project_name: &str,
    task_name: &str,
    archived: bool,
) -> bool {
    let _g = IO_LOCK.lock().unwrap();
    let mut sessions = load_sessions();
    let mut changed = false;
    for record in sessions.values_mut() {
        if record.project_name == project_name
            && record.task_name == task_name
            && record.archived != archived
        {
            record.archived = archived;
            changed = true;
        }
    }
    if changed {
        let _ = save_sessions(&sessions);
    }
    changed
}

/// Remove all session records matching a project and persist.
pub fn remove_project_session_records(project_name: &str) {
    let _g = IO_LOCK.lock().unwrap();
    let mut sessions = load_sessions();
    let before = sessions.len();
    sessions.retain(|_, r| r.project_name != project_name);
    if sessions.len() < before {
        let _ = save_sessions(&sessions);
    }
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

    /// Reload the full config from disk, discarding unsaved in-memory state.
    ///
    /// Call this immediately before a mutate-then-save so the mutation is
    /// applied on top of the latest on-disk state. A full reload preserves both
    /// externally-edited fields (e.g. `startup_skills`) AND project/task changes
    /// written concurrently by background ops — for example a task added by
    /// `Config::modify` while a long-running `create_session` is still finishing.
    /// A partial reload would silently drop those concurrent additions on the
    /// next save.
    pub fn reload(&mut self) {
        if let Ok(disk) = Self::load() {
            *self = disk;
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).context("Failed to create config directory")?;
        }
        let content = toml::to_string_pretty(self).context("Failed to serialize config")?;
        fs::write(&path, content).context("Failed to write config file")
    }

    /// Atomic load-modify-save under the global IO lock. Use from background
    /// threads when multiple ops may mutate the config concurrently.
    pub fn modify<F>(f: F) -> Result<Self>
    where
        F: FnOnce(&mut Self),
    {
        let _g = IO_LOCK.lock().unwrap();
        let mut config = Self::load()?;
        f(&mut config);
        config.save()?;
        Ok(config)
    }

    pub fn add_project(&mut self, name: String, path: String) {
        if !self.projects.iter().any(|p| p.path == path) {
            self.projects.push(Project {
                name,
                path,
                tasks: vec![],
                copy_patterns: vec![],
                setup_commands: vec![],
                run_command: None,
            });
        }
    }

    pub fn has_project_at(&self, path: &str) -> bool {
        self.projects.iter().any(|p| p.path == path)
    }

    pub fn add_task(&mut self, project_name: &str, task_name: String, branch: String) -> bool {
        if let Some(project) = self.projects.iter_mut().find(|p| p.name == project_name) {
            if !project.tasks.iter().any(|t| t.name == task_name) {
                project.tasks.push(Task {
                    name: task_name,
                    branch,
                    base_branch: None,
                    archived: false,
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

    /// Set the base branch for a task. Pass `None` to reset to the default ("main").
    pub fn set_task_base_branch(
        &mut self,
        project_name: &str,
        task_name: &str,
        base_branch: Option<String>,
    ) -> bool {
        if let Some(project) = self.projects.iter_mut().find(|p| p.name == project_name) {
            if let Some(task) = project.tasks.iter_mut().find(|t| t.name == task_name) {
                task.base_branch = base_branch
                    .map(|b| b.trim().to_string())
                    .filter(|b| !b.is_empty() && b != "main");
                return true;
            }
        }
        false
    }

    /// The configured run command for a project, trimmed; `None` if unset/empty.
    pub fn project_run_command(&self, project_name: &str) -> Option<&str> {
        self.projects
            .iter()
            .find(|p| p.name == project_name)
            .and_then(|p| p.run_command.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    /// Set (or clear, when empty) the run command for a project. Returns true if
    /// the project was found.
    pub fn set_project_run_command(&mut self, project_name: &str, command: String) -> bool {
        if let Some(project) = self.projects.iter_mut().find(|p| p.name == project_name) {
            let trimmed = command.trim();
            project.run_command = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            };
            return true;
        }
        false
    }

    /// Set the archived flag for a task. Returns true if the task was found.
    pub fn set_task_archived(
        &mut self,
        project_name: &str,
        task_name: &str,
        archived: bool,
    ) -> bool {
        if let Some(project) = self.projects.iter_mut().find(|p| p.name == project_name) {
            if let Some(task) = project.tasks.iter_mut().find(|t| t.name == task_name) {
                task.archived = archived;
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

    /// Find a task by its branch within the project identified by path.
    /// Unlike [`find_task`], this keys on `project_path` + `branch` — use it when
    /// reconciling persisted session records (whose name fields may be stale).
    pub fn find_task_by_branch(&self, project_path: &str, branch: &str) -> Option<&Task> {
        self.projects
            .iter()
            .find(|p| p.path == project_path)?
            .tasks
            .iter()
            .find(|t| t.branch == branch)
    }

    /// Whether a project with the given path still exists in the config.
    pub fn project_exists(&self, project_path: &str) -> bool {
        self.projects.iter().any(|p| p.path == project_path)
    }

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
    fn task_without_optional_fields_defaults() {
        let task: Task = toml::from_str("name = \"t\"\nbranch = \"b\"\n").unwrap();
        assert!(!task.archived);
        assert_eq!(task.base_branch(), "main");
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
    fn find_task_by_branch() {
        let mut cfg = empty_config();
        cfg.add_project("App".into(), "/tmp/app".into());
        cfg.add_task("App", "task-name".into(), "b1".into());
        let task = cfg.find_task_by_branch("/tmp/app", "b1");
        assert!(task.is_some());
        assert_eq!(task.unwrap().name, "task-name");
        // A branch that no longer exists (task deleted) is reported as gone.
        assert!(
            cfg.find_task_by_branch("/tmp/app", "missing-branch")
                .is_none()
        );
        assert!(cfg.find_task_by_branch("/tmp/other", "b1").is_none());
    }

    #[test]
    fn project_exists_by_path() {
        let mut cfg = empty_config();
        assert!(!cfg.project_exists("/tmp/app"));
        cfg.add_project("App".into(), "/tmp/app".into());
        assert!(cfg.project_exists("/tmp/app"));
        assert!(!cfg.project_exists("/tmp/gone"));
    }

    #[test]
    fn remove_project() {
        let mut cfg = empty_config();
        cfg.add_project("App".into(), "/tmp/app".into());
        cfg.remove_project("/tmp/app");
        assert!(cfg.projects.is_empty());
    }

    #[test]
    fn project_run_command_round_trip() {
        let mut cfg = empty_config();
        cfg.add_project("App".into(), "/tmp/app".into());
        assert_eq!(cfg.project_run_command("App"), None);

        assert!(cfg.set_project_run_command("App", "  npm run dev  ".into()));
        // Stored trimmed and read back.
        assert_eq!(cfg.project_run_command("App"), Some("npm run dev"));

        // Blank input clears it.
        assert!(cfg.set_project_run_command("App", "   ".into()));
        assert_eq!(cfg.project_run_command("App"), None);

        // Unknown project is a no-op miss.
        assert!(!cfg.set_project_run_command("Missing", "x".into()));
        assert_eq!(cfg.project_run_command("Missing"), None);
    }

    #[test]
    fn run_command_skipped_when_unset() {
        let mut cfg = empty_config();
        cfg.add_project("App".into(), "/tmp/app".into());
        let s = toml::to_string_pretty(&cfg).unwrap();
        assert!(!s.contains("run_command"));
        cfg.set_project_run_command("App", "make serve".into());
        let s = toml::to_string_pretty(&cfg).unwrap();
        assert!(s.contains("run_command = \"make serve\""));
    }

    #[test]
    fn set_task_archived_round_trip() {
        let mut cfg = empty_config();
        cfg.add_project("App".into(), "/tmp/app".into());
        cfg.add_task("App", "t1".into(), "b1".into());
        assert!(!cfg.projects[0].tasks[0].archived);
        assert!(cfg.set_task_archived("App", "t1", true));
        assert!(cfg.projects[0].tasks[0].archived);
        assert!(cfg.set_task_archived("App", "t1", false));
        assert!(!cfg.projects[0].tasks[0].archived);
        assert!(!cfg.set_task_archived("App", "missing", true));
        assert!(!cfg.set_task_archived("Missing", "t1", true));
    }

    #[test]
    fn task_archived_skipped_when_false() {
        let mut cfg = empty_config();
        cfg.add_project("App".into(), "/tmp/app".into());
        cfg.add_task("App", "t1".into(), "b1".into());
        let s = toml::to_string_pretty(&cfg).unwrap();
        assert!(!s.contains("archived"));
        cfg.set_task_archived("App", "t1", true);
        let s = toml::to_string_pretty(&cfg).unwrap();
        assert!(s.contains("archived = true"));
    }

    #[test]
    fn review_tool_defaults_to_hunk() {
        assert_eq!(Config::default().review_tool, ReviewTool::Hunk);
        // An existing config without the key deserializes to the default.
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.review_tool, ReviewTool::Hunk);
    }

    #[test]
    fn review_tool_parses_and_round_trips() {
        let cfg: Config = toml::from_str("review_tool = \"difit\"\n").unwrap();
        assert_eq!(cfg.review_tool, ReviewTool::Difit);
        let s = toml::to_string_pretty(&cfg).unwrap();
        assert!(s.contains("review_tool = \"difit\""));
        assert_eq!(
            toml::from_str::<Config>(&s).unwrap().review_tool,
            ReviewTool::Difit
        );
    }

    #[test]
    fn review_tool_serializes_before_project_tables() {
        // `review_tool` is a scalar; if emitted after `[[projects]]` it would be
        // invalid TOML and fail to re-parse. Guards the field ordering.
        let mut cfg = empty_config();
        cfg.review_tool = ReviewTool::Hunk;
        cfg.add_project("App".into(), "/tmp/app".into());
        let s = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&s).unwrap();
        assert_eq!(back.review_tool, ReviewTool::Hunk);
        assert_eq!(back.projects.len(), 1);
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
