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
            "checks-pending" => Code::new("checks-pending").exiting(8),
            "checks-failed" => Code::new("checks-failed").exiting(1),
            "checks-empty" => Code::new("checks-empty").exiting(1),
            "unauthorized" | "forbidden" | "not-found" | "unprocessable" | "rate-limited"
            | "unexpected-status" | "invalid-response" | "http-failed" | "invalid-request"
            | "request-ceiling" => Code::new(self.code).exiting(3),
            "unknown-capability" => Code::UNKNOWN_CAPABILITY,
            "merge-conflict" => Code::new("merge-conflict"),
            "pr-closed" => Code::new("pr-closed"),
            "pr-draft" => Code::new("pr-draft"),
            "head-changed" => Code::new("head-changed"),
            _ => Code::new("gh-failed"),
        }
    }
}
