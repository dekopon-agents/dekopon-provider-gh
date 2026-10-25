//! Read-side pull-request capabilities: list, read, files, diff, reviews, and status.

use crate::error::ProviderError;
use dekopon_provider_sdk::provider::endpoint::Base;
use dekopon_provider_sdk::provider::{HttpError, Request, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    ACCEPT_DIFF, ACCEPT_JSON, MAX_DESCRIPTION_OUT_BYTES, MAX_DIFF_OUT_BYTES, MAX_LIST_ITEMS,
    MAX_PR_BODY_OUT_BYTES, MAX_TITLE_OUT_BYTES, RawUser, bounded_optional, decode, has_next_link,
    invalid_input, invalid_response, is_sha, login_out, marked_optional, marked_text,
    percent_encode, send_get, timestamp, truncate_text, url, validate_login, validate_number,
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
    #[serde(default)]
    comments: bool,
}

pub(crate) fn read(
    input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let mut input = serde_json::from_value::<ReadInput>(input).map_err(|_| invalid_input())?;
    if input.comments && input.cli.is_none() {
        input.cli = Some(String::new());
    }
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
            let canonical = raw["base"]["repo"]["id"]
                .as_u64()
                .filter(|id| *id > 0)
                .map(|id| {
                    url(
                        base,
                        &format!("/repositories/{id}/pulls/{}/files", input.number),
                    )
                })
                .transpose()?;
            projected["files"] = json!(file_items(
                send,
                base,
                &format!("{path}pulls/{}/files?per_page=100", input.number),
                canonical.as_deref()
            )?);
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
        if input.comments || fields.split(',').any(|field| field == "comments") {
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

fn file_items(
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
    base: &Base,
    path: &str,
    canonical: Option<&str>,
) -> Result<Vec<Value>, ProviderError> {
    let original = url(base, path)?;
    let mut uri = original.clone();
    let mut files = Vec::new();
    for page in 1..=30 {
        let response = send_get(send, uri, ACCEPT_JSON)?;
        let items = decode::<Vec<Value>>(&response.body)?;
        if items.len() > 100
            || items.iter().any(|item| {
                ["filename", "status", "additions", "deletions"]
                    .iter()
                    .any(|key| item[*key].is_null())
            })
        {
            return Err(invalid_response());
        }
        files.extend(items.into_iter().map(|mut file| {
            json!({
                "path": file["filename"].take(),
                "status": file["status"].take(),
                "additions": file["additions"].take(),
                "deletions": file["deletions"].take(),
            })
        }));
        match next_file_page(&response, &original, canonical, page)? {
            Some(next) => uri = next,
            None => return Ok(files),
        }
    }
    Err(ProviderError::new(
        "request-ceiling",
        "pull-request files exceed the page ceiling",
    ))
}

fn next_file_page(
    response: &Response,
    original: &str,
    canonical: Option<&str>,
    page: u32,
) -> Result<Option<String>, ProviderError> {
    let mut next = None;
    for value in crate::header_values(response, "link") {
        let value = core::str::from_utf8(value).map_err(|_| invalid_response())?;
        for part in value.split(',') {
            let mut fields = part.trim().split(';');
            let target = fields.next().ok_or_else(invalid_response)?.trim();
            if !target.starts_with('<') || !target.ends_with('>') {
                return Err(invalid_response());
            }
            let mut is_next = false;
            for field in fields {
                let (name, value) = field.trim().split_once('=').ok_or_else(invalid_response)?;
                if name.trim() == "rel" {
                    is_next |= value
                        .trim()
                        .trim_matches('"')
                        .split_ascii_whitespace()
                        .any(|rel| rel == "next");
                }
            }
            if !is_next {
                continue;
            }
            let target = target
                .strip_prefix('<')
                .and_then(|s| s.strip_suffix('>'))
                .ok_or_else(invalid_response)?;
            let (path, query) = target.split_once('?').ok_or_else(invalid_response)?;
            let expected_path = original
                .strip_suffix("?per_page=100")
                .ok_or_else(invalid_response)?;
            if (path != expected_path && Some(path) != canonical) || next.is_some() {
                return Err(invalid_response());
            }
            let mut parameters = query.split('&').collect::<Vec<_>>();
            parameters.sort_unstable();
            let expected_page = format!("page={}", page + 1);
            if parameters != [expected_page.as_str(), "per_page=100"] {
                return Err(invalid_response());
            }
            next = Some(target.to_owned());
        }
    }
    Ok(next)
}

pub(crate) fn cli_pull(pull: &RawPull, raw: &Value, fields: &str) -> Result<Value, ProviderError> {
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

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum StateFilter {
    Open,
    Closed,
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
    page: Option<u32>,
    #[serde(default)]
    per_page: Option<u32>,
    #[serde(default, rename = "_cli")]
    cli: Option<String>,
    #[serde(default, rename = "_cliLimit")]
    cli_limit: Option<usize>,
}

pub(crate) fn list(
    input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let mut input = input;
    crate::qualifiers::remove_nulls("pr list", &mut input);
    if crate::qualifiers::has_values("pr list", &input) {
        return crate::list_filters::run("pr list", input, base, send);
    }
    let input = serde_json::from_value::<ListInput>(input).map_err(|_| invalid_input())?;
    validate_login(&input.owner)?;
    validate_repo(&input.repo)?;
    if let Some(author) = input.author.as_deref() {
        validate_login(author)?;
    }
    if input.cli.is_some() {
        if input.page.is_some() || input.per_page.is_some() {
            return Err(invalid_input());
        }
        let state = input.state.unwrap_or(StateFilter::Open);
        let path = format!(
            "/repos/{}/{}/pulls?state={}",
            percent_encode(&input.owner),
            percent_encode(&input.repo),
            state.as_str()
        );
        let fields = input.cli.as_deref().unwrap_or_default();
        let items =
            crate::collect_pages(send, base, &path, input.cli_limit.unwrap_or(30), |raw| {
                if matches!(state, StateFilter::Merged)
                    && raw["merged_at"].is_null()
                    && raw["merged"] != true
                {
                    return Ok(None);
                }
                if input.author.as_deref().is_some_and(|author| {
                    !raw["user"]["login"]
                        .as_str()
                        .is_some_and(|login| login.eq_ignore_ascii_case(author))
                }) {
                    return Ok(None);
                }
                let pull = serde_json::from_value::<RawPull>(raw.clone())
                    .map_err(|_| invalid_response())?;
                if !is_sha(&pull.head.sha) || !is_sha(&pull.base.sha) {
                    return Err(invalid_response());
                }
                cli_pull(&pull, &raw, fields).map(Some)
            })?;
        return Ok(json!({"pullRequests": items}));
    }
    if input.cli_limit.is_some() {
        return Err(invalid_input());
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

    let items = pulls
        .into_iter()
        .filter(|raw| {
            !matches!(state, StateFilter::Merged)
                || !raw["merged_at"].is_null()
                || raw["merged"] == true
        })
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
// gh.pull-request.diff
// ---------------------------------------------------------------------------

pub(crate) fn diff(
    input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let input = serde_json::from_value::<crate::diff::Input>(input).map_err(|_| invalid_input())?;
    validate_login(&input.owner)?;
    validate_repo(&input.repo)?;
    validate_number(input.number)?;

    let patterns = crate::diff::patterns(&input.exclude)?;
    if input.patch && input.name_only {
        return Err(invalid_input());
    }
    let uri = url(
        base,
        &format!(
            "/repos/{}/{}/pulls/{}",
            percent_encode(&input.owner),
            percent_encode(&input.repo),
            input.number,
        ),
    )?;
    let response = send_get(
        send,
        uri,
        if input.patch {
            "application/vnd.github.patch"
        } else {
            ACCEPT_DIFF
        },
    )?;
    let text = core::str::from_utf8(&response.body).map_err(|_| invalid_response())?;
    let transformed;
    let text = if input.name_only || !patterns.is_empty() {
        transformed = crate::diff::transform(text, input.name_only, &patterns)?;
        transformed.as_str()
    } else {
        text
    };
    let (diff, truncated) = truncate_text(text, MAX_DIFF_OUT_BYTES);

    Ok(json!({
        "number": input.number,
        "diff": diff,
        "diffTruncated": truncated,
    }))
}

// ---------------------------------------------------------------------------
// gh.pull-request.status
// ---------------------------------------------------------------------------

const ZERO_TIME: &str = "0001-01-01T00:00:00Z";

fn check_text(value: Option<&str>) -> Result<&str, ProviderError> {
    let value = value.unwrap_or("");
    if value.len() > MAX_DESCRIPTION_OUT_BYTES {
        return Err(invalid_response());
    }
    Ok(value)
}

fn check_state(state: &str) -> Result<(String, &'static str), ProviderError> {
    if state.is_empty()
        || state.len() > 64
        || !state.bytes().all(|b| b.is_ascii_alphabetic() || b == b'_')
    {
        return Err(invalid_response());
    }
    let state = state.to_ascii_uppercase();
    let bucket = match state.as_str() {
        "SUCCESS" => "pass",
        "SKIPPED" | "NEUTRAL" => "skipping",
        "ERROR" | "FAILURE" | "TIMED_OUT" | "ACTION_REQUIRED" => "fail",
        "CANCELLED" => "cancel",
        _ => "pending",
    };
    Ok((state, bucket))
}

fn check_time(value: Option<&str>) -> Result<&str, ProviderError> {
    let value = value.map_or(Ok(ZERO_TIME), timestamp)?;
    time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .map_err(|_| invalid_response())?;
    Ok(value)
}

fn keep_latest(rows: &mut std::collections::HashMap<String, Value>, key: String, row: Value) {
    let instant = |value: &Value| {
        value
            .get("_sortAt")
            .unwrap_or(&value["startedAt"])
            .as_str()
            .and_then(|text| {
                time::OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339)
                    .ok()
            })
    };
    let newer = rows
        .get(&key)
        .is_none_or(|old| instant(&row) > instant(old));
    if newer {
        rows.insert(key, row);
    }
}

fn checks_page(
    body: &[u8],
    field: &str,
    expected: &mut Option<u64>,
) -> Result<Vec<Value>, ProviderError> {
    let value = decode::<Value>(body)?;
    let rows = value[field].as_array().ok_or_else(invalid_response)?;
    let total = value["total_count"].as_u64().ok_or_else(invalid_response)?;
    if total < rows.len() as u64 || total > (crate::MAX_CLI_LIMIT * crate::MAX_LIST_REQUESTS) as u64
    {
        return Err(ProviderError::new(
            "request-ceiling",
            "checks exceed bounded collection",
        ));
    }
    if expected
        .replace(total)
        .is_some_and(|previous| previous != total)
    {
        return Err(invalid_response());
    }
    Ok(rows.clone())
}

fn checks_complete(rows: &[Value], total: Option<u64>) -> Result<(), ProviderError> {
    if total != Some(rows.len() as u64) {
        return Err(invalid_response());
    }
    Ok(())
}

pub(crate) fn status(
    input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    if input.get("comments").is_some() {
        return Err(invalid_input());
    }
    let input = serde_json::from_value::<ReadInput>(input).map_err(|_| invalid_input())?;
    validate_login(&input.owner)?;
    validate_repo(&input.repo)?;
    validate_number(input.number)?;

    let mut requests = 0;
    let result = (|| {
        let mut bounded_send = |request| {
            requests += 1;
            if requests > crate::MAX_LIST_REQUESTS {
                return Err(HttpError {
                    code: dekopon_provider_sdk::provider::HttpErrorCode::Denied,
                    message: "request ceiling".to_owned(),
                });
            }
            send(request)
        };
        let pull = fetch_pull(
            &mut bounded_send,
            base,
            &input.owner,
            &input.repo,
            input.number,
        )?;
        let owner = percent_encode(&input.owner);
        let repo = percent_encode(&input.repo);
        let head = percent_encode(&pull.head.sha);
        let path = format!("/repos/{owner}/{repo}/commits/{head}");
        let mut check_total = None;
        let check_runs = crate::collect_pages_with(
            &mut bounded_send,
            base,
            &format!("{path}/check-runs?filter=all"),
            crate::MAX_CLI_LIMIT * crate::MAX_LIST_REQUESTS,
            |body| checks_page(body, "check_runs", &mut check_total),
            crate::list_key,
            |row| Ok(Some(row)),
        )?;
        checks_complete(&check_runs, check_total)?;
        let statuses_response = send_get(
            &mut bounded_send,
            url(base, &format!("{path}/status?page=1&per_page=100"))?,
            ACCEPT_JSON,
        )?;
        if has_next_link(&statuses_response) {
            return Err(invalid_response());
        }
        let statuses = decode::<Value>(&statuses_response.body)?;
        if statuses["sha"] != pull.head.sha {
            return Err(invalid_response());
        }
        let status_rows = statuses["statuses"]
            .as_array()
            .ok_or_else(invalid_response)?;
        let status_count = statuses["total_count"]
            .as_u64()
            .ok_or_else(invalid_response)?;
        if status_count > 100 {
            return Err(ProviderError::new(
                "request-ceiling",
                "checks exceed bounded collection",
            ));
        }
        if status_count != status_rows.len() as u64 {
            return Err(invalid_response());
        }
        let mut action_total = None;
        let actions = crate::collect_pages_with(
            &mut bounded_send,
            base,
            &format!("/repos/{owner}/{repo}/actions/runs?head_sha={head}"),
            crate::MAX_CLI_LIMIT * crate::MAX_LIST_REQUESTS,
            |body| checks_page(body, "workflow_runs", &mut action_total),
            crate::list_key,
            |row| Ok(Some(row)),
        )?;
        checks_complete(&actions, action_total)?;
        let mut workflow = std::collections::HashMap::new();
        for action in actions {
            if action["head_sha"] != pull.head.sha {
                return Err(invalid_response());
            }
            if let (Some(suite), Some(name), Some(event)) = (
                action["check_suite_id"].as_u64(),
                action["name"].as_str(),
                action["event"].as_str(),
            ) {
                workflow.insert(
                    suite,
                    (
                        check_text(Some(name))?.to_owned(),
                        check_text(Some(event))?.to_owned(),
                    ),
                );
            }
        }
        let mut unique = std::collections::HashMap::new();
        for run in check_runs {
            let name = check_text(run["name"].as_str())?;
            if name.is_empty() {
                return Err(invalid_response());
            }
            let status = run["status"].as_str().ok_or_else(invalid_response)?;
            let raw_state = if status == "completed" {
                run["conclusion"].as_str().unwrap_or("pending")
            } else {
                status
            };
            let (state, bucket) = check_state(raw_state)?;
            let suite = run["check_suite"]["id"].as_u64();
            let (workflow_name, event) = suite
                .and_then(|id| workflow.get(&id))
                .map_or(("", ""), |(name, event)| (name.as_str(), event.as_str()));
            let started = check_time(run["started_at"].as_str())?;
            let completed = check_time(run["completed_at"].as_str())?;
            let row = json!({"name":name,"state":state,"bucket":bucket,"description":"",
            "workflow":workflow_name,"event":event,"link":check_text(run["details_url"].as_str())?,
            "startedAt":started,"completedAt":completed});
            let key = format!("run:{name}:{workflow_name}:{event}");
            keep_latest(&mut unique, key, row);
        }
        for status in status_rows {
            let name = check_text(status["context"].as_str())?;
            if name.is_empty() {
                return Err(invalid_response());
            }
            let (state, bucket) =
                check_state(status["state"].as_str().ok_or_else(invalid_response)?)?;
            let created = check_time(status["created_at"].as_str())?;
            let row = json!({"name":name,"state":state,"bucket":bucket,
            "description":check_text(status["description"].as_str())?,
            "workflow":"","event":"","link":check_text(status["target_url"].as_str())?,
            "startedAt":ZERO_TIME,"completedAt":ZERO_TIME,"_sortAt":created});
            keep_latest(&mut unique, format!("status:{name}"), row);
        }
        let mut checks: Vec<Value> = unique.into_values().collect();
        for row in &mut checks {
            row.as_object_mut()
                .expect("created rows are objects")
                .remove("_sortAt");
        }
        checks.sort_by(|a, b| {
            let priority = |row: &Value| if row["bucket"] == "fail" { 0 } else { 1 };
            priority(a)
                .cmp(&priority(b))
                .then_with(|| a["bucket"].as_str().cmp(&b["bucket"].as_str()))
                .then_with(|| a["name"].as_str().cmp(&b["name"].as_str()))
                .then_with(|| a["link"].as_str().cmp(&b["link"].as_str()))
        });
        Ok(json!({"checks": checks, "headRefName": pull.head.name}))
    })();
    if requests > crate::MAX_LIST_REQUESTS {
        return Err(ProviderError::new(
            "request-ceiling",
            "checks exceeded ten requests",
        ));
    }
    result
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
    fn native_file_page_host_limits_keep_the_existing_typed_failure() {
        use dekopon_provider_sdk::provider::{HttpError, HttpErrorCode};
        for code in [
            HttpErrorCode::ResponseTooLarge,
            HttpErrorCode::HostCallLimit,
        ] {
            let mut count = 0;
            let error = invoke_with(
                &capability("gh.pull-request.read"),
                json!({"owner":"octo","repo":"hello","number":7,"_cli":"files"}),
                |request| {
                    crate::testutil::assert_standard_headers(&request);
                    count += 1;
                    let path = "https://api.github.com/repos/octo/hello/pulls/7";
                    match count {
                        1 => {
                            assert_eq!(request.uri, path);
                            json_response(200, &pull_body(7, "open", false, false))
                        }
                        2 => {
                            assert_eq!(request.uri, format!("{path}/files?per_page=100"));
                            response_with_headers(
                                200,
                                &[(
                                    "link",
                                    &format!("<{path}/files?per_page=100&page=2>; rel=\"next\""),
                                )],
                                &json!([]),
                            )
                        }
                        3 => {
                            assert_eq!(request.uri, format!("{path}/files?per_page=100&page=2"));
                            Err(HttpError {
                                code,
                                message: "host-only detail".to_owned(),
                            })
                        }
                        _ => panic!("no further requests after refusal"),
                    }
                },
            )
            .expect_err("host limit fails the read");
            assert_eq!(count, 3);
            assert_eq!(error.code(), "http-failed");
            assert_eq!(error.message(), "broker HTTP request failed");
        }
    }

    #[test]
    fn native_file_cursors_change_only_the_consecutive_page() {
        let original = "https://ghe.example/api/v3/repos/octo/hello/pulls/7/files?per_page=100";
        for target in [
            format!("{original}&page=1"),
            format!("{original}&page=3"),
            format!("{original}&page=2&page=2"),
            format!("{original}&page=2&extra=yes"),
            format!("{original}&page=2#fragment"),
            original.replace("per_page=100", "per_page=50&page=2"),
            original.replace("https://ghe.example", "https://user@ghe.example") + "&page=2",
            original.replace("/api/v3/", "/api/") + "&page=2",
        ] {
            let response = response_with_headers(
                200,
                &[("link", &format!("<{target}>; rel=\"next\""))],
                &json!([]),
            );
            assert_eq!(
                super::next_file_page(&response.expect("response"), original, None, 1)
                    .expect_err("cursor refused")
                    .code(),
                "invalid-response"
            );
        }
        let target = original.replace("per_page=100", "page=2&per_page=100");
        let response = response_with_headers(
            200,
            &[("link", &format!("<{target}>; rel=\"next\""))],
            &json!([]),
        );
        assert_eq!(
            super::next_file_page(&response.expect("response"), original, None, 1)
                .expect("valid cursor"),
            Some(target)
        );
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
    fn merged_list_filters_closed_rows_in_typed_and_cli_modes() {
        for cli in [false, true] {
            let mut input = json!({"owner":"octo","repo":"hello","state":"merged"});
            if cli {
                input["_cli"] = json!("number");
                input["_cliLimit"] = json!(2);
            }
            let mut merged = pull_body(2, "closed", false, true);
            merged["merged_at"] = json!("2026-01-02T00:00:00Z");
            let uri = if cli {
                "https://api.github.com/repos/octo/hello/pulls?state=closed&page=1&per_page=100"
            } else {
                "https://api.github.com/repos/octo/hello/pulls?state=closed&page=1&per_page=20"
            };
            let output = invoke_with(
                &capability("gh.pull-request.list"),
                input,
                scripted(vec![step(
                    move |request| assert_eq!(request.uri, uri),
                    json_response(200, &json!([pull_body(1, "closed", false, false), merged])),
                )]),
            )
            .expect("merged subset");
            assert_eq!(output["pullRequests"].as_array().expect("rows").len(), 1);
            assert_eq!(output["pullRequests"][0]["number"], 2);
        }
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

    fn checks_steps(
        check_runs: Value,
        statuses: Value,
        actions: Value,
    ) -> Vec<crate::testutil::Step> {
        let head = "a".repeat(40);
        let base = "https://api.github.com/repos/octo/hello";
        let checks_uri = format!("{base}/commits/{head}/check-runs?filter=all&page=1&per_page=100");
        let statuses_uri = format!("{base}/commits/{head}/status?page=1&per_page=100");
        let actions_uri = format!("{base}/actions/runs?head_sha={head}&page=1&per_page=100");
        vec![
            step(
                move |request| assert_eq!(request.uri, format!("{base}/pulls/7")),
                json_response(200, &pull_body(7, "open", false, false)),
            ),
            step(
                move |request| assert_eq!(request.uri, checks_uri),
                json_response(200, &check_runs),
            ),
            step(
                move |request| assert_eq!(request.uri, statuses_uri),
                json_response(200, &statuses),
            ),
            step(
                move |request| assert_eq!(request.uri, actions_uri),
                json_response(200, &actions),
            ),
        ]
    }

    fn checks_input() -> Value {
        json!({"owner":"octo","repo":"hello","number":7})
    }

    fn counted_script(
        steps: Vec<crate::testutil::Step>,
        calls: std::rc::Rc<std::cell::Cell<usize>>,
    ) -> impl FnMut(crate::Request) -> Result<crate::Response, crate::HttpError> {
        let mut script = scripted(steps);
        move |request| {
            calls.set(calls.get() + 1);
            script(request)
        }
    }

    #[test]
    fn scripted_argv_checks_selects_runtime_output_and_exit_status() {
        use dekopon_provider_sdk::{
            CommandRunOutcome,
            provider::{self, Failure},
        };
        let head = "a".repeat(40);
        for json_output in [false, true] {
            for (status, conclusion, bucket, text_exit) in [
                ("completed", "failure", "fail", 1),
                ("in_progress", "", "pending", 8),
                ("completed", "success", "pass", 0),
            ] {
                let mut argv = vec!["pr", "checks", "7", "-R", "octo/hello"];
                if json_output {
                    argv.extend(["--json", "name,bucket"]);
                }
                let CommandRunOutcome::Proposed {
                    capability, input, ..
                } = provider::command::<crate::Gh>(
                    &argv.into_iter().map(str::to_owned).collect::<Vec<_>>(),
                    false,
                )
                else {
                    panic!("checks argv must propose");
                };
                assert_eq!(capability.as_str(), "gh.pull-request.status");
                let typed = serde_json::from_value::<crate::typed::PrStatusInput>(input)
                    .expect("proposed input must pass typed validation");
                let mut stdout = Vec::new();
                let mut send = scripted(checks_steps(
                    json!({"total_count":1,"check_runs":[{
                        "id":1,"name":"external-ci","status":status,
                        "conclusion": if conclusion.is_empty() { Value::Null } else { json!(conclusion) },
                        "details_url":"https://ci.example.invalid/1"}]}),
                    json!({"sha":head,"total_count":0,"statuses":[]}),
                    json!({"total_count":0,"workflow_runs":[]}),
                ));
                let result = crate::typed::PrStatus::run_with(
                    typed,
                    &crate::GITHUB_API,
                    &mut send,
                    &mut stdout,
                );
                let exit = result
                    .err()
                    .map(|error| Failure::code(&error).status().get())
                    .unwrap_or(0);
                let stdout = String::from_utf8(stdout).expect("UTF-8 output");
                if json_output {
                    assert_eq!(exit, 0, "selected JSON must not return a text verdict");
                    assert_eq!(
                        serde_json::from_str::<Value>(&stdout).expect("JSON output"),
                        json!([{"name":"external-ci","bucket":bucket}])
                    );
                } else {
                    assert_eq!(exit, text_exit, "{status}/{conclusion}");
                    assert_eq!(
                        stdout,
                        format!("external-ci\t{bucket}\t0\thttps://ci.example.invalid/1\t\n")
                    );
                }
            }
        }
    }

    #[test]
    fn native_checks_include_third_party_runs_and_latest_duplicate_statuses() {
        let head = "a".repeat(40);
        let output = invoke_with(&capability("gh.pull-request.status"), checks_input(),
            scripted(checks_steps(
                json!({"total_count":2,"check_runs":[
                    {"id": 1,"name":"third-party","status":"completed","conclusion":"failure",
                     "started_at":"2026-08-10T00:00:00Z","completed_at":"2026-08-10T00:05:00Z",
                     "details_url":"https://third.example/check/1"},
                    {"id": 2,"name":"ci","status":"completed","conclusion":"success",
                     "check_suite":{"id":42},"started_at":"2026-08-10T00:01:00Z",
                     "completed_at":"2026-08-10T00:03:00Z","details_url":"https://example.test/ci"}]}),
                json!({"sha":head,"total_count":2,"statuses":[
                    {"id":3,"state":"pending","context":"legacy","created_at":"2026-08-10T00:00:00Z"},
                    {"id":4,"state":"success","context":"legacy","created_at":"2026-08-10T00:04:00Z"}]}),
                json!({"total_count":1,"workflow_runs":[{"id":5,"check_suite_id":42,
                    "name":"Build","event":"pull_request","head_sha":head}]}),
            ))).expect("checks collection succeeds");
        let rows = output["checks"].as_array().expect("checks");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["name"], "third-party");
        assert_eq!(rows[0]["bucket"], "fail");
        assert_eq!(rows[1]["workflow"], "Build");
        assert_eq!(rows[1]["event"], "pull_request");
        assert_eq!(rows[2]["name"], "legacy");
        assert_eq!(rows[2]["state"], "SUCCESS");
        assert_eq!(rows[2]["startedAt"], super::ZERO_TIME);
    }

    #[test]
    fn native_third_party_check_only_survives_without_actions_or_legacy_statuses() {
        let head = "a".repeat(40);
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let output = invoke_with(
            &capability("gh.pull-request.status"),
            checks_input(),
            counted_script(
                checks_steps(
                    json!({"total_count":1,"check_runs":[{"id":1,"name":"external/security",
                    "status":"in_progress","started_at":"2026-08-10T00:00:00Z",
                    "details_url":"https://third.example/check/1"}]}),
                    json!({"sha":head,"state":"pending","total_count":0,"statuses":[]}),
                    json!({"total_count":0,"workflow_runs":[]}),
                ),
                std::rc::Rc::clone(&calls),
            ),
        )
        .expect("third-party-only checks succeed");
        assert_eq!(
            calls.get(),
            4,
            "pull, check runs, status and Actions are required"
        );
        assert_eq!(output["checks"].as_array().expect("checks").len(), 1);
        assert_eq!(output["checks"][0]["bucket"], "pending");
        assert_eq!(output["checks"][0]["workflow"], "");
        assert_eq!(output["checks"][0]["event"], "");
        use dekopon_provider_sdk::provider::Failure;
        let pending = crate::output::checks_verdict(&output).expect_err("pending exit");
        assert_eq!(Failure::code(&pending).status().get(), 8);
        let selected =
            crate::output::render("gh.pull-request.status", &json!({}), &output, "name,bucket")
                .expect("JSON selection succeeds without pending exit");
        assert_eq!(
            serde_json::from_str::<Value>(&selected).expect("JSON"),
            json!([{"name":"external/security","bucket":"pending"}])
        );
    }

    #[test]
    fn native_checks_refuse_incomplete_check_runs_without_success_shaped_output() {
        let head = "a".repeat(40);
        let error = invoke_with(
            &capability("gh.pull-request.status"),
            checks_input(),
            scripted(checks_steps(
                json!({"total_count":2,"check_runs":[{"id":1,"name":"ci","status":"completed",
                    "conclusion":"success","started_at":"2026-08-10T00:00:00Z"}]}),
                json!({"sha":head,"total_count":0,"statuses":[]}),
                json!({"total_count":0,"workflow_runs":[]}),
            )),
        )
        .expect_err("missing check-run page fails closed");
        assert_eq!(error.code(), "invalid-response");
    }

    #[test]
    fn native_checks_refuse_duplicate_ids_across_check_run_pages() {
        let head = "a".repeat(40);
        let first = json!({"total_count":2,"check_runs":[{"id":1,"name":"ci",
            "status":"completed","conclusion":"success","started_at":"2026-08-10T00:00:00Z"}]});
        let mut steps = checks_steps(
            first.clone(),
            json!({"sha":head,"total_count":0,"statuses":[]}),
            json!({"total_count":0,"workflow_runs":[]}),
        );
        let page2 = format!(
            "https://api.github.com/repos/octo/hello/commits/{head}/check-runs?filter=all&page=2&per_page=100"
        );
        steps[1].reply = response_with_headers(
            200,
            &[("link", &format!("<{page2}>; rel=\"next\""))],
            &first,
        );
        steps.insert(
            2,
            step(
                move |request| assert_eq!(request.uri, page2),
                json_response(200, &first),
            ),
        );
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let error = invoke_with(
            &capability("gh.pull-request.status"),
            checks_input(),
            counted_script(steps, std::rc::Rc::clone(&calls)),
        )
        .expect_err("repeated id cannot satisfy the upstream total");
        assert_eq!(error.code(), "invalid-response");
        assert_eq!(
            calls.get(),
            3,
            "no status or Actions request after incomplete check runs"
        );
    }

    #[test]
    fn native_checks_refuse_duplicate_ids_across_workflow_pages() {
        let head = "a".repeat(40);
        let first = json!({"total_count":2,"workflow_runs":[{"id":5,"check_suite_id":42,
            "name":"Build","event":"pull_request","head_sha":head}]});
        let mut steps = checks_steps(
            json!({"total_count":1,"check_runs":[{"id":1,"name":"ci","status":"completed",
                "conclusion":"success","check_suite":{"id":42},"started_at":"2026-08-10T00:00:00Z"}]}),
            json!({"sha":head,"total_count":0,"statuses":[]}),
            first.clone(),
        );
        let page2 = format!(
            "https://api.github.com/repos/octo/hello/actions/runs?head_sha={head}&page=2&per_page=100"
        );
        steps[3].reply = response_with_headers(
            200,
            &[("link", &format!("<{page2}>; rel=\"next\""))],
            &first,
        );
        steps.push(step(
            move |request| assert_eq!(request.uri, page2),
            json_response(200, &first),
        ));
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let error = invoke_with(
            &capability("gh.pull-request.status"),
            checks_input(),
            counted_script(steps, std::rc::Rc::clone(&calls)),
        )
        .expect_err("repeated workflow id cannot satisfy the upstream total");
        assert_eq!(error.code(), "invalid-response");
        assert_eq!(calls.get(), 5);
    }

    #[test]
    fn native_checks_refuse_a_total_that_changes_between_pages() {
        let head = "a".repeat(40);
        let first = json!({"total_count":3,"check_runs":[{"id":1,"name":"ci",
            "status":"completed","conclusion":"success","started_at":"2026-08-10T00:00:00Z"}]});
        let second = json!({"total_count":2,"check_runs":[{"id":2,"name":"lint",
            "status":"completed","conclusion":"success","started_at":"2026-08-10T00:01:00Z"}]});
        let mut steps = checks_steps(
            first.clone(),
            json!({"sha":head,"total_count":0,"statuses":[]}),
            json!({"total_count":0,"workflow_runs":[]}),
        );
        let page2 = format!(
            "https://api.github.com/repos/octo/hello/commits/{head}/check-runs?filter=all&page=2&per_page=100"
        );
        steps[1].reply = response_with_headers(
            200,
            &[("link", &format!("<{page2}>; rel=\"next\""))],
            &first,
        );
        steps.insert(
            2,
            step(
                move |request| assert_eq!(request.uri, page2),
                json_response(200, &second),
            ),
        );
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let error = invoke_with(
            &capability("gh.pull-request.status"),
            checks_input(),
            counted_script(steps, std::rc::Rc::clone(&calls)),
        )
        .expect_err("changing upstream total is invalid");
        assert_eq!(error.code(), "invalid-response");
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn native_check_runs_follow_valid_pages_and_keep_latest_named_attempt() {
        let head = "a".repeat(40);
        let base =
            format!("https://api.github.com/repos/octo/hello/commits/{head}/check-runs?filter=all");
        let next = format!("{base}&page=2&per_page=100");
        let status_uri = format!(
            "https://api.github.com/repos/octo/hello/commits/{head}/status?page=1&per_page=100"
        );
        let response = response_with_headers(
            200,
            &[("link", &format!("<{next}>; rel=\"next\""))],
            &json!({"total_count":2,"check_runs":[{"id":1,"name":"build","status":"completed",
                "conclusion":"failure","started_at":"2026-08-10T00:00:00Z"}]}),
        );
        let output = invoke_with(&capability("gh.pull-request.status"), checks_input(),
            scripted(vec![
                step(|request| assert_eq!(request.uri, "https://api.github.com/repos/octo/hello/pulls/7"),
                    json_response(200, &pull_body(7,"open",false,false))),
                step(move |request| assert_eq!(request.uri, format!("{base}&page=1&per_page=100")), response),
                step(move |request| assert_eq!(request.uri, next),
                    json_response(200, &json!({"total_count":2,"check_runs":[{"id":2,"name":"build",
                        "status":"completed","conclusion":"success","started_at":"2026-08-10T00:01:00Z"}]}))),
                step(move |request| assert_eq!(request.uri, status_uri),
                    json_response(200, &json!({"sha":head,"total_count":0,"statuses":[]}))),
                step(|request| assert!(request.uri.contains("/actions/runs?head_sha=")),
                    json_response(200, &json!({"total_count":0,"workflow_runs":[]}))),
            ])).expect("valid next page succeeds");
        assert_eq!(output["checks"].as_array().expect("rows").len(), 1);
        assert_eq!(output["checks"][0]["state"], "SUCCESS");
    }

    #[test]
    fn native_checks_ceiling_is_a_provider_failure_not_a_verdict() {
        let error = invoke_with(
            &capability("gh.pull-request.status"),
            checks_input(),
            scripted(vec![
                step(
                    |request| {
                        assert_eq!(
                            request.uri,
                            "https://api.github.com/repos/octo/hello/pulls/7"
                        )
                    },
                    json_response(200, &pull_body(7, "open", false, false)),
                ),
                step(
                    |request| {
                        assert!(
                            request
                                .uri
                                .ends_with("/check-runs?filter=all&page=1&per_page=100")
                        )
                    },
                    json_response(200, &json!({"total_count":1001,"check_runs":[]})),
                ),
            ]),
        )
        .expect_err("more than bounded checks must fail");
        assert_eq!(error.code(), "request-ceiling");
    }

    #[test]
    fn native_checks_empty_combined_status_is_not_a_pending_check() {
        let head = "a".repeat(40);
        let output = invoke_with(
            &capability("gh.pull-request.status"),
            checks_input(),
            scripted(checks_steps(
                json!({"total_count":0,"check_runs":[]}),
                json!({"sha":head,"state":"pending","total_count":0,"statuses":[]}),
                json!({"total_count":0,"workflow_runs":[]}),
            )),
        )
        .expect("empty collection succeeds before CLI verdict");
        assert_eq!(output["checks"], json!([]));
    }

    #[test]
    fn native_checks_reject_an_upstream_head_mismatch_without_partial_rows() {
        let error = invoke_with(
            &capability("gh.pull-request.status"),
            checks_input(),
            scripted(checks_steps(
                json!({"total_count":0,"check_runs":[]}),
                json!({"sha":"c".repeat(40),"total_count":0,"statuses":[]}),
                json!({"total_count":0,"workflow_runs":[]}),
            )),
        )
        .expect_err("wrong status head");
        assert_eq!(error.code(), "invalid-response");
    }
}
