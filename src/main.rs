mod app;
mod config;
mod theme;
mod tmux;
mod ui;
mod worker;

use std::io;
use std::time::Duration;

use anyhow::Result;
use crossterm::ExecutableCommand;
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use app::{App, InputMode};
use config::Config;

/// Handle non-TUI CLI invocations. Returns `Some(result)` when an argument was
/// recognized (the process should exit), or `None` to fall through to the TUI.
fn run_cli(args: &[String]) -> Option<Result<()>> {
    match args.first().map(String::as_str) {
        // `claude-manager set-stacked <project-path> <branch> [on|off]`
        // Used by the `stacked-pr` skill so an agent can enable stacked-PR mode
        // for its task without going through the TUI. The running TUI picks up
        // the change on its next idle config reload.
        Some("set-stacked") => Some(cmd_set_stacked(&args[1..])),
        // `claude-manager stack-publish <project-path> <branch>` — run `git spr update`
        // on the task branch (publish/refresh the stack). `stack-sync` runs `git spr sync`
        // (reconcile after merges / trunk moves). Both mirror the TUI's task actions so the
        // stacked-pr skill can drive them from a worktree.
        Some("stack-publish") => Some(cmd_stack(&args[1..], false)),
        Some("stack-sync") => Some(cmd_stack(&args[1..], true)),
        Some("--help" | "-h" | "help") => {
            println!(
                "claude-manager — TUI for managing Claude Code sessions\n\n\
                 Usage:\n  \
                 claude-manager                                  launch the TUI\n  \
                 claude-manager set-stacked <project-path> <branch> [on|off]\n  \
                 \t\tenable/disable stacked-PR mode for a task (default: on)\n  \
                 claude-manager stack-publish <project-path> <branch>\n  \
                 \t\tpublish/refresh the stack (git spr update) on the task branch\n  \
                 claude-manager stack-sync <project-path> <branch>\n  \
                 \t\treconcile the stack after merges (git spr sync)"
            );
            Some(Ok(()))
        }
        _ => None,
    }
}

fn cmd_stack(args: &[String], sync: bool) -> Result<()> {
    let verb = if sync { "stack-sync" } else { "stack-publish" };
    let project_path = args
        .first()
        .ok_or_else(|| anyhow::anyhow!("usage: {verb} <project-path> <branch>"))?;
    let branch = args
        .get(1)
        .ok_or_else(|| anyhow::anyhow!("usage: {verb} <project-path> <branch>"))?;

    // Resolve the project's display name (needed for the stack cache path).
    let cfg = Config::load()?;
    let project_name = cfg
        .projects
        .iter()
        .find(|p| p.path == *project_path)
        .map(|p| p.name.clone())
        .ok_or_else(|| anyhow::anyhow!("No registered project at path '{project_path}'"))?;

    // Publishing implies stacked mode — keep the flag in sync so the TUI routes correctly.
    let _ = Config::modify(|c| {
        c.set_task_stacked_by_branch(project_path, branch, true);
    });

    let prs = if sync {
        tmux::spr_sync(project_path, branch)?
    } else {
        tmux::spr_update(project_path, branch)?
    };
    config::write_stack_cache(&project_name, branch, &prs);

    println!("{} PR(s) in the stack (bottom→top):", prs.len());
    for (url, title) in &prs {
        println!("  {url}  {title}");
    }
    Ok(())
}

fn cmd_set_stacked(args: &[String]) -> Result<()> {
    let project_path = args
        .first()
        .ok_or_else(|| anyhow::anyhow!("usage: set-stacked <project-path> <branch> [on|off]"))?;
    let branch = args
        .get(1)
        .ok_or_else(|| anyhow::anyhow!("usage: set-stacked <project-path> <branch> [on|off]"))?;
    let on = !matches!(args.get(2).map(String::as_str), Some("off" | "false" | "0"));

    let mut found = false;
    Config::modify(|cfg| {
        found = cfg.set_task_stacked_by_branch(project_path, branch, on);
    })?;

    if found {
        println!(
            "Stacked-PR mode {} for branch '{branch}'",
            if on { "enabled" } else { "disabled" }
        );
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "No task found for project path '{project_path}' with branch '{branch}'"
        ))
    }
}

fn is_text_input_mode(mode: InputMode) -> bool {
    matches!(
        mode,
        InputMode::AddProjectName
            | InputMode::AddSessionName
            | InputMode::AddSessionPrompt
            | InputMode::AddAdhocSessionName
            | InputMode::AddTaskName
            | InputMode::AddTaskBranch
            | InputMode::AddTaskPrompt
            | InputMode::RenameProject
            | InputMode::RenameTask
            | InputMode::RenameSession
            | InputMode::RenameAdhocSession
            | InputMode::MergeCommitMessage
            | InputMode::SetBaseBranch
            | InputMode::Search
    )
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(result) = run_cli(&args) {
        return result;
    }

    let mut app = App::new()?;

    loop {
        enable_raw_mode()?;
        io::stdout().execute(EnterAlternateScreen)?;
        io::stdout().execute(EnableBracketedPaste)?;
        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::new(backend)?;

        run_tui(&mut terminal, &mut app)?;

        io::stdout().execute(DisableBracketedPaste)?;
        disable_raw_mode()?;
        io::stdout().execute(LeaveAlternateScreen)?;

        if app.should_quit {
            break;
        }

        if let Some(session_name) = app.should_attach.take() {
            tmux::attach_session(&session_name)?;
        } else if let Some(path) = app.should_open_editor.take() {
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vim".into());
            std::process::Command::new(&editor).arg(&path).status()?;
        }
    }

    Ok(())
}

fn run_tui(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> Result<()> {
    app.sync_worker_hints();

    loop {
        terminal.draw(|f| ui::draw(f, app))?;

        if event::poll(Duration::from_millis(100))? {
            let evt = event::read()?;

            if let Event::Paste(data) = evt {
                if is_text_input_mode(app.input_mode) {
                    let allow_newlines = matches!(
                        app.input_mode,
                        InputMode::AddTaskPrompt
                            | InputMode::AddSessionPrompt
                            | InputMode::MergeCommitMessage
                    );
                    for c in data.chars() {
                        match c {
                            '\r' => {}
                            '\n' if !allow_newlines => app.input_buffer.push(' '),
                            c if c.is_control() && c != '\n' => {}
                            c => app.input_buffer.push(c),
                        }
                    }
                }
                continue;
            }

            if let Event::Key(key) = evt {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                let kb = app.keybindings.clone();
                match app.input_mode {
                    InputMode::Normal => match key.code {
                        KeyCode::Char(c) if c == kb.quit => {
                            app.should_quit = true;
                            return Ok(());
                        }
                        KeyCode::Up => app.move_up(),
                        KeyCode::Down => app.move_down(),
                        KeyCode::Char(c) if c == kb.move_up => app.move_up(),
                        KeyCode::Char(c) if c == kb.move_down => app.move_down(),
                        KeyCode::Enter => {
                            app.enter_selected();
                            if app.should_attach.is_some() || app.should_open_editor.is_some() {
                                return Ok(());
                            }
                        }
                        KeyCode::Char(c) if c == kb.toggle_collapse => app.toggle_collapse(),
                        KeyCode::Char(c) if c == kb.context_menu => app.open_context_menu(),
                        KeyCode::Char(c) if c == kb.add_project => app.start_add_project(),
                        KeyCode::Char(c) if c == kb.toggle_archive_view => {
                            app.toggle_archive_view()
                        }
                        KeyCode::Char(c) if c == kb.search => app.start_search(),
                        KeyCode::Char(c) if c == kb.cycle_theme => app.cycle_theme(),
                        _ => {}
                    },
                    InputMode::ContextMenu => match key.code {
                        KeyCode::Esc => {
                            app.input_mode = InputMode::Normal;
                        }
                        KeyCode::Char(c) if c == kb.context_menu => {
                            app.input_mode = InputMode::Normal;
                        }
                        KeyCode::Up => {
                            if app.context_menu_selected > 0 {
                                app.context_menu_selected -= 1;
                            }
                        }
                        KeyCode::Char(c) if c == kb.move_up => {
                            if app.context_menu_selected > 0 {
                                app.context_menu_selected -= 1;
                            }
                        }
                        KeyCode::Down => {
                            if app.context_menu_selected + 1 < app.context_menu_items.len() {
                                app.context_menu_selected += 1;
                            }
                        }
                        KeyCode::Char(c) if c == kb.move_down => {
                            if app.context_menu_selected + 1 < app.context_menu_items.len() {
                                app.context_menu_selected += 1;
                            }
                        }
                        KeyCode::Enter => {
                            if let Some(item) =
                                app.context_menu_items.get(app.context_menu_selected)
                            {
                                let action = item.action;
                                app.execute_context_action(action);
                            }
                        }
                        KeyCode::Char(c) => {
                            if let Some(item) = app.context_menu_items.iter().find(|i| i.key == c) {
                                let action = item.action;
                                app.execute_context_action(action);
                            }
                        }
                        _ => {}
                    },
                    InputMode::AddProjectName => match key.code {
                        KeyCode::Enter => app.confirm_add_project(),
                        KeyCode::Esc => app.cancel_input(),
                        KeyCode::Backspace => {
                            app.input_buffer.pop();
                        }
                        KeyCode::Char(c) => app.input_buffer.push(c),
                        _ => {}
                    },
                    InputMode::AddTaskName => match key.code {
                        KeyCode::Enter => app.confirm_add_task(),
                        KeyCode::Esc => app.cancel_input(),
                        KeyCode::Backspace => {
                            app.input_buffer.pop();
                        }
                        KeyCode::Char(c) => app.input_buffer.push(c),
                        _ => {}
                    },
                    InputMode::AddTaskBranch => match key.code {
                        KeyCode::Enter => app.confirm_add_task_branch(),
                        KeyCode::Esc => app.cancel_input(),
                        KeyCode::Backspace => {
                            app.input_buffer.pop();
                        }
                        KeyCode::Char(c) => app.input_buffer.push(c),
                        _ => {}
                    },
                    InputMode::AddTaskPrompt => match key.code {
                        KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                            app.input_buffer.push('\n');
                        }
                        KeyCode::Enter => app.confirm_add_task_with_prompt(),
                        KeyCode::Esc => app.cancel_input(),
                        KeyCode::Backspace => {
                            app.input_buffer.pop();
                        }
                        KeyCode::Char(c) => app.input_buffer.push(c),
                        _ => {}
                    },
                    InputMode::AddSessionName => match key.code {
                        KeyCode::Enter => {
                            app.confirm_new_session();
                        }
                        KeyCode::Esc => app.cancel_input(),
                        KeyCode::Backspace => {
                            app.input_buffer.pop();
                        }
                        KeyCode::Char(c) => app.input_buffer.push(c),
                        _ => {}
                    },
                    InputMode::AddAdhocSessionName => match key.code {
                        KeyCode::Enter => app.confirm_new_adhoc_session(),
                        KeyCode::Esc => app.cancel_input(),
                        KeyCode::Backspace => {
                            app.input_buffer.pop();
                        }
                        KeyCode::Char(c) => app.input_buffer.push(c),
                        _ => {}
                    },
                    InputMode::AddSessionPrompt => match key.code {
                        KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                            app.input_buffer.push('\n');
                        }
                        KeyCode::Enter => {
                            app.confirm_new_session_with_prompt();
                        }
                        KeyCode::Esc => app.cancel_input(),
                        KeyCode::Backspace => {
                            app.input_buffer.pop();
                        }
                        KeyCode::Char(c) => app.input_buffer.push(c),
                        _ => {}
                    },
                    InputMode::ConfirmDelete => match key.code {
                        KeyCode::Char('y') => app.confirm_delete(),
                        KeyCode::Char('n') | KeyCode::Esc => app.cancel_input(),
                        _ => {}
                    },
                    InputMode::RenameProject
                    | InputMode::RenameTask
                    | InputMode::RenameSession
                    | InputMode::RenameAdhocSession => match key.code {
                        KeyCode::Enter => app.confirm_rename(),
                        KeyCode::Esc => app.cancel_input(),
                        KeyCode::Backspace => {
                            app.input_buffer.pop();
                        }
                        KeyCode::Char(c) => app.input_buffer.push(c),
                        _ => {}
                    },
                    InputMode::ConfirmCreatePr => match key.code {
                        KeyCode::Char('y') => app.confirm_create_pr(),
                        KeyCode::Char('n') | KeyCode::Esc => app.cancel_input(),
                        _ => {}
                    },
                    InputMode::MergeCommitMessage => match key.code {
                        KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                            app.input_buffer.push('\n');
                        }
                        KeyCode::Enter => app.confirm_merge_commit(),
                        KeyCode::Esc => app.cancel_input(),
                        KeyCode::Backspace => {
                            app.input_buffer.pop();
                        }
                        KeyCode::Char(c) => app.input_buffer.push(c),
                        _ => {}
                    },
                    InputMode::SetBaseBranch => match key.code {
                        KeyCode::Enter => app.confirm_set_base_branch(),
                        KeyCode::Esc => app.cancel_input(),
                        KeyCode::Backspace => {
                            app.input_buffer.pop();
                        }
                        KeyCode::Char(c) => app.input_buffer.push(c),
                        _ => {}
                    },
                    InputMode::Search => match key.code {
                        KeyCode::Enter => app.confirm_search(),
                        KeyCode::Esc => app.cancel_search(),
                        KeyCode::Backspace => {
                            app.input_buffer.pop();
                            app.update_search();
                        }
                        KeyCode::Char(c) => {
                            app.input_buffer.push(c);
                            app.update_search();
                        }
                        _ => {}
                    },
                }
            }
        }

        // Apply background updates (non-blocking)
        app.apply_worker_updates();
        app.apply_op_results();
        app.maybe_reload_config();
        app.tick = app.tick.wrapping_add(1);

        if app.should_quit {
            return Ok(());
        }
    }
}
