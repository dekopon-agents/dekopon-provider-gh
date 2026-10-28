use crate::error::ProviderError;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use dekopon_provider_sdk::{
    provider::{HttpError, Request, Response, endpoint::Base},
    signed_download as sd,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::Write;

const WINDOW: u32 = 65536;
const OUTPUT: usize = 16384;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Mode {
    All,
    Failed,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Input {
    pub owner: String,
    pub repo: String,
    pub run_id: crate::actions::RunId,
    pub attempt: Option<crate::actions::Attempt>,
    pub job: Option<crate::actions::JobId>,
    mode: Mode,
    cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    v: u8,
    owner: String,
    repo: String,
    run: u64,
    job: u64,
    attempt: u32,
    sha: String,
    mode: Mode,
    start: u64,
    end: u64,
    total: u64,
    next: u64,
    digest: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Outcome {
    Complete,
    Truncated,
    JobRunning,
    ExpiredLogs,
    PermissionDenied,
    NotFoundOrNotVisible,
    RateLimited,
    DownloadAuthorizationFailure,
    DownloadExpiry,
    IncompleteRead,
    SourceChanged,
    RedirectRefused,
    StorageError,
    InvalidResponse,
    InvalidRequest,
    Unauthorized,
    Unprocessable,
    UnexpectedStatus,
    HttpFailed,
    RequestLimit,
    Timeout,
}
impl Outcome {
    fn error(self) -> Option<ProviderError> {
        let (code, label) = match self {
            Self::Complete | Self::Truncated => return None,
            Self::InvalidResponse => return Some(crate::invalid_response()),
            Self::InvalidRequest => (
                "invalid-request",
                "could not construct log metadata request",
            ),
            Self::Unauthorized => (
                "unauthorized",
                "credential rejected: expired, revoked or malformed",
            ),
            Self::Unprocessable => ("unprocessable", "endpoint refused the request as invalid"),
            Self::UnexpectedStatus => return Some(crate::unexpected_status()),
            Self::HttpFailed => return Some(crate::http_failed()),
            Self::JobRunning => ("job-running", "job still running; logs not final."),
            Self::ExpiredLogs => ("expired-logs", "expired-logs"),
            Self::PermissionDenied => ("permission-denied", "permission-denied"),
            Self::NotFoundOrNotVisible => ("not-found-or-not-visible", "not-found-or-not-visible"),
            Self::RateLimited => ("rate-limited", "rate-limited"),
            Self::DownloadAuthorizationFailure => (
                "download-authorization-failure",
                "download authorization failure",
            ),
            Self::DownloadExpiry => ("download-expiry", "download expiry"),
            Self::IncompleteRead => ("incomplete-read", "incomplete-read"),
            Self::SourceChanged => ("source-changed", "source-changed"),
            Self::RedirectRefused => ("redirect-refused", "redirect-refused"),
            Self::StorageError => ("storage-error", "storage-error"),
            Self::RequestLimit => ("request-ceiling", "log request reservation exhausted"),
            Self::Timeout => ("http-failed", "log download timeout"),
        };
        Some(ProviderError::new(code, label))
    }
}

struct Budget {
    metadata: usize,
    tails: usize,
    offsets: usize,
}
impl Budget {
    const MAX_REQUESTS: usize = 32;
    fn used(&self) -> usize {
        self.metadata + 4 * self.tails + 5 * self.offsets
    }
    fn reserve(&mut self, offset: bool) -> Result<(), Outcome> {
        let cost = if offset { 5 } else { 4 };
        if self.used() + cost > Self::MAX_REQUESTS {
            return Err(Outcome::RequestLimit);
        }
        if offset {
            self.offsets += 1;
        } else {
            self.tails += 1;
        }
        Ok(())
    }
}

pub(crate) fn validate(value: Value) -> Result<Input, ProviderError> {
    let input: Input = serde_json::from_value(value).map_err(|_| crate::invalid_input())?;
    crate::validate_login(&input.owner)?;
    crate::validate_repo(&input.repo)?;
    if input.run_id.0 == 0
        || input.job.is_some_and(|v| v.0 == 0)
        || input.attempt.is_some_and(|v| v.0 == 0)
    {
        return Err(crate::invalid_input());
    }
    if let Some(raw) = &input.cursor {
        decode_cursor(raw, &input)?;
    }
    Ok(input)
}
fn encode_cursor(cursor: &Cursor) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(cursor).expect("bounded cursor"))
}
fn decode_cursor(raw: &str, input: &Input) -> Result<Cursor, ProviderError> {
    if raw.len() > 1536 {
        return Err(crate::invalid_input());
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| crate::invalid_input())?;
    let c: Cursor = serde_json::from_slice(&bytes).map_err(|_| crate::invalid_input())?;
    if c.v != 1
        || c.owner != input.owner
        || c.repo != input.repo
        || c.run != input.run_id.0
        || c.mode != input.mode
        || c.job == 0
        || c.attempt == 0
        || input.job.is_some_and(|job| job.0 != c.job)
        || input.attempt.is_some_and(|attempt| attempt.0 != c.attempt)
        || c.start >= c.end
        || c.end > c.total
        || c.end - c.start > u64::from(WINDOW)
        || c.next < c.start
        || c.next > c.end
        || c.sha.len() != 40
        || !c.sha.bytes().all(|b| b.is_ascii_hexdigit())
        || c.digest.len() != 64
        || !c.digest.bytes().all(|b| b.is_ascii_hexdigit())
        || encode_cursor(&c) != raw
    {
        return Err(crate::invalid_input());
    }
    Ok(c)
}

fn download_error(error: sd::DownloadError) -> Outcome {
    match error.code {
        sd::ErrorCode::Denied => Outcome::PermissionDenied,
        sd::ErrorCode::RedirectRefused => Outcome::RedirectRefused,
        sd::ErrorCode::RequestLimit => Outcome::RequestLimit,
        sd::ErrorCode::Timeout => Outcome::Timeout,
        sd::ErrorCode::ResponseTooLarge | sd::ErrorCode::Incomplete => Outcome::IncompleteRead,
        sd::ErrorCode::Storage | sd::ErrorCode::Internal => Outcome::StorageError,
    }
}
fn api_response(response: &sd::ApiResponse) -> Outcome {
    let limited = response.headers.iter().any(|header| {
        (header.name.eq_ignore_ascii_case("x-ratelimit-remaining") && header.value == b"0")
            || (header.name.eq_ignore_ascii_case("retry-after")
                && header.value.iter().any(|byte| !byte.is_ascii_whitespace()))
    });
    match response.status {
        410 => Outcome::ExpiredLogs,
        403 if limited => Outcome::RateLimited,
        403 => Outcome::PermissionDenied,
        404 => Outcome::NotFoundOrNotVisible,
        429 => Outcome::RateLimited,
        301 | 302 | 303 | 307 | 308 => Outcome::RedirectRefused,
        _ => Outcome::StorageError,
    }
}
fn storage_status(status: u16) -> Outcome {
    match status {
        403 => Outcome::DownloadAuthorizationFailure,
        410 => Outcome::DownloadExpiry,
        301 | 302 | 303 | 307 | 308 => Outcome::RedirectRefused,
        200 | 416 => Outcome::IncompleteRead,
        _ => Outcome::StorageError,
    }
}

type Source<'a> = dyn FnMut(u64, &mut [u8]) -> Result<usize, Outcome> + 'a;

fn read_exact(source: &mut Source<'_>, offset: u64, bytes: &mut [u8]) -> Result<(), Outcome> {
    let mut used = 0;
    while used < bytes.len() {
        let n = source(offset + used as u64, &mut bytes[used..])?;
        if n == 0 || n > bytes.len() - used {
            return Err(Outcome::IncompleteRead);
        }
        used += n;
    }
    Ok(())
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SourceSpan {
    start: u64,
    end: u64,
}

#[derive(Debug)]
struct WindowInspection {
    digest: String,
    contexts: Vec<ContextSpan>,
}

struct SourceCharacter {
    character: char,
    source_bytes: usize,
    replacements: usize,
}

struct PrefixToken {
    source_offset: usize,
    escaped_bytes: usize,
}

struct RenderedExcerpt {
    text: String,
    next_source_byte: u64,
    invalid_utf8_replacements: usize,
}

struct RenderedContext {
    start: u64,
    excerpt: RenderedExcerpt,
}

#[derive(Debug, PartialEq, Eq)]
struct ContextSpan {
    start: u64,
    end: u64,
    marker: Option<SourceSpan>,
}

fn inspect(
    source: &mut Source<'_>,
    length: u64,
    steps: &[String],
) -> Result<WindowInspection, Outcome> {
    let mut hasher = Sha256::new();
    let mut buffer = [0; 2048];
    let mut offset = 0;
    let mut spans = Vec::new();
    let mut overlap = Vec::new();
    while offset < length {
        let n = (length - offset).min(buffer.len() as u64) as usize;
        read_exact(source, offset, &mut buffer[..n])?;
        hasher.update(&buffer[..n]);
        overlap.extend_from_slice(&buffer[..n]);
        let needles = std::iter::once("##[error]").chain(steps.iter().map(String::as_str));
        for needle in needles {
            if spans.len() >= 3 || needle.is_empty() {
                break;
            }
            if let Some(index) = overlap
                .windows(needle.len())
                .position(|bytes| bytes == needle.as_bytes())
            {
                let absolute = offset.saturating_sub((overlap.len() - n) as u64) + index as u64;
                let marker_end = absolute + needle.len() as u64;
                let span = ContextSpan {
                    start: absolute.saturating_sub(128),
                    end: (absolute + 384).max(marker_end).min(length),
                    marker: Some(SourceSpan {
                        start: absolute,
                        end: marker_end,
                    }),
                };
                if !spans.contains(&span) {
                    spans.push(span);
                }
            }
        }
        overlap = overlap[overlap.len().saturating_sub(256)..].to_vec();
        offset += n as u64;
    }
    let mut extra = [0];
    if source(length, &mut extra)? != 0 {
        return Err(Outcome::IncompleteRead);
    }
    if length > 0 {
        spans.push(ContextSpan {
            start: length.saturating_sub(512),
            end: length,
            marker: None,
        });
    }
    Ok(WindowInspection {
        digest: format!("{:x}", hasher.finalize()),
        contexts: spans,
    })
}
fn escape(c: char) -> String {
    if c == '\n' || c == '\t' {
        return c.to_string();
    }
    if c.is_control()
        || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
    {
        format!("\\u{{{:04x}}}", c as u32)
    } else {
        c.to_string()
    }
}

fn source_character(available: &[u8]) -> SourceCharacter {
    let width = match available[0] {
        0..=0x7f => 1,
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => 1,
    };
    let valid = available
        .get(..width)
        .and_then(|b| std::str::from_utf8(b).ok())
        .and_then(|s| s.chars().next());
    valid.map_or(
        SourceCharacter {
            character: '�',
            source_bytes: 1,
            replacements: 1,
        },
        |character| SourceCharacter {
            character,
            source_bytes: width,
            replacements: 0,
        },
    )
}

fn context_excerpt(
    source: &mut Source<'_>,
    span: &ContextSpan,
    budget: usize,
) -> Result<Option<RenderedContext>, Outcome> {
    let mut start = span.start;
    let mut allowance = budget.min(512);
    if let Some(marker) = span.marker {
        let rendered_marker = excerpt(source, marker.start, marker.end, budget)?;
        if rendered_marker.next_source_byte != marker.end {
            return Ok(None);
        }
        allowance = budget.min(512.max(rendered_marker.text.len()));
        let prefix_budget = (allowance - rendered_marker.text.len()) / 2;
        let mut prefix = [0; 128];
        let length = (marker.start - start) as usize;
        read_exact(source, start, &mut prefix[..length])?;
        let mut tokens = Vec::new();
        let mut offset = 0;
        while offset < length {
            let decoded = source_character(&prefix[offset..length]);
            tokens.push(PrefixToken {
                source_offset: offset,
                escaped_bytes: escape(decoded.character).len(),
            });
            offset += decoded.source_bytes;
        }
        let mut escaped = 0;
        start = marker.start;
        for token in tokens.into_iter().rev() {
            if escaped + token.escaped_bytes > prefix_budget {
                break;
            }
            escaped += token.escaped_bytes;
            start = span.start + token.source_offset as u64;
        }
    }
    Ok(Some(RenderedContext {
        start,
        excerpt: excerpt(source, start, span.end, allowance)?,
    }))
}

fn excerpt(
    source: &mut Source<'_>,
    start: u64,
    end: u64,
    budget: usize,
) -> Result<RenderedExcerpt, Outcome> {
    let mut output = String::new();
    let mut offset = start;
    let mut replacements = 0;
    let mut segment = 0;
    let mut buffer = [0; 2052];
    let mut block_start = u64::MAX;
    let mut block_len = 0;
    while offset < end {
        if block_start == u64::MAX || offset + 4 > block_start + block_len as u64 {
            block_start = offset;
            block_len = (end - offset).min(buffer.len() as u64) as usize;
            read_exact(source, offset, &mut buffer[..block_len])?;
        }
        let index = (offset - block_start) as usize;
        let available = &buffer[index..block_len];
        let decoded = source_character(available);
        let text = escape(decoded.character);
        let break_line = segment + text.len() > 2048;
        if output.len() + text.len() + usize::from(break_line) > budget {
            break;
        }
        if break_line {
            output.push('\n');
            segment = 0;
        }
        output.push_str(&text);
        segment = if decoded.character == '\n' {
            0
        } else {
            segment + text.len()
        };
        offset += decoded.source_bytes as u64;
        replacements += decoded.replacements;
    }
    Ok(RenderedExcerpt {
        text: output,
        next_source_byte: offset,
        invalid_utf8_replacements: replacements,
    })
}

fn render_window(
    source: &mut Source<'_>,
    mut cursor: Cursor,
    previous: Option<&Cursor>,
    steps: &[String],
    budget: usize,
) -> Result<Value, Outcome> {
    let length = cursor.end - cursor.start;
    let inspection = inspect(source, length, steps)?;
    if previous.is_some_and(|p| {
        p.start == cursor.start && p.end == cursor.end && p.digest != inspection.digest
    }) {
        return Err(Outcome::SourceChanged);
    }
    cursor.digest = inspection.digest;
    let sequential = excerpt(source, cursor.next - cursor.start, length, budget / 2)?;
    let mut text = sequential.text;
    let next = sequential.next_source_byte;
    let mut replacements = sequential.invalid_utf8_replacements;
    let sequential_start = cursor.next;
    cursor.next = cursor.start + next;
    let mut spans = vec![SourceSpan {
        start: sequential_start,
        end: cursor.next,
    }];
    if previous.is_none() {
        for span in inspection.contexts {
            if span.start >= sequential_start - cursor.start && span.end <= next {
                continue;
            }
            let remaining = budget.saturating_sub(text.len() + 32);
            if remaining < 16 {
                break;
            }
            let Some(context) = context_excerpt(source, &span, remaining)? else {
                continue;
            };
            if context.excerpt.next_source_byte == context.start {
                continue;
            }
            text.push_str("\n[window-limited context]\n");
            text.push_str(&context.excerpt.text);
            replacements += context.excerpt.invalid_utf8_replacements;
            spans.push(SourceSpan {
                start: cursor.start + context.start,
                end: cursor.start + context.excerpt.next_source_byte,
            });
        }
    }
    let exhausted = cursor.next == cursor.end;
    let complete = exhausted && cursor.start == 0;
    let continuation = if complete {
        None
    } else {
        Some(encode_cursor(&cursor))
    };
    let outcome = if complete {
        Outcome::Complete
    } else {
        Outcome::Truncated
    };
    if let Some(raw) = &continuation {
        text.push_str(&format!(
            "\n[dekopon: truncated; fetched {}-{}/{}; rendered {}; continue {}]\n",
            cursor.start,
            cursor.end - 1,
            cursor.total,
            spans
                .iter()
                .map(|span| format!("{}-{}", span.start, span.end))
                .collect::<Vec<_>>()
                .join(","),
            raw
        ));
    }
    Ok(
        json!({"runId":cursor.run,"attempt":cursor.attempt,"jobId":cursor.job,"headSha":cursor.sha,"outcome":outcome,"complete":complete,"extraction":"window-limited","fetched":{"start":cursor.start,"endExclusive":cursor.end,"total":cursor.total},"renderedSpans":spans.iter().map(|span| [span.start,span.end]).collect::<Vec<_>>(),"nextSourceByte":cursor.next,"invalidUtf8Replacements":replacements,"continuation":continuation,"text":text}),
    )
}

type Download<'a> = dyn FnMut(&sd::Request) -> Result<sd::Response, sd::DownloadError> + 'a;
fn acquire(
    input: &Input,
    job_id: u64,
    previous: Option<&Cursor>,
    base: &Base,
    budget: &mut Budget,
    download: &mut Download<'_>,
) -> Result<sd::Response, Outcome> {
    let window = if let Some(c) = previous {
        let offset = if c.next == c.end {
            sd::OffsetWindow {
                start: c.start.saturating_sub(u64::from(WINDOW)),
                length: c.start.min(u64::from(WINDOW)) as u32,
            }
        } else {
            sd::OffsetWindow {
                start: c.start,
                length: (c.end - c.start) as u32,
            }
        };
        if offset.length == 0 {
            return Err(Outcome::IncompleteRead);
        }
        sd::Window::Offset(offset)
    } else {
        sd::Window::Tail(WINDOW)
    };
    budget.reserve(previous.is_some())?;
    let api_uri = crate::url(
        base,
        &format!(
            "/repos/{}/{}/actions/jobs/{job_id}/logs",
            input.owner, input.repo
        ),
    )
    .map_err(|_| Outcome::StorageError)?;
    download(&sd::Request { api_uri, window }).map_err(download_error)
}

struct FetchFailure {
    outcome: Outcome,
    fetched: Option<Value>,
}
impl From<Outcome> for FetchFailure {
    fn from(outcome: Outcome) -> Self {
        Self {
            outcome,
            fetched: None,
        }
    }
}

fn fetch(
    input: &Input,
    metadata: &Value,
    job: &Value,
    previous: Option<&Cursor>,
    base: &Base,
    budget: &mut Budget,
    text_budget: usize,
) -> Result<Value, FetchFailure> {
    if job["status"] != "completed" {
        return Err(Outcome::JobRunning.into());
    }
    let job_id = job["databaseId"].as_u64().ok_or(Outcome::IncompleteRead)?;
    let attempt = metadata["attempt"]
        .as_u64()
        .ok_or(Outcome::IncompleteRead)? as u32;
    let sha = metadata["headSha"]
        .as_str()
        .ok_or(Outcome::IncompleteRead)?;
    if previous.is_some_and(|c| c.job != job_id || c.attempt != attempt || c.sha != sha) {
        return Err(Outcome::SourceChanged.into());
    }
    let response = acquire(input, job_id, previous, base, budget, &mut sd::download)?;
    match response {
        sd::Response::Api(api) => Err(api_response(&api).into()),
        sd::Response::Storage {
            status,
            coverage,
            body,
        } => {
            if status != 206 {
                return Err(storage_status(status).into());
            }
            let coverage = coverage.ok_or(Outcome::IncompleteRead)?;
            if coverage.start >= coverage.end_exclusive
                || coverage.end_exclusive > coverage.total
                || coverage.end_exclusive - coverage.start > u64::from(WINDOW)
            {
                return Err(Outcome::IncompleteRead.into());
            }
            if let Some(c) = previous {
                if c.total != coverage.total {
                    return Err(Outcome::SourceChanged.into());
                }
                let expected_start = if c.next == c.end {
                    c.start.saturating_sub(u64::from(WINDOW))
                } else {
                    c.start
                };
                let expected_end = if c.next == c.end { c.start } else { c.end };
                if coverage.start != expected_start || coverage.end_exclusive != expected_end {
                    return Err(Outcome::IncompleteRead.into());
                }
            } else if coverage.end_exclusive != coverage.total
                || coverage.start != coverage.total.saturating_sub(u64::from(WINDOW))
            {
                return Err(Outcome::IncompleteRead.into());
            }
            let body = body.ok_or(Outcome::IncompleteRead)?;
            let next = previous
                .filter(|c| c.next != c.end)
                .map_or(coverage.start, |c| c.next);
            let cursor = Cursor {
                v: 1,
                owner: input.owner.clone(),
                repo: input.repo.clone(),
                run: input.run_id.0,
                job: job_id,
                attempt,
                sha: sha.to_owned(),
                mode: input.mode,
                start: coverage.start,
                end: coverage.end_exclusive,
                total: coverage.total,
                next,
                digest: String::new(),
            };
            let steps = job["steps"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|step| step["conclusion"] == "failure")
                .filter_map(|step| step["name"].as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.chars().take(256).collect())
                .collect::<Vec<String>>();
            render_window(
                &mut |offset, bytes| {
                    body.read_at(offset, bytes)
                        .map_err(|_| Outcome::IncompleteRead)
                },
                cursor,
                previous,
                &steps,
                text_budget,
            ).map_err(|outcome| FetchFailure { outcome, fetched: Some(json!({"start":coverage.start,"endExclusive":coverage.end_exclusive,"total":coverage.total})) })
        }
    }
}

fn metadata_outcome(error: crate::actions::LogMetadataError) -> Outcome {
    use crate::actions::LogMetadataError;
    match error {
        LogMetadataError::NotFound => Outcome::NotFoundOrNotVisible,
        LogMetadataError::PermissionDenied => Outcome::PermissionDenied,
        LogMetadataError::RateLimited => Outcome::RateLimited,
        LogMetadataError::RequestLimit => Outcome::RequestLimit,
        LogMetadataError::Transport => Outcome::HttpFailed,
        LogMetadataError::InvalidRequest => Outcome::InvalidRequest,
        LogMetadataError::InvalidResponse => Outcome::InvalidResponse,
        LogMetadataError::Unauthorized => Outcome::Unauthorized,
        LogMetadataError::Unprocessable => Outcome::Unprocessable,
        LogMetadataError::UnexpectedStatus => Outcome::UnexpectedStatus,
    }
}

pub(crate) fn execute(
    value: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
    out: &mut dyn Write,
) -> Result<(), ProviderError> {
    let mut input = validate(value)?;
    let previous = input
        .cursor
        .as_ref()
        .map(|raw| decode_cursor(raw, &input))
        .transpose()?;
    if let Some(c) = &previous {
        input.job = Some(crate::actions::JobId(c.job));
        input.attempt = Some(crate::actions::Attempt(c.attempt));
    }
    let metadata = crate::actions::log_metadata(&input, base, send)
        .map_err(|error| metadata_outcome(error).error().expect("metadata failure"))?;
    let mut budget = Budget {
        metadata: metadata["metadataRequests"]
            .as_u64()
            .ok_or_else(crate::invalid_response)? as usize,
        tails: 0,
        offsets: 0,
    };
    let mut rows = Vec::new();
    let mut failure = None;
    let jobs = metadata["jobs"]
        .as_array()
        .ok_or_else(crate::invalid_response)?;
    let selected = jobs
        .iter()
        .filter(|job| {
            input.mode == Mode::All
                || job["conclusion"] == "failure"
                || job["conclusion"] == "timed_out"
                || job["conclusion"] == "startup_failure"
                || job["status"] != "completed"
        })
        .collect::<Vec<_>>();
    for job in &selected {
        let used = serde_json::to_vec(&rows)
            .map_err(|_| crate::invalid_response())?
            .len();
        let remaining = OUTPUT.saturating_sub(used + 2048);
        if remaining < 4096 {
            failure = Some(Outcome::IncompleteRead);
            break;
        }
        match fetch(
            &input,
            &metadata,
            job,
            previous.as_ref(),
            base,
            &mut budget,
            (remaining.saturating_sub(2048) / 2).min(8192),
        ) {
            Ok(row) => rows.push(row),
            Err(error) => {
                let outcome = error.outcome;
                rows.push(json!({"runId":input.run_id.0,"attempt":metadata["attempt"],"headSha":metadata["headSha"],"jobId":job["databaseId"],"outcome":outcome,"complete":false,"fetched":error.fetched,"renderedSpans":[],"continuation":input.cursor}));
                failure = Some(outcome);
                break;
            }
        }
    }
    if metadata["coverage"]["incomplete"] == true {
        failure.get_or_insert(Outcome::IncompleteRead);
    }
    let complete = failure.is_none() && rows.iter().all(|r| r["complete"] == true);
    let result = json!({"runId":input.run_id.0,"attempt":metadata["attempt"],"headSha":metadata["headSha"],"complete":complete,"collectionComplete":failure.is_none(),"selectedJobs":selected.len(),"jobs":rows,"coverage":metadata["coverage"],"metadataRequests":budget.metadata,"downloadCalls":budget.tails+budget.offsets,"requestsReserved":budget.used()});
    let bytes = serde_json::to_vec(&result).map_err(|_| crate::invalid_response())?;
    if bytes.len() + 1 > OUTPUT {
        return Err(Outcome::IncompleteRead.error().expect("failure"));
    }
    out.write_all(&bytes)
        .and_then(|_| out.write_all(b"\n"))
        .map_err(|_| crate::invalid_response())?;
    if let Some(error) = failure.and_then(Outcome::error) {
        return Err(error);
    }
    Ok(())
}

pub(crate) fn unused(
    _: Value,
    _: &Base,
    _: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    unreachable!("log capability uses streaming execution")
}

pub(crate) fn schema() -> Value {
    crate::object_schema(
        json!({"owner":{"type":"string"},"repo":{"type":"string"},"runId":{"type":"integer","minimum":1},"job":{"type":"integer","minimum":1},"attempt":{"type":"integer","minimum":1,"maximum":u32::MAX},"mode":{"type":"string","enum":["all","failed"]},"cursor":{"type":"string","maxLength":1536}}),
        &["owner", "repo", "runId", "mode"],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use dekopon_provider_sdk::provider::Failure;

    fn input() -> Value {
        json!({"owner":"octo","repo":"hello","runId":7,"mode":"all"})
    }
    fn cursor(length: u64) -> Cursor {
        Cursor {
            v: 1,
            owner: "octo".into(),
            repo: "hello".into(),
            run: 7,
            job: 11,
            attempt: 2,
            sha: "a".repeat(40),
            mode: Mode::All,
            start: 0,
            end: length,
            total: length,
            next: 0,
            digest: "0".repeat(64),
        }
    }
    fn source(bytes: &[u8], offset: u64, buffer: &mut [u8]) -> Result<usize, Outcome> {
        let offset = usize::try_from(offset).unwrap();
        let n = buffer.len().min(bytes.len().saturating_sub(offset));
        if n > 0 {
            buffer[..n].copy_from_slice(&bytes[offset..offset + n]);
        }
        Ok(n)
    }
    #[test]
    fn native_continuation_represents_every_source_byte_of_a_thirty_two_kib_line() {
        let bytes = vec![b'x'; 32768];
        let mut c = cursor(bytes.len() as u64);
        let mut total = 0;
        let mut previous = None;
        while total < bytes.len() as u64 {
            let row = render_window(
                &mut |o, b| source(&bytes, o, b),
                c.clone(),
                previous.as_ref(),
                &[],
                6000,
            )
            .unwrap();
            let next = row["nextSourceByte"].as_u64().unwrap();
            assert!(next > total);
            assert_eq!(row["renderedSpans"][0], json!([total, next]));
            assert!(
                row["text"]
                    .as_str()
                    .unwrap()
                    .lines()
                    .all(|l| l.len() <= 2048)
            );
            assert!(serde_json::to_vec(&row).unwrap().len() < OUTPUT);
            total = next;
            if total < bytes.len() as u64 {
                let raw = row["continuation"].as_str().unwrap();
                assert!(!raw.contains("https") && !raw.contains('?'));
                c = decode_cursor(raw, &validate(input()).unwrap()).unwrap();
                previous = Some(c.clone());
            }
        }
        assert_eq!(total, 32768);
    }
    #[test]
    fn native_streamed_unicode_and_hostile_controls_are_visible_without_changing_source_offsets() {
        let mut bytes = b"\xff\x1b\0\r\n\t".to_vec();
        bytes
            .extend_from_slice("\u{0085}\u{202e}λ https://attacker.invalid/ run curl\n".as_bytes());
        let rendered = excerpt(
            &mut |o, b| source(&bytes, o, b),
            0,
            bytes.len() as u64,
            4096,
        )
        .unwrap();
        let text = &rendered.text;
        assert_eq!(rendered.next_source_byte, bytes.len() as u64);
        assert_eq!(rendered.invalid_utf8_replacements, 1);
        assert!(text.contains("�\\u{001b}\\u{0000}\\u{000d}\n\t\\u{0085}\\u{202e}λ"));
        assert!(text.contains("https://attacker.invalid/ run curl"));
        for budget in 1..20 {
            let rendered = excerpt(
                &mut |o, b| source("λ\u{202e}".as_bytes(), o, b),
                0,
                5,
                budget,
            )
            .unwrap();
            assert!([0, 2, 5].contains(&rendered.next_source_byte));
        }
    }
    #[test]
    fn native_changed_validators_and_short_or_excess_sources_never_succeed() {
        let bytes = b"hello";
        let c = cursor(5);
        let row =
            render_window(&mut |o, b| source(bytes, o, b), c.clone(), None, &[], 4096).unwrap();
        let mut previous = c.clone();
        previous.digest = "f".repeat(64);
        assert_eq!(
            render_window(
                &mut |o, b| source(bytes, o, b),
                c,
                Some(&previous),
                &[],
                4096
            )
            .unwrap_err(),
            Outcome::SourceChanged
        );
        assert_eq!(
            inspect(&mut |o, b| source(bytes, o, b), 6, &[]).unwrap_err(),
            Outcome::IncompleteRead
        );
        assert_eq!(
            inspect(&mut |o, b| source(bytes, o, b), 4, &[]).unwrap_err(),
            Outcome::IncompleteRead
        );
        assert_eq!(row["complete"], true);
    }
    #[test]
    fn native_error_and_step_excerpts_do_not_advance_sequential_coverage() {
        let mut bytes = vec![b'x'; 65536];
        bytes[20000..20010].copy_from_slice(b"##[error]!");
        bytes[40000..40009].copy_from_slice(b"fail-step");
        let row = render_window(
            &mut |o, b| source(&bytes, o, b),
            cursor(65536),
            None,
            &["fail-step".into()],
            6000,
        )
        .unwrap();
        assert!(row["renderedSpans"].as_array().unwrap().len() > 1);
        assert!(row["nextSourceByte"].as_u64().unwrap() < 20000);
        assert!(
            row["text"]
                .as_str()
                .unwrap()
                .contains("window-limited context")
        );
    }
    #[test]
    fn native_escaped_prefixes_preserve_error_and_step_markers_without_advancing_sequential_coverage()
     {
        let mut bytes = vec![b'x'; 65536];
        let baseline = render_window(
            &mut |o, b| source(&bytes, o, b),
            cursor(65536),
            None,
            &[],
            6000,
        )
        .unwrap();
        for (position, marker) in [
            (20000, b"##[error]".as_slice()),
            (40000, b"fail-step".as_slice()),
        ] {
            bytes[position - 128..position].fill(0x1b);
            bytes[position..position + marker.len()].copy_from_slice(marker);
        }
        let row = render_window(
            &mut |o, b| source(&bytes, o, b),
            cursor(65536),
            None,
            &["fail-step".into()],
            6000,
        )
        .unwrap();
        let text = row["text"].as_str().unwrap();
        assert!(text.contains("##[error]"));
        assert!(text.contains("fail-step"));
        assert!(text.contains("\\u{001b}"));
        assert_eq!(row["nextSourceByte"], baseline["nextSourceByte"]);
        assert_eq!(row["renderedSpans"][0], baseline["renderedSpans"][0]);
        assert!(serde_json::to_vec(&row).unwrap().len() < OUTPUT);
    }

    #[test]
    fn native_log_api_secondary_limits_are_classified_from_full_response_headers() {
        for headers in [
            vec![
                sd::Header {
                    name: "Retry-After".into(),
                    value: b"60".to_vec(),
                },
                sd::Header {
                    name: "x-ratelimit-remaining".into(),
                    value: b"100".to_vec(),
                },
            ],
            vec![sd::Header {
                name: "x-ratelimit-remaining".into(),
                value: b"0".to_vec(),
            }],
        ] {
            let response = sd::ApiResponse {
                status: 403,
                headers,
                body: b"untrusted response text".to_vec(),
            };
            assert_eq!(api_response(&response), Outcome::RateLimited);
        }
        let response = sd::ApiResponse {
            status: 403,
            headers: vec![sd::Header {
                name: "x-ratelimit-remaining".into(),
                value: b"100".to_vec(),
            }],
            body: vec![],
        };
        assert_eq!(api_response(&response), Outcome::PermissionDenied);
    }

    #[test]
    fn native_metadata_failures_map_exhaustively_to_log_outcomes() {
        use crate::actions::LogMetadataError as Error;
        for (error, outcome) in [
            (Error::NotFound, Outcome::NotFoundOrNotVisible),
            (Error::PermissionDenied, Outcome::PermissionDenied),
            (Error::RateLimited, Outcome::RateLimited),
            (Error::RequestLimit, Outcome::RequestLimit),
            (Error::Transport, Outcome::HttpFailed),
            (Error::InvalidRequest, Outcome::InvalidRequest),
            (Error::InvalidResponse, Outcome::InvalidResponse),
            (Error::Unauthorized, Outcome::Unauthorized),
            (Error::Unprocessable, Outcome::Unprocessable),
            (Error::UnexpectedStatus, Outcome::UnexpectedStatus),
        ] {
            assert_eq!(metadata_outcome(error), outcome);
        }
    }

    #[test]
    fn native_cursor_tampering_and_new_input_authority_are_refused_before_io() {
        for patch in [
            json!({"runId":0}),
            json!({"attempt":0}),
            json!({"job":0}),
            json!({"url":"https://example.invalid"}),
            json!({"range":"bytes=0-1"}),
            json!({"cursor":"bogus"}),
        ] {
            let mut value = input();
            value
                .as_object_mut()
                .unwrap()
                .extend(patch.as_object().unwrap().clone());
            assert!(validate(value).is_err());
        }
        let mut c = cursor(20);
        c.next = 21;
        assert!(decode_cursor(&encode_cursor(&c), &validate(input()).unwrap()).is_err());
        c.next = 1;
        c.owner = "other".into();
        assert!(decode_cursor(&encode_cursor(&c), &validate(input()).unwrap()).is_err());
    }
    #[test]
    fn native_request_reservations_include_four_tail_and_five_offset_calls() {
        let mut budget = Budget {
            metadata: 3,
            tails: 0,
            offsets: 0,
        };
        for _ in 0..7 {
            budget.reserve(false).unwrap();
        }
        assert_eq!(budget.used(), 31);
        assert_eq!(budget.reserve(false), Err(Outcome::RequestLimit));
        let mut budget = Budget {
            metadata: 7,
            tails: 0,
            offsets: 0,
        };
        for _ in 0..5 {
            budget.reserve(true).unwrap();
        }
        assert_eq!(budget.used(), 32);
        assert_eq!(budget.reserve(true), Err(Outcome::RequestLimit));
    }
    #[test]
    fn native_each_window_makes_one_download_with_canonical_uri_and_a_typed_tail_or_offset() {
        let input = validate(input()).unwrap();
        let base: Base = serde_json::from_value(json!("https://api.github.com/prefix")).unwrap();
        let mut budget = Budget {
            metadata: 4,
            tails: 0,
            offsets: 0,
        };
        let mut calls = 0;
        let mut c = cursor(100000);
        c.start = 34464;
        c.next = 35000;
        for (previous, expected) in [(None, None), (Some(&c), Some((34464, 65536)))] {
            acquire(&input, 11, previous, &base, &mut budget, &mut |request| {
                calls += 1;
                assert_eq!(
                    request.api_uri,
                    "https://api.github.com/prefix/repos/octo/hello/actions/jobs/11/logs"
                );
                match (&request.window, expected) {
                    (sd::Window::Tail(length), None) => assert_eq!(*length, 65536),
                    (sd::Window::Offset(window), Some((start, length))) => {
                        assert_eq!(window.start, start);
                        assert_eq!(window.length, length);
                    }
                    _ => panic!("wrong typed window"),
                }
                Ok(sd::Response::Api(sd::ApiResponse {
                    status: 410,
                    headers: vec![],
                    body: vec![],
                }))
            })
            .unwrap();
        }
        assert_eq!(calls, 2);
        assert_eq!(budget.used(), 13);
        c.next = c.end;
        acquire(&input, 11, Some(&c), &base, &mut budget, &mut |request| {
            match &request.window {
                sd::Window::Offset(window) => {
                    assert_eq!(window.start, 0);
                    assert_eq!(window.length, 34464);
                }
                sd::Window::Tail(_) => panic!("preceding window is explicit"),
            }
            Err(sd::DownloadError {
                code: sd::ErrorCode::Incomplete,
            })
        })
        .err()
        .filter(|outcome| *outcome == Outcome::IncompleteRead)
        .expect("typed incomplete");
        budget.metadata = 32;
        assert!(matches!(
            acquire(&input, 11, None, &base, &mut budget, &mut |_| panic!(
                "no call after cap"
            )),
            Err(Outcome::RequestLimit)
        ));
    }

    #[test]
    fn native_api_and_storage_failures_keep_distinct_closed_variants_and_exit_codes() {
        for (status, exhausted, expected) in [
            (410, false, Outcome::ExpiredLogs),
            (403, false, Outcome::PermissionDenied),
            (403, true, Outcome::RateLimited),
            (404, false, Outcome::NotFoundOrNotVisible),
            (429, false, Outcome::RateLimited),
        ] {
            let response = sd::ApiResponse {
                status,
                headers: if exhausted {
                    vec![sd::Header {
                        name: "x-ratelimit-remaining".into(),
                        value: b"0".to_vec(),
                    }]
                } else {
                    vec![]
                },
                body: vec![],
            };
            assert_eq!(api_response(&response), expected);
            assert_eq!(Failure::code(&expected.error().unwrap()).status().get(), 3);
        }
        assert_eq!(storage_status(403), Outcome::DownloadAuthorizationFailure);
        assert_eq!(storage_status(410), Outcome::DownloadExpiry);
        for status in [301, 302, 303, 307, 308] {
            assert_eq!(storage_status(status), Outcome::RedirectRefused);
        }
        assert_eq!(storage_status(200), Outcome::IncompleteRead);
        assert!(Outcome::Complete.error().is_none());
        assert!(Outcome::Truncated.error().is_none());
        assert_eq!(
            Failure::code(&Outcome::JobRunning.error().unwrap())
                .status()
                .get(),
            8
        );
        for (code, expected) in [
            (sd::ErrorCode::Incomplete, Outcome::IncompleteRead),
            (sd::ErrorCode::ResponseTooLarge, Outcome::IncompleteRead),
            (sd::ErrorCode::RedirectRefused, Outcome::RedirectRefused),
            (sd::ErrorCode::RequestLimit, Outcome::RequestLimit),
            (sd::ErrorCode::Storage, Outcome::StorageError),
        ] {
            assert_eq!(download_error(sd::DownloadError { code }), expected);
        }
    }
}
