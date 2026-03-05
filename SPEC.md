# Claude Manager

A minimal TUI (Rust + ratatui) to manage multiple Claude Code sessions across projects.

## Concepts

### Project
- A git repository registered in the central config file
- Must contain a `.git` folder (required for worktree support)
- Projects are globally visible regardless of where the TUI is launched
- When opening the TUI in an unregistered project directory, prompt the user to add it

### Session
- A tmux session containing a Claude Code instance, optionally with additional terminal panes
- Only sessions created by this tool are visible/managed (prefixed `cm-`)
- Tmux naming convention: `cm-<project>-<session-name>`
- User is prompted for a session name; if left empty, auto-generate one (e.g. incrementing number)
- Claude Code is launched with `--worktree --dangerously-skip-permissions` by default
- Worktree cleanup is handled by Claude Code itself on termination

### Config
- Central config file (e.g. `~/.config/claude-manager/config.toml`)
- Stores the list of registered projects (display name + path)
- Project display name is prompted when adding; defaults to directory name if left empty
- Directory path is shown in a muted font alongside the display name in the UI

## Features (MVP)

- List all registered projects with their sessions nested underneath
- Create a new session for a project (opens a tmux session with Claude Code)
  - Default: with `--worktree` flag
  - Hotkey variant: without `--worktree` flag
- Attach to an existing session (press Enter) to interact with it
- Detach from a session using standard tmux keybind (`Ctrl-b d`) to return to the TUI
- Delete a session (kills the tmux session)
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
