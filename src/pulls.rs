//! Read-side pull-request capabilities: list, read, files, diff, reviews, and status.

use dekopon_provider_http::{HttpError, Request, Response};
use dekopon_provider_sdk::ProviderError;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    ACCEPT_DIFF, ACCEPT_JSON, MAX_COMMENT_OUT_BYTES, MAX_DESCRIPTION_OUT_BYTES, MAX_DIFF_OUT_BYTES,
    MAX_LIST_ITEMS, MAX_PATCH_OUT_BYTES, MAX_PR_BODY_OUT_BYTES, MAX_TITLE_OUT_BYTES, RawUser,
    bounded_optional, decode, endpoint, has_next_link, invalid_input, invalid_response, is_sha,
    login_out, percent_encode, send_get, timestamp, truncate_text, validate_login, validate_number,
    validate_page, validate_repo,
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ReadInput {
    owner: String,
    repo: String,
    number: u32,
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
    #[serde(default)]
    endpoint: Option<String>,
}

pub(crate) fn list(
    input: Value,
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
    let endpoint = endpoint(input.endpoint.as_deref())?;

    let uri = format!(
        "{endpoint}/repos/{}/{}/pulls?state={}&page={page}&per_page={per_page}",
        percent_encode(&input.owner),
        percent_encode(&input.repo),
        state.as_str(),
    );
    let response = send_get(send, uri, ACCEPT_JSON)?;
    let has_more = has_next_link(&response);
    let pulls = decode::<Vec<RawPull>>(&response.body)?;

    // `author` filters the fetched page after pagination — the REST list endpoint has no author
    // parameter. The schema description says so, so a model is not surprised by a short page.
    let items = pulls
        .into_iter()
        .filter(|pull| match input.author.as_deref() {
            Some(author) => pull
                .user
                .as_ref()
                .is_some_and(|user| user.login.eq_ignore_ascii_case(author)),
            None => true,
        })
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
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let input = serde_json::from_value::<ReadInput>(input).map_err(|_| invalid_input())?;
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
