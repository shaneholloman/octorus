use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
    Frame,
};

use crate::ai::{RallyState, ReviewAction, RevieweeStatus};
use crate::app::{AiRallyState, App};

pub fn render(frame: &mut Frame, app: &App) {
    let Some(rally_state) = &app.ai_rally_state else {
        return;
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(10),   // Main content
            Constraint::Length(3), // Status bar
        ])
        .split(frame.area());

    render_header(frame, chunks[0], rally_state);
    render_main_content(frame, chunks[1], rally_state);
    render_status_bar(frame, chunks[2], rally_state);
}

fn render_header(frame: &mut Frame, area: Rect, state: &AiRallyState) {
    let state_text = match state.state {
        RallyState::Initializing => "Initializing...",
        RallyState::ReviewerReviewing => "Reviewer reviewing...",
        RallyState::RevieweeFix => "Reviewee fixing...",
        RallyState::WaitingForClarification => "Waiting for clarification",
        RallyState::WaitingForPermission => "Waiting for permission",
        RallyState::Completed => "Completed!",
        RallyState::Error => "Error",
    };

    let state_color = match state.state {
        RallyState::Initializing => Color::Blue,
        RallyState::ReviewerReviewing => Color::Yellow,
        RallyState::RevieweeFix => Color::Cyan,
        RallyState::WaitingForClarification | RallyState::WaitingForPermission => Color::Magenta,
        RallyState::Completed => Color::Green,
        RallyState::Error => Color::Red,
    };

    let title = format!(
        " AI Rally - Iteration {}/{} ",
        state.iteration, state.max_iterations
    );

    let header = Paragraph::new(Line::from(vec![
        Span::styled("Status: ", Style::default().fg(Color::Gray)),
        Span::styled(state_text, Style::default().fg(state_color).add_modifier(Modifier::BOLD)),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(Style::default().fg(state_color)),
    );

    frame.render_widget(header, area);
}

fn render_main_content(frame: &mut Frame, area: Rect, state: &AiRallyState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(60), // History
            Constraint::Percentage(40), // Logs
        ])
        .split(area);

    render_history(frame, chunks[0], state);
    render_logs(frame, chunks[1], state);
}

fn render_history(frame: &mut Frame, area: Rect, state: &AiRallyState) {
    let items: Vec<ListItem> = state
        .history
        .iter()
        .map(|event| {
            let (prefix, content, color) = match event {
                crate::ai::orchestrator::RallyEvent::IterationStarted(i) => {
                    (format!("[{}]", i), "Iteration started".to_string(), Color::Blue)
                }
                crate::ai::orchestrator::RallyEvent::ReviewCompleted(review) => {
                    let action_text = match review.action {
                        ReviewAction::Approve => "APPROVE",
                        ReviewAction::RequestChanges => "REQUEST_CHANGES",
                        ReviewAction::Comment => "COMMENT",
                    };
                    let color = match review.action {
                        ReviewAction::Approve => Color::Green,
                        ReviewAction::RequestChanges => Color::Red,
                        ReviewAction::Comment => Color::Yellow,
                    };
                    (
                        format!("Review: {}", action_text),
                        truncate_string(&review.summary, 60),
                        color,
                    )
                }
                crate::ai::orchestrator::RallyEvent::FixCompleted(fix) => {
                    let status_text = match fix.status {
                        RevieweeStatus::Completed => "COMPLETED",
                        RevieweeStatus::NeedsClarification => "NEEDS_CLARIFICATION",
                        RevieweeStatus::NeedsPermission => "NEEDS_PERMISSION",
                        RevieweeStatus::Error => "ERROR",
                    };
                    let color = match fix.status {
                        RevieweeStatus::Completed => Color::Green,
                        RevieweeStatus::NeedsClarification | RevieweeStatus::NeedsPermission => {
                            Color::Yellow
                        }
                        RevieweeStatus::Error => Color::Red,
                    };
                    (
                        format!("Fix: {}", status_text),
                        truncate_string(&fix.summary, 60),
                        color,
                    )
                }
                crate::ai::orchestrator::RallyEvent::ClarificationNeeded(q) => {
                    ("Clarification".to_string(), truncate_string(q, 60), Color::Magenta)
                }
                crate::ai::orchestrator::RallyEvent::PermissionNeeded(action, _) => (
                    "Permission".to_string(),
                    truncate_string(action, 60),
                    Color::Magenta,
                ),
                crate::ai::orchestrator::RallyEvent::Approved(summary) => {
                    ("APPROVED".to_string(), truncate_string(summary, 60), Color::Green)
                }
                crate::ai::orchestrator::RallyEvent::Error(e) => {
                    ("ERROR".to_string(), truncate_string(e, 60), Color::Red)
                }
                _ => ("".to_string(), "".to_string(), Color::Gray),
            };

            if prefix.is_empty() {
                ListItem::new(Line::from(vec![]))
            } else {
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{}: ", prefix),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(content, Style::default().fg(Color::White)),
                ]))
            }
        })
        .filter(|item| !item.height() == 0)
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" History ")
                .border_style(Style::default().fg(Color::Gray)),
        );

    frame.render_widget(list, area);
}

fn render_logs(frame: &mut Frame, area: Rect, state: &AiRallyState) {
    let log_text = state
        .logs
        .iter()
        .rev()
        .take(10)
        .rev()
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");

    let logs = Paragraph::new(log_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Logs ")
                .border_style(Style::default().fg(Color::Gray)),
        )
        .wrap(Wrap { trim: true });

    frame.render_widget(logs, area);
}

fn render_status_bar(frame: &mut Frame, area: Rect, state: &AiRallyState) {
    let help_text = match state.state {
        RallyState::WaitingForClarification => "y: Answer | n: Skip | q: Abort",
        RallyState::WaitingForPermission => "y: Grant | n: Deny | q: Abort",
        RallyState::Completed => "q: Close",
        RallyState::Error => "r: Retry | q: Close",
        _ => "q: Abort",
    };

    let status_bar = Paragraph::new(Line::from(vec![
        Span::styled(help_text, Style::default().fg(Color::Cyan)),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );

    frame.render_widget(status_bar, area);
}

fn truncate_string(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}
