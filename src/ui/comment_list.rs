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
            Constraint::Min(0),    // Comment list
            Constraint::Length(3), // Footer
        ])
        .split(frame.area());

    // Header
    let comment_count = app
        .review_comments
        .as_ref()
        .map(|c| c.len())
        .unwrap_or(0);
    let header = Paragraph::new(format!("Review Comments ({})", comment_count))
        .block(Block::default().borders(Borders::ALL).title("octorus"));
    frame.render_widget(header, chunks[0]);

    // Comment list
    if app.comments_loading {
        let loading = Paragraph::new("Loading comments...")
            .style(Style::default().fg(Color::Yellow))
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(loading, chunks[1]);
    } else if let Some(ref comments) = app.review_comments {
        if comments.is_empty() {
            let empty = Paragraph::new("No review comments found")
                .style(Style::default().fg(Color::DarkGray))
                .block(Block::default().borders(Borders::ALL));
            frame.render_widget(empty, chunks[1]);
        } else {
            let items: Vec<ListItem> = comments
                .iter()
                .enumerate()
                .map(|(i, comment)| {
                    let is_selected = i == app.selected_comment;
                    let prefix = if is_selected { "> " } else { "  " };

                    let style = if is_selected {
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    };

                    // First line: author, file, line
                    let line_info = comment
                        .line
                        .map(|l| format!(":{}", l))
                        .unwrap_or_default();
                    let header_line = Line::from(vec![
                        Span::raw(prefix),
                        Span::styled(
                            format!("@{}", comment.user.login),
                            Style::default().fg(Color::Cyan),
                        ),
                        Span::raw(" on "),
                        Span::styled(
                            format!("{}{}", comment.path, line_info),
                            Style::default().fg(Color::Green),
                        ),
                    ]);

                    // Second line: comment body (truncated)
                    let body_preview: String = comment
                        .body
                        .lines()
                        .next()
                        .unwrap_or("")
                        .chars()
                        .take(60)
                        .collect();
                    let body_line = Line::from(vec![
                        Span::raw("    "),
                        Span::styled(body_preview, style),
                    ]);

                    ListItem::new(vec![header_line, body_line, Line::from("")])
                })
                .collect();

            let list = List::new(items)
                .block(Block::default().borders(Borders::ALL))
                .highlight_style(Style::default().bg(Color::DarkGray));
            frame.render_widget(list, chunks[1]);
        }
    }

    // Footer
    let footer =
        Paragraph::new("j/k: move | Enter: jump to file | q/Esc: back")
            .block(Block::default().borders(Borders::ALL));
    frame.render_widget(footer, chunks[2]);
}
