mod app;
mod config;
mod tmux;
mod ui;
mod worker;

use std::io;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use app::{App, InputMode};

fn main() -> Result<()> {
    let mut app = App::new()?;

    loop {
        enable_raw_mode()?;
        io::stdout().execute(EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::new(backend)?;

        run_tui(&mut terminal, &mut app)?;

        disable_raw_mode()?;
        io::stdout().execute(LeaveAlternateScreen)?;

        if app.should_quit {
            break;
        }

        if let Some(session_name) = app.should_attach.take() {
            tmux::attach_session(&session_name)?;
        } else if let Some((session_name, window_idx)) = app.should_attach_window.take() {
            tmux::attach_session_window(&session_name, window_idx)?;
        } else if let Some(path) = app.should_open_editor.take() {
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vim".into());
            std::process::Command::new(&editor)
                .arg(&path)
                .status()?;
        }
    }

    Ok(())
}

fn run_tui(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> Result<()> {
    app.sync_worker_hints();

    loop {
        terminal.draw(|f| ui::draw(f, app))?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                if app.loading {
                    // Only allow quit while loading
                    if key.code == KeyCode::Char('q') {
                        app.should_quit = true;
                        return Ok(());
                    }
                    continue;
                }

                match app.input_mode {
                    InputMode::Normal => match key.code {
                        KeyCode::Char('q') => {
                            app.should_quit = true;
                            return Ok(());
                        }
                        KeyCode::Up | KeyCode::Char('k') => app.move_up(),
                        KeyCode::Down | KeyCode::Char('j') => app.move_down(),
                        KeyCode::Enter => {
                            app.enter_selected();
                            if app.should_attach.is_some()
                                || app.should_attach_window.is_some()
                                || app.should_open_editor.is_some()
                            {
                                return Ok(());
                            }
                        }
                        KeyCode::Char(' ') => app.toggle_collapse(),
                        KeyCode::Char('a') => app.open_context_menu(),
                        KeyCode::Char('p') => app.start_add_project(),
                        KeyCode::Char('J') => app.scroll_preview_down(),
                        KeyCode::Char('K') => app.scroll_preview_up(),
                        KeyCode::Tab => app.toggle_preview_mode(),
                        _ => {}
                    },
                    InputMode::ContextMenu => match key.code {
                        KeyCode::Esc | KeyCode::Char('a') => {
                            app.input_mode = InputMode::Normal;
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            if app.context_menu_selected > 0 {
                                app.context_menu_selected -= 1;
                            }
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            if app.context_menu_selected + 1 < app.context_menu_items.len() {
                                app.context_menu_selected += 1;
                            }
                        }
                        KeyCode::Enter => {
                            if let Some(item) = app.context_menu_items.get(app.context_menu_selected) {
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
                    InputMode::AddSessionPrompt => match key.code {
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
                    | InputMode::RenameSession => match key.code {
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
                        KeyCode::Enter => app.confirm_merge_commit(),
                        KeyCode::Esc => app.cancel_input(),
                        KeyCode::Backspace => {
                            app.input_buffer.pop();
                        }
                        KeyCode::Char(c) => app.input_buffer.push(c),
                        _ => {}
                    },
                }
            }
        }

        // Apply background updates (non-blocking)
        app.apply_worker_updates();
        app.apply_op_results();
        app.tick = app.tick.wrapping_add(1);

        if app.should_quit {
            return Ok(());
        }
    }
}
