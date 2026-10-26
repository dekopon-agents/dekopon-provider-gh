use dekopon_provider_sdk::provider::{Code, Failure};
use std::fmt;

#[derive(Debug)]
pub struct ProviderError {
    code: &'static str,
    message: String,
}

impl ProviderError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    pub(crate) fn code_str(&self) -> &str {
        self.code
    }
    #[cfg(test)]
    pub(crate) fn code(&self) -> &str {
        self.code
    }
    pub(crate) fn message(&self) -> &str {
        &self.message
    }
}
impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl Failure for ProviderError {
    fn code(&self) -> Code {
        match self.code {
            "invalid-input" => Code::INVALID_INPUT,
            "usage" => Code::USAGE,
            "request-too-large" => Code::new("request-too-large").exiting(2),
            "checks-pending" => Code::new("checks-pending").exiting(8),
            "job-running" => Code::new("job-running").exiting(8),
            "expired-logs"
            | "permission-denied"
            | "not-found-or-not-visible"
            | "download-authorization-failure"
            | "download-expiry"
            | "incomplete-read"
            | "source-changed"
            | "redirect-refused"
            | "storage-error" => Code::new(self.code).exiting(3),
            "run-failed" => Code::new("run-failed").exiting(1),
            "not-found-or-not-accessible" => Code::new("not-found-or-not-accessible").exiting(3),
            "checks-failed" => Code::new("checks-failed").exiting(1),
            "checks-empty" => Code::new("checks-empty").exiting(1),
            "unauthorized" | "forbidden" | "not-found" | "unprocessable" | "rate-limited"
            | "unexpected-status" | "invalid-response" | "http-failed" | "invalid-request"
            | "request-ceiling" | "incomplete-results" | "index-ceiling" | "graphql-error" => {
                Code::new(self.code).exiting(3)
            }
            "unknown-capability" => Code::UNKNOWN_CAPABILITY,
            "merge-conflict" => Code::new("merge-conflict"),
            "pr-closed" => Code::new("pr-closed"),
            "pr-draft" => Code::new("pr-draft"),
            "head-changed" => Code::new("head-changed"),
            _ => Code::new("gh-failed"),
        }
    }
}
