# Claude Manager

A terminal UI (TUI) for managing multiple Claude Code sessions organized by projects and tasks. Built with Rust using [ratatui](https://github.com/ratatui/ratatui).

Claude Manager uses tmux to run Claude Code sessions in the background, letting you organize them into projects and tasks, monitor their status, review diffs, and attach/detach freely.

## Prerequisites

- **Cargo** (Rust 1.85+) — [install via rustup](https://rustup.rs/)
- **tmux** — `brew install tmux` (macOS) or `apt install tmux` (Linux)
- **Claude Code CLI** (`claude`) — must be installed and available in your PATH
- **git** — for worktree and branch management
- **gh** (optional) — GitHub CLI, for PR creation features

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

### Keybindings

All keybindings are customizable via `~/.claude-manager/keybindings.toml`. The tables below show the defaults. See [`keybindings.example.toml`](keybindings.example.toml) for a full template.

#### Global

| Key | Action | Config key |
|-----|--------|------------|
| `j/k` or `Up/Down` | Navigate | `move_down` / `move_up` |
| `Enter` | Attach to session, or edit a task's context file in `$EDITOR` | — |
| `Space` | Collapse/expand project or task | `toggle_collapse` |
| `a` | Open context menu | `context_menu` |
| `p` | Add project | `add_project` |
| `/` | Filter projects/tasks/sessions | `search` |
| `Z` | Toggle archived view | `toggle_archive_view` |
| `t` | Cycle color theme | `cycle_theme` |
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
| `r` | Review branch-vs-base diff in difit | `review` |
| `u` | Update/rebase branch onto main | `update` |
| `B` | Set base branch | `set_base_branch` |
| `P` | Push branch | `push` |
| `b` | Checkout branch in project dir | `checkout` |
| `o` | Open/create PR (publishes the stack when stacked) | `open_pr` |
| `s` | Toggle stacked-PR mode | `toggle_stacked` |
| `A` | Archive | `archive` |
| `R` | Rename | `rename` |
| `d` | Delete | `delete` |

In **stacked-PR mode** a task publishes one PR per commit via [`git spr`](https://github.com/ejoffe/spr) instead of a single PR. Enable it with `s`, then `o` to publish the stack and `u` to refresh it after edits. The task row shows a `⑆ N PRs` badge and lists each PR (top of stack first) beneath it. Requires `git spr` installed and GitHub auth (`gh auth login`). See the `stacked-pr` agent skill for the commit-shaping workflow.

Stacked mode can also be driven from outside the TUI (the `stacked-pr` skill uses these so an agent can manage the stack for its own task — all operate on the task branch):

```bash
claude-manager set-stacked   <project-path> <branch> [on|off]   # toggle the flag (default: on)
claude-manager stack-publish <project-path> <branch>            # git spr update — create/refresh PRs
claude-manager stack-sync    <project-path> <branch>            # git spr sync — reconcile after merges
```

A running TUI picks up config/flag changes on its next idle refresh, and the published stack on its next poll.

**Session actions:**

| Key | Action | Config key |
|-----|--------|------------|
| `r` | Review uncommitted changes in difit | `review` |
| `m` | Merge into task branch | `merge` |
| `u` | Update/rebase onto task branch | `update` |
| `t` | Open/attach a terminal in the worktree | `terminal` |
| `y` | Copy worktree path to clipboard | `copy_path` |
| `R` | Rename | `rename` |
| `d` | Delete | `delete` |

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

#### Custom Keybindings

Create `~/.claude-manager/keybindings.toml` to override any default keybinding. Only the keys you specify are overridden; everything else keeps its default. Example:

```toml
quit = "Q"
context_menu = "o"

[context_menu_keys]
delete = "x"
```

## Development

### Git hooks

The repo ships a pre-commit hook (`.githooks/pre-commit`) that runs `rustfmt`
on staged Rust files and re-stages them, so commits always match the
`cargo fmt --check` step in CI. Enable it once after cloning:

```sh
git config core.hooksPath .githooks
```
