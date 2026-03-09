# Claude Manager

A minimal TUI (Rust + ratatui) to manage multiple Claude Code sessions across projects via tmux.

## Concepts

### Project
- A git repository registered in the central config file
- Must contain a `.git` folder (required for worktree support)
- Projects are globally visible regardless of where the TUI is launched
- Collapsible in the UI; collapsed by default when empty
- When collapsed, shows summary counts: `[3 tasks, 2 active]`

### Task
- A unit of work within a project, backed by a git branch
- Branch can be auto-generated from the task name (e.g. "Fix Login Bug" -> `fix-login-bug`) or use an existing branch
- New branches are created from `origin/main` (falls back to local `main`)
- Every session must belong to a task
- Stored in the config file under its parent project
- Collapsible in the UI; all tasks collapsed by default
- Shows aggregated diff stats (+/-) against main
- Shows active session count

### Session
- A tmux session containing a Claude Code instance (window 0)
- Only sessions created by this tool are visible/managed
- Tmux naming convention: `cm__<project>__<task>__<session>` (double underscore separator)
- User is prompted for a session name; if left empty, auto-increments
- User is prompted for an initial message which is passed to the `claude` command on launch
- By default, a git worktree is created from the task branch for each session
  - Worktree branch: `<task-branch>-<session-name>`
  - Worktree location: `~/.claude-manager/worktrees/<project>/<task>-<session>`
  - Git-ignored files (`.env`, build caches, etc.) are copied from the original project via `rsync` in a background thread
- Claude Code is launched with `--dangerously-skip-permissions` in the worktree directory
- Worktree and session branch cleanup is handled on session deletion
- Hotkey variant (`N`): without worktree (runs in project directory)
- Shows diff stats (+/-) for the session's changes against the task branch

### Shared Task Context
- Each task has a shared context file at `~/.claude-manager/tasks/<project>/<branch>/TASK_CONTEXT.md`
- Automatically injected into every Claude Code prompt via `UserPromptSubmit` hook
- Automatically updated when a Claude session stops via `Stop` hook (uses `claude -p` to summarize)
- PR URL is stored alongside the context and included in updates
- Editable via nvim in the context preview tab

### Config
- Central config file: `~/.claude-manager/config.toml`
- Worktrees stored under: `~/.claude-manager/worktrees/`
- Stores the list of registered projects with their tasks
- Projects support configurable `copy_patterns` for files to sync to worktrees (in addition to `.gitignore`d files)
- Project display name is prompted when adding; defaults to directory name if left empty

## Features

### Session Management
- Three-level hierarchy: Project > Task > Session (all collapsible with `Space`)
- Create a new task (`t`) — prompts for name then branch (existing or new)
- Create a new session (`n` with worktree, `N` without)
- Attach to a session (`Enter`) to interact with Claude Code
- Detach via standard tmux keybind (`Ctrl-b d`) to return to the TUI
- Delete sessions, tasks, and projects (`d`) with confirmation
- Rename projects, tasks, and sessions (`R`)

### Preview Panel
- Tabbed preview panel to the right of the list with tabs:
  - **context** — task context file rendered via nvim in a tmux session (default tab for tasks)
  - **agent** — live Claude Code output (ANSI-rendered via `tmux capture-pane`)
  - **diff** — session's changes with styled diff, file separator headers, sticky stats header, and line number gutter
  - **term1, term2, ...** — terminal window previews
- Scrollable with `J`/`K` (3-line increments; sends `C-y`/`C-e` to nvim on context tab)
- Tab switching with `Tab`
- `Enter` on context tab attaches to the nvim session for editing

### Terminal Windows
- Create up to 4 persistent terminal windows per session (`c`)
- Terminals open in the session's working directory as additional tmux windows
- Kill the currently viewed terminal (`x`)
- Attach to a terminal by pressing `Enter` while on its tab
- Terminal count shown as tabs in the preview panel

### Git Operations
- **Merge** (`m`) — merge a session's worktree branch into the task branch (ff-only, falls back to merge commit). Prompts for commit message if worktree has uncommitted changes.
- **Update** (`u`) — on a task: fetch and rebase onto `origin/main`. On a session: rebase onto task branch.
- **Push** (`P`) — push task branch to origin with `--force-with-lease`
- **Checkout** (`b`) — checkout the task branch in the main worktree

### GitHub Integration
- PR detection via `gh pr view` (checked every ~10 seconds in background)
- PR icon (Nerd Font) shown on task lines that have an open PR
- PR URL shown in status bar when a task with a PR is selected
- Open PR in browser (`o`), or create a new PR if none exists (with confirmation)

### Session Status Detection
- Background worker polls every 500ms with colored indicators:
  - **Spinning** (yellow) — Claude process alive, content actively changing
  - **● green** — Claude process alive, content stable, waiting for input
  - **! magenta** — permission prompt detected
  - **● red** — Claude process exited / session finished
- Detection via tmux `pane_pid` + child process check + content hash diffing + `capture-pane` analysis

### Background Operations
- Long-running operations (create session, delete, merge, update, push, create PR) run in background threads
- Loading spinner shown in status bar during operations
- UI remains responsive; only `q` (quit) is accepted during loading

## Visual Design
- Tree-drawing characters (`├─`, `└─`, `│`) for hierarchy
- Selection indicator (`▸`) and collapse chevrons (`▶`/`▼`)
- Rounded borders on preview panel
- Subtle color palette: cyan accent, muted grays for secondary info, distinct colors for tasks (yellow), sessions (green), status indicators
- Compact help bar with highlighted keys

## Keyboard Shortcuts

| Key | Action |
|-----|--------|
| `t` | Create task |
| `n` / `N` | Create session (with / without worktree) |
| `Enter` | Attach to session or terminal |
| `Space` | Collapse/expand |
| `d` | Delete |
| `R` | Rename |
| `m` | Merge session into task branch |
| `u` | Update (rebase task on main / session on task) |
| `P` | Push task branch |
| `o` | Open/create PR |
| `b` | Checkout task branch |
| `c` | Create terminal |
| `x` | Kill terminal |
| `Tab` | Switch preview tab |
| `J` / `K` | Scroll preview |
| `a` | Add project |
| `q` | Quit |

## Tech Stack

- Rust with ratatui for the TUI
- tmux for session management
- `ansi-to-tui` for rendering terminal output with ANSI colors
- `gh` CLI for GitHub PR integration

## Non-Functional Requirements

- Non-blocking UI: long operations run in background threads
- Background worker for status polling, diff computation, terminal counts, and PR detection
- Fast and snappy
- Clean dark aesthetic with color accents
