use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Frame;

use crate::app::{self, App, InputMode};

const ACCENT: Color = Color::Cyan;
const MUTED: Color = Color::DarkGray;
const SESSION_COLOR: Color = Color::Green;

pub fn draw(f: &mut Frame, app: &App) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(5),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(f.area());

    draw_title(f, chunks[0]);
    draw_list(f, app, chunks[1]);
    draw_help(f, app, chunks[2]);
    draw_status(f, app, chunks[3]);
}

fn draw_title(f: &mut Frame, area: Rect) {
    let title = Paragraph::new(Line::from(vec![
        Span::styled("Claude Manager", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
    ]));
    f.render_widget(title, area);
}

fn draw_list(f: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let is_selected = i == app.selected;
            match item {
                app::ListItem::Project(project) => {
                    let style = if is_selected {
                        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
                    };
                    let line = Line::from(vec![
                        Span::styled(&project.name, style),
                        Span::styled(format!("  {}", project.path), Style::default().fg(MUTED)),
                    ]);
                    ListItem::new(line)
                }
                app::ListItem::Session(session) => {
                    let style = if is_selected {
                        Style::default().fg(SESSION_COLOR).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(SESSION_COLOR)
                    };
                    let line = Line::from(vec![
                        Span::raw("  "),
                        Span::styled(format!("  {}", session.session_name), style),
                    ]);
                    ListItem::new(line)
                }
            }
        })
        .collect();

    let cursor = if !app.items.is_empty() { ">" } else { "" };
    let list = List::new(items)
        .block(Block::default().borders(Borders::NONE))
        .highlight_symbol(cursor);

    f.render_widget(list, area);

    // Draw selection indicator manually
    if !app.items.is_empty() && app.selected < app.items.len() {
        let y = area.y + app.selected as u16;
        if y < area.y + area.height {
            let indicator = Paragraph::new(Span::styled(
                ">",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ));
            f.render_widget(indicator, Rect::new(area.x, y, 1, 1));
        }
    }
}

fn draw_help(f: &mut Frame, app: &App, area: Rect) {
    let help_text = match app.input_mode {
        InputMode::Normal => {
            "n: new session  N: new (no worktree)  Enter: attach  d: delete  a: add project  q: quit"
        }
        InputMode::AddProjectName | InputMode::AddSessionName => {
            "Enter: confirm  Esc: cancel"
        }
        InputMode::ConfirmDelete => {
            "y: confirm  n/Esc: cancel"
        }
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
            InputMode::AddProjectName | InputMode::AddSessionName
        ) {
            format!("{}{}", msg, app.input_buffer)
        } else {
            msg.clone()
        };

        let status = Paragraph::new(Span::styled(content, style));
        f.render_widget(status, area);
    }
}
