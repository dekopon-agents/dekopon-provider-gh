//! A "fake `gh`": narrow GitHub operations as separately named Dekopon capabilities.
//!
//! Almost every operation is one fixed REST request shape (or one fixed pre-read plus one write)
//! against `api.github.com`, projected into a small bounded output. The two exceptions are
//! `gh.pull-request.list` and `gh.issue.list`, which take a second, mutually exclusive request
//! shape when their optional `search` field is present: GitHub's search endpoint
//! (`GET /search/issues`) instead of the plain list endpoint, with the caller's query text parsed,
//! allowlisted, and rebuilt — never forwarded — before it is prepended with this capability's own
//! `repo:`/`is:` scope (see `build_search_query` below). There is deliberately no generic
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

use dekopon_provider_http::{Header, HttpError, Request, Response, method};
use dekopon_provider_sdk::{
    CapabilityId, CommandRun, EffectKind, Provider, ProviderApiVersion, ProviderCapability,
    ProviderError, ProviderManifest, RiskLevel,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

mod commands;
mod content;
mod issues;
mod pulls;
mod repos;
mod reviews;

const DEFAULT_ENDPOINT: &str = "https://api.github.com";
const PRODUCTION_HOST: &str = "api.github.com";
const MAX_ENDPOINT_BYTES: usize = 512;

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
// GitHub's REST API itself caps `per_page` at 100 on every list endpoint this provider calls; this
// is that ceiling, not an arbitrary tighter one. `gh --per-page 100` is a completely ordinary
// argument against the real API, and a model reaching for gh's own remembered limits should not be
// refused something GitHub itself allows. `MAX_LIST_ITEMS` (below) still separately bounds how many
// projected items an invocation ever returns, and every paginated capability's `hasMore` now also
// considers that per-invocation truncation, not just GitHub's `Link` header, since a per-request
// fetch of 51-100 raw rows would otherwise be silently truncated to 50 while `hasMore` said false.
const MAX_PER_PAGE: u32 = 100;
const MAX_LABEL_BYTES: usize = 50;
const MAX_MILESTONE_BYTES: usize = 32;
const MAX_ISSUE_TYPE_BYTES: usize = 50;
const MAX_COMMIT_TITLE_IN_BYTES: usize = 256;
// GitHub's own search `q` parameter is capped around 256 characters once every qualifier is
// counted. This bounds only the caller-supplied fragment; `repo:{owner}/{repo} is:{scope}` (up to
// roughly 150 bytes for the longest legal owner/repo) is prepended afterward, so this leaves
// headroom under GitHub's real ceiling rather than risking a query that GitHub itself would 422.
const MAX_SEARCH_QUERY_BYTES: usize = 200;

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

mod bindings {
    wit_bindgen::generate!({
        path: "wit",
        world: "provider",
        generate_all,
        pub_export_macro: true,
    });
}

struct Gh;

impl Provider for Gh {
    fn manifest() -> ProviderManifest {
        ProviderManifest {
            api_version: ProviderApiVersion::V1Alpha1,
            id: "gh".parse().expect("static provider ID is valid"),
            description:
                "Narrow GitHub repository, pull-request, and issue operations over broker HTTP"
                    .to_owned(),
            command_words: vec!["gh".to_owned()],
            capabilities: capabilities(),
        }
    }

    fn run_command(argv: &[String], stdin: Option<&str>) -> Result<CommandRun, ProviderError> {
        commands::run(argv, stdin)
    }

    fn invoke(capability: &CapabilityId, input: Value) -> Result<Value, ProviderError> {
        invoke_with(capability, input, dekopon_provider_http::send)
    }
}

/// Routes one invocation to its capability implementation.
///
/// The send function is injected so native tests script exact request/response exchanges without
/// any network. It is `FnMut` rather than `FnOnce` because the write capabilities perform a
/// pre-read before their write, and `gh.pull-request.status` reads the pull, Actions workflow runs,
/// and legacy commit statuses.
fn invoke_with<F>(
    capability: &CapabilityId,
    input: Value,
    mut send: F,
) -> Result<Value, ProviderError>
where
    F: FnMut(Request) -> Result<Response, HttpError>,
{
    let send: &mut dyn FnMut(Request) -> Result<Response, HttpError> = &mut send;
    match capability.as_str() {
        ids::CONTENT_READ => content::read(input, send),
        ids::PR_LIST => pulls::list(input, send),
        ids::PR_READ => pulls::read(input, send),
        ids::PR_FILES => pulls::files(input, send),
        ids::PR_DIFF => pulls::diff(input, send),
        ids::PR_REVIEWS => pulls::reviews(input, send),
        ids::PR_STATUS => pulls::status(input, send),
        ids::PR_APPROVE => reviews::approve(input, send),
        ids::PR_COMMENT => reviews::comment(input, send),
        ids::PR_REQUEST_CHANGES => reviews::request_changes(input, send),
        ids::PR_MERGE => reviews::merge(input, send),
        ids::REPO_READ => repos::repo(input, send),
        ids::BRANCH_READ => repos::branch(input, send),
        ids::COMMIT_READ => repos::commit(input, send),
        ids::USER_READ => repos::user(input, send),
        ids::ISSUE_READ => issues::read(input, send),
        ids::ISSUE_LIST => issues::list(input, send),
        ids::ISSUE_COMMENTS_READ => issues::comments(input, send),
        ids::ISSUE_COMMENT => issues::comment(input, send),
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
            "Lists pull requests with optional state, author, base/head, assignee, label, and draft filters",
            repo_schema(
                json!({
                    "state": pull_state_property(),
                    "author": {"type": "string", "maxLength": MAX_OWNER_BYTES, "description": "Optional login filter; applied to the fetched page, after pagination (GitHub's pull-request list endpoint has no author query parameter)."},
                    "base": {"type": "string", "maxLength": MAX_REF_BYTES, "description": "Filter by base branch name."},
                    "head": {"type": "string", "maxLength": MAX_REF_BYTES, "description": "Filter by head branch name (bare branch, not owner:branch)."},
                    "assignee": {"type": "string", "maxLength": MAX_OWNER_BYTES, "description": "Optional login filter; applied to the fetched page, after pagination, same as author."},
                    "labels": {"type": "array", "items": {"type": "string", "maxLength": MAX_LABEL_BYTES}, "maxItems": MAX_LABELS, "description": "Filter requiring every given label; applied to the fetched page, after pagination."},
                    "draft": {"type": "boolean", "description": "Filter by draft state; applied to the fetched page, after pagination."},
                    "search": {"type": "string", "maxLength": MAX_SEARCH_QUERY_BYTES, "description": "GitHub search query text; parsed, allowlisted, and rebuilt with this capability's own repo/type scope prepended. Cannot be combined with the other filters above; express them as qualifiers inside this query instead."},
                    "page": page_property(),
                    "perPage": per_page_property(30),
                }),
                &[],
            ),
        ),
        read(
            ids::PR_READ,
            "Reads one pull request's metadata, state, and head/base",
            repo_schema(
                json!({
                    "number": number_property(),
                    "comments": {"type": "boolean", "description": "Include a bounded page of the pull request's conversation comments; defaults to false."},
                }),
                &["number"],
            ),
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
            repo_schema(
                json!({
                    "number": number_property(),
                    "nameOnly": {"type": "boolean", "description": "Return only the changed file paths instead of the diff text; defaults to false."},
                }),
                &["number"],
            ),
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
            repo_schema(
                json!({
                    "number": number_property(),
                    "comments": {"type": "boolean", "description": "Include a bounded page of the issue's comments; defaults to false."},
                }),
                &["number"],
            ),
        ),
        read(
            ids::ISSUE_LIST,
            "Lists issues (GitHub includes pull requests; each item is flagged) with optional author, assignee, label, milestone, mention, and type filters",
            repo_schema(
                json!({
                    "state": state_property(),
                    "author": {"type": "string", "maxLength": MAX_OWNER_BYTES, "description": "Filter by author login."},
                    "assignee": {"type": "string", "maxLength": MAX_OWNER_BYTES, "description": "Filter by assignee login."},
                    "labels": {"type": "array", "items": {"type": "string", "maxLength": MAX_LABEL_BYTES}, "maxItems": MAX_LABELS, "description": "Filter requiring every given label."},
                    "milestone": {"type": "string", "maxLength": MAX_MILESTONE_BYTES, "description": "Filter by milestone number, or the literal * or none; a milestone title is not supported."},
                    "mention": {"type": "string", "maxLength": MAX_OWNER_BYTES, "description": "Filter by mentioned login."},
                    "type": {"type": "string", "maxLength": MAX_ISSUE_TYPE_BYTES, "description": "Filter by issue type name, or the literal * or none."},
                    "search": {"type": "string", "maxLength": MAX_SEARCH_QUERY_BYTES, "description": "GitHub search query text; parsed, allowlisted, and rebuilt with this capability's own repo/type scope prepended. Cannot be combined with the other filters above; express them as qualifiers inside this query instead."},
                    "page": page_property(),
                    "perPage": per_page_property(30),
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
                    "commitTitle": {"type": "string", "minLength": 1, "maxLength": MAX_COMMIT_TITLE_IN_BYTES, "description": "Optional commit title for the merge commit."},
                    "commitMessage": body_property("Optional commit message body for the merge commit."),
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
                    "endpoint": endpoint_property(),
                }),
                &["login"],
            ),
        ),
    ]
}

/// Builds an object schema whose properties always include `owner`, `repo`, and `endpoint`.
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
    properties.insert("endpoint".to_owned(), endpoint_property());
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

fn endpoint_property() -> Value {
    json!({
        "type": "string",
        "maxLength": MAX_ENDPOINT_BYTES,
        "description": "Optional broker-constrained endpoint; defaults to the GitHub API. Plain HTTP accepts only literal loopback test endpoints."
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

/// Pull requests additionally have a `merged` state real `gh pr list --state` accepts; issues never
/// do, so this stays a separate schema from `state_property` rather than a shared, wider enum.
fn pull_state_property() -> Value {
    json!({"type": "string", "enum": ["open", "closed", "merged", "all"], "description": "State filter; defaults to open. `merged` is requested from GitHub as closed, then filtered client-side on mergedAt."})
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

/// Validates a GitHub label name: bounded, no control characters. GitHub itself allows nearly any
/// printable character in a label name (including spaces), so this stays deliberately permissive.
fn validate_label(value: &str) -> Result<(), ProviderError> {
    if value.is_empty()
        || value.len() > MAX_LABEL_BYTES
        || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(invalid_input());
    }
    Ok(())
}

/// Validates a milestone filter: a positive integer, or the literal `*`/`none`. Real `gh` also
/// accepts a milestone *title* and resolves it to a number with an extra lookup this provider does
/// not perform; titles are out of scope here and rejected the same as any other malformed value.
fn validate_milestone(value: &str) -> Result<(), ProviderError> {
    if value == "*" || value == "none" {
        return Ok(());
    }
    if !value.is_empty()
        && value.len() <= MAX_MILESTONE_BYTES
        && value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Ok(());
    }
    Err(invalid_input())
}

/// Validates an issue-type filter: bounded, no control characters, or the literal `*`/`none`.
fn validate_issue_type(value: &str) -> Result<(), ProviderError> {
    if value == "*" || value == "none" {
        return Ok(());
    }
    if value.is_empty()
        || value.len() > MAX_ISSUE_TYPE_BYTES
        || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(invalid_input());
    }
    Ok(())
}

fn validate_body(value: &str) -> Result<(), ProviderError> {
    if value.is_empty() || value.len() > MAX_BODY_IN_BYTES {
        return Err(invalid_input());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// `--search`: a parsed-and-rebuilt GitHub issue/PR search query
// ---------------------------------------------------------------------------
//
// This is deliberately a parse-then-rebuild, not a passthrough. `commands.rs` copies the caller's
// raw `--search` text into the capability input unexamined, and a capability is also directly
// invocable with an arbitrary `search` field, bypassing `commands.rs` entirely — so the only place
// this can be validated safely is here, at the same native layer every other input field is
// re-validated at, never trusting that some other, bypassable layer already checked it. Every
// accepted qualifier is re-serialized from its parsed form (never the caller's original bytes)
// into a query this component itself prepends `repo:{owner}/{repo} is:{scope}` to, so a rejected
// `repo:`/`org:`/`user:`/`owner:` qualifier is refused before it is ever concatenated into
// anything sent to GitHub, and a query cannot re-scope itself to a different repository no matter
// how it is spelled.

/// Qualifiers this provider allows inside `--search` text, because each one only narrows results
/// within whatever repository and issue/PR type the provider has already scoped the query to.
const ALLOWED_SEARCH_QUALIFIERS: &[&str] = &[
    "author",
    "assignee",
    "mentions",
    "commenter",
    "involves",
    "label",
    "state",
    "is",
    "milestone",
    "base",
    "head",
    "created",
    "updated",
    "closed",
    "merged",
    "comments",
    "reactions",
    "interactions",
    "no",
    "sort",
    "draft",
    "review",
    "reviewed-by",
    "review-requested",
    "linked",
    "project",
];

/// Qualifiers refused by name with a specific reason, checked before the "unrecognized qualifier"
/// catch-all so the message explains *why*, not just that it wasn't on the list.
const REJECTED_SEARCH_QUALIFIERS: &[(&str, &str)] = &[
    (
        "repo",
        "this provider sets the repository scope from -R; a query cannot repoint it",
    ),
    (
        "org",
        "this provider is scoped to one repository; there is no organization-wide search",
    ),
    (
        "user",
        "this provider is scoped to one repository; there is no user-wide search",
    ),
    (
        "owner",
        "this provider is scoped to one repository; there is no owner-wide search",
    ),
    ("in", "field-scoped text search is not supported"),
    (
        "archived",
        "this provider is scoped to one repository, which is not itself searchable by archival state",
    ),
    (
        "fork",
        "this provider is scoped to one repository, which is not itself searchable by fork status",
    ),
    (
        "language",
        "cross-repository language filtering has no meaning inside one repository",
    ),
    ("type", "issue type is set with --type, not inside --search"),
];

fn invalid_search_query(reason: impl core::fmt::Display) -> ProviderError {
    ProviderError::new("invalid-search-query", format!("gh: --search: {reason}"))
}

/// One token of a search query, split before its meaning (qualifier vs. free text) is decided.
enum SearchToken {
    Qualifier {
        key: String,
        negated: bool,
        value: String,
    },
    Term(String),
}

/// Splits a search string into tokens on unquoted whitespace, keeping a double-quoted run —
/// spaces included — as one token, matching GitHub's own search syntax. Quote characters are kept
/// in the token; [`validate_search_value`] and the canonical rebuild both operate on them as-is.
fn tokenize_search(raw: &str) -> Result<Vec<String>, ProviderError> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for ch in raw.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                current.push(ch);
            }
            ch if ch.is_whitespace() && !in_quotes => {
                if !current.is_empty() {
                    tokens.push(core::mem::take(&mut current));
                }
            }
            ch => current.push(ch),
        }
    }
    if in_quotes {
        return Err(invalid_search_query("an unterminated quote"));
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    Ok(tokens)
}

/// A qualifier key is `gh`'s own alphabet for one: starts with a letter, then letters, digits, or
/// hyphens (`reviewed-by`, `review-requested`).
fn is_qualifier_key(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
}

/// Accepts a bare term, a quoted phrase, or a qualifier's value: any non-empty run with no literal
/// whitespace (already guaranteed by [`tokenize_search`]) and balanced quoting. This is
/// deliberately permissive beyond that — comparison values (`>2026-01-01`) and ranges (`10..20`,
/// `2026-01-01..*`) are just non-whitespace strings under this grammar, so they need no dedicated
/// case; GitHub's own search endpoint is the authority on whether one is semantically well-formed
/// for its qualifier, the same way it already is for every other qualifier value this provider
/// forwards without native semantic checking.
fn validate_search_value(value: &str) -> Result<(), ProviderError> {
    if value.is_empty() {
        return Err(invalid_search_query("an empty value"));
    }
    match value.matches('"').count() {
        0 => Ok(()),
        2 if value.starts_with('"') && value.ends_with('"') && value.len() > 2 => Ok(()),
        _ => Err(invalid_search_query(format!(
            "{value:?} has an unbalanced quote"
        ))),
    }
}

/// Parses one token into a qualifier or a free-text term. A token outside this grammar — a bare
/// `*` standing in for a wildcard GitHub's search does not support as a term, or a value with
/// unbalanced quoting — is refused by name rather than forwarded.
fn parse_search_token(token: &str) -> Result<SearchToken, ProviderError> {
    if token == "*" {
        return Err(invalid_search_query(
            "'*' is not a valid search term (GitHub's search has no bare wildcard)",
        ));
    }
    let (negated, unsigned) = match token.strip_prefix('-') {
        Some(rest) if !rest.is_empty() => (true, rest),
        _ => (false, token),
    };
    if let Some(colon) = unsigned.find(':') {
        let key = &unsigned[..colon];
        let value = &unsigned[colon + 1..];
        if is_qualifier_key(key) && !value.is_empty() {
            validate_search_value(value)?;
            return Ok(SearchToken::Qualifier {
                key: key.to_ascii_lowercase(),
                negated,
                value: value.to_owned(),
            });
        }
    }
    validate_search_value(token)?;
    Ok(SearchToken::Term(token.to_owned()))
}

fn check_qualifier_allowed(key: &str) -> Result<(), ProviderError> {
    if let Some((_, reason)) = REJECTED_SEARCH_QUALIFIERS
        .iter()
        .find(|(name, _)| *name == key)
    {
        return Err(invalid_search_query(format!(
            "qualifier '{key}:' is not allowed: {reason}"
        )));
    }
    if !ALLOWED_SEARCH_QUALIFIERS.contains(&key) {
        return Err(invalid_search_query(format!(
            "qualifier '{key}:' is not recognized"
        )));
    }
    Ok(())
}

/// Parses, validates, and canonically rebuilds a caller's `--search` text, then prepends this
/// provider's own repository and issue/PR-type scope. The rebuilt query is what reaches GitHub —
/// never the caller's original bytes — so a rejected qualifier is refused before this function
/// returns, long before any HTTP request exists to carry it.
pub(crate) fn build_search_query(
    owner: &str,
    repo: &str,
    scope: &'static str,
    raw: &str,
) -> Result<String, ProviderError> {
    if raw.is_empty() {
        return Err(invalid_search_query("requires a non-empty query"));
    }
    if raw.len() > MAX_SEARCH_QUERY_BYTES {
        return Err(invalid_search_query(format!(
            "query must be at most {MAX_SEARCH_QUERY_BYTES} bytes"
        )));
    }
    let tokens = tokenize_search(raw)?;
    if tokens.is_empty() {
        return Err(invalid_search_query("requires a non-empty query"));
    }
    let mut rebuilt = format!("repo:{owner}/{repo} is:{scope}");
    for token in &tokens {
        match parse_search_token(token)? {
            SearchToken::Qualifier {
                key,
                negated,
                value,
            } => {
                check_qualifier_allowed(&key)?;
                let sign = if negated { "-" } else { "" };
                rebuilt.push_str(&format!(" {sign}{key}:{value}"));
            }
            SearchToken::Term(term) => {
                rebuilt.push(' ');
                rebuilt.push_str(&term);
            }
        }
    }
    Ok(rebuilt)
}

/// `--search` and this provider's own structured filters (`--state`, `--author`, …) are mutually
/// exclusive — `clap` already enforces this on the argv path via `conflicts_with_all`, but a
/// capability is also directly invocable with arbitrary JSON input, bypassing `commands.rs`
/// entirely, so the invoke-time check here is what actually holds the line. Silently ignoring one
/// side would be the exact "caller believes both were applied" failure this codebase's own
/// `REJECTED_FLAGS` philosophy exists to avoid for the argv surface; refusing both together is the
/// same discipline applied at the native layer.
pub(crate) fn search_conflicts_with_other_filters() -> ProviderError {
    invalid_search_query(
        "cannot be combined with this capability's other filters; express them as qualifiers \
         inside the search query instead",
    )
}

/// The shape every GitHub search response is wrapped in; only `items` is projected further here,
/// so `total_count`/`incomplete_results` are left undeclared and simply ignored rather than
/// re-declared unused.
#[derive(serde::Deserialize)]
pub(crate) struct RawSearchResponse<T> {
    pub(crate) items: Vec<T>,
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
// Endpoint and request construction
// ---------------------------------------------------------------------------

/// Resolves the request origin: production GitHub HTTPS or a literal loopback test endpoint.
fn endpoint(value: Option<&str>) -> Result<String, ProviderError> {
    let value = value.unwrap_or(DEFAULT_ENDPOINT);
    if value.len() > MAX_ENDPOINT_BYTES {
        return Err(invalid_endpoint());
    }
    if matches!(value, "https://api.github.com" | "https://api.github.com/") {
        return Ok(format!("https://{PRODUCTION_HOST}"));
    }
    let authority = value.strip_prefix("http://").ok_or_else(invalid_endpoint)?;
    let authority = authority.strip_suffix('/').unwrap_or(authority);
    let address = authority
        .parse::<std::net::SocketAddr>()
        .map_err(|_| invalid_endpoint())?;
    if address.port() == 0 || !address.ip().is_loopback() {
        return Err(invalid_endpoint());
    }
    Ok(format!("http://{address}"))
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
        401 => ProviderError::new("unauthorized", "endpoint rejected the request credentials"),
        403 if rate_limit_exhausted(response) => rate_limited(),
        403 => ProviderError::new("forbidden", "endpoint refused the request"),
        404 => ProviderError::new("not-found", "the requested resource was not found"),
        422 => ProviderError::new("unprocessable", "endpoint refused the request as invalid"),
        429 => rate_limited(),
        _ => unexpected_status(),
    }
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

fn invalid_endpoint() -> ProviderError {
    ProviderError::new(
        "invalid-endpoint",
        "endpoint must be production GitHub HTTPS or explicit loopback HTTP",
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

dekopon_provider_sdk::export_provider_with_cli!(Gh, bindings);

// ---------------------------------------------------------------------------
// Test support and shared tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod testutil {
    use std::collections::VecDeque;

    use dekopon_provider_http::{Header, HttpError, Request, Response};
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
    use dekopon_provider_http::HttpErrorCode;
    use dekopon_provider_sdk::{EffectKind, Provider, RiskLevel};
    use serde_json::json;

    use super::testutil::{capability, scripted, step};
    use super::{
        Gh, MAX_SEARCH_QUERY_BYTES, build_search_query, endpoint, invoke_with, truncate_text,
    };

    #[test]
    fn manifest_covers_the_full_designed_surface() {
        let manifest = Gh::manifest();
        assert_eq!(manifest.id.as_str(), "gh");
        assert_eq!(manifest.capabilities.len(), 19);

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
    fn endpoints_fail_closed() {
        assert_eq!(
            endpoint(None).expect("default endpoint is valid"),
            "https://api.github.com"
        );
        assert_eq!(
            endpoint(Some("https://api.github.com/")).expect("trailing slash accepted"),
            "https://api.github.com"
        );
        for denied in [
            "https://api.github.com.evil.com",
            "https://github.com",
            "https://example.com",
            "https://user@api.github.com",
            "https://api.github.com/repos",
            "https://api.github.com?x=1",
            "http://api.github.com",
            "http://127.0.0.1",
            "http://192.168.1.10:8080",
            "http://localhost:8080",
        ] {
            assert!(endpoint(Some(denied)).is_err(), "accepted {denied}");
        }
        assert!(
            endpoint(Some("http://127.0.0.1:43123")).is_ok(),
            "literal loopback with port is the test escape hatch"
        );
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
                Err(dekopon_provider_http::HttpError {
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
                Ok(dekopon_provider_http::Response {
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
                json!({"owner": "octo", "repo": "x", "state": "bogus"}),
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

    // -----------------------------------------------------------------------
    // `--search` query parsing, allowlisting, and canonical rebuild
    // -----------------------------------------------------------------------

    #[test]
    fn search_prepends_repo_and_scope() {
        let query = build_search_query("octo", "hello", "pr", "is:open").expect("valid query");
        assert_eq!(query, "repo:octo/hello is:pr is:open");
    }

    #[test]
    fn search_accepts_bare_terms_phrases_and_qualifiers() {
        let query = build_search_query(
            "octo",
            "hello",
            "issue",
            r#"memory leak "out of order" label:bug -label:wontfix author:cpetersen"#,
        )
        .expect("valid query");
        assert_eq!(
            query,
            r#"repo:octo/hello is:issue memory leak "out of order" label:bug -label:wontfix author:cpetersen"#
        );
    }

    #[test]
    fn search_accepts_comparison_and_range_values() {
        let query = build_search_query(
            "octo",
            "hello",
            "pr",
            "created:>2026-01-01 comments:10..20 updated:2026-01-01..*",
        )
        .expect("valid query");
        assert_eq!(
            query,
            "repo:octo/hello is:pr created:>2026-01-01 comments:10..20 updated:2026-01-01..*"
        );
    }

    #[test]
    fn search_rejects_a_bare_wildcard() {
        let error =
            build_search_query("octo", "hello", "pr", "*").expect_err("bare wildcard refused");
        assert_eq!(error.code(), "invalid-search-query");
        assert!(error.message().contains('*'), "{error:?}");
    }

    #[test]
    fn search_rejects_an_unbalanced_quote() {
        let error = build_search_query("octo", "hello", "pr", r#"label:"in progress"#)
            .expect_err("unterminated quote refused");
        assert_eq!(error.code(), "invalid-search-query");
    }

    #[test]
    fn search_rejects_repo_org_user_owner_by_name() {
        for (token, name) in [
            ("repo:other/repo", "repo"),
            ("org:other-org", "org"),
            ("user:someone", "user"),
            ("owner:someone", "owner"),
        ] {
            let error =
                build_search_query("octo", "hello", "pr", token).expect_err("scope escape refused");
            assert_eq!(error.code(), "invalid-search-query", "{token}");
            assert!(error.message().contains(name), "{token}: {error:?}");
        }
    }

    #[test]
    fn search_rejects_repo_qualifier_before_any_scope_change_is_possible() {
        // The exact live-risk case named in review: a query that tries to repoint the search at a
        // different repository is refused, and the rejection happens inside the pure parse/rebuild
        // function itself — nothing here ever touches HTTP, so there is no path for this string to
        // reach a request even indirectly.
        let error = build_search_query("octo", "hello", "pr", "repo:other/repo is:open")
            .expect_err("repo qualifier refused");
        assert_eq!(error.code(), "invalid-search-query");
        assert!(error.message().contains("repo"), "{error:?}");
    }

    #[test]
    fn search_rejects_in_archived_fork_language_type_and_unknown_qualifiers() {
        for token in [
            "in:title",
            "archived:false",
            "fork:true",
            "language:rust",
            "type:pr",
            "bogus:value",
        ] {
            let error =
                build_search_query("octo", "hello", "pr", token).expect_err("rejected qualifier");
            assert_eq!(error.code(), "invalid-search-query", "{token}");
        }
    }

    #[test]
    fn search_allows_every_documented_qualifier() {
        for qualifier in [
            "author",
            "assignee",
            "mentions",
            "commenter",
            "involves",
            "label",
            "state",
            "is",
            "milestone",
            "base",
            "head",
            "created",
            "updated",
            "closed",
            "merged",
            "comments",
            "reactions",
            "interactions",
            "no",
            "sort",
            "draft",
            "review",
            "reviewed-by",
            "review-requested",
            "linked",
            "project",
        ] {
            build_search_query("octo", "hello", "pr", &format!("{qualifier}:a"))
                .unwrap_or_else(|error| panic!("{qualifier} should be allowed: {error:?}"));
        }
    }

    #[test]
    fn search_rejects_empty_and_oversize_queries() {
        assert_eq!(
            build_search_query("octo", "hello", "pr", "")
                .expect_err("empty refused")
                .code(),
            "invalid-search-query"
        );
        let oversize = "x".repeat(MAX_SEARCH_QUERY_BYTES + 1);
        assert_eq!(
            build_search_query("octo", "hello", "pr", &oversize)
                .expect_err("oversize refused")
                .code(),
            "invalid-search-query"
        );
    }
}
