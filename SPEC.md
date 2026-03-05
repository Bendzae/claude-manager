# Claude Manager

A minimal TUI (Rust + ratatui) to manage multiple Claude Code sessions across projects.

## Concepts

### Project
- A git repository registered in the central config file
- Must contain a `.git` folder (required for worktree support)
- Projects are globally visible regardless of where the TUI is launched
- When opening the TUI in an unregistered project directory, prompt the user to add it
- Collapsible in the UI

### Task
- A unit of work within a project, backed by a git branch
- Branch is auto-generated from the task name (e.g. "Fix Login Bug" -> `fix-login-bug`)
- Branch is created from `main` after pulling latest
- Every session must belong to a task
- Stored in the config file under its parent project
- Collapsible in the UI

### Session
- A tmux session containing a Claude Code instance
- Only sessions created by this tool are visible/managed
- Tmux naming convention: `cm__<project>__<task>__<session>` (double underscore separator)
- User is prompted for a session name; if left empty, auto-generate one (e.g. incrementing number)
- By default, a git worktree is created from the task branch for each session
  - Worktree branch: `<task-branch>-<session-name>`
  - Worktree location: `~/.local/share/claude-manager/worktrees/<project>/<task>-<session>`
- Claude Code is launched with `--dangerously-skip-permissions` in the worktree directory
- Worktree cleanup is handled by the manager on session deletion
- Hotkey variant: without worktree (runs in project directory)

### Config
- Central config file (`~/.config/claude-manager/config.toml`)
- Stores the list of registered projects with their tasks
- Project display name is prompted when adding; defaults to directory name if left empty
- Directory path is shown in a muted font alongside the display name in the UI

## Features (MVP)

- Three-level hierarchy: Project > Task > Session (all collapsible)
- Create a new task for a project (creates a git branch from latest main)
- Create a new session for a task (opens a tmux session with Claude Code)
  - Default: with git worktree based on task branch
  - Hotkey variant: without worktree
- Attach to an existing session (press Enter) to interact with it
- Detach from a session using standard tmux keybind (`Ctrl-b d`) to return to the TUI
- Delete a session (kills the tmux session, cleans up worktree)
- Delete a task (only if no active sessions)
- Rename projects, tasks, and sessions
- Prompt to add current directory as a project if not yet registered
- Preview of the hovered session to the right (via `tmux capture-pane`)

## Features (Later)

- Session status detection (running, finished, waiting for input)
- Hotkey to open a plain terminal in the worktree folder (persistent, for long-running commands)

## Tech Stack

- Rust with ratatui for the TUI
- tmux for session management

## Non-Functional Requirements

- Non-blocking UI: never block on tmux operations
- Fast and snappy
- Clean dark aesthetic with color accents
