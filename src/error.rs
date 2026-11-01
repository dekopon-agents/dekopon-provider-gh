use dekopon_provider_sdk::provider::{Code, Failure};
use std::fmt;

#[derive(Clone, Copy, Debug)]
pub(crate) enum WriteOutcome {
    Conflict,
    Unknown,
}

impl WriteOutcome {
    fn label(self) -> &'static str {
        match self {
            Self::Conflict => "content-conflict",
            Self::Unknown => "write-outcome-unknown",
        }
    }
}

#[derive(Debug)]
enum ErrorCode {
    Existing(&'static str),
    Write(WriteOutcome),
}

#[cfg(test)]
impl ErrorCode {
    fn label(&self) -> &str {
        match self {
            Self::Existing(code) => code,
            Self::Write(outcome) => outcome.label(),
        }
    }
}

#[derive(Debug)]
pub struct ProviderError {
    code: ErrorCode,
    message: String,
}

impl ProviderError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::Existing(code),
            message: message.into(),
        }
    }
    pub(crate) fn write(outcome: WriteOutcome) -> Self {
        let message = match outcome {
            WriteOutcome::Conflict => "file write conflicted with the current file or branch state",
            WriteOutcome::Unknown => {
                "file write outcome is unknown; read state before deciding what to do next"
            }
        };
        Self {
            code: ErrorCode::Write(outcome),
            message: message.into(),
        }
    }
    #[cfg(test)]
    pub(crate) fn code(&self) -> &str {
        self.code.label()
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
        let code = match self.code {
            ErrorCode::Existing(code) => code,
            ErrorCode::Write(outcome) => return Code::new(outcome.label()).exiting(3),
        };
        match code {
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
            | "storage-error" => Code::new(code).exiting(3),
            "run-failed" => Code::new("run-failed").exiting(1),
            "not-found-or-not-accessible" => Code::new("not-found-or-not-accessible").exiting(3),
            "checks-failed" => Code::new("checks-failed").exiting(1),
            "checks-empty" => Code::new("checks-empty").exiting(1),
            "unauthorized" | "forbidden" | "not-found" | "unprocessable" | "rate-limited"
            | "unexpected-status" | "invalid-response" | "http-failed" | "invalid-request"
            | "request-ceiling" | "incomplete-results" | "index-ceiling" | "graphql-error" => {
                Code::new(code).exiting(3)
            }
            "unknown-capability" => Code::UNKNOWN_CAPABILITY,
            "actions-conflict" => Code::new("actions-conflict").exiting(3),
            "merge-conflict" => Code::new("merge-conflict"),
            "pr-closed" => Code::new("pr-closed"),
            "pr-draft" => Code::new("pr-draft"),
            "head-changed" => Code::new("head-changed"),
            _ => Code::new("gh-failed"),
        }
    }
}
