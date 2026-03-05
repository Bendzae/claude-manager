# Claude Manager

A minimal TUI to handle multiple claude code sessions across projects.

## Features (POC)

- Opens to a list of projects that have nested under them the current claude sessions
- Open new session for the project via hotkey, creates a new tmux session and opens a claude instance with the --worktree and --claude --worktree --dangerously-skip-permissions by default
- There should be a separate hotkey to open it without a the worktree flag
- Pressing enter on the session should attach to the tmux session allowing to interact with it
- There needs to be a hotkey to detach from it again and go back to the tui
- Abitlity to delete a session
- Show status of session in overview (running, finished, waiting for input, maybe more)


## Features (Later)

- Preview of the hovered session to the right
- Hotkey to open an empty terminal in the worktree folder (persistent) could be used to run long running comands and later get back to it

## Other Requirements

- Avoid blocking ui while waiting for tmux operations
- Should be as fast and snappy as possible

## Visual
- Clean dark aesthetic with color accents
