#![allow(dead_code)]

use anyhow::{anyhow, Context as AnyhowContext, Result};
use async_trait::async_trait;
use serde::Deserialize;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::ai::adapter::{
    AgentAdapter, Context, PermissionRequest, ReviewAction, ReviewComment, RevieweeOutput,
    RevieweeStatus, ReviewerOutput,
};

const REVIEWER_SCHEMA: &str = include_str!("../schemas/reviewer.json");
const REVIEWEE_SCHEMA: &str = include_str!("../schemas/reviewee.json");

/// Claude Code adapter
pub struct ClaudeAdapter {
    reviewer_session_id: Option<String>,
    reviewee_session_id: Option<String>,
}

impl ClaudeAdapter {
    pub fn new() -> Self {
        Self {
            reviewer_session_id: None,
            reviewee_session_id: None,
        }
    }

    async fn run_claude(
        &self,
        prompt: &str,
        schema: &str,
        allowed_tools: &str,
        working_dir: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<ClaudeResponse> {
        let mut cmd = Command::new("claude");
        cmd.arg("-p").arg(prompt);
        cmd.arg("--output-format").arg("json");
        cmd.arg("--json-schema").arg(schema);
        cmd.arg("--allowedTools").arg(allowed_tools);

        if let Some(session) = session_id {
            cmd.arg("--resume").arg(session);
        }

        if let Some(dir) = working_dir {
            cmd.current_dir(dir);
        }

        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let mut child = cmd.spawn().context("Failed to spawn claude process")?;

        let stdout = child.stdout.take().expect("stdout should be available");
        let stderr = child.stderr.take().expect("stderr should be available");

        // Read output line by line for streaming support
        let mut stdout_reader = BufReader::new(stdout).lines();
        let mut stderr_reader = BufReader::new(stderr).lines();

        let mut output_lines = Vec::new();
        let mut error_lines = Vec::new();

        // Read stdout and stderr concurrently
        loop {
            tokio::select! {
                line = stdout_reader.next_line() => {
                    match line {
                        Ok(Some(l)) => output_lines.push(l),
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

        let status = child.wait().await.context("Failed to wait for claude process")?;

        if !status.success() {
            let stderr_output = error_lines.join("\n");
            return Err(anyhow!(
                "Claude process failed with status {}: {}",
                status,
                stderr_output
            ));
        }

        let stdout_output = output_lines.join("\n");
        let response: ClaudeResponse = serde_json::from_str(&stdout_output)
            .context("Failed to parse claude output as JSON")?;

        Ok(response)
    }

    async fn continue_session(&self, session_id: &str, message: &str) -> Result<ClaudeResponse> {
        let mut cmd = Command::new("claude");
        cmd.arg("-p").arg(message);
        cmd.arg("--resume").arg(session_id);
        cmd.arg("--output-format").arg("json");

        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let output = cmd.output().await.context("Failed to execute claude")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("Claude process failed: {}", stderr));
        }

        let response: ClaudeResponse = serde_json::from_slice(&output.stdout)
            .context("Failed to parse claude output as JSON")?;

        Ok(response)
    }
}

impl Default for ClaudeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentAdapter for ClaudeAdapter {
    fn name(&self) -> &str {
        "claude"
    }

    async fn run_reviewer(&mut self, prompt: &str, context: &Context) -> Result<ReviewerOutput> {
        let allowed_tools = "Read,Glob,Grep,Bash(gh pr:*),Bash(gh api:*)";

        let response = self
            .run_claude(
                prompt,
                REVIEWER_SCHEMA,
                allowed_tools,
                context.working_dir.as_deref(),
                None,
            )
            .await?;

        self.reviewer_session_id = Some(response.session_id.clone());

        parse_reviewer_output(&response)
    }

    async fn run_reviewee(&mut self, prompt: &str, context: &Context) -> Result<RevieweeOutput> {
        let allowed_tools = "Read,Edit,Write,Glob,Grep,Bash(git:*),Bash(gh:*),Bash(cargo:*),Bash(npm:*),Bash(pnpm:*),Bash(bun:*)";

        let response = self
            .run_claude(
                prompt,
                REVIEWEE_SCHEMA,
                allowed_tools,
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

        let response = self.continue_session(&session_id, message).await?;
        parse_reviewer_output(&response)
    }

    async fn continue_reviewee(&mut self, message: &str) -> Result<RevieweeOutput> {
        let session_id = self
            .reviewee_session_id
            .as_ref()
            .ok_or_else(|| anyhow!("No reviewee session to continue"))?
            .clone();

        let response = self.continue_session(&session_id, message).await?;
        parse_reviewee_output(&response)
    }
}

/// Claude Code JSON output format
#[derive(Debug, Deserialize)]
struct ClaudeResponse {
    session_id: String,
    #[serde(default)]
    result: Option<serde_json::Value>,
    #[serde(default)]
    cost_usd: Option<f64>,
    #[serde(default)]
    duration_ms: Option<u64>,
}

/// Raw reviewer output from Claude
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

/// Raw reviewee output from Claude
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

fn parse_reviewer_output(response: &ClaudeResponse) -> Result<ReviewerOutput> {
    let result = response
        .result
        .as_ref()
        .ok_or_else(|| anyhow!("No result in claude response"))?;

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
                "critical" => crate::ai::adapter::CommentSeverity::Critical,
                "major" => crate::ai::adapter::CommentSeverity::Major,
                "minor" => crate::ai::adapter::CommentSeverity::Minor,
                "suggestion" => crate::ai::adapter::CommentSeverity::Suggestion,
                _ => crate::ai::adapter::CommentSeverity::Minor,
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

fn parse_reviewee_output(response: &ClaudeResponse) -> Result<RevieweeOutput> {
    let result = response
        .result
        .as_ref()
        .ok_or_else(|| anyhow!("No result in claude response"))?;

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
