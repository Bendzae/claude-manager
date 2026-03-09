# Claude Manager

A terminal UI (TUI) for managing multiple Claude Code sessions organized by projects and tasks. Built with Rust using [ratatui](https://github.com/ratatui/ratatui).

Claude Manager uses tmux to run Claude Code sessions in the background, letting you organize them into projects and tasks, monitor their status, preview diffs, and attach/detach freely.

## Prerequisites

- **Rust** (1.85+) — [install via rustup](https://rustup.rs/)
- **tmux** — `brew install tmux` (macOS) or `apt install tmux` (Linux)
- **Claude Code CLI** (`claude`) — must be installed and available in your PATH
- **git** — for worktree and branch management
- **gh** (optional) — GitHub CLI, for PR creation features

## Installation

```bash
git clone git@github.com:Bendzae/claude-manager.git
cd claude-manager
cargo install --path .
```

This installs the `claude-manager` binary to `~/.cargo/bin/`.

## Usage

```bash
claude-manager
```

Launch from any directory. Configuration is stored in `~/.claude-manager/config.toml`.

### Concepts

- **Project** — A git repository you want to manage Claude sessions for. Added by its filesystem path.
- **Task** — A unit of work within a project, tied to a git branch. Each task can have multiple Claude sessions.
- **Session** — A Claude Code instance running in a tmux session. Sessions can be created with an optional initial prompt.

### Keybindings

#### Global

| Key | Action |
|-----|--------|
| `j/k` or `Up/Down` | Navigate |
| `Enter` | Attach to session / expand item |
| `Space` | Collapse/expand project or task |
| `a` | Open context menu (actions for selected item) |
| `p` | Add project |
| `Tab` | Toggle preview mode (diff/context) |
| `J/K` | Scroll preview pane |
| `q` | Quit |

#### Context Menu (press `a` to open)

The context menu shows actions relevant to the selected item. Press the hotkey character to execute directly, or navigate with `j/k` and confirm with `Enter`.

**Project actions:**

| Key | Action |
|-----|--------|
| `t` | Add task |
| `R` | Rename |
| `d` | Delete |

**Task actions:**

| Key | Action |
|-----|--------|
| `n` | New session (with worktree) |
| `N` | New session (without worktree) |
| `u` | Update/rebase branch onto main |
| `P` | Push branch |
| `b` | Checkout branch in project dir |
| `o` | Open/create PR |
| `R` | Rename |
| `d` | Delete |

**Session actions:**

| Key | Action |
|-----|--------|
| `m` | Merge into task branch |
| `u` | Update/rebase onto task branch |
| `c` | Create terminal window |
| `k` | Kill terminal window |
| `R` | Rename |
| `d` | Delete |

### Session Status Indicators

Sessions display their current status:
- **Running** — Claude is actively working
- **Waiting for input** — Claude is waiting for your response
- **Waiting for permission** — Claude needs tool approval
- **Finished** — Claude has completed its work

### Worktrees

When creating a session with `n` (via the context menu on a task), Claude Manager creates a git worktree so each session works on an isolated copy of the codebase. Use `N` to skip worktree creation and work directly in the project directory.

You can configure file patterns to copy into new worktrees (e.g. `.env` files) by adding `copy_patterns` to your project config:

```toml
[[projects]]
name = "My App"
path = "/path/to/my-app"
copy_patterns = [".env", ".env.local"]
```

### Configuration

The config file at `~/.claude-manager/config.toml` is managed automatically through the TUI, but can also be edited manually:

```toml
[[projects]]
name = "My App"
path = "/home/user/my-app"
copy_patterns = [".env"]

[[projects.tasks]]
name = "fix-auth-bug"
branch = "fix/auth-bug"

[[projects.tasks]]
name = "add-dark-mode"
branch = "feature/dark-mode"
```
