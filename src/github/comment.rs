use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::client::{gh_api, gh_api_post};
use super::pr::User;

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewComment {
    pub id: u64,
    pub path: String,
    pub line: Option<u32>,
    pub body: String,
    pub user: User,
    pub created_at: String,
}

#[allow(dead_code)]
pub async fn fetch_review_comments(repo: &str, pr_number: u32) -> Result<Vec<ReviewComment>> {
    let endpoint = format!("repos/{}/pulls/{}/comments", repo, pr_number);
    let json = gh_api(&endpoint).await?;
    serde_json::from_value(json).context("Failed to parse review comments response")
}

pub async fn create_review_comment(
    repo: &str,
    pr_number: u32,
    commit_id: &str,
    path: &str,
    line: u32,
    body: &str,
) -> Result<()> {
    let endpoint = format!("repos/{}/pulls/{}/comments", repo, pr_number);
    gh_api_post(
        &endpoint,
        &[
            ("body", body),
            ("commit_id", commit_id),
            ("path", path),
            ("line", &line.to_string()),
            ("side", "RIGHT"),
        ],
    )
    .await?;
    Ok(())
}
