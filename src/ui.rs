use ansi_to_tui::IntoText;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, List, ListItem, Paragraph};
use ratatui::Frame;

use crate::app::{self, App, InputMode, PreviewMode};
use crate::tmux::{self, SessionStatus};

const ACCENT: Color = Color::Cyan;
const MUTED: Color = Color::Rgb(90, 90, 100);
const TREE: Color = Color::Rgb(60, 60, 70);
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
        Constraint::Min(5),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(outer);

    let show_panel = matches!(
        app.selected_item(),
        Some(app::ListItem::Session { .. } | app::ListItem::Task { .. })
    );
    if show_panel {
        let columns = Layout::horizontal([
            Constraint::Percentage(30),
            Constraint::Percentage(70),
        ])
        .split(chunks[1]);

        draw_list(f, app, columns[0]);
        match app.selected_item() {
            Some(app::ListItem::Task { .. }) => draw_task_diff_panel(f, app, columns[1]),
            _ => draw_preview_panel(f, app, columns[1]),
        }
    } else {
        draw_list(f, app, chunks[1]);
    }

    draw_help(f, app, chunks[2]);
    draw_status(f, app, chunks[3]);
}

fn is_project_collapsed(app: &App, name: &str) -> bool {
    app.collapsed.contains(&format!("p:{name}"))
}

/// Check if a task is the last task in its project (looking past child sessions).
fn is_last_task(items: &[app::ListItem], i: usize, project_name: &str) -> bool {
    for j in (i + 1)..items.len() {
        match &items[j] {
            app::ListItem::Session { .. } => continue,
            app::ListItem::Task {
                project_name: pn, ..
            } => return pn != project_name,
            _ => return true,
        }
    }
    true
}

/// Check if a session is the last session under its task.
fn is_last_session(items: &[app::ListItem], i: usize, project_name: &str, task_name: &str) -> bool {
    match items.get(i + 1) {
        Some(app::ListItem::Session {
            project_name: pn,
            task: t,
            ..
        }) => pn != project_name || t.name != task_name,
        _ => true,
    }
}

/// Find whether the parent task of a session is the last task in the project.
fn parent_task_is_last(items: &[app::ListItem], session_idx: usize, project_name: &str, task_name: &str) -> bool {
    for j in (0..session_idx).rev() {
        if let app::ListItem::Task {
            project_name: pn,
            task: t,
            ..
        } = &items[j]
        {
            if pn == project_name && t.name == task_name {
                return is_last_task(items, j, project_name);
            }
        }
    }
    true
}

fn draw_list(f: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<ListItem> = Vec::new();
    let indicator_style = Style::default().fg(ACCENT).add_modifier(Modifier::BOLD);
    let tree_style = Style::default().fg(TREE);

    for (i, item) in app.items.iter().enumerate() {
        let is_selected = i == app.selected;

        if matches!(item, app::ListItem::Project { .. }) && !lines.is_empty() {
            lines.push(ListItem::new(Line::raw("")));
        }

        match item {
            app::ListItem::Project { project } => {
                let indicator = if is_selected { " ▸ " } else { "   " };
                let collapsed = is_project_collapsed(app, &project.name);
                let chevron = if collapsed { "▶ " } else { "▼ " };
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
                let indicator = if is_selected { " ▸ " } else { "   " };
                let last = is_last_task(&app.items, i, project_name);
                let branch_char = if last { "└─ " } else { "├─ " };
                let style = if is_selected {
                    Style::default()
                        .fg(TASK_COLOR)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(TASK_COLOR)
                };
                let mut spans = vec![
                    Span::styled(indicator, indicator_style),
                    Span::styled(branch_char, tree_style),
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

                // Show PR icon if a PR exists for this branch
                if app.pr_urls.contains_key(&task.branch) {
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(
                        "\u{e728}",
                        Style::default().fg(Color::Magenta),
                    ));
                }

                spans.push(Span::styled(
                    format!("  ({})", task.branch),
                    Style::default().fg(MUTED),
                ));

                // Show active session count when task is collapsed
                if app.collapsed.contains(&format!("t:{project_name}:{}", task.name)) {
                    let sessions =
                        tmux::sessions_for_task(project_name, &task.name, &app.sessions);
                    let active = sessions
                        .iter()
                        .filter(|s| {
                            app.session_statuses
                                .get(&s.name)
                                .map_or(false, |st| *st != SessionStatus::Finished)
                        })
                        .count();
                    if active > 0 {
                        spans.push(Span::styled(
                            format!("  [{active} active]"),
                            Style::default().fg(Color::Green),
                        ));
                    }
                }

                lines.push(ListItem::new(Line::from(spans)));
            }
            app::ListItem::Session {
                project_name,
                task,
                session,
                ..
            } => {
                let indicator = if is_selected { " ▸ " } else { "   " };
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
                    SessionStatus::WaitingForInput => ("●", Color::Green),
                    SessionStatus::WaitingForPermission => ("!", Color::Magenta),
                    SessionStatus::Finished => ("●", Color::Red),
                };

                let parent_last = parent_task_is_last(&app.items, i, project_name, &task.name);
                let session_last = is_last_session(&app.items, i, project_name, &task.name);
                let continuation = if parent_last { "   " } else { "│  " };
                let branch_char = if session_last { "└─ " } else { "├─ " };

                let wt = session.worktree_path();
                let mut spans = vec![
                    Span::styled(indicator, indicator_style),
                    Span::styled(continuation, tree_style),
                    Span::styled(branch_char, tree_style),
                    Span::styled(
                        format!("{status_icon} "),
                        Style::default().fg(status_color),
                    ),
                ];
                if wt.is_some() {
                    spans.push(Span::styled(
                        "\u{e0a0} ",
                        Style::default().fg(TREE),
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
        .border_type(BorderType::Rounded)
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

    let tab_style = |active: bool| if active { active_style } else { inactive_style };
    let sep_span = Span::styled(" │ ", Style::default().fg(TREE));

    let mut tab_spans = vec![
        Span::styled(" agent", tab_style(app.preview_mode == PreviewMode::Output)),
        sep_span.clone(),
        Span::styled("diff", tab_style(app.preview_mode == PreviewMode::Diff)),
    ];

    // Add terminal tabs
    if let Some(app::ListItem::Session { session, .. }) = app.selected_item() {
        let term_count = app
            .terminal_counts
            .get(&session.name)
            .copied()
            .unwrap_or(0);
        for i in 0..term_count {
            tab_spans.push(sep_span.clone());
            let label = format!("term{}", i + 1);
            tab_spans.push(Span::styled(
                label,
                tab_style(app.preview_mode == PreviewMode::Terminal(i)),
            ));
        }
    }

    let tabs = Paragraph::new(Line::from(tab_spans));
    f.render_widget(tabs, rows[0]);

    // Draw separator line
    let sep = "─".repeat(rows[1].width as usize);
    let separator = Paragraph::new(Span::styled(sep, Style::default().fg(TREE)));
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
        PreviewMode::Output | PreviewMode::Terminal(_) => {
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
        PreviewMode::Context => {}
    }
}

fn draw_task_diff_panel(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
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

    let active_style = Style::default().fg(ACCENT).add_modifier(Modifier::BOLD);
    let inactive_style = Style::default().fg(MUTED);
    let tab_style = |active: bool| if active { active_style } else { inactive_style };
    let sep_span = Span::styled(" │ ", Style::default().fg(TREE));
    let is_context = app.preview_mode == PreviewMode::Context;

    let tab = Paragraph::new(Line::from(vec![
        Span::styled(" context", tab_style(is_context)),
        sep_span,
        Span::styled("diff", tab_style(!is_context)),
    ]));
    f.render_widget(tab, rows[0]);

    let sep = "─".repeat(rows[1].width as usize);
    let separator = Paragraph::new(Span::styled(sep, Style::default().fg(TREE)));
    f.render_widget(separator, rows[1]);

    let content_area = rows[2];
    let visible_height = content_area.height as usize;
    if visible_height == 0 {
        return;
    }

    if is_context {
        match &app.task_context_content {
            Some(content) => {
                let text = match content.as_bytes().into_text() {
                    Ok(text) => text,
                    Err(_) => return,
                };
                let visible_lines: Vec<Line> = text
                    .lines
                    .into_iter()
                    .take(visible_height)
                    .collect();
                let paragraph = Paragraph::new(visible_lines);
                f.render_widget(paragraph, content_area);
            }
            None => {
                let msg = Paragraph::new(Span::styled("No task context", Style::default().fg(MUTED)));
                f.render_widget(msg, content_area);
            }
        }
    } else {
        let stats = match &app.task_diff {
            Some(stats) => stats,
            None => {
                render_loading(f, app, content_area);
                return;
            }
        };
        render_diff_with_stats(f, &stats.diff_output, stats.added, stats.removed, content_area, visible_height, app.preview_scroll);
    }
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

fn style_diff_lines(content: &str, width: usize) -> Vec<Line<'_>> {
    let mut lines = Vec::new();
    let mut first_file = true;

    for line in content.lines() {
        if line.starts_with("diff ") {
            // Extract filename from "diff --git a/path b/path"
            let filename = line
                .split(" b/")
                .nth(1)
                .unwrap_or(line);
            if !first_file {
                lines.push(Line::raw(""));
            }
            first_file = false;
            let sep_len = width.saturating_sub(filename.len() + 3);
            let sep = "─".repeat(sep_len);
            lines.push(Line::from(vec![
                Span::styled(
                    format!("── {filename} "),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(sep, Style::default().fg(TREE)),
            ]));
        } else if line.starts_with("index ") || line.starts_with("---") || line.starts_with("+++")
        {
            // Skip verbose git diff metadata
            continue;
        } else if line.starts_with("@@") {
            lines.push(Line::styled(line, Style::default().fg(Color::Cyan)));
        } else if line.starts_with('+') {
            lines.push(Line::styled(line, Style::default().fg(Color::Green)));
        } else if line.starts_with('-') {
            lines.push(Line::styled(line, Style::default().fg(Color::Red)));
        } else {
            lines.push(Line::raw(line));
        }
    }
    lines
}

fn render_diff_content(f: &mut Frame, content: &str, area: Rect, visible_height: usize, scroll: usize) {
    let diff_lines = style_diff_lines(content, area.width as usize);
    let visible_lines: Vec<Line> = diff_lines.into_iter().skip(scroll).take(visible_height).collect();
    let paragraph = Paragraph::new(visible_lines);
    f.render_widget(paragraph, area);
}

fn render_diff_with_stats(f: &mut Frame, content: &str, added: usize, removed: usize, area: Rect, visible_height: usize, scroll: usize) {
    // Extract changed file names from diff headers
    let files: Vec<&str> = content
        .lines()
        .filter_map(|line| {
            line.strip_prefix("+++ b/")
                .or_else(|| line.strip_prefix("+++ ").filter(|s| *s != "/dev/null"))
        })
        .collect();

    // Build sticky header
    let mut header_lines: Vec<Line> = Vec::new();
    header_lines.push(Line::from(vec![
        Span::styled(
            format!("+{added}"),
            Style::default().fg(Color::Green),
        ),
        Span::styled(",", Style::default().fg(MUTED)),
        Span::styled(
            format!("-{removed}"),
            Style::default().fg(Color::Red),
        ),
        Span::styled(
            format!("  {} file(s)", files.len()),
            Style::default().fg(MUTED),
        ),
    ]));
    header_lines.push(Line::raw(""));

    let header_height = header_lines.len().min(visible_height);
    let remaining_height = visible_height.saturating_sub(header_height);

    // Render sticky header
    let header_area = Rect {
        height: header_height as u16,
        ..area
    };
    let header_paragraph = Paragraph::new(header_lines);
    f.render_widget(header_paragraph, header_area);

    // Render scrollable diff content below
    if remaining_height > 0 {
        let diff_area = Rect {
            y: area.y + header_height as u16,
            height: remaining_height as u16,
            ..area
        };
        let diff_lines = style_diff_lines(content, diff_area.width as usize);
        let visible_lines: Vec<Line> = diff_lines.into_iter().skip(scroll).take(remaining_height).collect();
        let paragraph = Paragraph::new(visible_lines);
        f.render_widget(paragraph, diff_area);
    }
}

fn draw_help(f: &mut Frame, app: &App, area: Rect) {
    let help_spans = match app.input_mode {
        InputMode::Normal => {
            help_bar(&[
                ("t", "task"), ("n/N", "session"), ("⏎", "attach"), ("␣", "collapse"),
                ("d", "del"), ("R", "rename"), ("m", "merge"), ("u", "update"),
                ("P", "push"), ("o", "PR"), ("b", "checkout"),
                ("c/x", "term"), ("⇥", "switch"), ("J/K", "scroll"),
                ("a", "project"), ("q", "quit"),
            ])
        }
        InputMode::AddProjectName
        | InputMode::AddSessionName
        | InputMode::AddSessionPrompt
        | InputMode::AddTaskName
        | InputMode::AddTaskBranch
        | InputMode::RenameProject
        | InputMode::RenameTask
        | InputMode::RenameSession
        | InputMode::MergeCommitMessage => {
            help_bar(&[("⏎", "confirm"), ("Esc", "cancel")])
        }
        InputMode::ConfirmDelete | InputMode::ConfirmCreatePr => {
            help_bar(&[("y", "confirm"), ("n/Esc", "cancel")])
        }
    };

    let help = Paragraph::new(Line::from(help_spans));
    f.render_widget(help, area);
}

fn help_bar(items: &[(&str, &str)]) -> Vec<Span<'static>> {
    let key_style = Style::default().fg(Color::Rgb(140, 140, 150));
    let desc_style = Style::default().fg(MUTED);
    let sep_style = Style::default().fg(Color::Rgb(50, 50, 60));

    let mut spans = Vec::new();
    for (i, (key, desc)) in items.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", sep_style));
        }
        spans.push(Span::styled(key.to_string(), key_style));
        spans.push(Span::styled(format!(" {desc}"), desc_style));
    }
    spans
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    // Show PR URL when a task with a PR is selected and no other status message
    if app.status_message.is_none() && app.input_mode == InputMode::Normal {
        if let Some(app::ListItem::Task { task, .. }) = app.selected_item() {
            if let Some(url) = app.pr_urls.get(&task.branch) {
                let pr_line = Paragraph::new(Line::from(vec![
                    Span::styled("\u{e728} ", Style::default().fg(Color::Magenta)),
                    Span::styled(url.as_str(), Style::default().fg(MUTED)),
                ]));
                f.render_widget(pr_line, area);
                return;
            }
        }
    }
    if let Some(msg) = &app.status_message {
        let style = if msg.starts_with("Error") {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::Yellow)
        };

        let content = if app.loading {
            let spinner = LOADING_SPINNER[app.tick % LOADING_SPINNER.len()];
            format!("{spinner} {msg}")
        } else if matches!(
            app.input_mode,
            InputMode::AddProjectName
                | InputMode::AddSessionName
                | InputMode::AddSessionPrompt
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
