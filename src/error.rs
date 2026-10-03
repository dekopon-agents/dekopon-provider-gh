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
            "unauthorized" => Code::new("unauthorized"),
            "forbidden" => Code::new("forbidden"),
            "not-found" => Code::new("not-found"),
            "unprocessable" => Code::new("unprocessable"),
            "rate-limited" => Code::new("rate-limited"),
            "unexpected-status" => Code::new("unexpected-status"),
            "invalid-response" => Code::new("invalid-response"),
            "http-failed" => Code::new("http-failed"),
            "invalid-endpoint" => Code::new("invalid-endpoint"),
            "invalid-request" => Code::new("invalid-request"),
            "unknown-capability" => Code::UNKNOWN_CAPABILITY,
            "merge-conflict" => Code::new("merge-conflict"),
            "pr-closed" => Code::new("pr-closed"),
            "pr-draft" => Code::new("pr-draft"),
            "head-changed" => Code::new("head-changed"),
            _ => Code::new("gh-failed"),
        }
    }
}
