mod app;
mod config;
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

fn is_text_input_mode(mode: InputMode) -> bool {
    matches!(
        mode,
        InputMode::AddProjectName
            | InputMode::AddSessionName
            | InputMode::AddSessionPrompt
            | InputMode::AddTaskName
            | InputMode::AddTaskBranch
            | InputMode::AddTaskPrompt
            | InputMode::RenameProject
            | InputMode::RenameTask
            | InputMode::RenameSession
            | InputMode::MergeCommitMessage
    )
}

fn main() -> Result<()> {
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
        } else if let Some((session_name, window_idx)) = app.should_attach_window.take() {
            tmux::attach_session_window(&session_name, window_idx)?;
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
                        KeyCode::Char(c) if c == kb.move_up && app.diff_focused() => {
                            app.move_diff_cursor_up()
                        }
                        KeyCode::Char(c) if c == kb.move_down && app.diff_focused() => {
                            app.move_diff_cursor_down()
                        }
                        KeyCode::Char(c) if c == kb.move_up => app.move_up(),
                        KeyCode::Char(c) if c == kb.move_down => app.move_down(),
                        KeyCode::Char('c') if app.diff_focused() => app.start_add_diff_comment(),
                        KeyCode::Char('x') if app.diff_focused() => app.delete_diff_comment(),
                        KeyCode::Char('s') if app.diff_focused() => {
                            app.submit_diff_comments(false)
                        }
                        KeyCode::Char('S') if app.diff_focused() => {
                            app.submit_diff_comments(true)
                        }
                        KeyCode::Enter => {
                            app.enter_selected();
                            if app.should_attach.is_some()
                                || app.should_attach_window.is_some()
                                || app.should_open_editor.is_some()
                            {
                                return Ok(());
                            }
                        }
                        KeyCode::Char(c) if c == kb.toggle_collapse => app.toggle_collapse(),
                        KeyCode::Char(c) if c == kb.context_menu => app.open_context_menu(),
                        KeyCode::Char(c) if c == kb.add_project => app.start_add_project(),
                        KeyCode::Char(c) if c == kb.scroll_preview_down => {
                            app.scroll_preview_down()
                        }
                        KeyCode::Char(c) if c == kb.scroll_preview_up => app.scroll_preview_up(),
                        KeyCode::Tab => app.toggle_preview_mode(),
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
                    InputMode::RenameProject | InputMode::RenameTask | InputMode::RenameSession => {
                        match key.code {
                            KeyCode::Enter => app.confirm_rename(),
                            KeyCode::Esc => app.cancel_input(),
                            KeyCode::Backspace => {
                                app.input_buffer.pop();
                            }
                            KeyCode::Char(c) => app.input_buffer.push(c),
                            _ => {}
                        }
                    }
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
                    InputMode::AddDiffComment => match key.code {
                        KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                            app.input_buffer.push('\n');
                        }
                        KeyCode::Enter => app.confirm_add_diff_comment(),
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
