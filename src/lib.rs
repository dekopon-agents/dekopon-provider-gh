//! A "fake `gh`": narrow GitHub operations as separately named Dekopon capabilities.
//!
//! Every operation is one fixed REST request shape (or one fixed pre-read plus one write) against
//! the GitHub REST API, projected into a small bounded output. The origin is `api.github.com`
//! unless the owner sets `providerSettings.gh.baseUrl`; no model input names it. There is deliberately no generic
//! `gh.api.*` passthrough and no GraphQL: broker HTTP constraints bind host and method but not
//! path, so path discipline is exactly what this guest exists to provide. A grant of
//! `gh.pull-request.read` is authority to read pull requests, not authority over everything the
//! broker credential can reach.
//!
//! Review events are separate capabilities (`gh.pull-request.approve`, `.comment`,
//! `.request-changes`) rather than one capability with an `event` argument, so policy can grant
//! approval authority independently of the other two. The write capabilities pre-read their pull
//! request and pin the observed head SHA into the write, refusing closed, merged, and
//! (for approval) draft pull requests, and refusing when the caller's `expectedHeadSha` no longer
//! matches — a retry against the same head converges instead of blessing new commits.
//!
//! This guest never sets `authorization`; the host rejects the header from guests by construction,
//! and broker-owned credential injection is the only path a credential takes — added inside the
//! native HTTP engine, for destinations inside the binding, where no guest can observe it.
//! Transport failures are reported as the constant `http-failed` so host detail never reaches a
//! model.
//!
//! Unlike the workspace crates, this guest cannot `#![forbid(unsafe_code)]`: the generated
//! component bindings contain `unsafe` by construction. No hand-written code here is unsafe.

use crate::error::ProviderError;
#[cfg(test)]
use dekopon_provider_sdk::CapabilityId;
use dekopon_provider_sdk::provider::endpoint::Base;
use dekopon_provider_sdk::provider::{Header, HttpError, Request, Response, method};
use dekopon_provider_sdk::provider::{Proposal, Provider, Usage};
use dekopon_provider_sdk::{EffectKind, ProviderCapability, RiskLevel};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

mod auth;
pub mod commands;
mod content;
pub mod error;
mod issues;
mod pulls;
mod repos;
mod reviews;
pub mod typed;

const GITHUB_API: Base = Base::from_static("https://api.github.com");

/// The owner's `providerSettings.gh`: `baseUrl` replaces the GitHub API origin, for GitHub
/// Enterprise Server or a recording proxy. The broker's grant decides what it may reach.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GhSettings {
    #[serde(default = "default_base")]
    base_url: Base,
}

fn default_base() -> Base {
    GITHUB_API
}

impl GhSettings {
    fn base(self) -> Base {
        self.base_url
    }
}

const ACCEPT_JSON: &str = "application/vnd.github+json";
const ACCEPT_DIFF: &str = "application/vnd.github.diff";
const API_VERSION: &str = "2022-11-28";
/// Constant on purpose: GitHub rejects requests without a `user-agent`, and interpolating anything
/// input-derived into it would hand a script a header side channel.
const USER_AGENT: &str = "dekopon-gh-provider/0.1";

// Input bounds, re-validated natively; the JSON Schemas below are model-facing metadata only.
const MAX_OWNER_BYTES: usize = 39;
const MAX_REPO_BYTES: usize = 100;
const MAX_NUMBER: u32 = 1_000_000;
const MAX_REF_BYTES: usize = 256;
const MAX_PATH_BYTES: usize = 1024;
const MAX_BODY_IN_BYTES: usize = 4 * 1024;
const MAX_PAGE: u32 = 50;
const MAX_PER_PAGE: u32 = 50;

// Output projection bounds. The broker host already ceilings total serialized output; these keep
// each field useful instead of failing the whole invocation on one large response.
const MAX_TITLE_OUT_BYTES: usize = 256;
const MAX_PR_BODY_OUT_BYTES: usize = 16 * 1024;
const MAX_PATCH_OUT_BYTES: usize = 8 * 1024;
const MAX_CONTENT_OUT_BYTES: usize = 192 * 1024;
const MAX_DIFF_OUT_BYTES: usize = 192 * 1024;
const MAX_DIR_ENTRIES: usize = 200;
const MAX_COMMENT_OUT_BYTES: usize = 4 * 1024;
const MAX_MESSAGE_OUT_BYTES: usize = 4 * 1024;
const MAX_DESCRIPTION_OUT_BYTES: usize = 1024;
const MAX_TIMESTAMP_BYTES: usize = 64;
const MAX_LOGIN_OUT_BYTES: usize = 64;
const MAX_LIST_ITEMS: usize = 50;
const MAX_LABELS: usize = 20;

/// Every capability identifier this component exports, named once.
///
/// `capabilities()`, `invoke_with`, and the `gh` command tree in [`commands`] all read these, so
/// renaming a capability is a compile error rather than an exit code a model discovers
/// mid-session.
pub(crate) mod ids {
    pub(crate) const AUTH_STATUS: &str = "gh.auth.status";
    pub(crate) const CONTENT_READ: &str = "gh.content.read";
    pub(crate) const PR_LIST: &str = "gh.pull-request.list";
    pub(crate) const PR_READ: &str = "gh.pull-request.read";
    pub(crate) const PR_FILES: &str = "gh.pull-request.files";
    pub(crate) const PR_DIFF: &str = "gh.pull-request.diff";
    pub(crate) const PR_REVIEWS: &str = "gh.pull-request.reviews";
    pub(crate) const PR_STATUS: &str = "gh.pull-request.status";
    pub(crate) const PR_APPROVE: &str = "gh.pull-request.approve";
    pub(crate) const PR_COMMENT: &str = "gh.pull-request.comment";
    pub(crate) const PR_REQUEST_CHANGES: &str = "gh.pull-request.request-changes";
    pub(crate) const PR_MERGE: &str = "gh.pull-request.merge";
    pub(crate) const REPO_READ: &str = "gh.repo.read";
    pub(crate) const BRANCH_READ: &str = "gh.branch.read";
    pub(crate) const COMMIT_READ: &str = "gh.commit.read";
    pub(crate) const USER_READ: &str = "gh.user.read";
    pub(crate) const ISSUE_READ: &str = "gh.issue.read";
    pub(crate) const ISSUE_LIST: &str = "gh.issue.list";
    pub(crate) const ISSUE_COMMENTS_READ: &str = "gh.issue-comments.read";
    pub(crate) const ISSUE_COMMENT: &str = "gh.issue.comment";
}

pub struct Gh;

impl Provider for Gh {
    const ID: &'static str = "gh";
    const DESCRIPTION: &'static str =
        "Narrow GitHub repository, pull-request, and issue operations over broker HTTP";
    const COMMAND_WORDS: &'static [&'static str] = &["gh"];
    type Args = commands::GhArgs;
    type Capabilities = (
        typed::ContentRead,
        typed::PrList,
        typed::PrRead,
        typed::PrFiles,
        typed::PrApprove,
        typed::PrReviews,
        typed::PrComment,
        typed::PrRequestChanges,
        typed::PrDiff,
        typed::PrStatus,
        typed::RepoRead,
        typed::BranchRead,
        typed::CommitRead,
        typed::IssueRead,
        typed::IssueList,
        typed::IssueCommentsRead,
        typed::IssueComment,
        typed::PrMerge,
        typed::UserRead,
        typed::AuthStatus,
    );
    fn propose(args: Self::Args, stdin_piped: bool) -> Result<Proposal<Self>, Usage> {
        commands::propose(args, stdin_piped)
    }
}

#[allow(unsafe_code)]
mod export {
    dekopon_provider_sdk::export!(super::Gh);
}

/// Routes one invocation to its capability implementation.
///
/// The send function is injected so native tests script exact request/response exchanges without
/// any network. It is `FnMut` rather than `FnOnce` because the write capabilities perform a
/// pre-read before their write, and `gh.pull-request.status` reads the pull, Actions workflow runs,
/// and legacy commit statuses.
#[cfg(test)]
fn invoke_with<F>(
    capability: &CapabilityId,
    input: Value,
    mut send: F,
) -> Result<Value, ProviderError>
where
    F: FnMut(Request) -> Result<Response, HttpError>,
{
    let send: &mut dyn FnMut(Request) -> Result<Response, HttpError> = &mut send;
    let base = &GITHUB_API;
    match capability.as_str() {
        ids::CONTENT_READ => content::read(input, base, send),
        ids::PR_LIST => pulls::list(input, base, send),
        ids::PR_READ => pulls::read(input, base, send),
        ids::PR_FILES => pulls::files(input, base, send),
        ids::PR_DIFF => pulls::diff(input, base, send),
        ids::PR_REVIEWS => pulls::reviews(input, base, send),
        ids::PR_STATUS => pulls::status(input, base, send),
        ids::PR_APPROVE => reviews::approve(input, base, send),
        ids::PR_COMMENT => reviews::comment(input, base, send),
        ids::PR_REQUEST_CHANGES => reviews::request_changes(input, base, send),
        ids::PR_MERGE => reviews::merge(input, base, send),
        ids::REPO_READ => repos::repo(input, base, send),
        ids::BRANCH_READ => repos::branch(input, base, send),
        ids::COMMIT_READ => repos::commit(input, base, send),
        ids::USER_READ => repos::user(input, base, send),
        ids::AUTH_STATUS => auth::status(input, base, send),
        ids::ISSUE_READ => issues::read(input, base, send),
        ids::ISSUE_LIST => issues::list(input, base, send),
        ids::ISSUE_COMMENTS_READ => issues::comments(input, base, send),
        ids::ISSUE_COMMENT => issues::comment(input, base, send),
        _ => Err(ProviderError::new(
            "unknown-capability",
            "unsupported gh capability",
        )),
    }
}

// ---------------------------------------------------------------------------
// Manifest
// ---------------------------------------------------------------------------

fn capabilities() -> Vec<ProviderCapability> {
    let read = |id: &str, description: &str, schema: Value| ProviderCapability {
        id: id.parse().expect("static capability ID is valid"),
        description: description.to_owned(),
        effect: EffectKind::ReadOnly,
        risk: RiskLevel::Low,
        input_schema: schema,
    };
    let write = |id: &str, description: &str, risk, schema: Value| ProviderCapability {
        id: id.parse().expect("static capability ID is valid"),
        description: description.to_owned(),
        effect: EffectKind::ExternalWrite,
        risk,
        input_schema: schema,
    };

    vec![
        read(
            ids::AUTH_STATUS,
            "Reads the credential core quota and available authentication metadata",
            object_schema(json!({}), &[]),
        ),
        // Tier 1 — the review workflow slice.
        read(
            ids::CONTENT_READ,
            "Reads one file or directory listing at a path and optional ref",
            repo_schema(
                json!({
                    "path": {"type": "string", "maxLength": MAX_PATH_BYTES, "description": "Repository-relative path; empty string lists the repository root."},
                    "ref": ref_property(),
                }),
                &["path"],
            ),
        ),
        read(
            ids::PR_LIST,
            "Lists pull requests with optional state and author filters",
            repo_schema(
                json!({
                    "state": state_property(),
                    "author": {"type": "string", "maxLength": MAX_OWNER_BYTES, "description": "Optional login filter applied to the fetched page, after pagination."},
                    "page": page_property(),
                    "perPage": per_page_property(20),
                }),
                &[],
            ),
        ),
        read(
            ids::PR_READ,
            "Reads one pull request's metadata, state, and head/base",
            repo_schema(json!({"number": number_property()}), &["number"]),
        ),
        read(
            ids::PR_FILES,
            "Lists one pull request's changed files with bounded patches",
            repo_schema(
                json!({
                    "number": number_property(),
                    "page": page_property(),
                    "perPage": per_page_property(30),
                    "includePatch": {"type": "boolean", "description": "Include a bounded unified patch per file; defaults to true."},
                }),
                &["number"],
            ),
        ),
        write(
            ids::PR_APPROVE,
            "Submits an APPROVE review pinned to the verified head SHA",
            RiskLevel::High,
            repo_schema(
                json!({
                    "number": number_property(),
                    "body": body_property("Optional review comment."),
                    "expectedHeadSha": sha_property(),
                }),
                &["number"],
            ),
        ),
        // Tier 2 — review completeness.
        read(
            ids::PR_REVIEWS,
            "Lists existing reviews on one pull request",
            repo_schema(
                json!({
                    "number": number_property(),
                    "page": page_property(),
                    "perPage": per_page_property(20),
                }),
                &["number"],
            ),
        ),
        write(
            ids::PR_COMMENT,
            "Submits a COMMENT review pinned to the verified head SHA",
            RiskLevel::Medium,
            repo_schema(
                json!({
                    "number": number_property(),
                    "body": body_property("Review comment; required."),
                    "expectedHeadSha": sha_property(),
                }),
                &["number", "body"],
            ),
        ),
        write(
            ids::PR_REQUEST_CHANGES,
            "Submits a REQUEST_CHANGES review pinned to the verified head SHA",
            RiskLevel::Medium,
            repo_schema(
                json!({
                    "number": number_property(),
                    "body": body_property("Reason for requesting changes; required."),
                    "expectedHeadSha": sha_property(),
                }),
                &["number", "body"],
            ),
        ),
        read(
            ids::PR_DIFF,
            "Reads one pull request's unified diff, truncated with a marker",
            repo_schema(json!({"number": number_property()}), &["number"]),
        ),
        read(
            ids::PR_STATUS,
            "Reads one pull request's head Actions workflow runs and legacy commit statuses",
            repo_schema(json!({"number": number_property()}), &["number"]),
        ),
        // Tier 3 — broader read surface plus the two remaining writes.
        read(
            ids::REPO_READ,
            "Reads repository metadata: default branch, visibility, and flags",
            repo_schema(json!({}), &[]),
        ),
        read(
            ids::BRANCH_READ,
            "Reads one branch's head SHA and protection flag",
            repo_schema(
                json!({"branch": {"type": "string", "maxLength": MAX_REF_BYTES, "description": "Branch name."}}),
                &["branch"],
            ),
        ),
        read(
            ids::COMMIT_READ,
            "Reads one commit's message, author, stats, and bounded file list",
            repo_schema(json!({"ref": ref_property()}), &["ref"]),
        ),
        read(
            ids::ISSUE_READ,
            "Reads one issue with a bounded body",
            repo_schema(json!({"number": number_property()}), &["number"]),
        ),
        read(
            ids::ISSUE_LIST,
            "Lists issues (GitHub includes pull requests; each item is flagged)",
            repo_schema(
                json!({
                    "state": state_property(),
                    "page": page_property(),
                    "perPage": per_page_property(20),
                }),
                &[],
            ),
        ),
        read(
            ids::ISSUE_COMMENTS_READ,
            "Lists comments on one issue or pull request",
            repo_schema(
                json!({
                    "number": number_property(),
                    "page": page_property(),
                    "perPage": per_page_property(20),
                }),
                &["number"],
            ),
        ),
        write(
            ids::ISSUE_COMMENT,
            "Posts one comment on an issue or pull request",
            RiskLevel::Medium,
            repo_schema(
                json!({
                    "number": number_property(),
                    "body": body_property("Comment body; required."),
                }),
                &["number", "body"],
            ),
        ),
        write(
            ids::PR_MERGE,
            "Merges one pull request, pinned to the verified head SHA",
            RiskLevel::High,
            repo_schema(
                json!({
                    "number": number_property(),
                    "mergeMethod": {"type": "string", "enum": ["merge", "squash", "rebase"], "description": "Merge strategy; defaults to merge."},
                    "expectedHeadSha": sha_property(),
                }),
                &["number"],
            ),
        ),
        read(
            ids::USER_READ,
            "Reads one user's public profile",
            object_schema(
                json!({
                    "login": {"type": "string", "maxLength": MAX_OWNER_BYTES, "description": "GitHub login."},
                }),
                &["login"],
            ),
        ),
    ]
}

/// Builds an object schema whose properties always include `owner` and `repo`.
fn repo_schema(mut extra: Value, required: &[&str]) -> Value {
    let properties = extra.as_object_mut().expect("schema fragments are objects");
    properties.insert(
        "owner".to_owned(),
        json!({"type": "string", "maxLength": MAX_OWNER_BYTES, "description": "Repository owner login or organization."}),
    );
    properties.insert(
        "repo".to_owned(),
        json!({"type": "string", "maxLength": MAX_REPO_BYTES, "description": "Repository name."}),
    );
    let mut all_required = vec!["owner", "repo"];
    all_required.extend_from_slice(required);
    object_schema(Value::Object(properties.clone()), &all_required)
}

fn object_schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

fn number_property() -> Value {
    json!({"type": "integer", "minimum": 1, "maximum": MAX_NUMBER})
}

fn page_property() -> Value {
    json!({"type": "integer", "minimum": 1, "maximum": MAX_PAGE, "description": "Result page, starting at 1."})
}

fn per_page_property(default: u32) -> Value {
    json!({"type": "integer", "minimum": 1, "maximum": MAX_PER_PAGE, "description": format!("Items per page; defaults to {default}.")})
}

fn state_property() -> Value {
    json!({"type": "string", "enum": ["open", "closed", "all"], "description": "State filter; defaults to open."})
}

fn ref_property() -> Value {
    json!({"type": "string", "maxLength": MAX_REF_BYTES, "description": "Branch, tag, or commit SHA."})
}

fn sha_property() -> Value {
    json!({"type": "string", "minLength": 40, "maxLength": 40, "description": "Optional 40-hex expected head SHA; the write refuses if the head moved."})
}

fn body_property(description: &str) -> Value {
    json!({"type": "string", "minLength": 1, "maxLength": MAX_BODY_IN_BYTES, "description": description})
}

// ---------------------------------------------------------------------------
// Shared input validation
// ---------------------------------------------------------------------------

/// Validates a GitHub owner or user login: 1–39 of `[A-Za-z0-9-]`, no leading, trailing, or
/// doubled hyphen. The same grammar guards every URL segment interpolation of a login.
fn validate_login(value: &str) -> Result<(), ProviderError> {
    let bytes = value.as_bytes();
    if value.is_empty()
        || value.len() > MAX_OWNER_BYTES
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
        || bytes.first() == Some(&b'-')
        || bytes.last() == Some(&b'-')
        || value.contains("--")
    {
        return Err(invalid_input());
    }
    Ok(())
}

/// Validates a repository name: 1–100 of `[A-Za-z0-9._-]`, and never a dot-only name.
fn validate_repo(value: &str) -> Result<(), ProviderError> {
    if value.is_empty()
        || value.len() > MAX_REPO_BYTES
        || value == "."
        || value == ".."
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(invalid_input());
    }
    Ok(())
}

fn validate_number(value: u32) -> Result<(), ProviderError> {
    if !(1..=MAX_NUMBER).contains(&value) {
        return Err(invalid_input());
    }
    Ok(())
}

/// Validates a git ref (branch, tag, or SHA): bounded, `[A-Za-z0-9._/-]`, no `..` segment, no
/// leading `-` or `/`, no empty segment.
fn validate_ref(value: &str) -> Result<(), ProviderError> {
    if value.is_empty()
        || value.len() > MAX_REF_BYTES
        || value.starts_with('-')
        || value.starts_with('/')
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/'))
        || value
            .split('/')
            .any(|segment| segment.is_empty() || segment == "..")
    {
        return Err(invalid_input());
    }
    Ok(())
}

/// Validates a repository path. The empty path is the repository root; otherwise every segment
/// must be non-empty, not `.` or `..`, and free of control bytes.
fn validate_path(value: &str) -> Result<(), ProviderError> {
    if value.len() > MAX_PATH_BYTES {
        return Err(invalid_input());
    }
    if value.is_empty() {
        return Ok(());
    }
    if value.starts_with('/') || value.ends_with('/') {
        return Err(invalid_input());
    }
    for segment in value.split('/') {
        if segment.is_empty()
            || segment == "."
            || segment == ".."
            || segment.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(invalid_input());
        }
    }
    Ok(())
}

fn validate_body(value: &str) -> Result<(), ProviderError> {
    if value.is_empty() || value.len() > MAX_BODY_IN_BYTES {
        return Err(invalid_input());
    }
    Ok(())
}

fn validate_page(
    page: Option<u32>,
    per_page: Option<u32>,
) -> Result<(u32, Option<u32>), ProviderError> {
    let page = page.unwrap_or(1);
    if !(1..=MAX_PAGE).contains(&page) {
        return Err(invalid_input());
    }
    if let Some(per_page) = per_page
        && !(1..=MAX_PER_PAGE).contains(&per_page)
    {
        return Err(invalid_input());
    }
    Ok((page, per_page))
}

fn is_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_expected_sha(value: Option<&str>) -> Result<(), ProviderError> {
    if let Some(value) = value
        && !is_sha(value)
    {
        return Err(invalid_input());
    }
    Ok(())
}

/// Percent-encodes one path segment, keeping only RFC 3986 unreserved bytes.
///
/// The validators above make this nearly a no-op, but encoding anyway means a future relaxation
/// of a charset cannot silently become path injection.
fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char);
            }
            other => {
                encoded.push('%');
                encoded.push(
                    char::from_digit(u32::from(other >> 4), 16)
                        .expect("nibble")
                        .to_ascii_uppercase(),
                );
                encoded.push(
                    char::from_digit(u32::from(other & 0xf), 16)
                        .expect("nibble")
                        .to_ascii_uppercase(),
                );
            }
        }
    }
    encoded
}

/// Percent-encodes a validated multi-segment path, preserving `/` separators.
fn encode_path(value: &str) -> String {
    value
        .split('/')
        .map(percent_encode)
        .collect::<Vec<_>>()
        .join("/")
}

// ---------------------------------------------------------------------------
// Request construction
// ---------------------------------------------------------------------------

/// Joins a `/`-rooted request path onto the configured base, keeping any base path prefix once.
fn url(base: &Base, path: &str) -> Result<String, ProviderError> {
    base.join(path).map_err(|_| invalid_request())
}

/// Builds a request carrying exactly the constant GitHub headers.
///
/// `authorization` is never set here or anywhere else in this guest: the host rejects guest-set
/// credential headers, and broker-owned injection is the only path a credential may take.
fn github_request(
    http_method: &'static str,
    uri: String,
    accept: &'static str,
) -> Result<Request, ProviderError> {
    Ok(Request::new(http_method, uri)
        .map_err(|_| invalid_request())?
        .with_header(header("accept", accept)?)
        .with_header(header("x-github-api-version", API_VERSION)?)
        .with_header(header("user-agent", USER_AGENT)?))
}

/// Builds a JSON write request: the constant headers plus `content-type` and a serialized body.
fn github_json_request(
    http_method: &'static str,
    uri: String,
    body: &Value,
) -> Result<Request, ProviderError> {
    let body = serde_json::to_vec(body).map_err(|_| invalid_request())?;
    Ok(github_request(http_method, uri, ACCEPT_JSON)?
        .with_header(header("content-type", "application/json")?)
        .with_body(body))
}

fn header(name: &'static str, value: &'static str) -> Result<Header, ProviderError> {
    Header::text(name, value).map_err(|_| invalid_request())
}

/// Sends one GET and maps every non-200 status to its stable error code.
fn send_get(
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
    uri: String,
    accept: &'static str,
) -> Result<Response, ProviderError> {
    let response = send(github_request(method::GET, uri, accept)?).map_err(|_| http_failed())?;
    if response.status != 200 {
        return Err(status_error(&response));
    }
    Ok(response)
}

/// Maps a non-success GitHub status to a stable, model-safe error code.
///
/// A 403 is rate limiting only when GitHub says the primary quota is exhausted; a plain 403 is an
/// authorization refusal. A 429 is rate limiting by definition, with or without the header.
fn status_error(response: &Response) -> ProviderError {
    match response.status {
        401 => ProviderError::new(
            "unauthorized",
            "credential rejected: expired, revoked or malformed",
        ),
        403 if rate_limit_exhausted(response) => rate_limited(),
        403 => forbidden(response),
        404 => ProviderError::new("not-found", "not found, or not visible to this credential"),
        422 => ProviderError::new("unprocessable", "endpoint refused the request as invalid"),
        429 => rate_limited(),
        _ => unexpected_status(),
    }
}

fn forbidden(response: &Response) -> ProviderError {
    let message = if let Some(permissions) = header_text(response, "x-accepted-github-permissions")
    {
        format!("credential lacks a permission this call needs; GitHub accepts: {permissions}")
    } else if let Some(accepted) = header_text(response, "x-accepted-oauth-scopes") {
        let held = header_text(response, "x-oauth-scopes").unwrap_or_default();
        format!("credential lacks a scope; GitHub accepts: {accepted}; credential has: {held}")
    } else {
        "credential is not permitted to make this call".to_owned()
    };
    ProviderError::new("forbidden", message)
}

fn header_text<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
    response
        .headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case(name))
        .and_then(|header| core::str::from_utf8(&header.value).ok())
}

fn rate_limited() -> ProviderError {
    ProviderError::new("rate-limited", "endpoint rate limit is exhausted")
}

/// Returns all values for a case-insensitive header name, in wire order.
///
/// `dekopon-provider-http` carried this as `Response::header_values` until 0.13.0 deleted it as
/// unreferenced; it is two callers' worth of code, and duplicate field names are preserved on the
/// wire, so the whole point is that it is an iterator rather than a lookup.
fn header_values<'a>(response: &'a Response, name: &'a str) -> impl Iterator<Item = &'a [u8]> {
    response
        .headers
        .iter()
        .filter(move |header| header.name.eq_ignore_ascii_case(name))
        .map(|header| header.value.as_slice())
}

fn rate_limit_exhausted(response: &Response) -> bool {
    header_values(response, "x-ratelimit-remaining").any(|value| value == b"0")
}

/// Reports whether a paginated response advertises another page via `Link: rel="next"`.
fn has_next_link(response: &Response) -> bool {
    header_values(response, "link").any(|value| {
        core::str::from_utf8(value)
            .is_ok_and(|link| link.split(',').any(|part| part.contains("rel=\"next\"")))
    })
}

// ---------------------------------------------------------------------------
// Shared response handling
// ---------------------------------------------------------------------------

/// Decodes a JSON response body, collapsing every parse failure to `invalid-response`.
fn decode<T: DeserializeOwned>(body: &[u8]) -> Result<T, ProviderError> {
    serde_json::from_slice::<T>(body).map_err(|_| invalid_response())
}

/// Returns a bounded copy of `value` and whether it was truncated, never splitting a character.
fn truncate_text(value: &str, max: usize) -> (String, bool) {
    if value.len() <= max {
        return (value.to_owned(), false);
    }
    let mut end = max;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_owned(), true)
}

/// Projects an optional free-text field into `(text, truncated)` JSON values.
fn bounded_optional(value: Option<&str>, max: usize) -> (Value, bool) {
    match value {
        Some(text) => {
            let (text, truncated) = truncate_text(text, max);
            (Value::String(text), truncated)
        }
        None => (Value::Null, false),
    }
}

/// Accepts a response timestamp only in its expected bounded shape.
fn timestamp(value: &str) -> Result<&str, ProviderError> {
    if value.is_empty() || value.len() > MAX_TIMESTAMP_BYTES {
        return Err(invalid_response());
    }
    Ok(value)
}

/// Projects an optional response login, bounding it rather than trusting response sizes.
fn login_out(value: Option<&RawUser>) -> Value {
    match value {
        Some(user) if !user.login.is_empty() && user.login.len() <= MAX_LOGIN_OUT_BYTES => {
            Value::String(user.login.clone())
        }
        _ => Value::Null,
    }
}

/// The `user`/`author` object shape GitHub embeds in most resources.
#[derive(Debug, serde::Deserialize)]
struct RawUser {
    login: String,
}

// ---------------------------------------------------------------------------
// Stable errors
// ---------------------------------------------------------------------------

fn invalid_input() -> ProviderError {
    ProviderError::new(
        "invalid-input",
        "input does not match the capability contract",
    )
}

fn invalid_request() -> ProviderError {
    ProviderError::new(
        "invalid-request",
        "could not construct bounded HTTP request",
    )
}

fn http_failed() -> ProviderError {
    ProviderError::new("http-failed", "broker HTTP request failed")
}

fn unexpected_status() -> ProviderError {
    ProviderError::new(
        "unexpected-status",
        "endpoint returned an unexpected status",
    )
}

fn invalid_response() -> ProviderError {
    ProviderError::new("invalid-response", "endpoint returned an invalid resource")
}

// ---------------------------------------------------------------------------
// Test support and shared tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod testutil {
    use std::collections::VecDeque;

    use dekopon_provider_sdk::provider::{Header, HttpError, Request, Response};
    use serde_json::Value;

    /// One scripted exchange: assertions to run on the request, then the canned reply.
    pub(crate) struct Step {
        pub check: Box<dyn Fn(&Request)>,
        pub reply: Result<Response, HttpError>,
    }

    pub(crate) fn step(
        check: impl Fn(&Request) + 'static,
        reply: Result<Response, HttpError>,
    ) -> Step {
        Step {
            check: Box::new(check),
            reply,
        }
    }

    /// Builds a scripted send function that fails loudly on any extra or out-of-order request.
    ///
    /// Every request is also held to the guest's constant-header contract: `user-agent` and
    /// `x-github-api-version` present, `authorization` absent — the latter asserted here so no
    /// individual test can forget it.
    pub(crate) fn scripted(steps: Vec<Step>) -> impl FnMut(Request) -> Result<Response, HttpError> {
        let mut steps = steps.into_iter().collect::<VecDeque<_>>();
        move |request: Request| {
            let step = steps.pop_front().expect("unexpected extra HTTP request");
            assert_standard_headers(&request);
            (step.check)(&request);
            step.reply
        }
    }

    pub(crate) fn assert_standard_headers(request: &Request) {
        assert!(
            !request
                .headers
                .iter()
                .any(|header| header.name.eq_ignore_ascii_case("authorization")),
            "guest must never set authorization"
        );
        assert!(
            request
                .headers
                .iter()
                .any(|header| header.name.eq_ignore_ascii_case("user-agent")),
            "user-agent is required by GitHub"
        );
        assert!(
            request.headers.iter().any(|header| header
                .name
                .eq_ignore_ascii_case("x-github-api-version")
                && header.value == b"2022-11-28"),
            "api version pin is required"
        );
    }

    pub(crate) fn json_response(status: u16, body: &Value) -> Result<Response, HttpError> {
        Ok(Response {
            status,
            headers: Vec::new(),
            body: serde_json::to_vec(body).expect("mock body serializes"),
        })
    }

    pub(crate) fn response_with_headers(
        status: u16,
        headers: &[(&str, &str)],
        body: &Value,
    ) -> Result<Response, HttpError> {
        Ok(Response {
            status,
            headers: headers
                .iter()
                .map(|(name, value)| Header::text(*name, *value).expect("mock header"))
                .collect(),
            body: serde_json::to_vec(body).expect("mock body serializes"),
        })
    }

    pub(crate) fn capability(value: &str) -> dekopon_provider_sdk::CapabilityId {
        value.parse().expect("valid capability fixture")
    }

    pub(crate) fn accept_of(request: &Request) -> Vec<u8> {
        request
            .headers
            .iter()
            .find(|header| header.name.eq_ignore_ascii_case("accept"))
            .map(|header| header.value.clone())
            .expect("accept header present")
    }
}

#[cfg(test)]
mod tests {
    use dekopon_provider_sdk::provider;
    use dekopon_provider_sdk::provider::HttpErrorCode;
    use dekopon_provider_sdk::{EffectKind, RiskLevel};
    use dekopon_provider_sdk_testkit::{HttpScript, Native};
    use serde_json::json;

    use super::testutil::{capability, scripted, step};
    use super::{Gh, HttpError, Response, invoke_with, truncate_text};

    #[test]
    fn manifest_covers_the_full_designed_surface() {
        let manifest = provider::manifest::<Gh>().expect("manifest");
        assert_eq!(manifest.id.as_str(), "gh");
        assert_eq!(manifest.capabilities.len(), 20);

        let external_writes = manifest
            .capabilities
            .iter()
            .filter(|capability| capability.effect == EffectKind::ExternalWrite)
            .map(|capability| capability.id.as_str().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            external_writes,
            vec![
                "gh.pull-request.approve",
                "gh.pull-request.comment",
                "gh.pull-request.request-changes",
                "gh.issue.comment",
                "gh.pull-request.merge",
            ]
        );

        for capability in &manifest.capabilities {
            assert_eq!(
                capability.input_schema["type"], "object",
                "{}",
                capability.id
            );
            assert_eq!(
                capability.input_schema["additionalProperties"],
                json!(false),
                "{}",
                capability.id
            );
            // No passthrough capability wears the gh costume.
            assert!(
                !capability.id.as_str().starts_with("gh.api"),
                "{}",
                capability.id
            );
        }

        let high_risk = manifest
            .capabilities
            .iter()
            .filter(|capability| capability.risk == RiskLevel::High)
            .map(|capability| capability.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            high_risk,
            vec!["gh.pull-request.approve", "gh.pull-request.merge"]
        );
    }

    #[test]
    fn typed_manifest_preserves_all_operation_descriptions() {
        let typed = provider::manifest::<Gh>().expect("typed manifest");
        for previous in super::capabilities() {
            let current = typed
                .capabilities
                .iter()
                .find(|capability| capability.id == previous.id)
                .expect("every prior capability remains declared");
            assert_eq!(current.description, previous.description, "{}", previous.id);
            assert_ne!(current.description, previous.id.as_str());
        }
    }

    fn repo_reply() -> Response {
        Response {
            status: 200,
            headers: Vec::new(),
            body: json!({"name":"hello","private":false,"default_branch":"main","fork":false,"updated_at":"2026-01-01T00:00:00Z"})
                .to_string()
                .into_bytes(),
        }
    }

    #[test]
    fn base_url_setting_replaces_the_github_origin_and_fails_closed() {
        let repo = json!({"owner": "octo", "repo": "hello"}).to_string();
        for (settings, host, uri) in [
            (
                None,
                "api.github.com",
                "https://api.github.com/repos/octo/hello",
            ),
            (
                Some(json!({})),
                "api.github.com",
                "https://api.github.com/repos/octo/hello",
            ),
            (
                Some(json!({"baseUrl": "https://ghe.example.test/api/v3/"})),
                "ghe.example.test",
                "https://ghe.example.test/api/v3/repos/octo/hello",
            ),
        ] {
            let mut native = Native::<Gh>::new().http(HttpScript::new(host, "GET", repo_reply()));
            if let Some(settings) = settings.clone() {
                native = native.settings(settings);
            }
            let output = native.call("gh.repo.read", &repo);
            assert_eq!(output.status, 0, "{settings:?}: {}", output.stderr);
            let sent = native.requests();
            assert_eq!(sent.len(), 1, "{settings:?}");
            assert_eq!(sent[0].uri, uri, "{settings:?}");
        }
        for settings in [
            json!({"baseUrl": "https://api.github.com?x=1"}),
            json!({"baseUrl": "https://user@api.github.com"}),
            json!({"baseUrl": "https://api.github.com/#top"}),
            json!({"baseUrl": "ftp://api.github.com"}),
            json!({"baseUrl": "api.github.com"}),
            json!({"baseUrl": "https://"}),
            json!({"baseUrl": 443}),
            json!({"endpoint": "https://api.github.com"}),
        ] {
            let native = Native::<Gh>::new()
                .settings(settings.clone())
                .http(HttpScript::new("api.github.com", "GET", repo_reply()));
            let output = native.call("gh.repo.read", &repo);
            assert_ne!(output.status, 0, "accepted {settings}");
            assert!(
                output.stderr.contains("settings"),
                "{settings}: {}",
                output.stderr
            );
            assert!(native.requests().is_empty(), "{settings}");
        }
        let native =
            Native::<Gh>::new().http(HttpScript::new("api.github.com", "GET", repo_reply()));
        let output = native.call(
            "gh.repo.read",
            &json!({"owner": "octo", "repo": "hello", "endpoint": "http://127.0.0.1:43123"})
                .to_string(),
        );
        assert_ne!(output.status, 0, "a model-supplied endpoint is refused");
        assert!(native.requests().is_empty());
    }

    #[test]
    fn no_capability_schema_names_an_origin() {
        let manifest = provider::manifest::<Gh>().expect("manifest");
        assert_eq!(manifest.capabilities.len(), 20);
        for capability in &manifest.capabilities {
            let properties = capability.input_schema["properties"]
                .as_object()
                .expect("object schema");
            for name in properties.keys() {
                let lower = name.to_ascii_lowercase();
                assert!(
                    !lower.contains("endpoint") && !lower.contains("url"),
                    "{} carries {name}",
                    capability.id
                );
            }
        }
    }

    #[test]
    fn unknown_capabilities_are_rejected_without_http() {
        let error = invoke_with(&capability("gh.api.get"), json!({}), |_| {
            unreachable!("unknown capability must not call HTTP")
        })
        .expect_err("unknown capability fails");
        assert_eq!(error.code(), "unknown-capability");
    }

    #[test]
    fn transport_failures_never_expose_host_detail() {
        let error = invoke_with(
            &capability("gh.repo.read"),
            json!({"owner": "octo", "repo": "hello"}),
            scripted(vec![step(
                |_| {},
                Err(HttpError {
                    code: HttpErrorCode::Denied,
                    message: "secret internal path and header detail".to_owned(),
                }),
            )]),
        )
        .expect_err("host denial fails the invocation");
        assert_eq!(error.code(), "http-failed");
        assert_eq!(error.message(), "broker HTTP request failed");
    }

    #[test]
    fn rate_limit_discrimination_follows_the_remaining_header() {
        for (status, headers, expected) in [
            (
                403_u16,
                vec![("x-ratelimit-remaining", "0")],
                "rate-limited",
            ),
            (403, vec![("x-ratelimit-remaining", "12")], "forbidden"),
            (403, vec![], "forbidden"),
            (401, vec![], "unauthorized"),
            (429, vec![], "rate-limited"),
            (422, vec![], "unprocessable"),
            (404, vec![], "not-found"),
            (500, vec![], "unexpected-status"),
        ] {
            let error = invoke_with(
                &capability("gh.repo.read"),
                json!({"owner": "octo", "repo": "hello"}),
                scripted(vec![step(
                    |_| {},
                    super::testutil::response_with_headers(status, &headers, &json!({})),
                )]),
            )
            .expect_err("non-200 fails");
            assert_eq!(error.code(), expected, "status {status}");
        }
    }

    #[test]
    fn malformed_bodies_are_invalid_response() {
        let error = invoke_with(
            &capability("gh.repo.read"),
            json!({"owner": "octo", "repo": "hello"}),
            scripted(vec![step(|_| {}, {
                Ok(Response {
                    status: 200,
                    headers: Vec::new(),
                    body: b"not json".to_vec(),
                })
            })]),
        )
        .expect_err("malformed body fails");
        assert_eq!(error.code(), "invalid-response");
    }

    #[test]
    fn input_grammar_failures_never_reach_http() {
        for (capability_id, input) in [
            ("gh.repo.read", json!({"owner": "-bad", "repo": "x"})),
            ("gh.repo.read", json!({"owner": "a--b", "repo": "x"})),
            ("gh.repo.read", json!({"owner": "octo", "repo": ".."})),
            (
                "gh.pull-request.read",
                json!({"owner": "octo", "repo": "x", "number": 0}),
            ),
            (
                "gh.pull-request.read",
                json!({"owner": "octo", "repo": "x", "number": 1_000_001}),
            ),
            (
                "gh.content.read",
                json!({"owner": "octo", "repo": "x", "path": "a/../b"}),
            ),
            (
                "gh.content.read",
                json!({"owner": "octo", "repo": "x", "path": "/leading"}),
            ),
            (
                "gh.commit.read",
                json!({"owner": "octo", "repo": "x", "ref": "-rev"}),
            ),
            (
                "gh.commit.read",
                json!({"owner": "octo", "repo": "x", "ref": "a//b"}),
            ),
            (
                "gh.pull-request.approve",
                json!({"owner": "octo", "repo": "x", "number": 1, "expectedHeadSha": "short"}),
            ),
            (
                "gh.pull-request.comment",
                json!({"owner": "octo", "repo": "x", "number": 1, "body": ""}),
            ),
            (
                "gh.pull-request.list",
                json!({"owner": "octo", "repo": "x", "page": 51}),
            ),
            (
                "gh.pull-request.list",
                json!({"owner": "octo", "repo": "x", "state": "merged"}),
            ),
            ("gh.user.read", json!({"login": "bad login"})),
            (
                "gh.repo.read",
                json!({"owner": "octo", "repo": "x", "extra": true}),
            ),
        ] {
            let error = invoke_with(&capability(capability_id), input.clone(), |_| {
                unreachable!("invalid input must not call HTTP: {capability_id} {input}")
            })
            .expect_err("invalid input fails");
            assert_eq!(error.code(), "invalid-input", "{capability_id} {input}");
        }
    }

    #[test]
    fn truncation_is_exact_and_character_safe() {
        let (text, truncated) = truncate_text("abcdef", 6);
        assert_eq!((text.as_str(), truncated), ("abcdef", false));
        let (text, truncated) = truncate_text("abcdefg", 6);
        assert_eq!((text.as_str(), truncated), ("abcdef", true));
        // A four-byte scalar straddling the boundary is dropped whole.
        let (text, truncated) = truncate_text("abcd😀", 6);
        assert_eq!((text.as_str(), truncated), ("abcd", true));
    }
}
