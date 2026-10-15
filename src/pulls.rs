//! Read-side pull-request capabilities: list, read, files, diff, reviews, and status.

use crate::error::ProviderError;
use dekopon_provider_sdk::provider::endpoint::Base;
use dekopon_provider_sdk::provider::{HttpError, Request, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    ACCEPT_DIFF, ACCEPT_JSON, MAX_COMMENT_OUT_BYTES, MAX_DESCRIPTION_OUT_BYTES, MAX_DIFF_OUT_BYTES,
    MAX_LIST_ITEMS, MAX_PATCH_OUT_BYTES, MAX_PR_BODY_OUT_BYTES, MAX_TITLE_OUT_BYTES, RawUser,
    bounded_optional, decode, has_next_link, invalid_input, invalid_response, is_sha, login_out,
    marked_optional, marked_text, percent_encode, send_get, timestamp, truncate_text, url,
    validate_login, validate_number, validate_page, validate_repo,
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
    pub(crate) body: Option<String>,
    #[serde(default)]
    pub(crate) user: Option<RawUser>,
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
    base: &Base,
    owner: &str,
    repo: &str,
    number: u32,
) -> Result<RawPull, ProviderError> {
    fetch_pull_with_raw(send, base, owner, repo, number).map(|(pull, _)| pull)
}

fn fetch_pull_with_raw(
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
    base: &Base,
    owner: &str,
    repo: &str,
    number: u32,
) -> Result<(RawPull, Value), ProviderError> {
    let uri = url(
        base,
        &format!(
            "/repos/{}/{}/pulls/{number}",
            percent_encode(owner),
            percent_encode(repo)
        ),
    )?;
    let response = send_get(send, uri, ACCEPT_JSON)?;
    let raw = decode::<Value>(&response.body)?;
    let pull = serde_json::from_value::<RawPull>(raw.clone()).map_err(|_| invalid_response())?;
    pull.validate(number)?;
    Ok((pull, raw))
}

// ---------------------------------------------------------------------------
// gh.pull-request.read
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ReadInput {
    owner: String,
    repo: String,
    number: u32,
    #[serde(default, rename = "_cli")]
    cli: Option<String>,
}

pub(crate) fn read(
    input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let input = serde_json::from_value::<ReadInput>(input).map_err(|_| invalid_input())?;
    validate_login(&input.owner)?;
    validate_repo(&input.repo)?;
    validate_number(input.number)?;
    let (pull, raw) = fetch_pull_with_raw(send, base, &input.owner, &input.repo, input.number)?;
    if let Some(fields) = input.cli.as_deref() {
        let mut projected = cli_pull(&pull, &raw, fields)?;
        let path = format!(
            "/repos/{}/{}/",
            percent_encode(&input.owner),
            percent_encode(&input.repo)
        );
        if fields.split(',').any(|field| field == "files") {
            projected["files"] = json!(crate::view_items(send, base, &format!("{path}pulls/{}/files?per_page=100", input.number), &["filename", "status", "additions", "deletions"])?
                .into_iter().map(|file| json!({"path": file["filename"], "status": file["status"], "additions": file["additions"], "deletions": file["deletions"]})).collect::<Vec<_>>());
        }
        if fields
            .split(',')
            .any(|field| matches!(field, "reviews" | "latestReviews"))
        {
            let reviews = crate::view_items(send, base, &format!("{path}pulls/{}/reviews?per_page=100", input.number), &["state"])?
                .into_iter().map(|review| json!({"author": review["user"]["login"].as_str().map(|login| json!({"login": login})), "body": review["body"], "state": review["state"], "submittedAt": review["submitted_at"]})).collect::<Vec<_>>();
            if fields.split(',').any(|field| field == "latestReviews") {
                let mut latest = std::collections::BTreeMap::new();
                for review in &reviews {
                    if let Some(login) = review["author"]["login"].as_str() {
                        let previous = latest
                            .entry(login.to_owned())
                            .or_insert_with(|| review.clone());
                        if review["submittedAt"].as_str() > previous["submittedAt"].as_str() {
                            *previous = review.clone();
                        }
                    }
                }
                projected["latestReviews"] = json!(latest.into_values().collect::<Vec<_>>());
            }
            projected["reviews"] = json!(reviews);
        }
        if fields.split(',').any(|field| field == "comments") {
            projected["comments"] = json!(crate::view_items(send, base, &format!("{path}issues/{}/comments?per_page=100", input.number), &["created_at"])?
                .into_iter().map(|comment| json!({"author": comment["user"]["login"].as_str().map(|login| json!({"login": login})), "body": comment["body"], "createdAt": comment["created_at"]})).collect::<Vec<_>>());
        }
        return Ok(projected);
    }

    let (title, _) = truncate_text(&pull.title, MAX_TITLE_OUT_BYTES);
    let (body, body_truncated) = bounded_optional(pull.body.as_deref(), MAX_PR_BODY_OUT_BYTES);
    Ok(json!({
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
    }))
}

fn cli_pull(pull: &RawPull, raw: &Value, fields: &str) -> Result<Value, ProviderError> {
    let title = marked_text(&pull.title, MAX_TITLE_OUT_BYTES);
    let body = marked_optional(pull.body.as_deref(), MAX_PR_BODY_OUT_BYTES);
    let state = if pull.merged.unwrap_or(false) || !raw["merged_at"].is_null() {
        "MERGED"
    } else if pull.state == "closed" {
        "CLOSED"
    } else {
        "OPEN"
    };
    if (fields.split(',').any(|field| field == "assignees")
        && raw["assignees"]
            .as_array()
            .is_some_and(|items| items.len() > MAX_LIST_ITEMS))
        || (fields.split(',').any(|field| field == "labels")
            && raw["labels"]
                .as_array()
                .is_some_and(|items| items.len() > crate::MAX_LABELS))
    {
        return Err(invalid_response());
    }
    let users = |key: &str| {
        raw[key]
            .as_array()
            .map(|users| {
                users
                    .iter()
                    .filter_map(|user| user["login"].as_str().map(|login| json!({"login": login})))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    Ok(json!({
        "number": pull.number, "title": title, "body": body, "state": state,
        "author": raw["user"]["login"].as_str().map(|login| json!({"login": login})),
        "assignees": users("assignees"), "labels": raw["labels"].as_array().cloned().unwrap_or_default(),
        "baseRefName": pull.base.name, "baseRefOid": pull.base.sha,
        "headRefName": pull.head.name, "headRefOid": pull.head.sha,
        "isDraft": pull.draft, "closedAt": raw["closed_at"], "mergedAt": raw["merged_at"],
        "createdAt": timestamp(&pull.created_at)?, "updatedAt": timestamp(&pull.updated_at)?,
        "url": raw["html_url"],
    }))
}

// ---------------------------------------------------------------------------
// gh.pull-request.list
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum StateFilter {
    Open,
    Closed,
    All,
}

impl StateFilter {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
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
    page: Option<u32>,
    #[serde(default)]
    per_page: Option<u32>,
    #[serde(default, rename = "_cli")]
    cli: Option<String>,
}

pub(crate) fn list(
    input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let input = serde_json::from_value::<ListInput>(input).map_err(|_| invalid_input())?;
    validate_login(&input.owner)?;
    validate_repo(&input.repo)?;
    if let Some(author) = input.author.as_deref() {
        validate_login(author)?;
    }
    let (page, per_page) = validate_page(input.page, input.per_page)?;
    let per_page = per_page.unwrap_or(20);
    let state = input.state.unwrap_or(StateFilter::Open);

    let uri = url(
        base,
        &format!(
            "/repos/{}/{}/pulls?state={}&page={page}&per_page={per_page}",
            percent_encode(&input.owner),
            percent_encode(&input.repo),
            state.as_str(),
        ),
    )?;
    let response = send_get(send, uri, ACCEPT_JSON)?;
    let has_more = has_next_link(&response);
    let pulls = decode::<Vec<Value>>(&response.body)?;

    // `author` filters the fetched page after pagination — the REST list endpoint has no author
    // parameter. The schema description says so, so a model is not surprised by a short page.
    let items = pulls
        .into_iter()
        .filter(|raw| match input.author.as_deref() {
            Some(author) => raw["user"]["login"]
                .as_str()
                .is_some_and(|login| login.eq_ignore_ascii_case(author)),
            None => true,
        })
        .take(MAX_LIST_ITEMS)
        .map(|raw| {
            let pull =
                serde_json::from_value::<RawPull>(raw.clone()).map_err(|_| invalid_response())?;
            if !is_sha(&pull.head.sha) {
                return Err(invalid_response());
            }
            if let Some(fields) = input.cli.as_deref() {
                return cli_pull(&pull, &raw, fields);
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

// ---------------------------------------------------------------------------
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

#[expect(dead_code, reason = "file selection moves into pr view in S2")]
pub(crate) fn files(
    input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let input = serde_json::from_value::<FilesInput>(input).map_err(|_| invalid_input())?;
    validate_login(&input.owner)?;
    validate_repo(&input.repo)?;
    validate_number(input.number)?;
    let (page, per_page) = validate_page(input.page, input.per_page)?;
    let per_page = per_page.unwrap_or(30);
    let include_patch = input.include_patch.unwrap_or(true);

    let uri = url(
        base,
        &format!(
            "/repos/{}/{}/pulls/{}/files?page={page}&per_page={per_page}",
            percent_encode(&input.owner),
            percent_encode(&input.repo),
            input.number,
        ),
    )?;
    let response = send_get(send, uri, ACCEPT_JSON)?;
    let has_more = has_next_link(&response);
    let files = decode::<Vec<RawFile>>(&response.body)?;

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

pub(crate) fn diff(
    input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let input = serde_json::from_value::<ReadInput>(input).map_err(|_| invalid_input())?;
    validate_login(&input.owner)?;
    validate_repo(&input.repo)?;
    validate_number(input.number)?;

    let uri = url(
        base,
        &format!(
            "/repos/{}/{}/pulls/{}",
            percent_encode(&input.owner),
            percent_encode(&input.repo),
            input.number,
        ),
    )?;
    let response = send_get(send, uri, ACCEPT_DIFF)?;
    let text = core::str::from_utf8(&response.body).map_err(|_| invalid_response())?;
    let (diff, truncated) = truncate_text(text, MAX_DIFF_OUT_BYTES);

    Ok(json!({
        "number": input.number,
        "diff": diff,
        "diffTruncated": truncated,
    }))
}

// ---------------------------------------------------------------------------
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

#[expect(dead_code, reason = "review selection moves into pr view in S2")]
pub(crate) fn reviews(
    input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let input = serde_json::from_value::<ReviewsInput>(input).map_err(|_| invalid_input())?;
    validate_login(&input.owner)?;
    validate_repo(&input.repo)?;
    validate_number(input.number)?;
    let (page, per_page) = validate_page(input.page, input.per_page)?;
    let per_page = per_page.unwrap_or(20);

    let uri = url(
        base,
        &format!(
            "/repos/{}/{}/pulls/{}/reviews?page={page}&per_page={per_page}",
            percent_encode(&input.owner),
            percent_encode(&input.repo),
            input.number,
        ),
    )?;
    let response = send_get(send, uri, ACCEPT_JSON)?;
    let has_more = has_next_link(&response);
    let reviews = decode::<Vec<RawReviewItem>>(&response.body)?;

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
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let input = serde_json::from_value::<ReadInput>(input).map_err(|_| invalid_input())?;
    validate_login(&input.owner)?;
    validate_repo(&input.repo)?;
    validate_number(input.number)?;

    let pull = fetch_pull(send, base, &input.owner, &input.repo, input.number)?;
    let owner = percent_encode(&input.owner);
    let repo = percent_encode(&input.repo);
    let head = percent_encode(&pull.head.sha);

    // Fine-grained personal access tokens expose Actions and Commit statuses permissions, but not
    // the Checks permission required by `/check-runs`. Workflow runs give the useful GitHub
    // Actions result at the same head without widening this read capability to a POST surface.
    let workflows_uri = url(
        base,
        &format!(
            "/repos/{owner}/{repo}/actions/runs?head_sha={head}&page=1&per_page={MAX_LIST_ITEMS}"
        ),
    )?;
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
    let statuses_uri = url(
        base,
        &format!("/repos/{owner}/{repo}/commits/{head}/status?page=1&per_page={MAX_LIST_ITEMS}"),
    )?;
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
    fn native_view_expands_selected_fields_on_one_grant_or_fails_without_partial_output() {
        use std::{cell::Cell, rc::Rc};
        let path = "https://api.github.com/repos/octo/hello";
        let responses = [
            (
                format!("{path}/pulls/7"),
                json_response(200, &pull_body(7, "open", false, false)),
            ),
            (
                format!("{path}/pulls/7/files?per_page=100"),
                json_response(
                    200,
                    &json!([{"filename":"src/lib.rs","status":"modified","additions":2,"deletions":1}]),
                ),
            ),
            (
                format!("{path}/pulls/7/reviews?per_page=100"),
                json_response(
                    200,
                    &json!([{"state":"APPROVED","user":{"login":"dev"},"submitted_at":"2026-01-01T00:00:00Z"},{"state":"COMMENTED","user":{"login":"dev"},"submitted_at":"2026-01-02T00:00:00Z"}]),
                ),
            ),
            (
                format!("{path}/issues/7/comments?per_page=100"),
                json_response(
                    200,
                    &json!([{"user":{"login":"reviewer"},"body":"ship","created_at":"2026-01-03T00:00:00Z"}]),
                ),
            ),
        ];
        for failing in [false, true] {
            let seen = Rc::new(Cell::new(0));
            let steps = responses
                .iter()
                .enumerate()
                .map(|(index, (uri, reply))| {
                    let uri = uri.clone();
                    let seen = Rc::clone(&seen);
                    let reply = if failing && index == 3 {
                        json_response(502, &json!({}))
                    } else {
                        reply.clone()
                    };
                    step(
                        move |request| {
                            assert_eq!(request.uri, uri);
                            assert_eq!(request.method, "GET");
                            seen.set(seen.get() + 1);
                        },
                        reply,
                    )
                })
                .collect();
            let result = invoke_with(
                &capability("gh.pull-request.read"),
                json!({"owner":"octo","repo":"hello","number":7,"_cli":"files,reviews,latestReviews,comments"}),
                scripted(steps),
            );
            assert_eq!(seen.get(), 4);
            if failing {
                assert_eq!(
                    result
                        .expect_err("late upstream failure must refuse")
                        .code(),
                    "unexpected-status"
                );
            } else {
                let value = result.expect("all reads complete");
                assert_eq!(value["files"][0]["path"], "src/lib.rs");
                assert_eq!(value["reviews"].as_array().expect("reviews").len(), 2);
                assert_eq!(value["latestReviews"][0]["state"], "COMMENTED");
                assert_eq!(value["comments"][0]["author"]["login"], "reviewer");
            }
        }
    }

    #[test]
    fn native_view_refuses_an_unfinished_extra_page() {
        use std::{cell::Cell, rc::Rc};
        for link in [
            "<https://api.github.com/repos/octo/hello/pulls/7/files?page=2>; rel=\"next\"",
            "malformed cursor",
        ] {
            let seen = Rc::new(Cell::new(0));
            let first = Rc::clone(&seen);
            let second = Rc::clone(&seen);
            let output = invoke_with(&capability("gh.pull-request.read"), json!({"owner":"octo","repo":"hello","number":7,"_cli":"files"}), scripted(vec![
                step(move |request| { assert_eq!(request.uri, "https://api.github.com/repos/octo/hello/pulls/7"); first.set(first.get() + 1); }, json_response(200, &pull_body(7, "open", false, false))),
                step(move |request| { assert_eq!(request.uri, "https://api.github.com/repos/octo/hello/pulls/7/files?per_page=100"); second.set(second.get() + 1); }, response_with_headers(200, &[("link", link)], &json!([{"filename":"a","status":"modified","additions":1,"deletions":0}]))),
            ])).expect_err("partial files are not success");
            assert_eq!(
                seen.get(),
                2,
                "both reads must precede the pagination refusal"
            );
            assert_eq!(output.code(), "invalid-response");
        }
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
                        "https://api.github.com/repos/octo/hello/pulls?state=open&page=1&per_page=20"
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
                Ok(dekopon_provider_sdk::provider::Response {
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
