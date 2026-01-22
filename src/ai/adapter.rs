#![allow(dead_code)]

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Context information passed to agents
#[derive(Debug, Clone)]
pub struct Context {
    pub repo: String,
    pub pr_number: u32,
    pub pr_title: String,
    pub pr_body: Option<String>,
    pub diff: String,
    pub working_dir: Option<String>,
}

/// Review action from reviewer
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewAction {
    Approve,
    RequestChanges,
    Comment,
}

/// Comment from reviewer with location and severity
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewComment {
    pub path: String,
    pub line: u32,
    pub body: String,
    pub severity: CommentSeverity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommentSeverity {
    Critical,
    Major,
    Minor,
    Suggestion,
}

/// Output from reviewer agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewerOutput {
    pub action: ReviewAction,
    pub summary: String,
    pub comments: Vec<ReviewComment>,
    pub blocking_issues: Vec<String>,
}

/// Status from reviewee agent
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevieweeStatus {
    Completed,
    NeedsClarification,
    NeedsPermission,
    Error,
}

/// Permission request from reviewee
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub action: String,
    pub reason: String,
}

/// Output from reviewee agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevieweeOutput {
    pub status: RevieweeStatus,
    pub summary: String,
    pub files_modified: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_request: Option<PermissionRequest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_details: Option<String>,
}

/// Trait for agent adapters
#[async_trait]
pub trait AgentAdapter: Send + Sync {
    /// Agent name (claude, codex, gemini, etc.)
    fn name(&self) -> &str;

    /// Run as reviewer
    async fn run_reviewer(&mut self, prompt: &str, context: &Context) -> Result<ReviewerOutput>;

    /// Run as reviewee
    async fn run_reviewee(&mut self, prompt: &str, context: &Context) -> Result<RevieweeOutput>;

    /// Continue reviewer session (for clarification answers)
    async fn continue_reviewer(&mut self, message: &str) -> Result<ReviewerOutput>;

    /// Continue reviewee session (for permission grants or clarification answers)
    async fn continue_reviewee(&mut self, message: &str) -> Result<RevieweeOutput>;
}

/// Supported agent types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportedAgent {
    Claude,
    // Codex,  // Future
    // Gemini, // Future
}

impl SupportedAgent {
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_lowercase().as_str() {
            "claude" => Some(Self::Claude),
            // "codex" => Some(Self::Codex),
            // "gemini" => Some(Self::Gemini),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Claude => "claude",
            // Self::Codex => "codex",
            // Self::Gemini => "gemini",
        }
    }
}
