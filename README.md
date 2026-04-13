# Claude Manager

A terminal UI (TUI) for managing multiple Claude Code sessions organized by projects and tasks. Built with Rust using [ratatui](https://github.com/ratatui/ratatui).

Claude Manager uses tmux to run Claude Code sessions in the background, letting you organize them into projects and tasks, monitor their status, preview diffs, and attach/detach freely.

## Prerequisites

- **Cargo** (Rust 1.85+) — [install via rustup](https://rustup.rs/)
- **tmux** — `brew install tmux` (macOS) or `apt install tmux` (Linux)
- **Claude Code CLI** (`claude`) — must be installed and available in your PATH
- **git** — for worktree and branch management
- **gh** (optional) — GitHub CLI, for PR creation and detection features

## Installation

```bash
cargo install claude-manager
```

Or build from source:

```bash
git clone git@github.com:Bendzae/claude-manager.git
cd claude-manager
cargo install --path .
```

## Usage

```bash
claude-manager
```

Launch from any directory. Configuration is stored in `~/.claude-manager/config.toml`.

### Concepts

- **Project** — A git repository you want to manage Claude sessions for. Added by its filesystem path.
- **Task** — A unit of work within a project, tied to a git branch. Each task can have multiple Claude sessions.
- **Session** — A Claude Code instance running in a tmux session. Sessions can be created with an optional initial prompt.

### UI Overview

The main view shows a tree of projects, tasks, and sessions with a preview panel on the right.

**Tree view indicators:**
- **Projects** show the current git branch and path, with a summary badge when collapsed (e.g. `[3 tasks, 2 active]`)
- **Tasks** show diff stats against main (`+42, -15`), a PR icon when a PR is open, and an active session count when collapsed
- **Sessions** show their status (see below), a merge checkmark (`✓`) when fully merged into the task branch, and diff stats for uncommitted/committed changes

**Preview panel tabs:**
- `agent` — live Claude Code output from the selected session
- `diff` — git diff of session changes against the task branch
- `context` — shared task context file (when a task is selected)
- `term1`, `term2`, ... — additional terminal windows attached to the session

Use `Tab` to cycle preview tabs and `J`/`K` to scroll.

### Session Status Indicators

Sessions display their current status with a visual indicator:
- **Running** — animated spinner, Claude is actively working
- **Waiting for input** — green dot, Claude is waiting for your response
- **Waiting for permission** — magenta `!`, Claude needs tool approval
- **Finished** — red dot, Claude has completed its work

### Keybindings

All keybindings are customizable via `~/.claude-manager/keybindings.toml`. The tables below show the defaults. See [`keybindings.example.toml`](keybindings.example.toml) for a full template.

#### Global

| Key | Action | Config key |
|-----|--------|------------|
| `j/k` or `Up/Down` | Navigate | `move_down` / `move_up` |
| `Enter` | Attach to session / expand item | — |
| `Space` | Collapse/expand project or task | `toggle_collapse` |
| `a` | Open context menu | `context_menu` |
| `p` | Add project | `add_project` |
| `Tab` | Cycle preview tabs | — |
| `J/K` | Scroll preview pane | `scroll_preview_down` / `scroll_preview_up` |
| `q` | Quit | `quit` |

#### Context Menu (press `a` to open)

The context menu shows actions relevant to the selected item. Press the hotkey character to execute directly, or navigate with `j/k` and confirm with `Enter`. Context menu keys are configured under the `[context_menu_keys]` section.

**Project actions:**

| Key | Action | Config key |
|-----|--------|------------|
| `t` | Add task | `add_task` |
| `R` | Rename | `rename` |
| `d` | Delete | `delete` |

**Task actions:**

| Key | Action | Config key |
|-----|--------|------------|
| `n` | New session (with worktree) | `new_session` |
| `N` | New session (without worktree) | `new_session_no_worktree` |
| `x` | Toggle auto-context | `toggle_auto_context` |
| `u` | Update/rebase branch onto main | `update` |
| `P` | Push branch | `push` |
| `b` | Checkout branch in project dir | `checkout` |
| `o` | Open/create PR (as draft) | `open_pr` |
| `R` | Rename | `rename` |
| `d` | Delete | `delete` |

**Session actions:**

| Key | Action | Config key |
|-----|--------|------------|
| `m` | Merge into task branch | `merge` |
| `u` | Update/rebase onto task branch | `update` |
| `c` | Create terminal window | `create_terminal` |
| `k` | Kill terminal window | `kill_terminal` |
| `R` | Rename | `rename` |
| `d` | Delete | `delete` |

### Worktrees

When creating a session with `n` (via the context menu on a task), Claude Manager creates a git worktree so each session works on an isolated copy of the codebase. Use `N` to skip worktree creation and work directly in the project directory. Worktree-less sessions are marked with a `⌂` icon in the tree view.

You can configure file patterns to copy into new worktrees (e.g. `.env` files) and setup commands to run after worktree creation:

```toml
[[projects]]
name = "My App"
path = "/path/to/my-app"
copy_patterns = [".env", ".env.local"]
setup_commands = ["./gradlew configureGitHooks"]
```

The `.claude/` directory is always copied automatically.

### Task Auto-Context

Tasks can maintain a shared context file that keeps all sessions on the same page. Toggle it per-task with `x` in the context menu.

When enabled:
- A shared `TASK_CONTEXT.md` file is maintained at `~/.claude-manager/tasks/[project]/[branch]/`
- Context is automatically regenerated when a session finishes
- Sessions are informed about the shared context file and can update it on demand
- A document icon appears next to the task name in the tree view

This is especially useful when running multiple sessions on the same task — each session can read the shared context to understand what other sessions have done.

### Startup Skills

You can configure Claude Code skills to run automatically before the initial prompt when creating new sessions. This is useful for loading project context or setting communication modes:

```toml
startup_skills = ["/prime", "/caveman ultra"]
```

Skills are executed sequentially before the user's task prompt is sent.

### PR Integration

If `gh` (GitHub CLI) is installed, Claude Manager integrates with GitHub PRs:
- **Create PRs** from the task context menu (`o`) — PRs are created as drafts by default
- **PR detection** — open PRs for task branches are automatically detected and shown with a PR icon in the tree view

### Session Persistence

Session metadata is saved to `~/.claude-manager/sessions.json`. If tmux sessions are lost (e.g. after a system restart), Claude Manager will automatically recreate them on startup.

### Configuration

The config file at `~/.claude-manager/config.toml` is managed automatically through the TUI, but can also be edited manually:

```toml
startup_skills = ["/prime"]

[[projects]]
name = "My App"
path = "/home/user/my-app"
copy_patterns = [".env"]
setup_commands = ["npm install"]

[[projects.tasks]]
name = "fix-auth-bug"
branch = "fix/auth-bug"

[[projects.tasks]]
name = "add-dark-mode"
branch = "feature/dark-mode"
```

#### Custom Keybindings

Create `~/.claude-manager/keybindings.toml` to override any default keybinding. Only the keys you specify are overridden; everything else keeps its default. Example:

```toml
quit = "Q"
context_menu = "o"

[context_menu_keys]
delete = "x"
```
