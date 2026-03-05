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
const PAD_LEFT: u16 = 1;
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

    let show_panel = matches!(
        app.selected_item(),
        Some(app::ListItem::Session { .. } | app::ListItem::Task { .. })
    );
    if show_panel {
        let columns = Layout::horizontal([
            Constraint::Percentage(30),
            Constraint::Percentage(70),
        ])
        .split(chunks[2]);

        draw_list(f, app, columns[0]);
        match app.selected_item() {
            Some(app::ListItem::Task { .. }) => draw_task_diff_panel(f, app, columns[1]),
            _ => draw_preview_panel(f, app, columns[1]),
        }
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
    )]))
    .alignment(ratatui::layout::Alignment::Center);
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
                let mut spans = vec![
                    Span::styled(indicator, indicator_style),
                    Span::raw("  "),
                    Span::styled(chevron, Style::default().fg(MUTED)),
                    Span::styled(&task.name, style),
                ];

                // Show diff stats for the task branch vs main
                let (added, removed) = app
                    .task_diff_stats
                    .get(&task.branch)
                    .map(|s| (s.added, s.removed))
                    .unwrap_or((0, 0));
                if added > 0 || removed > 0 {
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(
                        format!("+{added}"),
                        Style::default().fg(Color::Green),
                    ));
                    spans.push(Span::styled(",", Style::default().fg(MUTED)));
                    spans.push(Span::styled(
                        format!("-{removed}"),
                        Style::default().fg(Color::Red),
                    ));
                }

                spans.push(Span::styled(
                    format!("  ({})", task.branch),
                    Style::default().fg(MUTED),
                ));

                lines.push(ListItem::new(Line::from(spans)));
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
                    Span::raw("    "),
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

    let content = match app.preview_content.as_deref() {
        Some(c) => c,
        None => {
            render_loading(f, app, content_area);
            return;
        }
    };

    match app.preview_mode {
        PreviewMode::Output => {
            let text = match content.as_bytes().into_text() {
                Ok(text) => text,
                Err(_) => return,
            };
            let visible_lines: Vec<Line> = text.lines.into_iter().skip(app.preview_scroll).take(visible_height).collect();
            let paragraph = Paragraph::new(visible_lines);
            f.render_widget(paragraph, content_area);
        }
        PreviewMode::Diff => {
            if let Some(app::ListItem::Session { session, .. }) = app.selected_item() {
                if let Some(stats) = app.diff_stats.get(&session.name) {
                    render_diff_with_stats(f, content, stats.added, stats.removed, content_area, visible_height, app.preview_scroll);
                } else {
                    render_diff_content(f, content, content_area, visible_height, app.preview_scroll);
                }
            } else {
                render_diff_content(f, content, content_area, visible_height, app.preview_scroll);
            }
        }
    }
}

fn draw_task_diff_panel(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height < 2 {
        return;
    }

    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .split(inner);

    // Tab header (diff only, no switching)
    let tab = Paragraph::new(Line::from(vec![
        Span::styled(
            "diff",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
    ]));
    f.render_widget(tab, rows[0]);

    let sep = "─".repeat(rows[1].width as usize);
    let separator = Paragraph::new(Span::styled(sep, Style::default().fg(ACCENT)));
    f.render_widget(separator, rows[1]);

    let content_area = rows[2];
    let visible_height = content_area.height as usize;
    if visible_height == 0 {
        return;
    }

    let stats = match &app.task_diff {
        Some(stats) => stats,
        None => {
            render_loading(f, app, content_area);
            return;
        }
    };

    render_diff_with_stats(f, &stats.diff_output, stats.added, stats.removed, content_area, visible_height, app.preview_scroll);
}

const LOADING_SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn render_loading(f: &mut Frame, app: &App, area: Rect) {
    let frame = LOADING_SPINNER[app.tick % LOADING_SPINNER.len()];
    let loading = Paragraph::new(Line::from(vec![
        Span::styled(format!("{frame} "), Style::default().fg(MUTED)),
        Span::styled("loading…", Style::default().fg(MUTED)),
    ]));
    f.render_widget(loading, area);
}

fn style_diff_lines(content: &str) -> Vec<Line<'_>> {
    content
        .lines()
        .map(|line| {
            if line.starts_with("@@") {
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
            }
        })
        .collect()
}

fn render_diff_content(f: &mut Frame, content: &str, area: Rect, visible_height: usize, scroll: usize) {
    let diff_lines = style_diff_lines(content);
    let visible_lines: Vec<Line> = diff_lines.into_iter().skip(scroll).take(visible_height).collect();
    let paragraph = Paragraph::new(visible_lines);
    f.render_widget(paragraph, area);
}

fn render_diff_with_stats(f: &mut Frame, content: &str, added: usize, removed: usize, area: Rect, visible_height: usize, scroll: usize) {
    let mut diff_lines: Vec<Line> = Vec::new();

    diff_lines.push(Line::from(vec![
        Span::styled(
            format!("{added} additions(+)"),
            Style::default().fg(Color::Green),
        ),
        Span::raw("  "),
        Span::styled(
            format!("{removed} deletions(-)"),
            Style::default().fg(Color::Red),
        ),
    ]));
    diff_lines.push(Line::raw(""));

    diff_lines.extend(style_diff_lines(content));

    let visible_lines: Vec<Line> = diff_lines.into_iter().skip(scroll).take(visible_height).collect();
    let paragraph = Paragraph::new(visible_lines);
    f.render_widget(paragraph, area);
}

fn draw_help(f: &mut Frame, app: &App, area: Rect) {
    let help_text = match app.input_mode {
        InputMode::Normal => {
            "t: new task  n/N: new session  Enter: attach  Space: collapse  d: delete  R: rename  m: merge  u: update  Tab: diff  J/K: scroll  a: add project  q: quit"
        }
        InputMode::AddProjectName
        | InputMode::AddSessionName
        | InputMode::AddTaskName
        | InputMode::AddTaskBranch
        | InputMode::RenameProject
        | InputMode::RenameTask
        | InputMode::RenameSession
        | InputMode::MergeCommitMessage => "Enter: confirm  Esc: cancel",
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
                | InputMode::AddTaskBranch
                | InputMode::RenameProject
                | InputMode::RenameTask
                | InputMode::RenameSession
                | InputMode::MergeCommitMessage
        ) {
            format!("{}{}", msg, app.input_buffer)
        } else {
            msg.clone()
        };

        let status = Paragraph::new(Span::styled(content, style));
        f.render_widget(status, area);
    }
}
