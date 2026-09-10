//! Structured provider failures for local session records and headless callers.
//! The TUI uses only the summary. Diagnostics never become model context.

use serde::{Deserialize, Serialize};

use crate::core::providers::{FailureCause, FailureStage, ProviderError, ResponseContext};

/// Why the terminal failure was not followed by another request.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RetryDecision {
    PartialOutput,
    NotRetryable,
    Disabled,
    Exhausted,
}

/// Facts about one failed attempt, without request bodies or authentication headers.
/// The session record's parent links it to the preceding work on its branch.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ErrorDetails {
    pub summary: String,
    pub detail: String,
    pub cause: FailureCause,
    pub stage: FailureStage,
    pub response: ResponseContext,
    pub provider_code: Option<String>,
    pub provider: String,
    pub model: String,
    pub timestamp_ms: u64,
    pub attempt_elapsed_ms: u64,
    pub step: u32,
    pub attempt: u32,
    pub max_attempts: u32,
    pub retry_after_secs: Option<u64>,
    pub partial_tool_argument_bytes: u64,
    pub retry_decision: RetryDecision,
    pub partial_text_bytes: usize,
    pub reasoning_received: bool,
    pub unexecuted_tool_calls: usize,
    pub settled_tools: usize,
    pub failed_tools: usize,
    pub recovery: String,
}

impl ErrorDetails {
    /// Read the file-overridable headline off the async worker, retaining its home.
    pub async fn summary(error: &ProviderError) -> String {
        let (key, default) = match error.cause {
            FailureCause::Auth => ("auth", "Provider authentication failed."),
            FailureCause::Network => ("network", "Could not connect to the provider."),
            FailureCause::Stalled => ("stalled", "Provider response interrupted."),
            FailureCause::RateLimited => ("rate_limited", "Provider rate limit reached."),
            FailureCause::QuotaExhausted => ("quota", "Provider quota exhausted."),
            FailureCause::ProviderUnavailable => ("unavailable", "Provider unavailable."),
            FailureCause::Rejected => ("rejected", "Provider request failed."),
        };
        let home = crate::core::config::home::home();
        tokio::task::spawn_blocking(move || {
            crate::core::config::home::with_home(home, || {
                crate::core::config::settings::get_string(&format!("error_{key}"))
            })
        })
        .await
        .ok()
        .flatten()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| default.into())
    }

    /// Record the same decision the turn loop made, without inferring root cause.
    pub fn retry_decision(
        error: &ProviderError,
        partial: bool,
        max_attempts: u32,
    ) -> RetryDecision {
        if partial {
            RetryDecision::PartialOutput
        } else if !error.cause.is_retryable() {
            RetryDecision::NotRetryable
        } else if max_attempts == 0 {
            RetryDecision::Disabled
        } else {
            RetryDecision::Exhausted
        }
    }

    /// Recovery belongs in backend details rather than the visible error block.
    pub fn recovery(cause: FailureCause, decision: RetryDecision) -> String {
        if decision == RetryDecision::PartialOutput {
            return "Partial output is retained. Current response's tool calls were not executed. Ask to continue from the retained work; do not blindly replay earlier tools.".into();
        }
        match cause {
            FailureCause::Auth => "Check the selected provider's credentials and sign in again.",
            FailureCause::QuotaExhausted => "Check the provider's account quota or select another provider.",
            FailureCause::RateLimited => "Wait for the provider's rate limit to clear before retrying.",
            FailureCause::Rejected => "Inspect the provider detail and correct the rejected request before retrying.",
            _ => "Check provider availability and connectivity before retrying. The diagnostic alone does not establish which network component failed.",
        }.into()
    }
}
