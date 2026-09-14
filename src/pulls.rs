//! Read-side pull-request capabilities: list, read, files, diff, reviews, and status.

use dekopon_provider_http::{HttpError, Request, Response};
use dekopon_provider_sdk::ProviderError;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    ACCEPT_DIFF, ACCEPT_JSON, MAX_COMMENT_OUT_BYTES, MAX_DESCRIPTION_OUT_BYTES, MAX_DIFF_OUT_BYTES,
    MAX_LIST_ITEMS, MAX_PATCH_OUT_BYTES, MAX_PR_BODY_OUT_BYTES, MAX_TITLE_OUT_BYTES,
    RawSearchResponse, RawUser, bounded_optional, build_search_query, decode, endpoint,
    has_next_link, invalid_input, invalid_response, is_sha, login_out, percent_encode,
    search_conflicts_with_other_filters, send_get, timestamp, truncate_text, validate_label,
    validate_login, validate_number, validate_page, validate_ref, validate_repo,
};

// ---------------------------------------------------------------------------
// Shared raw shapes (also consumed by the write capabilities' pre-read)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct RawRef {
    #[serde(rename = "ref")]
    pub(crate) name: String,
    pub(crate) sha: String,
}

/// The label shape GitHub embeds on issues and pull requests alike. Kept local to this file rather
/// than shared with `issues.rs`'s identically-shaped `RawLabel`, to avoid coupling two concurrently
/// edited files over a one-field struct.
#[derive(Debug, Deserialize)]
struct RawLabel {
    name: String,
}

/// One pull request as GitHub returns it; list items omit the single-read counters.
#[derive(Debug, Deserialize)]
pub(crate) struct RawPull {
    pub(crate) number: u32,
    pub(crate) title: String,
    pub(crate) state: String,
    #[serde(default)]
    pub(crate) draft: bool,
    #[serde(default)]
    pub(crate) merged: Option<bool>,
    #[serde(default)]
    pub(crate) merged_at: Option<String>,
    #[serde(default)]
    pub(crate) body: Option<String>,
    #[serde(default)]
    pub(crate) user: Option<RawUser>,
    #[serde(default)]
    pub(crate) assignees: Vec<RawUser>,
    #[serde(default)]
    labels: Vec<RawLabel>,
    pub(crate) head: RawRef,
    pub(crate) base: RawRef,
    #[serde(default)]
    pub(crate) additions: Option<u64>,
    #[serde(default)]
    pub(crate) deletions: Option<u64>,
    #[serde(default)]
    pub(crate) changed_files: Option<u64>,
    #[serde(default)]
    pub(crate) mergeable_state: Option<String>,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
}

impl RawPull {
    /// Binds a decoded pull to the request that asked for it.
    pub(crate) fn validate(&self, requested: u32) -> Result<(), ProviderError> {
        if self.number != requested || !is_sha(&self.head.sha) || !is_sha(&self.base.sha) {
            return Err(invalid_response());
        }
        Ok(())
    }
}

/// Fetches one pull request and validates the number echo and SHA shapes.
pub(crate) fn fetch_pull(
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
    endpoint: &str,
    owner: &str,
    repo: &str,
    number: u32,
) -> Result<RawPull, ProviderError> {
    let uri = format!(
        "{endpoint}/repos/{}/{}/pulls/{number}",
        percent_encode(owner),
        percent_encode(repo),
    );
    let response = send_get(send, uri, ACCEPT_JSON)?;
    let pull = decode::<RawPull>(&response.body)?;
    pull.validate(number)?;
    Ok(pull)
}

// ---------------------------------------------------------------------------
// gh.pull-request.read
// ---------------------------------------------------------------------------

/// One conversation comment, shared by issues and pull requests alike (GitHub uses the same
/// `/issues/{number}/comments` endpoint for both). Kept local rather than shared with
/// `issues.rs`'s identically-shaped `RawComment`, to avoid coupling two concurrently edited files
/// over a four-field struct.
#[derive(Debug, Deserialize)]
struct RawComment {
    id: u64,
    #[serde(default)]
    user: Option<RawUser>,
    #[serde(default)]
    body: Option<String>,
    created_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ReadInput {
    owner: String,
    repo: String,
    number: u32,
    #[serde(default)]
    comments: Option<bool>,
    #[serde(default)]
    endpoint: Option<String>,
}

pub(crate) fn read(
    input: Value,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let input = serde_json::from_value::<ReadInput>(input).map_err(|_| invalid_input())?;
    validate_login(&input.owner)?;
    validate_repo(&input.repo)?;
    validate_number(input.number)?;
    let endpoint = endpoint(input.endpoint.as_deref())?;
    let pull = fetch_pull(send, &endpoint, &input.owner, &input.repo, input.number)?;

    let (title, _) = truncate_text(&pull.title, MAX_TITLE_OUT_BYTES);
    let (body, body_truncated) = bounded_optional(pull.body.as_deref(), MAX_PR_BODY_OUT_BYTES);
    let mut output = json!({
        "number": pull.number,
        "title": title,
        "state": pull.state,
        "draft": pull.draft,
        "merged": pull.merged.unwrap_or(false),
        "author": login_out(pull.user.as_ref()),
        "body": body,
        "bodyTruncated": body_truncated,
        "headRef": pull.head.name,
        "headSha": pull.head.sha,
        "baseRef": pull.base.name,
        "baseSha": pull.base.sha,
        "additions": pull.additions,
        "deletions": pull.deletions,
        "changedFiles": pull.changed_files,
        "mergeableState": pull.mergeable_state,
        "createdAt": timestamp(&pull.created_at)?,
        "updatedAt": timestamp(&pull.updated_at)?,
    });

    if input.comments == Some(true) {
        let uri = format!(
            "{endpoint}/repos/{}/{}/issues/{}/comments?page=1&per_page={MAX_LIST_ITEMS}",
            percent_encode(&input.owner),
            percent_encode(&input.repo),
            input.number,
        );
        let response = send_get(send, uri, ACCEPT_JSON)?;
        let raw_comments = decode::<Vec<RawComment>>(&response.body)?;
        let comments_truncated = has_next_link(&response) || raw_comments.len() > MAX_LIST_ITEMS;
        let comments = raw_comments
            .into_iter()
            .take(MAX_LIST_ITEMS)
            .map(|comment| {
                let (body, body_truncated) =
                    bounded_optional(comment.body.as_deref(), MAX_COMMENT_OUT_BYTES);
                Ok(json!({
                    "commentId": comment.id,
                    "author": login_out(comment.user.as_ref()),
                    "body": body,
                    "bodyTruncated": body_truncated,
                    "createdAt": timestamp(&comment.created_at)?.to_owned(),
                }))
            })
            .collect::<Result<Vec<_>, ProviderError>>()?;
        let map = output
            .as_object_mut()
            .expect("read output is a JSON object");
        // Named `recentComments`/`recentCommentsTruncated` rather than `comments`/
        // `commentsTruncated`: a numeric `comments` count field is a natural future addition to
        // this output (as it already exists on `gh.issue.read`), and this name sidesteps that
        // collision rather than relying on it never happening.
        map.insert("recentComments".to_owned(), Value::Array(comments));
        map.insert(
            "recentCommentsTruncated".to_owned(),
            Value::Bool(comments_truncated),
        );
    }

    Ok(output)
}

// ---------------------------------------------------------------------------
// gh.pull-request.list
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum StateFilter {
    Open,
    Closed,
    /// GitHub's REST API has no `state=merged` value; real `gh` requests `state=closed` and
    /// filters client-side on whether the pull request actually merged. `as_str()` reflects that
    /// wire reality (it returns `"closed"`, same as `Closed`); `list()` separately checks for this
    /// variant to add the client-side `mergedAt` requirement `Closed` alone does not carry.
    Merged,
    All,
}

impl StateFilter {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed | Self::Merged => "closed",
            Self::All => "all",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ListInput {
    owner: String,
    repo: String,
    #[serde(default)]
    state: Option<StateFilter>,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    base: Option<String>,
    #[serde(default)]
    head: Option<String>,
    #[serde(default)]
    assignee: Option<String>,
    #[serde(default)]
    labels: Option<Vec<String>>,
    #[serde(default)]
    draft: Option<bool>,
    #[serde(default)]
    search: Option<String>,
    #[serde(default)]
    page: Option<u32>,
    #[serde(default)]
    per_page: Option<u32>,
    #[serde(default)]
    endpoint: Option<String>,
}

/// One pull request as GitHub's search endpoint returns it: an "issue" shape carrying only what
/// search results project, never `head`/`base` — a full pull-request read via `gh.pull-request.read`
/// is the way to get a matched PR's ref/SHA once its number is known from a search.
#[derive(Debug, Deserialize)]
struct RawSearchPull {
    number: u32,
    title: String,
    state: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    user: Option<RawUser>,
    created_at: String,
    updated_at: String,
}

/// Applies every `gh.pull-request.list` filter GitHub's list endpoint cannot express itself:
/// author, assignee, labels, draft, and (for a requested `merged` state) actually having merged.
fn matches_filters(pull: &RawPull, input: &ListInput, state: &StateFilter) -> bool {
    if let Some(author) = input.author.as_deref()
        && !pull
            .user
            .as_ref()
            .is_some_and(|user| user.login.eq_ignore_ascii_case(author))
    {
        return false;
    }
    if let Some(assignee) = input.assignee.as_deref()
        && !pull
            .assignees
            .iter()
            .any(|user| user.login.eq_ignore_ascii_case(assignee))
    {
        return false;
    }
    if let Some(labels) = input.labels.as_deref()
        && !labels.iter().all(|wanted| {
            pull.labels
                .iter()
                .any(|label| label.name.eq_ignore_ascii_case(wanted))
        })
    {
        return false;
    }
    if let Some(wanted_draft) = input.draft
        && pull.draft != wanted_draft
    {
        return false;
    }
    if matches!(state, StateFilter::Merged) && pull.merged_at.is_none() {
        return false;
    }
    true
}

pub(crate) fn list(
    input: Value,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let input = serde_json::from_value::<ListInput>(input).map_err(|_| invalid_input())?;
    validate_login(&input.owner)?;
    validate_repo(&input.repo)?;

    if let Some(search) = input.search.as_deref() {
        // `--search` and this capability's other structured filters are mutually exclusive: gh's
        // own search qualifier syntax already covers state/author/assignee/label/base/head/draft
        // (all allowlisted qualifiers, see `build_search_query`), so combining them here would
        // just be two ways of saying the same thing that could silently disagree. `clap` already
        // enforces this on the argv path; this is the invoke-time half of the same rule, since a
        // capability is also directly invocable with arbitrary JSON input.
        if input.state.is_some()
            || input.author.is_some()
            || input.assignee.is_some()
            || input.base.is_some()
            || input.head.is_some()
            || input.labels.is_some()
            || input.draft.is_some()
        {
            return Err(search_conflicts_with_other_filters());
        }
        return list_via_search(&input, search, send);
    }

    if let Some(author) = input.author.as_deref() {
        validate_login(author)?;
    }
    if let Some(assignee) = input.assignee.as_deref() {
        validate_login(assignee)?;
    }
    if let Some(base) = input.base.as_deref() {
        validate_ref(base)?;
    }
    if let Some(head) = input.head.as_deref() {
        validate_ref(head)?;
    }
    if let Some(labels) = input.labels.as_deref() {
        for label in labels {
            validate_label(label)?;
        }
    }
    let (page, per_page) = validate_page(input.page, input.per_page)?;
    let per_page = per_page.unwrap_or(30);
    let state = input.state.unwrap_or(StateFilter::Open);
    let endpoint = endpoint(input.endpoint.as_deref())?;

    let mut uri = format!(
        "{endpoint}/repos/{}/{}/pulls?state={}&page={page}&per_page={per_page}",
        percent_encode(&input.owner),
        percent_encode(&input.repo),
        state.as_str(),
    );
    if let Some(base) = input.base.as_deref() {
        uri.push_str(&format!("&base={}", percent_encode(base)));
    }
    if let Some(head) = input.head.as_deref() {
        // GitHub's REST `head` filter requires the qualified `owner:branch` form. Encode the
        // *raw* validated owner joined with the raw head, not the already-percent-encoded owner,
        // to avoid double-encoding.
        uri.push_str(&format!(
            "&head={}",
            percent_encode(&format!("{}:{}", input.owner, head))
        ));
    }
    let response = send_get(send, uri, ACCEPT_JSON)?;
    let pulls = decode::<Vec<RawPull>>(&response.body)?;
    // `hasMore` must also consider per-invocation truncation, not just GitHub's `Link` header:
    // `MAX_PER_PAGE` (100) exceeds `MAX_LIST_ITEMS` (50), so a single request can legitimately
    // return more raw rows than this capability ever projects. Compute this right after decode,
    // before the filter/take chain below reduces `pulls.len()`.
    let has_more = has_next_link(&response) || pulls.len() > MAX_LIST_ITEMS;

    // `base` and `head` are real REST list-endpoint query parameters, sent above. `author`,
    // `assignee`, `labels`, `draft`, and (for a `merged` state request) actually having merged all
    // filter the fetched page after pagination instead — GitHub's pull-request list endpoint has
    // no author/assignee/label/draft parameter, and no `state=merged` value at all.
    let items = pulls
        .into_iter()
        .filter(|pull| matches_filters(pull, &input, &state))
        .take(MAX_LIST_ITEMS)
        .map(|pull| {
            if !is_sha(&pull.head.sha) {
                return Err(invalid_response());
            }
            let (title, _) = truncate_text(&pull.title, MAX_TITLE_OUT_BYTES);
            Ok(json!({
                "number": pull.number,
                "title": title,
                "state": pull.state,
                "draft": pull.draft,
                "author": login_out(pull.user.as_ref()),
                "headRef": pull.head.name,
                "headSha": pull.head.sha,
                "baseRef": pull.base.name,
                "createdAt": timestamp(&pull.created_at)?.to_owned(),
                "updatedAt": timestamp(&pull.updated_at)?.to_owned(),
            }))
        })
        .collect::<Result<Vec<_>, ProviderError>>()?;

    Ok(json!({
        "pullRequests": items,
        "page": page,
        "hasMore": has_more,
    }))
}

/// The `--search` path for `gh.pull-request.list`: `build_search_query` has already parsed,
/// allowlisted, and rebuilt `search` with this capability's own `repo:`/`is:pr` scope prepended,
/// so what reaches GitHub here is never the caller's original bytes. GitHub's search endpoint
/// returns issue-shaped results with no `head`/`base` at all, so those three fields are always
/// null on a search-originated row — a full `gh.pull-request.read` on the matched number is the
/// way to get them once a search has found it.
fn list_via_search(
    input: &ListInput,
    search: &str,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let query = build_search_query(&input.owner, &input.repo, "pr", search)?;
    let (page, per_page) = validate_page(input.page, input.per_page)?;
    let per_page = per_page.unwrap_or(30);
    let endpoint = endpoint(input.endpoint.as_deref())?;

    let uri = format!(
        "{endpoint}/search/issues?q={}&page={page}&per_page={per_page}",
        percent_encode(&query)
    );
    let response = send_get(send, uri, ACCEPT_JSON)?;
    let raw = decode::<RawSearchResponse<RawSearchPull>>(&response.body)?;
    let has_more = has_next_link(&response) || raw.items.len() > MAX_LIST_ITEMS;

    let items = raw
        .items
        .into_iter()
        .take(MAX_LIST_ITEMS)
        .map(|pull| {
            let (title, _) = truncate_text(&pull.title, MAX_TITLE_OUT_BYTES);
            Ok(json!({
                "number": pull.number,
                "title": title,
                "state": pull.state,
                "draft": pull.draft,
                "author": login_out(pull.user.as_ref()),
                "headRef": Value::Null,
                "headSha": Value::Null,
                "baseRef": Value::Null,
                "createdAt": timestamp(&pull.created_at)?.to_owned(),
                "updatedAt": timestamp(&pull.updated_at)?.to_owned(),
            }))
        })
        .collect::<Result<Vec<_>, ProviderError>>()?;

    Ok(json!({
        "pullRequests": items,
        "page": page,
        "hasMore": has_more,
    }))
}

// ---------------------------------------------------------------------------
// gh.pull-request.files
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct FilesInput {
    owner: String,
    repo: String,
    number: u32,
    #[serde(default)]
    page: Option<u32>,
    #[serde(default)]
    per_page: Option<u32>,
    #[serde(default)]
    include_patch: Option<bool>,
    #[serde(default)]
    endpoint: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawFile {
    filename: String,
    status: String,
    additions: u64,
    deletions: u64,
    #[serde(default)]
    patch: Option<String>,
}

pub(crate) fn files(
    input: Value,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let input = serde_json::from_value::<FilesInput>(input).map_err(|_| invalid_input())?;
    validate_login(&input.owner)?;
    validate_repo(&input.repo)?;
    validate_number(input.number)?;
    let (page, per_page) = validate_page(input.page, input.per_page)?;
    let per_page = per_page.unwrap_or(30);
    let include_patch = input.include_patch.unwrap_or(true);
    let endpoint = endpoint(input.endpoint.as_deref())?;

    let uri = format!(
        "{endpoint}/repos/{}/{}/pulls/{}/files?page={page}&per_page={per_page}",
        percent_encode(&input.owner),
        percent_encode(&input.repo),
        input.number,
    );
    let response = send_get(send, uri, ACCEPT_JSON)?;
    let files = decode::<Vec<RawFile>>(&response.body)?;
    // See the identical comment in `list()`: `MAX_PER_PAGE` (100) exceeds `MAX_LIST_ITEMS` (50),
    // so a single request can return more raw rows than this capability ever projects.
    let has_more = has_next_link(&response) || files.len() > MAX_LIST_ITEMS;

    let items = files
        .into_iter()
        .take(MAX_LIST_ITEMS)
        .map(|file| {
            if file.filename.is_empty() || file.filename.len() > 1024 {
                return Err(invalid_response());
            }
            let mut item = json!({
                "path": file.filename,
                "status": file.status,
                "additions": file.additions,
                "deletions": file.deletions,
                "patchTruncated": false,
            });
            if include_patch && let Some(patch) = file.patch.as_deref() {
                let (patch, truncated) = truncate_text(patch, MAX_PATCH_OUT_BYTES);
                item["patch"] = Value::String(patch);
                item["patchTruncated"] = Value::Bool(truncated);
            }
            Ok(item)
        })
        .collect::<Result<Vec<_>, ProviderError>>()?;

    Ok(json!({
        "files": items,
        "page": page,
        "hasMore": has_more,
    }))
}

// ---------------------------------------------------------------------------
// gh.pull-request.diff
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct DiffInput {
    owner: String,
    repo: String,
    number: u32,
    #[serde(default)]
    name_only: Option<bool>,
    #[serde(default)]
    endpoint: Option<String>,
}

pub(crate) fn diff(
    input: Value,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let input = serde_json::from_value::<DiffInput>(input).map_err(|_| invalid_input())?;
    validate_login(&input.owner)?;
    validate_repo(&input.repo)?;
    validate_number(input.number)?;
    let endpoint = endpoint(input.endpoint.as_deref())?;

    let uri = format!(
        "{endpoint}/repos/{}/{}/pulls/{}",
        percent_encode(&input.owner),
        percent_encode(&input.repo),
        input.number,
    );
    let response = send_get(send, uri, ACCEPT_DIFF)?;
    let text = core::str::from_utf8(&response.body).map_err(|_| invalid_response())?;

    if input.name_only == Some(true) {
        // Parsed from the *untruncated* diff text: a file-path list is far smaller than the diff
        // body it is derived from, so name-only mode should not be limited by the diff-text
        // truncation boundary below. `diff --git a/<path> b/<path>` headers are emitted for every
        // changed file, including pure deletions (as `b/dev/null`, a documented limitation) and
        // renames (where taking the `b/` half yields the current, post-rename path).
        let mut paths = Vec::new();
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("diff --git a/")
                && let Some(b_idx) = rest.find(" b/")
            {
                paths.push(rest[b_idx + 3..].to_owned());
            }
        }
        let files_truncated = paths.len() > MAX_LIST_ITEMS;
        paths.truncate(MAX_LIST_ITEMS);
        return Ok(json!({
            "number": input.number,
            "files": paths,
            "filesTruncated": files_truncated,
        }));
    }

    let (diff, truncated) = truncate_text(text, MAX_DIFF_OUT_BYTES);
    Ok(json!({
        "number": input.number,
        "diff": diff,
        "diffTruncated": truncated,
    }))
}

// ---------------------------------------------------------------------------
// gh.pull-request.reviews
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ReviewsInput {
    owner: String,
    repo: String,
    number: u32,
    #[serde(default)]
    page: Option<u32>,
    #[serde(default)]
    per_page: Option<u32>,
    #[serde(default)]
    endpoint: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawReviewItem {
    id: u64,
    state: String,
    #[serde(default)]
    user: Option<RawUser>,
    #[serde(default)]
    commit_id: Option<String>,
    #[serde(default)]
    submitted_at: Option<String>,
    #[serde(default)]
    body: Option<String>,
}

pub(crate) fn reviews(
    input: Value,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let input = serde_json::from_value::<ReviewsInput>(input).map_err(|_| invalid_input())?;
    validate_login(&input.owner)?;
    validate_repo(&input.repo)?;
    validate_number(input.number)?;
    let (page, per_page) = validate_page(input.page, input.per_page)?;
    let per_page = per_page.unwrap_or(20);
    let endpoint = endpoint(input.endpoint.as_deref())?;

    let uri = format!(
        "{endpoint}/repos/{}/{}/pulls/{}/reviews?page={page}&per_page={per_page}",
        percent_encode(&input.owner),
        percent_encode(&input.repo),
        input.number,
    );
    let response = send_get(send, uri, ACCEPT_JSON)?;
    let reviews = decode::<Vec<RawReviewItem>>(&response.body)?;
    // See the identical comment in `list()`: `MAX_PER_PAGE` (100) exceeds `MAX_LIST_ITEMS` (50),
    // so a single request can return more raw rows than this capability ever projects.
    let has_more = has_next_link(&response) || reviews.len() > MAX_LIST_ITEMS;

    let items = reviews
        .into_iter()
        .take(MAX_LIST_ITEMS)
        .map(|review| {
            let (body, body_truncated) =
                bounded_optional(review.body.as_deref(), MAX_COMMENT_OUT_BYTES);
            Ok(json!({
                "reviewId": review.id,
                "state": review.state,
                "author": login_out(review.user.as_ref()),
                "commitId": review.commit_id,
                "submittedAt": review.submitted_at,
                "body": body,
                "bodyTruncated": body_truncated,
            }))
        })
        .collect::<Result<Vec<_>, ProviderError>>()?;

    Ok(json!({
        "reviews": items,
        "page": page,
        "hasMore": has_more,
    }))
}

// ---------------------------------------------------------------------------
// gh.pull-request.status
// ---------------------------------------------------------------------------

const MAX_STATUS_TOKEN_BYTES: usize = 64;

#[derive(Debug, Deserialize)]
struct RawWorkflowRuns {
    total_count: u64,
    workflow_runs: Vec<RawWorkflowRun>,
}

#[derive(Debug, Deserialize)]
struct RawWorkflowRun {
    id: u64,
    #[serde(default)]
    name: Option<String>,
    display_title: String,
    event: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    conclusion: Option<String>,
    head_sha: String,
    run_number: u64,
    #[serde(default)]
    run_attempt: Option<u64>,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Deserialize)]
struct RawCombinedStatus {
    state: String,
    sha: String,
    total_count: u64,
    statuses: Vec<RawCommitStatus>,
}

#[derive(Debug, Deserialize)]
struct RawCommitStatus {
    id: u64,
    state: String,
    context: String,
    #[serde(default)]
    description: Option<String>,
    created_at: String,
    updated_at: String,
}

fn status_token(value: &str) -> Result<&str, ProviderError> {
    if value.is_empty() || value.len() > MAX_STATUS_TOKEN_BYTES {
        return Err(invalid_response());
    }
    Ok(value)
}

pub(crate) fn status(
    input: Value,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let input = serde_json::from_value::<ReadInput>(input).map_err(|_| invalid_input())?;
    validate_login(&input.owner)?;
    validate_repo(&input.repo)?;
    validate_number(input.number)?;
    let endpoint = endpoint(input.endpoint.as_deref())?;

    let pull = fetch_pull(send, &endpoint, &input.owner, &input.repo, input.number)?;
    let owner = percent_encode(&input.owner);
    let repo = percent_encode(&input.repo);
    let head = percent_encode(&pull.head.sha);

    // Fine-grained personal access tokens expose Actions and Commit statuses permissions, but not
    // the Checks permission required by `/check-runs`. Workflow runs give the useful GitHub
    // Actions result at the same head without widening this read capability to a POST surface.
    let workflows_uri = format!(
        "{endpoint}/repos/{owner}/{repo}/actions/runs?head_sha={head}&page=1&per_page={MAX_LIST_ITEMS}"
    );
    let workflows_response = send_get(send, workflows_uri, ACCEPT_JSON)?;
    let workflows = decode::<RawWorkflowRuns>(&workflows_response.body)?;
    if workflows.total_count < workflows.workflow_runs.len() as u64 {
        return Err(invalid_response());
    }
    let workflow_runs_truncated = workflows.total_count > MAX_LIST_ITEMS as u64
        || workflows.workflow_runs.len() > MAX_LIST_ITEMS;
    let workflow_runs = workflows
        .workflow_runs
        .into_iter()
        .take(MAX_LIST_ITEMS)
        .map(|run| {
            if run.head_sha != pull.head.sha || run.display_title.is_empty() {
                return Err(invalid_response());
            }
            let raw_name = run.name.as_deref().unwrap_or(&run.display_title);
            if raw_name.is_empty() {
                return Err(invalid_response());
            }
            let (name, name_truncated) = truncate_text(raw_name, MAX_TITLE_OUT_BYTES);
            let (display_title, display_title_truncated) =
                truncate_text(&run.display_title, MAX_TITLE_OUT_BYTES);
            let status = run
                .status
                .as_deref()
                .map(status_token)
                .transpose()?
                .map(str::to_owned);
            let conclusion = run
                .conclusion
                .as_deref()
                .map(status_token)
                .transpose()?
                .map(str::to_owned);
            Ok(json!({
                "runId": run.id,
                "name": name,
                "nameTruncated": name_truncated,
                "displayTitle": display_title,
                "displayTitleTruncated": display_title_truncated,
                "event": status_token(&run.event)?,
                "status": status,
                "conclusion": conclusion,
                "runNumber": run.run_number,
                "runAttempt": run.run_attempt,
                "createdAt": timestamp(&run.created_at)?,
                "updatedAt": timestamp(&run.updated_at)?,
            }))
        })
        .collect::<Result<Vec<_>, ProviderError>>()?;

    // Legacy commit statuses are a separate GitHub surface and still back some external CI. Keep
    // them alongside Actions rather than flattening two APIs into a misleading check-run shape.
    let statuses_uri = format!(
        "{endpoint}/repos/{owner}/{repo}/commits/{head}/status?page=1&per_page={MAX_LIST_ITEMS}"
    );
    let statuses_response = send_get(send, statuses_uri, ACCEPT_JSON)?;
    let statuses = decode::<RawCombinedStatus>(&statuses_response.body)?;
    if statuses.sha != pull.head.sha || statuses.total_count < statuses.statuses.len() as u64 {
        return Err(invalid_response());
    }
    let commit_statuses_truncated =
        statuses.total_count > MAX_LIST_ITEMS as u64 || statuses.statuses.len() > MAX_LIST_ITEMS;
    let commit_statuses = statuses
        .statuses
        .into_iter()
        .take(MAX_LIST_ITEMS)
        .map(|status| {
            if status.context.is_empty() {
                return Err(invalid_response());
            }
            let (context, context_truncated) = truncate_text(&status.context, MAX_TITLE_OUT_BYTES);
            let (description, description_truncated) =
                bounded_optional(status.description.as_deref(), MAX_DESCRIPTION_OUT_BYTES);
            Ok(json!({
                "statusId": status.id,
                "context": context,
                "contextTruncated": context_truncated,
                "state": status_token(&status.state)?,
                "description": description,
                "descriptionTruncated": description_truncated,
                "createdAt": timestamp(&status.created_at)?,
                "updatedAt": timestamp(&status.updated_at)?,
            }))
        })
        .collect::<Result<Vec<_>, ProviderError>>()?;
    // GitHub returns `pending` for an empty combined-status collection. Null avoids presenting
    // that API default as a real pending check to a model.
    let commit_status_state = (statuses.total_count > 0)
        .then(|| status_token(&statuses.state).map(str::to_owned))
        .transpose()?;

    Ok(json!({
        "pullNumber": pull.number,
        "headSha": pull.head.sha,
        "workflowRunCount": workflows.total_count,
        "workflowRuns": workflow_runs,
        "workflowRunsTruncated": workflow_runs_truncated,
        "commitStatusState": commit_status_state,
        "commitStatusCount": statuses.total_count,
        "commitStatuses": commit_statuses,
        "commitStatusesTruncated": commit_statuses_truncated,
    }))
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use crate::invoke_with;
    use crate::testutil::{
        accept_of, capability, json_response, response_with_headers, scripted, step,
    };

    pub(crate) fn pull_body(number: u32, state: &str, draft: bool, merged: bool) -> Value {
        json!({
            "number": number,
            "title": "Add ferocious test coverage",
            "state": state,
            "draft": draft,
            "merged": merged,
            "body": "A body",
            "user": {"login": "cpetersen"},
            "head": {"ref": "feature/x", "sha": "a".repeat(40)},
            "base": {"ref": "main", "sha": "b".repeat(40)},
            "additions": 10,
            "deletions": 2,
            "changed_files": 3,
            "mergeable_state": "clean",
            "created_at": "2026-08-01T00:00:00Z",
            "updated_at": "2026-08-02T00:00:00Z",
        })
    }

    #[test]
    fn read_projects_the_designed_shape() {
        let output = invoke_with(
            &capability("gh.pull-request.read"),
            json!({"owner": "octo", "repo": "hello", "number": 7}),
            scripted(vec![step(
                |request| {
                    assert_eq!(request.method, "GET");
                    assert_eq!(
                        request.uri,
                        "https://api.github.com/repos/octo/hello/pulls/7"
                    );
                },
                json_response(200, &pull_body(7, "open", false, false)),
            )]),
        )
        .expect("read succeeds");

        assert_eq!(output["number"], 7);
        assert_eq!(output["author"], "cpetersen");
        assert_eq!(output["headSha"], "a".repeat(40));
        assert_eq!(output["baseRef"], "main");
        assert_eq!(output["merged"], false);
        assert_eq!(output["bodyTruncated"], false);
        assert_eq!(output["mergeableState"], "clean");
    }

    #[test]
    fn read_rejects_a_number_echo_mismatch() {
        let error = invoke_with(
            &capability("gh.pull-request.read"),
            json!({"owner": "octo", "repo": "hello", "number": 7}),
            scripted(vec![step(
                |_| {},
                json_response(200, &pull_body(8, "open", false, false)),
            )]),
        )
        .expect_err("echo mismatch fails");
        assert_eq!(error.code(), "invalid-response");
    }

    #[test]
    fn read_rejects_malformed_shas() {
        let mut body = pull_body(7, "open", false, false);
        body["head"]["sha"] = json!("not-a-sha");
        let error = invoke_with(
            &capability("gh.pull-request.read"),
            json!({"owner": "octo", "repo": "hello", "number": 7}),
            scripted(vec![step(|_| {}, json_response(200, &body))]),
        )
        .expect_err("bad sha fails");
        assert_eq!(error.code(), "invalid-response");
    }

    #[test]
    fn read_includes_comments_when_requested() {
        let output = invoke_with(
            &capability("gh.pull-request.read"),
            json!({"owner": "octo", "repo": "hello", "number": 7, "comments": true}),
            scripted(vec![
                step(
                    |request| {
                        assert_eq!(
                            request.uri,
                            "https://api.github.com/repos/octo/hello/pulls/7"
                        );
                    },
                    json_response(200, &pull_body(7, "open", false, false)),
                ),
                step(
                    |request| {
                        assert_eq!(
                            request.uri,
                            format!(
                                "https://api.github.com/repos/octo/hello/issues/7/comments?page=1&per_page={}",
                                crate::MAX_LIST_ITEMS
                            )
                        );
                    },
                    json_response(
                        200,
                        &json!([
                            {"id": 11, "user": {"login": "reviewer"}, "body": "looks good", "created_at": "2026-08-03T00:00:00Z"},
                        ]),
                    ),
                ),
            ]),
        )
        .expect("read succeeds");

        assert_eq!(output["recentComments"][0]["commentId"], 11);
        assert_eq!(output["recentComments"][0]["author"], "reviewer");
        assert_eq!(output["recentComments"][0]["body"], "looks good");
        assert_eq!(output["recentCommentsTruncated"], false);
    }

    #[test]
    fn read_does_not_fetch_comments_unless_requested() {
        for input in [
            json!({"owner": "octo", "repo": "hello", "number": 7}),
            json!({"owner": "octo", "repo": "hello", "number": 7, "comments": false}),
        ] {
            // Exactly one scripted step: a second HTTP call here would panic on "unexpected extra
            // HTTP request", which is exactly the assertion that comments were not fetched.
            let output = invoke_with(
                &capability("gh.pull-request.read"),
                input,
                scripted(vec![step(
                    |_| {},
                    json_response(200, &pull_body(7, "open", false, false)),
                )]),
            )
            .expect("read succeeds");

            assert!(output.get("recentComments").is_none());
            assert!(output.get("recentCommentsTruncated").is_none());
        }
    }

    #[test]
    fn list_paginates_and_reports_has_more_from_the_link_header() {
        let output = invoke_with(
            &capability("gh.pull-request.list"),
            json!({"owner": "octo", "repo": "hello", "state": "all", "page": 2, "perPage": 10}),
            scripted(vec![step(
                |request| {
                    assert_eq!(
                        request.uri,
                        "https://api.github.com/repos/octo/hello/pulls?state=all&page=2&per_page=10"
                    );
                },
                response_with_headers(
                    200,
                    &[(
                        "link",
                        "<https://api.github.com/repositories/1/pulls?page=3>; rel=\"next\", <https://api.github.com/repositories/1/pulls?page=9>; rel=\"last\"",
                    )],
                    &json!([pull_body(1, "open", false, false), pull_body(2, "closed", true, false)]),
                ),
            )]),
        )
        .expect("list succeeds");

        assert_eq!(output["page"], 2);
        assert_eq!(output["hasMore"], true);
        let items = output["pullRequests"].as_array().expect("items");
        assert_eq!(items.len(), 2);
        assert_eq!(items[1]["draft"], true);
    }

    #[test]
    fn list_filters_by_author_after_pagination() {
        let mut other = pull_body(2, "open", false, false);
        other["user"] = json!({"login": "someone-else"});
        let output = invoke_with(
            &capability("gh.pull-request.list"),
            json!({"owner": "octo", "repo": "hello", "author": "cpetersen"}),
            scripted(vec![step(
                |request| {
                    assert_eq!(
                        request.uri,
                        "https://api.github.com/repos/octo/hello/pulls?state=open&page=1&per_page=30"
                    );
                },
                json_response(200, &json!([pull_body(1, "open", false, false), other])),
            )]),
        )
        .expect("list succeeds");

        let items = output["pullRequests"].as_array().expect("items");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["number"], 1);
        assert_eq!(output["hasMore"], false);
    }

    #[test]
    fn list_sends_base_and_head_as_rest_query_params() {
        let output = invoke_with(
            &capability("gh.pull-request.list"),
            json!({
                "owner": "octo",
                "repo": "hello",
                "base": "main",
                "head": "feature/x",
            }),
            scripted(vec![step(
                |request| {
                    assert_eq!(
                        request.uri,
                        "https://api.github.com/repos/octo/hello/pulls?state=open&page=1&per_page=30&base=main&head=octo%3Afeature%2Fx"
                    );
                },
                json_response(200, &json!([pull_body(1, "open", false, false)])),
            )]),
        )
        .expect("list succeeds");

        assert_eq!(output["pullRequests"].as_array().expect("items").len(), 1);
    }

    #[test]
    fn list_filters_by_assignee_labels_and_draft_after_pagination() {
        let mut matching = pull_body(1, "open", true, false);
        matching["assignees"] = json!([{"login": "reviewer"}]);
        matching["labels"] = json!([{"name": "bug"}, {"name": "p1"}]);

        let mut wrong_assignee = pull_body(2, "open", true, false);
        wrong_assignee["assignees"] = json!([{"login": "someone-else"}]);
        wrong_assignee["labels"] = json!([{"name": "bug"}, {"name": "p1"}]);

        let mut missing_label = pull_body(3, "open", true, false);
        missing_label["assignees"] = json!([{"login": "reviewer"}]);
        missing_label["labels"] = json!([{"name": "bug"}]);

        let mut not_draft = pull_body(4, "open", false, false);
        not_draft["assignees"] = json!([{"login": "reviewer"}]);
        not_draft["labels"] = json!([{"name": "bug"}, {"name": "p1"}]);

        let output = invoke_with(
            &capability("gh.pull-request.list"),
            json!({
                "owner": "octo",
                "repo": "hello",
                "assignee": "REVIEWER",
                "labels": ["bug", "P1"],
                "draft": true,
            }),
            scripted(vec![step(
                |_| {},
                json_response(
                    200,
                    &json!([matching, wrong_assignee, missing_label, not_draft]),
                ),
            )]),
        )
        .expect("list succeeds");

        let items = output["pullRequests"].as_array().expect("items");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["number"], 1);
    }

    #[test]
    fn list_requests_closed_for_merged_state_and_filters_on_merged_at() {
        let mut merged = pull_body(1, "closed", false, true);
        merged["merged_at"] = json!("2026-08-05T00:00:00Z");
        let mut closed_not_merged = pull_body(2, "closed", false, false);
        closed_not_merged["merged_at"] = Value::Null;

        let output = invoke_with(
            &capability("gh.pull-request.list"),
            json!({"owner": "octo", "repo": "hello", "state": "merged"}),
            scripted(vec![step(
                |request| {
                    assert_eq!(
                        request.uri,
                        "https://api.github.com/repos/octo/hello/pulls?state=closed&page=1&per_page=30"
                    );
                },
                json_response(200, &json!([merged, closed_not_merged])),
            )]),
        )
        .expect("list succeeds");

        let items = output["pullRequests"].as_array().expect("items");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["number"], 1);
    }

    #[test]
    fn list_reports_has_more_when_the_raw_page_exceeds_max_list_items() {
        let pulls: Vec<Value> = (1..=(crate::MAX_LIST_ITEMS as u32 + 1))
            .map(|number| pull_body(number, "open", false, false))
            .collect();
        let output = invoke_with(
            &capability("gh.pull-request.list"),
            json!({"owner": "octo", "repo": "hello", "perPage": 100}),
            scripted(vec![step(|_| {}, json_response(200, &json!(pulls)))]),
        )
        .expect("list succeeds");

        assert_eq!(output["hasMore"], true);
        assert_eq!(
            output["pullRequests"].as_array().expect("items").len(),
            crate::MAX_LIST_ITEMS
        );
    }

    #[test]
    fn list_via_search_sends_the_rebuilt_query_and_nulls_head_base() {
        let output = invoke_with(
            &capability("gh.pull-request.list"),
            json!({"owner": "octo", "repo": "hello", "search": "review:required label:bug"}),
            scripted(vec![step(
                |request| {
                    assert_eq!(
                        request.uri,
                        "https://api.github.com/search/issues?\
                         q=repo%3Aocto%2Fhello%20is%3Apr%20review%3Arequired%20label%3Abug\
                         &page=1&per_page=30"
                    );
                },
                json_response(
                    200,
                    &json!({
                        "total_count": 1,
                        "incomplete_results": false,
                        "items": [{
                            "number": 9,
                            "title": "Fix the thing",
                            "state": "open",
                            "draft": false,
                            "user": {"login": "cpetersen"},
                            "created_at": "2026-08-01T00:00:00Z",
                            "updated_at": "2026-08-02T00:00:00Z",
                        }],
                    }),
                ),
            )]),
        )
        .expect("search list succeeds");

        let items = output["pullRequests"].as_array().expect("items");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["number"], 9);
        assert_eq!(items[0]["author"], "cpetersen");
        assert_eq!(items[0]["headRef"], Value::Null);
        assert_eq!(items[0]["headSha"], Value::Null);
        assert_eq!(items[0]["baseRef"], Value::Null);
    }

    /// The exact live-risk case named in review: a query trying to repoint the search at a
    /// different repository is refused before any HTTP request is constructed, not merely
    /// declined by GitHub after the fact.
    #[test]
    fn list_via_search_refuses_a_repo_qualifier_before_any_http_call() {
        let error = invoke_with(
            &capability("gh.pull-request.list"),
            json!({"owner": "octo", "repo": "hello", "search": "repo:other/repo"}),
            |_| unreachable!("a scope-escape query must never reach HTTP"),
        )
        .expect_err("scope escape refused");
        assert_eq!(error.code(), "invalid-search-query");
    }

    #[test]
    fn list_via_search_rejects_being_combined_with_other_filters() {
        let error = invoke_with(
            &capability("gh.pull-request.list"),
            json!({"owner": "octo", "repo": "hello", "search": "label:bug", "author": "cpetersen"}),
            |_| unreachable!("a conflicting combination must never reach HTTP"),
        )
        .expect_err("combination refused");
        assert_eq!(error.code(), "invalid-search-query");
    }

    #[test]
    fn files_bounds_patches_with_an_exact_flag() {
        let long_patch = "x".repeat(crate::MAX_PATCH_OUT_BYTES + 1);
        let output = invoke_with(
            &capability("gh.pull-request.files"),
            json!({"owner": "octo", "repo": "hello", "number": 7}),
            scripted(vec![step(
                |request| {
                    assert_eq!(
                        request.uri,
                        "https://api.github.com/repos/octo/hello/pulls/7/files?page=1&per_page=30"
                    );
                },
                json_response(
                    200,
                    &json!([
                        {"filename": "src/lib.rs", "status": "modified", "additions": 5, "deletions": 1, "patch": "@@ -1 +1 @@"},
                        {"filename": "big.rs", "status": "added", "additions": 100, "deletions": 0, "patch": long_patch},
                    ]),
                ),
            )]),
        )
        .expect("files succeeds");

        let files = output["files"].as_array().expect("files");
        assert_eq!(files[0]["patch"], "@@ -1 +1 @@");
        assert_eq!(files[0]["patchTruncated"], false);
        assert_eq!(files[1]["patchTruncated"], true);
        assert_eq!(
            files[1]["patch"].as_str().expect("patch").len(),
            crate::MAX_PATCH_OUT_BYTES
        );
    }

    #[test]
    fn files_omits_patches_when_asked() {
        let output = invoke_with(
            &capability("gh.pull-request.files"),
            json!({"owner": "octo", "repo": "hello", "number": 7, "includePatch": false}),
            scripted(vec![step(
                |_| {},
                json_response(
                    200,
                    &json!([
                        {"filename": "src/lib.rs", "status": "modified", "additions": 5, "deletions": 1, "patch": "@@ -1 +1 @@"},
                    ]),
                ),
            )]),
        )
        .expect("files succeeds");

        assert!(output["files"][0].get("patch").is_none());
    }

    #[test]
    fn diff_requests_the_diff_media_type() {
        let output = invoke_with(
            &capability("gh.pull-request.diff"),
            json!({"owner": "octo", "repo": "hello", "number": 7}),
            scripted(vec![step(
                |request| {
                    assert_eq!(accept_of(request), b"application/vnd.github.diff");
                    assert_eq!(
                        request.uri,
                        "https://api.github.com/repos/octo/hello/pulls/7"
                    );
                },
                Ok(dekopon_provider_http::Response {
                    status: 200,
                    headers: Vec::new(),
                    body: b"diff --git a/x b/x\n".to_vec(),
                }),
            )]),
        )
        .expect("diff succeeds");

        assert_eq!(output["diff"], "diff --git a/x b/x\n");
        assert_eq!(output["diffTruncated"], false);
    }

    #[test]
    fn diff_name_only_extracts_changed_file_paths() {
        let diff_text = concat!(
            "diff --git a/src/lib.rs b/src/lib.rs\n",
            "index 000..111 100644\n",
            "--- a/src/lib.rs\n",
            "+++ b/src/lib.rs\n",
            "@@ -1 +1 @@\n",
            "-old\n",
            "+new\n",
            "diff --git a/README.md b/README.md\n",
            "index 000..111 100644\n",
            "--- a/README.md\n",
            "+++ b/README.md\n",
            "@@ -1 +1 @@\n",
            "-old\n",
            "+new\n",
        );
        let output = invoke_with(
            &capability("gh.pull-request.diff"),
            json!({"owner": "octo", "repo": "hello", "number": 7, "nameOnly": true}),
            scripted(vec![step(
                |request| {
                    assert_eq!(accept_of(request), b"application/vnd.github.diff");
                },
                Ok(dekopon_provider_http::Response {
                    status: 200,
                    headers: Vec::new(),
                    body: diff_text.as_bytes().to_vec(),
                }),
            )]),
        )
        .expect("name-only diff succeeds");

        assert_eq!(output["files"], json!(["src/lib.rs", "README.md"]));
        assert_eq!(output["filesTruncated"], false);
        assert!(output.get("diff").is_none());
    }

    #[test]
    fn reviews_projects_bounded_items() {
        let output = invoke_with(
            &capability("gh.pull-request.reviews"),
            json!({"owner": "octo", "repo": "hello", "number": 7}),
            scripted(vec![step(
                |request| {
                    assert_eq!(
                        request.uri,
                        "https://api.github.com/repos/octo/hello/pulls/7/reviews?page=1&per_page=20"
                    );
                },
                json_response(
                    200,
                    &json!([
                        {"id": 900, "state": "APPROVED", "user": {"login": "boss"}, "commit_id": "c".repeat(40), "submitted_at": "2026-08-10T00:00:00Z", "body": "lgtm"},
                    ]),
                ),
            )]),
        )
        .expect("reviews succeeds");

        assert_eq!(output["reviews"][0]["reviewId"], 900);
        assert_eq!(output["reviews"][0]["author"], "boss");
        assert_eq!(output["reviews"][0]["state"], "APPROVED");
    }

    #[test]
    fn status_reads_actions_and_legacy_statuses_at_the_pull_head() {
        let head = "a".repeat(40);
        let expected_workflows_uri = format!(
            "https://api.github.com/repos/octo/hello/actions/runs?head_sha={head}&page=1&per_page=50"
        );
        let expected_statuses_uri = format!(
            "https://api.github.com/repos/octo/hello/commits/{head}/status?page=1&per_page=50"
        );
        let output = invoke_with(
            &capability("gh.pull-request.status"),
            json!({"owner": "octo", "repo": "hello", "number": 7}),
            scripted(vec![
                step(
                    |request| {
                        assert_eq!(request.method, "GET");
                        assert_eq!(
                            request.uri,
                            "https://api.github.com/repos/octo/hello/pulls/7"
                        );
                    },
                    json_response(200, &pull_body(7, "open", false, false)),
                ),
                step(
                    move |request| {
                        assert_eq!(request.method, "GET");
                        assert_eq!(request.uri, expected_workflows_uri);
                    },
                    json_response(
                        200,
                        &json!({
                            "total_count": 2,
                            "workflow_runs": [
                                {
                                    "id": 101,
                                    "name": "ci",
                                    "display_title": "Run the suite",
                                    "event": "pull_request",
                                    "status": "completed",
                                    "conclusion": "success",
                                    "head_sha": head,
                                    "run_number": 42,
                                    "run_attempt": 1,
                                    "created_at": "2026-08-10T00:00:00Z",
                                    "updated_at": "2026-08-10T00:05:00Z"
                                },
                                {
                                    "id": 102,
                                    "name": "lint",
                                    "display_title": "Lint the branch",
                                    "event": "pull_request",
                                    "status": "in_progress",
                                    "conclusion": null,
                                    "head_sha": head,
                                    "run_number": 43,
                                    "run_attempt": 2,
                                    "created_at": "2026-08-10T00:01:00Z",
                                    "updated_at": "2026-08-10T00:06:00Z"
                                }
                            ]
                        }),
                    ),
                ),
                step(
                    move |request| {
                        assert_eq!(request.method, "GET");
                        assert_eq!(request.uri, expected_statuses_uri);
                    },
                    json_response(
                        200,
                        &json!({
                            "state": "failure",
                            "sha": head,
                            "total_count": 1,
                            "statuses": [{
                                "id": 201,
                                "state": "failure",
                                "context": "external-ci",
                                "description": "A legacy status failed",
                                "created_at": "2026-08-10T00:02:00Z",
                                "updated_at": "2026-08-10T00:07:00Z"
                            }]
                        }),
                    ),
                ),
            ]),
        )
        .expect("status succeeds");

        assert_eq!(output["workflowRunCount"], 2);
        assert_eq!(output["workflowRuns"][0]["conclusion"], "success");
        assert_eq!(output["workflowRuns"][1]["status"], "in_progress");
        assert_eq!(output["workflowRuns"][1]["runAttempt"], 2);
        assert_eq!(output["commitStatusState"], "failure");
        assert_eq!(output["commitStatuses"][0]["context"], "external-ci");
        assert_eq!(output["headSha"], "a".repeat(40));
    }

    #[test]
    fn status_does_not_report_githubs_empty_status_default_as_pending() {
        let head = "a".repeat(40);
        let output = invoke_with(
            &capability("gh.pull-request.status"),
            json!({"owner": "octo", "repo": "hello", "number": 7}),
            scripted(vec![
                step(
                    |_| {},
                    json_response(200, &pull_body(7, "open", false, false)),
                ),
                step(
                    |_| {},
                    json_response(200, &json!({"total_count": 0, "workflow_runs": []})),
                ),
                step(
                    |_| {},
                    json_response(
                        200,
                        &json!({
                            "state": "pending",
                            "sha": head,
                            "total_count": 0,
                            "statuses": []
                        }),
                    ),
                ),
            ]),
        )
        .expect("empty status succeeds");

        assert_eq!(output["workflowRunCount"], 0);
        assert_eq!(output["commitStatusCount"], 0);
        assert_eq!(output["commitStatusState"], Value::Null);
    }

    #[test]
    fn status_rejects_workflow_runs_for_a_different_head() {
        let wrong = "c".repeat(40);
        let error = invoke_with(
            &capability("gh.pull-request.status"),
            json!({"owner": "octo", "repo": "hello", "number": 7}),
            scripted(vec![
                step(
                    |_| {},
                    json_response(200, &pull_body(7, "open", false, false)),
                ),
                step(
                    |_| {},
                    json_response(
                        200,
                        &json!({
                            "total_count": 1,
                            "workflow_runs": [{
                                "id": 101,
                                "name": "ci",
                                "display_title": "Run the suite",
                                "event": "pull_request",
                                "status": "completed",
                                "conclusion": "success",
                                "head_sha": wrong,
                                "run_number": 42,
                                "run_attempt": 1,
                                "created_at": "2026-08-10T00:00:00Z",
                                "updated_at": "2026-08-10T00:05:00Z"
                            }]
                        }),
                    ),
                ),
            ]),
        )
        .expect_err("wrong workflow head fails");

        assert_eq!(error.code(), "invalid-response");
    }

    #[test]
    fn status_rejects_legacy_statuses_for_a_different_head() {
        let error = invoke_with(
            &capability("gh.pull-request.status"),
            json!({"owner": "octo", "repo": "hello", "number": 7}),
            scripted(vec![
                step(
                    |_| {},
                    json_response(200, &pull_body(7, "open", false, false)),
                ),
                step(
                    |_| {},
                    json_response(200, &json!({"total_count": 0, "workflow_runs": []})),
                ),
                step(
                    |_| {},
                    json_response(
                        200,
                        &json!({
                            "state": "success",
                            "sha": "c".repeat(40),
                            "total_count": 0,
                            "statuses": []
                        }),
                    ),
                ),
            ]),
        )
        .expect_err("wrong legacy-status head fails");

        assert_eq!(error.code(), "invalid-response");
    }

    #[test]
    fn status_bounds_pages_and_accepts_nullable_workflow_fields() {
        let head = "a".repeat(40);
        let output = invoke_with(
            &capability("gh.pull-request.status"),
            json!({"owner": "octo", "repo": "hello", "number": 7}),
            scripted(vec![
                step(
                    |_| {},
                    json_response(200, &pull_body(7, "open", false, false)),
                ),
                step(
                    |_| {},
                    json_response(
                        200,
                        &json!({
                            "total_count": 51,
                            "workflow_runs": [{
                                "id": 101,
                                "name": null,
                                "display_title": "Fallback title",
                                "event": "workflow_dispatch",
                                "status": null,
                                "conclusion": null,
                                "head_sha": head,
                                "run_number": 42,
                                "created_at": "2026-08-10T00:00:00Z",
                                "updated_at": "2026-08-10T00:05:00Z"
                            }]
                        }),
                    ),
                ),
                step(
                    |_| {},
                    json_response(
                        200,
                        &json!({
                            "state": "success",
                            "sha": head,
                            "total_count": 51,
                            "statuses": [{
                                "id": 201,
                                "state": "success",
                                "context": "legacy",
                                "description": null,
                                "created_at": "2026-08-10T00:02:00Z",
                                "updated_at": "2026-08-10T00:07:00Z"
                            }]
                        }),
                    ),
                ),
            ]),
        )
        .expect("nullable workflow fields are valid");

        assert_eq!(output["workflowRunsTruncated"], true);
        assert_eq!(output["workflowRuns"][0]["name"], "Fallback title");
        assert_eq!(output["workflowRuns"][0]["status"], Value::Null);
        assert_eq!(output["workflowRuns"][0]["runAttempt"], Value::Null);
        assert_eq!(output["commitStatusesTruncated"], true);
    }
}
