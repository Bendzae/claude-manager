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
            // Worker thread keeps running, will pick up changes automatically
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
                            if app.should_attach.is_some() {
                                return Ok(());
                            }
                        }
                        KeyCode::Char(' ') => app.toggle_collapse(),
                        KeyCode::Char('t') => app.start_add_task(),
                        KeyCode::Char('n') => app.start_new_session(true),
                        KeyCode::Char('N') => app.start_new_session(false),
                        KeyCode::Char('d') => app.start_delete(),
                        KeyCode::Char('a') => app.start_add_project(),
                        KeyCode::Char('R') => app.start_rename(),
                        KeyCode::Char('m') => app.start_merge(),
                        KeyCode::Char('u') => app.update_session(),
                        KeyCode::Char('J') => app.scroll_preview_down(),
                        KeyCode::Char('K') => app.scroll_preview_up(),
                        KeyCode::Tab => app.toggle_preview_mode(),
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
                            if app.should_attach.is_some() {
                                return Ok(());
                            }
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

        // Apply background worker updates (non-blocking)
        app.apply_worker_updates();
        app.tick = app.tick.wrapping_add(1);

        if app.should_quit {
            return Ok(());
        }
    }
}
