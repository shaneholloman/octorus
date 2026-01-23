use anyhow::{anyhow, Context as AnyhowContext, Result};
use async_trait::async_trait;
use serde::Deserialize;
use std::io::Write;
use std::process::Stdio;
use tempfile::NamedTempFile;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::ai::adapter::{
    AgentAdapter, CommentSeverity, Context, PermissionRequest, ReviewAction, ReviewComment,
    RevieweeOutput, RevieweeStatus, ReviewerOutput,
};
use crate::ai::orchestrator::RallyEvent;

const REVIEWER_SCHEMA: &str = include_str!("../schemas/reviewer.json");
const REVIEWEE_SCHEMA: &str = include_str!("../schemas/reviewee.json");

/// Codex-specific errors
#[derive(Debug, Error)]
pub enum CodexError {
    #[error("Codex CLI not found. Install it with: npm install -g @openai/codex")]
    #[allow(dead_code)]
    CliNotFound,
    #[error("Codex authentication failed. Run 'codex auth' to authenticate")]
    AuthenticationFailed,
    #[error("Turn failed: {reason}")]
    TurnFailed { reason: String },
    #[error("Invalid JSON event: {0}")]
    #[allow(dead_code)]
    InvalidJsonEvent(#[from] serde_json::Error),
    #[error("Event channel closed")]
    #[allow(dead_code)]
    ChannelClosed,
}

/// OpenAI Codex CLI adapter
pub struct CodexAdapter {
    reviewer_session_id: Option<String>,
    reviewee_session_id: Option<String>,
    event_sender: Option<mpsc::Sender<RallyEvent>>,
}

impl CodexAdapter {
    pub fn new() -> Self {
        Self {
            reviewer_session_id: None,
            reviewee_session_id: None,
            event_sender: None,
        }
    }

    /// Check if Codex CLI is available
    #[allow(dead_code)]
    pub fn check_availability() -> Result<(), CodexError> {
        let output = std::process::Command::new("codex")
            .arg("--version")
            .output();

        match output {
            Ok(o) if o.status.success() => Ok(()),
            _ => Err(CodexError::CliNotFound),
        }
    }

    async fn send_event(&self, event: RallyEvent) {
        if let Some(ref sender) = self.event_sender {
            let _ = sender.send(event).await;
        }
    }

    /// Run Codex CLI with streaming JSON output
    async fn run_codex_streaming(
        &self,
        prompt: &str,
        schema: &str,
        full_auto: bool,
        working_dir: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<CodexResponse> {
        // Write schema to temporary file (Codex requires file path for --output-schema)
        let mut schema_file =
            NamedTempFile::new().context("Failed to create temporary schema file")?;
        schema_file
            .write_all(schema.as_bytes())
            .context("Failed to write schema to temporary file")?;

        let mut cmd = Command::new("codex");

        // Handle session resume
        if let Some(sid) = session_id {
            cmd.arg("exec").arg("resume").arg(sid);
            cmd.arg("--message").arg(prompt);
        } else {
            cmd.arg("exec").arg(prompt);
        }

        cmd.arg("--json");
        cmd.arg("--output-schema").arg(schema_file.path());

        // Set working directory
        if let Some(dir) = working_dir {
            cmd.arg("--cd").arg(dir);
        }

        // Set sandbox mode
        // - Reviewer: default (read-only)
        // - Reviewee: --full-auto (workspace-write)
        if full_auto {
            cmd.arg("--full-auto");
        }

        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let mut child = cmd.spawn().context("Failed to spawn codex process")?;

        let stdout = child.stdout.take().expect("stdout should be available");
        let stderr = child.stderr.take().expect("stderr should be available");

        let mut stdout_reader = BufReader::new(stdout).lines();
        let mut stderr_reader = BufReader::new(stderr).lines();

        let mut final_response: Option<CodexResponse> = None;
        let mut error_lines = Vec::new();
        let mut thread_id: Option<String> = None;

        // Process NDJSON stream
        loop {
            tokio::select! {
                line = stdout_reader.next_line() => {
                    match line {
                        Ok(Some(l)) => {
                            if l.trim().is_empty() {
                                continue;
                            }
                            // Parse Codex event
                            match serde_json::from_str::<CodexEvent>(&l) {
                                Ok(event) => {
                                    if let Some(result) = self.handle_codex_event(&event, &mut thread_id).await? {
                                        final_response = Some(result);
                                    }
                                }
                                Err(_) => {
                                    // Unknown event format, ignore
                                }
                            }
                        }
                        Ok(None) => break,
                        Err(e) => return Err(anyhow!("Error reading stdout: {}", e)),
                    }
                }
                line = stderr_reader.next_line() => {
                    match line {
                        Ok(Some(l)) => error_lines.push(l),
                        Ok(None) => {},
                        Err(e) => return Err(anyhow!("Error reading stderr: {}", e)),
                    }
                }
            }
        }

        let status = child
            .wait()
            .await
            .context("Failed to wait for codex process")?;

        // schema_file is dropped here and the temporary file is deleted

        if !status.success() {
            let stderr_output = error_lines.join("\n");

            // Check for authentication error
            if stderr_output.contains("auth") || stderr_output.contains("unauthorized") {
                return Err(CodexError::AuthenticationFailed.into());
            }

            return Err(anyhow!(
                "Codex process failed with status {}: {}",
                status,
                stderr_output
            ));
        }

        final_response.ok_or_else(|| anyhow!("No result received from codex"))
    }

    /// Handle Codex streaming event and convert to RallyEvent
    async fn handle_codex_event(
        &self,
        event: &CodexEvent,
        thread_id: &mut Option<String>,
    ) -> Result<Option<CodexResponse>> {
        match event {
            CodexEvent::ThreadStarted { thread_id: tid } => {
                *thread_id = Some(tid.clone());
                self.send_event(RallyEvent::AgentThinking("Starting...".to_string()))
                    .await;
            }
            CodexEvent::TurnStarted { .. } => {
                self.send_event(RallyEvent::AgentThinking("Processing...".to_string()))
                    .await;
            }
            CodexEvent::TurnCompleted { result, .. } => {
                if let Some(result_value) = result {
                    return Ok(Some(CodexResponse {
                        session_id: thread_id.clone().unwrap_or_default(),
                        result: Some(result_value.clone()),
                    }));
                }
            }
            CodexEvent::TurnFailed { error } => {
                return Err(CodexError::TurnFailed {
                    reason: error.clone(),
                }
                .into());
            }
            CodexEvent::ItemStarted { item } | CodexEvent::ItemUpdated { item } => {
                self.handle_item_event(item, false).await;
            }
            CodexEvent::ItemCompleted { item } => {
                self.handle_item_event(item, true).await;
            }
            CodexEvent::Unknown => {
                // Ignore unknown events
            }
        }
        Ok(None)
    }

    /// Handle item events (command, message, file_change)
    async fn handle_item_event(&self, item: &CodexItem, completed: bool) {
        match item {
            CodexItem::Command { command, output } => {
                if completed {
                    self.send_event(RallyEvent::AgentToolResult(
                        command.clone(),
                        output.clone().unwrap_or_else(|| "completed".to_string()),
                    ))
                    .await;
                } else {
                    self.send_event(RallyEvent::AgentToolUse(
                        command.clone(),
                        "running...".to_string(),
                    ))
                    .await;
                }
            }
            CodexItem::Message { content } => {
                if completed {
                    self.send_event(RallyEvent::AgentText(content.clone()))
                        .await;
                } else {
                    self.send_event(RallyEvent::AgentThinking(content.clone()))
                        .await;
                }
            }
            CodexItem::FileChange { path, .. } => {
                if completed {
                    self.send_event(RallyEvent::AgentToolResult(
                        format!("edit:{}", path),
                        "file modified".to_string(),
                    ))
                    .await;
                } else {
                    self.send_event(RallyEvent::AgentToolUse(
                        format!("edit:{}", path),
                        "modifying...".to_string(),
                    ))
                    .await;
                }
            }
            CodexItem::Unknown => {}
        }
    }
}

impl Default for CodexAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentAdapter for CodexAdapter {
    fn name(&self) -> &str {
        "codex"
    }

    fn set_event_sender(&mut self, sender: mpsc::Sender<RallyEvent>) {
        self.event_sender = Some(sender);
    }

    async fn run_reviewer(&mut self, prompt: &str, context: &Context) -> Result<ReviewerOutput> {
        // Reviewer runs in default sandbox mode (read-only)
        // Codex doesn't have fine-grained tool control like Claude's --allowedTools
        // Instead, it uses sandbox policies:
        // - default: read-only filesystem access
        // - full-auto: workspace write access
        let response = self
            .run_codex_streaming(
                prompt,
                REVIEWER_SCHEMA,
                false, // read-only sandbox for reviewer
                context.working_dir.as_deref(),
                None,
            )
            .await?;

        self.reviewer_session_id = Some(response.session_id.clone());

        parse_reviewer_output(&response)
    }

    async fn run_reviewee(&mut self, prompt: &str, context: &Context) -> Result<RevieweeOutput> {
        // Reviewee runs in full-auto mode (workspace-write)
        // NOTE: full-auto allows git push, but the prompt explicitly prohibits it
        let response = self
            .run_codex_streaming(
                prompt,
                REVIEWEE_SCHEMA,
                true, // full-auto sandbox for reviewee
                context.working_dir.as_deref(),
                None,
            )
            .await?;

        self.reviewee_session_id = Some(response.session_id.clone());

        parse_reviewee_output(&response)
    }

    async fn continue_reviewer(&mut self, message: &str) -> Result<ReviewerOutput> {
        let session_id = self
            .reviewer_session_id
            .as_ref()
            .ok_or_else(|| anyhow!("No reviewer session to continue"))?
            .clone();

        let response = self
            .run_codex_streaming(message, REVIEWER_SCHEMA, false, None, Some(&session_id))
            .await?;

        parse_reviewer_output(&response)
    }

    async fn continue_reviewee(&mut self, message: &str) -> Result<RevieweeOutput> {
        let session_id = self
            .reviewee_session_id
            .as_ref()
            .ok_or_else(|| anyhow!("No reviewee session to continue"))?
            .clone();

        let response = self
            .run_codex_streaming(message, REVIEWEE_SCHEMA, true, None, Some(&session_id))
            .await?;

        parse_reviewee_output(&response)
    }
}

// Codex event types (type-safe enum with serde tag)
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CodexEvent {
    #[serde(rename = "thread.started")]
    ThreadStarted { thread_id: String },
    #[serde(rename = "turn.started")]
    TurnStarted {
        #[serde(default)]
        #[allow(dead_code)]
        turn_id: Option<String>,
    },
    #[serde(rename = "turn.completed")]
    TurnCompleted {
        #[serde(default)]
        #[allow(dead_code)]
        turn_id: Option<String>,
        #[serde(default)]
        result: Option<serde_json::Value>,
    },
    #[serde(rename = "turn.failed")]
    TurnFailed { error: String },
    #[serde(rename = "item.started")]
    ItemStarted { item: CodexItem },
    #[serde(rename = "item.updated")]
    ItemUpdated { item: CodexItem },
    #[serde(rename = "item.completed")]
    ItemCompleted { item: CodexItem },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "item_type", rename_all = "snake_case")]
pub enum CodexItem {
    #[serde(rename = "command")]
    Command {
        command: String,
        #[serde(default)]
        output: Option<String>,
    },
    #[serde(rename = "message")]
    Message { content: String },
    #[serde(rename = "file_change")]
    FileChange {
        path: String,
        #[serde(default)]
        #[allow(dead_code)]
        diff: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

/// Codex response structure
#[derive(Debug)]
struct CodexResponse {
    session_id: String,
    result: Option<serde_json::Value>,
}

/// Raw reviewer output from Codex
#[derive(Debug, Deserialize)]
struct RawReviewerOutput {
    action: String,
    summary: String,
    comments: Vec<RawReviewComment>,
    blocking_issues: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RawReviewComment {
    path: String,
    line: u32,
    body: String,
    severity: String,
}

/// Raw reviewee output from Codex
#[derive(Debug, Deserialize)]
struct RawRevieweeOutput {
    status: String,
    summary: String,
    files_modified: Vec<String>,
    #[serde(default)]
    question: Option<String>,
    #[serde(default)]
    permission_request: Option<RawPermissionRequest>,
    #[serde(default)]
    error_details: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawPermissionRequest {
    action: String,
    reason: String,
}

fn parse_reviewer_output(response: &CodexResponse) -> Result<ReviewerOutput> {
    let result = response
        .result
        .as_ref()
        .ok_or_else(|| anyhow!("No result in codex response"))?;

    let raw: RawReviewerOutput =
        serde_json::from_value(result.clone()).context("Failed to parse reviewer output")?;

    let action = match raw.action.as_str() {
        "approve" => ReviewAction::Approve,
        "request_changes" => ReviewAction::RequestChanges,
        "comment" => ReviewAction::Comment,
        _ => return Err(anyhow!("Unknown review action: {}", raw.action)),
    };

    let comments = raw
        .comments
        .into_iter()
        .map(|c| {
            let severity = match c.severity.as_str() {
                "critical" => CommentSeverity::Critical,
                "major" => CommentSeverity::Major,
                "minor" => CommentSeverity::Minor,
                "suggestion" => CommentSeverity::Suggestion,
                _ => CommentSeverity::Minor,
            };
            ReviewComment {
                path: c.path,
                line: c.line,
                body: c.body,
                severity,
            }
        })
        .collect();

    Ok(ReviewerOutput {
        action,
        summary: raw.summary,
        comments,
        blocking_issues: raw.blocking_issues,
    })
}

fn parse_reviewee_output(response: &CodexResponse) -> Result<RevieweeOutput> {
    let result = response
        .result
        .as_ref()
        .ok_or_else(|| anyhow!("No result in codex response"))?;

    let raw: RawRevieweeOutput =
        serde_json::from_value(result.clone()).context("Failed to parse reviewee output")?;

    let status = match raw.status.as_str() {
        "completed" => RevieweeStatus::Completed,
        "needs_clarification" => RevieweeStatus::NeedsClarification,
        "needs_permission" => RevieweeStatus::NeedsPermission,
        "error" => RevieweeStatus::Error,
        _ => return Err(anyhow!("Unknown reviewee status: {}", raw.status)),
    };

    let permission_request = raw.permission_request.map(|p| PermissionRequest {
        action: p.action,
        reason: p.reason,
    });

    Ok(RevieweeOutput {
        status,
        summary: raw.summary,
        files_modified: raw.files_modified,
        question: raw.question,
        permission_request,
        error_details: raw.error_details,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_thread_started_event() {
        let json = r#"{"type": "thread.started", "thread_id": "thread_123"}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        match event {
            CodexEvent::ThreadStarted { thread_id } => {
                assert_eq!(thread_id, "thread_123");
            }
            _ => panic!("Expected ThreadStarted event"),
        }
    }

    #[test]
    fn test_parse_turn_completed_event() {
        let json = r#"{"type": "turn.completed", "turn_id": "turn_1", "result": {"action": "approve", "summary": "LGTM", "comments": [], "blocking_issues": []}}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        match event {
            CodexEvent::TurnCompleted { turn_id, result } => {
                assert_eq!(turn_id, Some("turn_1".to_string()));
                assert!(result.is_some());
            }
            _ => panic!("Expected TurnCompleted event"),
        }
    }

    #[test]
    fn test_parse_turn_failed_event() {
        let json = r#"{"type": "turn.failed", "error": "Something went wrong"}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        match event {
            CodexEvent::TurnFailed { error } => {
                assert_eq!(error, "Something went wrong");
            }
            _ => panic!("Expected TurnFailed event"),
        }
    }

    #[test]
    fn test_parse_item_command_event() {
        let json =
            r#"{"type": "item.started", "item": {"item_type": "command", "command": "ls -la"}}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        match event {
            CodexEvent::ItemStarted { item } => match item {
                CodexItem::Command { command, output } => {
                    assert_eq!(command, "ls -la");
                    assert!(output.is_none());
                }
                _ => panic!("Expected Command item"),
            },
            _ => panic!("Expected ItemStarted event"),
        }
    }

    #[test]
    fn test_parse_item_message_event() {
        let json =
            r#"{"type": "item.completed", "item": {"item_type": "message", "content": "Done!"}}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        match event {
            CodexEvent::ItemCompleted { item } => match item {
                CodexItem::Message { content } => {
                    assert_eq!(content, "Done!");
                }
                _ => panic!("Expected Message item"),
            },
            _ => panic!("Expected ItemCompleted event"),
        }
    }

    #[test]
    fn test_parse_item_file_change_event() {
        let json = r#"{"type": "item.updated", "item": {"item_type": "file_change", "path": "src/main.rs", "diff": "+new line"}}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        match event {
            CodexEvent::ItemUpdated { item } => match item {
                CodexItem::FileChange { path, diff } => {
                    assert_eq!(path, "src/main.rs");
                    assert_eq!(diff, Some("+new line".to_string()));
                }
                _ => panic!("Expected FileChange item"),
            },
            _ => panic!("Expected ItemUpdated event"),
        }
    }

    #[test]
    fn test_parse_unknown_event() {
        let json = r#"{"type": "some.unknown.event", "data": "whatever"}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        matches!(event, CodexEvent::Unknown);
    }

    #[test]
    fn test_parse_unknown_item() {
        let json =
            r#"{"type": "item.started", "item": {"item_type": "unknown_type", "foo": "bar"}}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        match event {
            CodexEvent::ItemStarted { item } => {
                matches!(item, CodexItem::Unknown);
            }
            _ => panic!("Expected ItemStarted event"),
        }
    }

    #[test]
    fn test_parse_reviewer_output() {
        let response = CodexResponse {
            session_id: "session_123".to_string(),
            result: Some(serde_json::json!({
                "action": "request_changes",
                "summary": "Found some issues",
                "comments": [
                    {
                        "path": "src/lib.rs",
                        "line": 42,
                        "body": "Consider using a constant here",
                        "severity": "suggestion"
                    }
                ],
                "blocking_issues": ["Missing error handling"]
            })),
        };

        let output = parse_reviewer_output(&response).unwrap();
        assert_eq!(output.action, ReviewAction::RequestChanges);
        assert_eq!(output.summary, "Found some issues");
        assert_eq!(output.comments.len(), 1);
        assert_eq!(output.comments[0].path, "src/lib.rs");
        assert_eq!(output.comments[0].line, 42);
        assert_eq!(output.comments[0].severity, CommentSeverity::Suggestion);
        assert_eq!(output.blocking_issues.len(), 1);
    }

    #[test]
    fn test_parse_reviewee_output() {
        let response = CodexResponse {
            session_id: "session_456".to_string(),
            result: Some(serde_json::json!({
                "status": "completed",
                "summary": "Fixed all issues",
                "files_modified": ["src/lib.rs", "src/main.rs"]
            })),
        };

        let output = parse_reviewee_output(&response).unwrap();
        assert_eq!(output.status, RevieweeStatus::Completed);
        assert_eq!(output.summary, "Fixed all issues");
        assert_eq!(output.files_modified.len(), 2);
    }

    #[test]
    fn test_parse_reviewee_needs_permission() {
        let response = CodexResponse {
            session_id: "session_789".to_string(),
            result: Some(serde_json::json!({
                "status": "needs_permission",
                "summary": "Need to run a command",
                "files_modified": [],
                "permission_request": {
                    "action": "run npm install",
                    "reason": "Required to install new dependency"
                }
            })),
        };

        let output = parse_reviewee_output(&response).unwrap();
        assert_eq!(output.status, RevieweeStatus::NeedsPermission);
        assert!(output.permission_request.is_some());
        let perm = output.permission_request.unwrap();
        assert_eq!(perm.action, "run npm install");
    }
}
