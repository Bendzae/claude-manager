use ansi_to_tui::IntoText;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Frame;

use crate::app::{self, App, InputMode, PreviewMode};
use crate::tmux::SessionStatus;

const ACCENT: Color = Color::Cyan;
const MUTED: Color = Color::DarkGray;
const TASK_COLOR: Color = Color::Yellow;
const SESSION_COLOR: Color = Color::Green;
const PAD_LEFT: u16 = 2;
const PAD_TOP: u16 = 1;

pub fn draw(f: &mut Frame, app: &App) {
    let outer = f.area().inner(Margin {
        horizontal: PAD_LEFT,
        vertical: 0,
    });

    let chunks = Layout::vertical([
        Constraint::Length(PAD_TOP),
        Constraint::Length(1),
        Constraint::Min(5),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(outer);

    draw_title(f, chunks[1]);

    let has_preview = app.preview_content.is_some();
    if has_preview {
        let columns = Layout::horizontal([
            Constraint::Percentage(30),
            Constraint::Percentage(70),
        ])
        .split(chunks[2]);

        draw_list(f, app, columns[0]);
        draw_preview_panel(f, app, columns[1]);
    } else {
        draw_list(f, app, chunks[2]);
    }

    draw_help(f, app, chunks[3]);
    draw_status(f, app, chunks[4]);
}

fn draw_title(f: &mut Frame, area: Rect) {
    let title = Paragraph::new(Line::from(vec![Span::styled(
        "Claude Manager",
        Style::default()
            .fg(ACCENT)
            .add_modifier(Modifier::BOLD),
    )]));
    f.render_widget(title, area);
}

fn is_project_collapsed(app: &App, name: &str) -> bool {
    app.collapsed.contains(&format!("p:{name}"))
}

fn is_task_collapsed(app: &App, project: &str, task: &str) -> bool {
    app.collapsed.contains(&format!("t:{project}:{task}"))
}

fn draw_list(f: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<ListItem> = Vec::new();
    let indicator_style = Style::default().fg(ACCENT).add_modifier(Modifier::BOLD);

    for (i, item) in app.items.iter().enumerate() {
        let is_selected = i == app.selected;

        if matches!(item, app::ListItem::Project { .. }) && !lines.is_empty() {
            lines.push(ListItem::new(Line::raw("")));
        }

        match item {
            app::ListItem::Project { project } => {
                let indicator = if is_selected { "> " } else { "  " };
                let collapsed = is_project_collapsed(app, &project.name);
                let chevron = if collapsed { "+ " } else { "- " };
                let name_style = if is_selected {
                    Style::default()
                        .fg(ACCENT)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                };
                let line = Line::from(vec![
                    Span::styled(indicator, indicator_style),
                    Span::styled(chevron, Style::default().fg(MUTED)),
                    Span::styled(&project.name, name_style),
                    Span::styled(
                        format!("  {}", project.path),
                        Style::default().fg(MUTED),
                    ),
                ]);
                lines.push(ListItem::new(line));
            }
            app::ListItem::Task {
                project_name,
                task,
                ..
            } => {
                let indicator = if is_selected { "> " } else { "  " };
                let collapsed = is_task_collapsed(app, project_name, &task.name);
                let chevron = if collapsed { "+ " } else { "- " };
                let style = if is_selected {
                    Style::default()
                        .fg(TASK_COLOR)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(TASK_COLOR)
                };
                let line = Line::from(vec![
                    Span::styled(indicator, indicator_style),
                    Span::raw("    "),
                    Span::styled(chevron, Style::default().fg(MUTED)),
                    Span::styled(&task.name, style),
                    Span::styled(
                        format!("  ({})", task.branch),
                        Style::default().fg(MUTED),
                    ),
                ]);
                lines.push(ListItem::new(line));
            }
            app::ListItem::Session { session, .. } => {
                let indicator = if is_selected { "> " } else { "  " };
                let style = if is_selected {
                    Style::default()
                        .fg(SESSION_COLOR)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(SESSION_COLOR)
                };

                let status = app
                    .session_statuses
                    .get(&session.name)
                    .copied()
                    .unwrap_or(SessionStatus::Finished);
                const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                let (status_icon, status_color) = match status {
                    SessionStatus::Running => {
                        let frame = SPINNER[app.tick % SPINNER.len()];
                        (frame, Color::Yellow)
                    }
                    SessionStatus::WaitingForInput => ("\u{25CF}", Color::Green),
                    SessionStatus::WaitingForPermission => ("!", Color::Magenta),
                    SessionStatus::Finished => ("\u{25CF}", Color::Red),
                };

                let wt = session.worktree_path();
                let mut spans = vec![
                    Span::styled(indicator, indicator_style),
                    Span::raw("        "),
                    Span::styled(
                        format!("{status_icon} "),
                        Style::default().fg(status_color),
                    ),
                ];
                if wt.is_some() {
                    spans.push(Span::styled(
                        "\u{e0a0} ",
                        Style::default().fg(MUTED),
                    ));
                }
                spans.push(Span::styled(&session.session_name, style));
                if let Some(ref path) = wt {
                    let dir_name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    spans.push(Span::styled(
                        format!("  {dir_name}"),
                        Style::default().fg(MUTED),
                    ));
                }
                if let Some(stats) = app.diff_stats.get(&session.name) {
                    if !stats.is_empty() {
                        spans.push(Span::raw("  "));
                        spans.push(Span::styled(
                            format!("+{}", stats.added),
                            Style::default().fg(Color::Green),
                        ));
                        spans.push(Span::styled(",", Style::default().fg(MUTED)));
                        spans.push(Span::styled(
                            format!("-{}", stats.removed),
                            Style::default().fg(Color::Red),
                        ));
                    }
                }
                lines.push(ListItem::new(Line::from(spans)));
            }
        }
    }

    let list = List::new(lines).block(Block::default().borders(Borders::NONE));
    f.render_widget(list, area);
}

fn draw_preview_panel(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height < 3 {
        return;
    }

    // Split inner area: tabs (1 line) + separator (1 line) + content
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .split(inner);

    // Draw tabs
    let active_style = Style::default().fg(ACCENT).add_modifier(Modifier::BOLD);
    let inactive_style = Style::default().fg(MUTED);

    let output_style = if app.preview_mode == PreviewMode::Output {
        active_style
    } else {
        inactive_style
    };
    let diff_style = if app.preview_mode == PreviewMode::Diff {
        active_style
    } else {
        inactive_style
    };

    let tabs = Paragraph::new(Line::from(vec![
        Span::styled("agent", output_style),
        Span::styled(" │ ", Style::default().fg(MUTED)),
        Span::styled("diff", diff_style),
    ]));
    f.render_widget(tabs, rows[0]);

    // Draw separator line
    let sep = "─".repeat(rows[1].width as usize);
    let separator = Paragraph::new(Span::styled(sep, Style::default().fg(ACCENT)));
    f.render_widget(separator, rows[1]);

    // Draw content
    let content_area = rows[2];
    let visible_height = content_area.height as usize;
    if visible_height == 0 {
        return;
    }

    let content = app.preview_content.as_deref().unwrap_or("");

    match app.preview_mode {
        PreviewMode::Output => {
            let text = match content.as_bytes().into_text() {
                Ok(text) => text,
                Err(_) => return,
            };
            let total_lines = text.lines.len();
            let start = total_lines.saturating_sub(visible_height);
            let visible_lines: Vec<Line> = text.lines.into_iter().skip(start).collect();
            let paragraph = Paragraph::new(visible_lines);
            f.render_widget(paragraph, content_area);
        }
        PreviewMode::Diff => {
            let mut diff_lines: Vec<Line> = Vec::new();

            if let Some(app::ListItem::Session { session, .. }) = app.selected_item() {
                if let Some(stats) = app.diff_stats.get(&session.name) {
                    diff_lines.push(Line::from(vec![
                        Span::styled(
                            format!("{} additions(+)", stats.added),
                            Style::default().fg(Color::Green),
                        ),
                        Span::raw("  "),
                        Span::styled(
                            format!("{} deletions(-)", stats.removed),
                            Style::default().fg(Color::Red),
                        ),
                    ]));
                    diff_lines.push(Line::raw(""));
                }
            }

            for line in content.lines() {
                let styled_line = if line.starts_with("@@") {
                    Line::styled(line, Style::default().fg(Color::Cyan))
                } else if line.starts_with('+') && !line.starts_with("+++") {
                    Line::styled(line, Style::default().fg(Color::Green))
                } else if line.starts_with('-') && !line.starts_with("---") {
                    Line::styled(line, Style::default().fg(Color::Red))
                } else if line.starts_with("diff ") || line.starts_with("index ") {
                    Line::styled(line, Style::default().fg(MUTED))
                } else if line.starts_with("---") || line.starts_with("+++") {
                    Line::styled(
                        line,
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    Line::raw(line)
                };
                diff_lines.push(styled_line);
            }

            let total_lines = diff_lines.len();
            let start = total_lines.saturating_sub(visible_height);
            let visible_lines: Vec<Line> = diff_lines.into_iter().skip(start).collect();
            let paragraph = Paragraph::new(visible_lines);
            f.render_widget(paragraph, content_area);
        }
    }
}

fn draw_help(f: &mut Frame, app: &App, area: Rect) {
    let help_text = match app.input_mode {
        InputMode::Normal => {
            "t: new task  n: new session  N: no worktree  Enter: attach  Space: collapse  d: delete  R: rename  Tab: diff  a: add project  q: quit"
        }
        InputMode::AddProjectName
        | InputMode::AddSessionName
        | InputMode::AddTaskName
        | InputMode::RenameProject
        | InputMode::RenameTask
        | InputMode::RenameSession => "Enter: confirm  Esc: cancel",
        InputMode::ConfirmDelete => "y: confirm  n/Esc: cancel",
    };

    let help = Paragraph::new(Span::styled(help_text, Style::default().fg(MUTED)));
    f.render_widget(help, area);
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    if let Some(msg) = &app.status_message {
        let style = if msg.starts_with("Error") {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::Yellow)
        };

        let content = if matches!(
            app.input_mode,
            InputMode::AddProjectName
                | InputMode::AddSessionName
                | InputMode::AddTaskName
                | InputMode::RenameProject
                | InputMode::RenameTask
                | InputMode::RenameSession
        ) {
            format!("{}{}", msg, app.input_buffer)
        } else {
            msg.clone()
        };

        let status = Paragraph::new(Span::styled(content, style));
        f.render_widget(status, area);
    }
}
