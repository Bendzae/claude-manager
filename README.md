# Claude Manager

A terminal UI (TUI) for managing multiple Claude Code sessions organized by projects and tasks. Built with Rust using [ratatui](https://github.com/ratatui/ratatui).

Claude Manager uses tmux to run Claude Code sessions in the background, letting you organize them into projects and tasks, monitor their status, review diffs, and attach/detach freely.
<img width="800" height="430" alt="showcase-gif" src="https://github.com/user-attachments/assets/63b6a5b1-b821-44b3-b764-481ee40fd33f" />

## Prerequisites

- **Cargo** (Rust 1.85+) — [install via rustup](https://rustup.rs/)
- **tmux** — `brew install tmux` (macOS) or `apt install tmux` (Linux)
- **Claude Code CLI** (`claude`) — must be installed and available in your PATH
- **git** — for worktree and branch management
- **gh** (optional) — GitHub CLI, for PR creation features
- **difit** (optional) — the default diff review tool (`r`). Installed globally (`npm i -g difit`) it launches instantly; otherwise it runs via `npx` automatically (Node.js required), fetched on first use.
- **hunk** (optional) — an alternative terminal-based diff review tool ([modem-dev/hunk](https://github.com/modem-dev/hunk)), used when `review_tool = "hunk"` is set in config. Installed globally (`npm i -g hunkdiff`) it launches instantly; otherwise it runs via `npx hunkdiff` automatically (Node.js required). Unlike difit it runs in your terminal (suspending the TUI); review comments are still forwarded back to the agent.

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

> **Tip:** When you attach to a session (with `Enter`), you're inside a tmux session. To detach and get back to claude-manager, use your tmux detach binding — with the default prefix that's `Ctrl-b d`. The session keeps running in the background.

### Concepts

- **Project** — A git repository you want to manage Claude sessions for. Added by its filesystem path.
- **Task** — A unit of work within a project, tied to a git branch. Each task can have multiple Claude sessions.
- **Session** — A Claude Code instance running in a tmux session. Sessions can be created with an optional initial prompt, and (by default) in their own git worktree so they don't collide.
- **Adhoc session** — A project-scoped session that runs Claude directly in the project directory on whatever branch is checked out, with no task or worktree. Created with `A` from a project's context menu and grouped under the project. Handy for quick, throwaway work that doesn't warrant a task.

### Keybindings

All keybindings are customizable via `~/.claude-manager/keybindings.toml`. The tables below show the defaults. See [`keybindings.example.toml`](keybindings.example.toml) for a full template.

#### Global

| Key | Action | Config key |
|-----|--------|------------|
| `j/k` or `Up/Down` | Navigate | `move_down` / `move_up` |
| `Enter` | Attach to session, or collapse/expand project or task | — |
| `Space` | Collapse/expand project or task | `toggle_collapse` |
| `a` | Open context menu | `context_menu` |
| `p` | Add project | `add_project` |
| `/` | Filter projects/tasks/sessions | `search` |
| `Z` | Toggle archived view | `toggle_archive_view` |
| `t` | Cycle color theme | `cycle_theme` |
| `q` | Quit | `quit` |

`t` cycles through the built-in themes: **default**, **catppuccin**, **tokyo-night**, and **dracula**.

#### Context Menu (press `a` to open)

The context menu shows actions relevant to the selected item. Press the hotkey character to execute directly, or navigate with `j/k` and confirm with `Enter`. Context menu keys are configured under the `[context_menu_keys]` section.

**Project actions:**

| Key | Action | Config key |
|-----|--------|------------|
| `t` | Add task | `add_task` |
| `A` | New adhoc session | `new_adhoc_session` |
| `x` | Run the project's configured run command | `run` |
| `b` | Checkout branch (fuzzy-search the branch list) | `checkout` |
| `f` | Fetch & pull all branches (`git fetch --all --prune` + ff-only pull) | `fetch_pull` |
| `y` | Copy project path to clipboard | `copy_path` |
| `R` | Rename | `rename` |
| `d` | Delete | `delete` |

The branch checkout picker opens a fuzzy finder over the project's local and remote branches — type to filter, `↑/↓` to navigate, `Enter` to check out (a remote-only branch is checked out as a new local tracking branch).

**Run command** (`x`, available on projects, tasks, and sessions) launches a per-project command in a dedicated tmux session and attaches to it. The first time you run it for a project you're prompted for the command (e.g. `npm run dev`); it's saved to that project's `run_command` config and reused everywhere afterwards. It runs in the selected item's working directory: the session's worktree for a session, the task's first session worktree (or the project dir) for a task, and the project dir for a project.

Each item shows a green run indicator while it has a live run session — an animated spinner (`⠙`) while the command is still executing, and a static `▷` once it finishes (the shell stays open so you can read its output). Detaching with `Ctrl-b d` leaves the command running in the background.

Pressing Run (`x`) again on an item that **already has a live run session** opens a small menu instead of relaunching:

| Key | Action |
|-----|--------|
| `a` | **Attach** — re-attach to the running session |
| `r` | **Restart** — kill and relaunch the run command |
| `k` | **Kill** — stop the run session |

Run sessions are independent per item, so running on a different item starts a separate session. (Outside the TUI you can also list them with `tmux ls` and attach via `tmux attach -t cmrun-<name>`.)

**Task actions:**

| Key | Action | Config key |
|-----|--------|------------|
| `n` | New session (with worktree) | `new_session` |
| `N` | New session (without worktree) | `new_session_no_worktree` |
| `r` | Review branch-vs-base diff (difit or hunk) | `review` |
| `x` | Run the project's configured run command | `run` |
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
| `r` | Review uncommitted changes (difit or hunk) | `review` |
| `m` | Merge into task branch | `merge` |
| `u` | Update/rebase onto task branch | `update` |
| `t` | Open/attach a terminal in the worktree | `terminal` |
| `x` | Run the project's configured run command | `run` |
| `y` | Copy worktree path to clipboard | `copy_path` |
| `R` | Rename | `rename` |
| `d` | Delete | `delete` |

The **Review** action (`r`) launches the configured diff review tool on the relevant diff — branch-vs-base for a task, uncommitted changes for a session. Choose the tool with `review_tool` in `~/.claude-manager/config.toml` (`"difit"`, the default, or `"hunk"`):

- **difit** (default) opens a browser-based viewer in the background, so the TUI stays interactive. Any comments you leave are captured on exit and forwarded back to the agent's Claude session as a new prompt, so you can review a diff and hand the feedback straight to the agent.
- **hunk** ([modem-dev/hunk](https://github.com/modem-dev/hunk)) opens a terminal viewer in the foreground, suspending the TUI until you exit. Comments you leave are polled from hunk's live review session while it runs and forwarded to the agent on exit, the same as difit. (A comment added in the last fraction of a second before quitting may be missed, since hunk's session is gone once it closes.)

```toml
# ~/.claude-manager/config.toml
review_tool = "hunk"   # or "difit" (default)
```

### Session Status Indicators

Sessions display their current status:
- **Running** — Claude is actively working
- **Waiting for input** — Claude is waiting for your response
- **Waiting for permission** — Claude needs tool approval
- **Finished** — Claude has completed its work

### Worktrees

When creating a session with `n` (via the context menu on a task), Claude Manager creates a git worktree so each session works on an isolated copy of the codebase. Use `N` to skip worktree creation and work directly in the project directory.

You can configure file patterns to copy into new worktrees (e.g. `.env` files) by adding `copy_patterns` to your project config. To run commands inside each freshly-created worktree (installing dependencies, configuring git hooks, etc.), add `setup_commands` (a single string or a list):

```toml
[[projects]]
name = "My App"
path = "/path/to/my-app"
copy_patterns = [".env", ".env.local"]
setup_commands = ["npm install", "./scripts/configure-hooks.sh"]
```

### Configuration

The config file at `~/.claude-manager/config.toml` is managed automatically through the TUI, but can also be edited manually:

```toml
# Global: skills/slash-commands run in every new session before the initial
# prompt (a single string or a list). Useful for priming context.
startup_skills = ["/prime"]

[[projects]]
name = "My App"
path = "/home/user/my-app"
copy_patterns = [".env"]               # files copied into each new worktree
setup_commands = ["npm install"]       # commands run in each new worktree
run_command = "npm run dev"            # command launched by the Run action (x)

[[projects.tasks]]
name = "fix-auth-bug"
branch = "fix/auth-bug"
base_branch = "develop"                # rebase/diff target (defaults to "main")

[[projects.tasks]]
name = "add-dark-mode"
branch = "feature/dark-mode"
stacked = true                         # publish commits as a stack of PRs
```

Most of these fields are set for you through the TUI (`run_command` on first Run, `base_branch` via `B`, `stacked` via `s`), so manual editing is rarely necessary. A running TUI picks up external edits on its next idle refresh.

#### Custom Keybindings

Create `~/.claude-manager/keybindings.toml` to override any default keybinding. Only the keys you specify are overridden; everything else keeps its default. Example:

```toml
quit = "Q"
context_menu = "o"

[context_menu_keys]
delete = "x"
```

## Mobile web UI (`serve`)

`claude-manager serve` starts an HTTP server with a phone-friendly web UI for managing sessions remotely — view session status, read live output, send messages and keys (permission prompts included), create tasks/sessions, and kill sessions.

```sh
claude-manager serve                          # default 127.0.0.1:7878
claude-manager serve --bind 0.0.0.0:7878     # bind another interface
```

The server has no authentication — keep it on localhost and expose it through your tailnet:

```sh
tailscale serve --bg 7878
```

This gives you a valid-HTTPS URL reachable only from your own devices. Open it on your phone and use "Add to Home Screen" to install it as an app.

## Claude Code plugin

The repo ships a Claude Code plugin (`claude-manager-plugin/`) with skills that let an agent running inside a session drive its own Claude Manager task without leaving the worktree:

- **`commit-push-task`** — commit changes on the worktree branch, fast-forward merge into the task branch, and push the task branch.
- **`stacked-pr`** — work on a task whose changes ship as a stack of dependent PRs (one PR per commit, via `git spr`), including the commit-shaping workflow. Pairs with the `set-stacked` / `stack-publish` / `stack-sync` CLI subcommands.

Add the plugin to Claude Code and the skills become available as `/commit-push-task` and `/stacked-pr` inside any session.

## Development

### Git hooks

The repo ships a pre-commit hook (`.githooks/pre-commit`) that runs `rustfmt`
on staged Rust files and re-stages them, so commits always match the
`cargo fmt --check` step in CI. Enable it once after cloning:

```sh
git config core.hooksPath .githooks
```
