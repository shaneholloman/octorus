use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame,
};

use crate::app::App;

pub fn render(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(0),    // File list
            Constraint::Length(3), // Footer
        ])
        .split(frame.area());

    // Header
    let header = Paragraph::new(format!(
        "PR #{}: {} by @{}",
        app.pr.number, app.pr.title, app.pr.user.login
    ))
    .block(Block::default().borders(Borders::ALL).title("hxpr"));
    frame.render_widget(header, chunks[0]);

    // File list
    let items: Vec<ListItem> = app
        .files
        .iter()
        .enumerate()
        .map(|(i, file)| {
            let style = if i == app.selected_file {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            let status_color = match file.status.as_str() {
                "added" => Color::Green,
                "removed" => Color::Red,
                "modified" => Color::Yellow,
                _ => Color::White,
            };

            let status_char = match file.status.as_str() {
                "added" => 'A',
                "removed" => 'D',
                "modified" => 'M',
                "renamed" => 'R',
                _ => '?',
            };

            let line = Line::from(vec![
                Span::styled(
                    format!("[{}] ", status_char),
                    Style::default().fg(status_color),
                ),
                Span::styled(&file.filename, style),
                Span::raw(format!(" +{} -{}", file.additions, file.deletions)),
            ]);

            ListItem::new(line)
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("Changed Files ({})", app.files.len())),
        )
        .highlight_style(Style::default().bg(Color::DarkGray));
    frame.render_widget(list, chunks[1]);

    // Footer
    let footer = Paragraph::new(
        "j/k: move | Enter: view diff | a: approve | r: request changes | m: comment | q: quit | ?: help",
    )
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(footer, chunks[2]);
}
