use ansi_to_tui::IntoText;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Frame;

use crate::app::{self, App, InputMode};

const ACCENT: Color = Color::Cyan;
const MUTED: Color = Color::DarkGray;
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
        Constraint::Length(2),
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
        draw_preview(f, app, columns[1]);
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

fn draw_list(f: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<ListItem> = Vec::new();

    for (i, item) in app.items.iter().enumerate() {
        let is_selected = i == app.selected;

        if matches!(item, app::ListItem::Project(_)) && !lines.is_empty() {
            lines.push(ListItem::new(Line::raw("")));
        }

        match item {
            app::ListItem::Project(project) => {
                let indicator = if is_selected { "> " } else { "  " };
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
                    Span::styled(
                        indicator,
                        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(&project.name, name_style),
                    Span::styled(format!("  {}", project.path), Style::default().fg(MUTED)),
                ]);
                lines.push(ListItem::new(line));
            }
            app::ListItem::Session(session) => {
                let indicator = if is_selected { "> " } else { "  " };
                let style = if is_selected {
                    Style::default()
                        .fg(SESSION_COLOR)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(SESSION_COLOR)
                };
                let line = Line::from(vec![
                    Span::styled(
                        indicator,
                        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("    "),
                    Span::styled(&session.session_name, style),
                ]);
                lines.push(ListItem::new(line));
            }
        }
    }

    let list = List::new(lines).block(Block::default().borders(Borders::NONE));
    f.render_widget(list, area);
}

fn draw_preview(f: &mut Frame, app: &App, area: Rect) {
    let content = app.preview_content.as_deref().unwrap_or("");

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(MUTED));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let visible_height = inner.height as usize;
    if visible_height == 0 {
        return;
    }

    // Parse ANSI escape sequences into styled ratatui Text
    let text = match content.as_bytes().into_text() {
        Ok(text) => text,
        Err(_) => return,
    };

    // Show the last N lines that fit the preview area
    let total_lines = text.lines.len();
    let start = total_lines.saturating_sub(visible_height);
    let visible_lines: Vec<Line> = text.lines.into_iter().skip(start).collect();

    let paragraph = Paragraph::new(visible_lines);
    f.render_widget(paragraph, inner);
}

fn draw_help(f: &mut Frame, app: &App, area: Rect) {
    let help_text = match app.input_mode {
        InputMode::Normal => {
            "n: new session  N: no worktree  Enter: attach  d: delete  R: rename  a: add project  q: quit"
        }
        InputMode::AddProjectName
        | InputMode::AddSessionName
        | InputMode::RenameProject
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
                | InputMode::RenameProject
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
