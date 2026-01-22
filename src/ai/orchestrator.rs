#![allow(dead_code)]

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;

use crate::config::AiConfig;

use super::adapter::{AgentAdapter, Context, ReviewAction, RevieweeOutput, RevieweeStatus, ReviewerOutput};
use super::adapters::create_adapter;
use super::prompts::{
    build_clarification_prompt, build_permission_granted_prompt, build_rereview_prompt,
    build_reviewee_prompt, build_reviewer_prompt,
};
use super::session::{
    write_history_entry, write_session, HistoryEntryType, RallySession,
};

/// Rally state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RallyState {
    Initializing,
    ReviewerReviewing,
    RevieweeFix,
    WaitingForClarification,
    WaitingForPermission,
    Completed,
    Error,
}

/// Event emitted during rally for TUI updates
#[derive(Debug, Clone)]
pub enum RallyEvent {
    StateChanged(RallyState),
    IterationStarted(u32),
    ReviewCompleted(ReviewerOutput),
    FixCompleted(RevieweeOutput),
    ClarificationNeeded(String),
    PermissionNeeded(String, String), // action, reason
    Approved(String),                 // summary
    Error(String),
    Log(String),
}

/// Result of the rally process
#[derive(Debug)]
pub enum RallyResult {
    Approved {
        iteration: u32,
        summary: String,
    },
    MaxIterationsReached {
        iteration: u32,
    },
    Aborted {
        iteration: u32,
        reason: String,
    },
    Error {
        iteration: u32,
        error: String,
    },
}

/// Main orchestrator for AI rally
pub struct Orchestrator {
    repo: String,
    pr_number: u32,
    config: AiConfig,
    reviewer_adapter: Box<dyn AgentAdapter>,
    reviewee_adapter: Box<dyn AgentAdapter>,
    session: RallySession,
    context: Option<Context>,
    last_review: Option<ReviewerOutput>,
    event_sender: mpsc::Sender<RallyEvent>,
}

impl Orchestrator {
    pub fn new(
        repo: &str,
        pr_number: u32,
        config: AiConfig,
        event_sender: mpsc::Sender<RallyEvent>,
    ) -> Result<Self> {
        let reviewer_adapter = create_adapter(&config.reviewer)?;
        let reviewee_adapter = create_adapter(&config.reviewee)?;
        let session = RallySession::new(repo, pr_number);

        Ok(Self {
            repo: repo.to_string(),
            pr_number,
            config,
            reviewer_adapter,
            reviewee_adapter,
            session,
            context: None,
            last_review: None,
            event_sender,
        })
    }

    /// Set the context for the rally
    pub fn set_context(&mut self, context: Context) {
        self.context = Some(context);
    }

    /// Run the rally process
    pub async fn run(&mut self) -> Result<RallyResult> {
        let context = self
            .context
            .as_ref()
            .ok_or_else(|| anyhow!("Context not set"))?
            .clone();

        self.send_event(RallyEvent::StateChanged(RallyState::Initializing))
            .await;

        // Main loop
        while self.session.iteration < self.config.max_iterations {
            self.session.increment_iteration();
            let iteration = self.session.iteration;

            self.send_event(RallyEvent::IterationStarted(iteration))
                .await;
            self.send_event(RallyEvent::Log(format!(
                "Starting iteration {}",
                iteration
            )))
            .await;

            // Run reviewer
            self.session.update_state(RallyState::ReviewerReviewing);
            self.send_event(RallyEvent::StateChanged(RallyState::ReviewerReviewing))
                .await;
            write_session(&self.session)?;

            let review_result = self.run_reviewer_with_timeout(&context, iteration).await?;

            // Store the review for later use
            write_history_entry(
                &self.repo,
                self.pr_number,
                iteration,
                &HistoryEntryType::Review(review_result.clone()),
            )?;

            self.send_event(RallyEvent::ReviewCompleted(review_result.clone()))
                .await;
            self.last_review = Some(review_result.clone());

            // Check for approval
            if review_result.action == ReviewAction::Approve {
                self.session.update_state(RallyState::Completed);
                write_session(&self.session)?;

                self.send_event(RallyEvent::Approved(review_result.summary.clone()))
                    .await;
                self.send_event(RallyEvent::StateChanged(RallyState::Completed))
                    .await;

                return Ok(RallyResult::Approved {
                    iteration,
                    summary: review_result.summary,
                });
            }

            // Run reviewee to fix issues
            self.session.update_state(RallyState::RevieweeFix);
            self.send_event(RallyEvent::StateChanged(RallyState::RevieweeFix))
                .await;
            write_session(&self.session)?;

            let fix_result = self
                .run_reviewee_with_timeout(&context, &review_result, iteration)
                .await?;

            write_history_entry(
                &self.repo,
                self.pr_number,
                iteration,
                &HistoryEntryType::Fix(fix_result.clone()),
            )?;

            self.send_event(RallyEvent::FixCompleted(fix_result.clone()))
                .await;

            // Handle reviewee status
            match fix_result.status {
                RevieweeStatus::Completed => {
                    self.send_event(RallyEvent::Log(format!(
                        "Fix completed: {}",
                        fix_result.summary
                    )))
                    .await;
                    // Continue to next iteration
                }
                RevieweeStatus::NeedsClarification => {
                    if let Some(question) = &fix_result.question {
                        self.session.update_state(RallyState::WaitingForClarification);
                        write_session(&self.session)?;

                        self.send_event(RallyEvent::ClarificationNeeded(question.clone()))
                            .await;
                        self.send_event(RallyEvent::StateChanged(
                            RallyState::WaitingForClarification,
                        ))
                        .await;

                        // In the TUI, this will pause and wait for user input
                        // For now, we'll return and let the caller handle it
                        return Ok(RallyResult::Aborted {
                            iteration,
                            reason: format!("Clarification needed: {}", question),
                        });
                    }
                }
                RevieweeStatus::NeedsPermission => {
                    if let Some(perm) = &fix_result.permission_request {
                        self.session.update_state(RallyState::WaitingForPermission);
                        write_session(&self.session)?;

                        self.send_event(RallyEvent::PermissionNeeded(
                            perm.action.clone(),
                            perm.reason.clone(),
                        ))
                        .await;
                        self.send_event(RallyEvent::StateChanged(RallyState::WaitingForPermission))
                            .await;

                        return Ok(RallyResult::Aborted {
                            iteration,
                            reason: format!("Permission needed: {}", perm.action),
                        });
                    }
                }
                RevieweeStatus::Error => {
                    self.session.update_state(RallyState::Error);
                    write_session(&self.session)?;

                    let error = fix_result
                        .error_details
                        .unwrap_or_else(|| "Unknown error".to_string());
                    self.send_event(RallyEvent::Error(error.clone())).await;
                    self.send_event(RallyEvent::StateChanged(RallyState::Error))
                        .await;

                    return Ok(RallyResult::Error {
                        iteration,
                        error,
                    });
                }
            }
        }

        self.send_event(RallyEvent::Log(format!(
            "Max iterations ({}) reached",
            self.config.max_iterations
        )))
        .await;

        Ok(RallyResult::MaxIterationsReached {
            iteration: self.session.iteration,
        })
    }

    /// Continue after clarification answer
    pub async fn continue_with_clarification(&mut self, answer: &str) -> Result<()> {
        // Ask reviewer for clarification
        let prompt = build_clarification_prompt(answer);
        let _ = self.reviewer_adapter.continue_reviewer(&prompt).await?;

        // Continue reviewee with the answer
        self.reviewee_adapter.continue_reviewee(answer).await?;

        self.session.update_state(RallyState::RevieweeFix);
        write_session(&self.session)?;

        Ok(())
    }

    /// Continue after permission granted
    pub async fn continue_with_permission(&mut self, action: &str) -> Result<()> {
        let prompt = build_permission_granted_prompt(action);
        self.reviewee_adapter.continue_reviewee(&prompt).await?;

        self.session.update_state(RallyState::RevieweeFix);
        write_session(&self.session)?;

        Ok(())
    }

    async fn run_reviewer_with_timeout(
        &mut self,
        context: &Context,
        iteration: u32,
    ) -> Result<ReviewerOutput> {
        let prompt = if iteration == 1 {
            build_reviewer_prompt(context, iteration)
        } else {
            // Re-review after fixes
            let changes_summary = self
                .last_review
                .as_ref()
                .map(|r| r.summary.clone())
                .unwrap_or_default();
            build_rereview_prompt(context, iteration, &changes_summary)
        };

        let duration = Duration::from_secs(self.config.timeout_secs);

        timeout(duration, self.reviewer_adapter.run_reviewer(&prompt, context))
            .await
            .map_err(|_| anyhow!("Reviewer timeout after {} seconds", self.config.timeout_secs))?
    }

    async fn run_reviewee_with_timeout(
        &mut self,
        context: &Context,
        review: &ReviewerOutput,
        iteration: u32,
    ) -> Result<RevieweeOutput> {
        let prompt = build_reviewee_prompt(context, review, iteration);
        let duration = Duration::from_secs(self.config.timeout_secs);

        timeout(duration, self.reviewee_adapter.run_reviewee(&prompt, context))
            .await
            .map_err(|_| anyhow!("Reviewee timeout after {} seconds", self.config.timeout_secs))?
    }

    async fn send_event(&self, event: RallyEvent) {
        let _ = self.event_sender.send(event).await;
    }

    pub fn session(&self) -> &RallySession {
        &self.session
    }
}
