//! Issue capabilities: read, list, comment listing, and the one issue write.

use dekopon_provider_http::{HttpError, Request, Response, method};
use dekopon_provider_sdk::ProviderError;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    ACCEPT_JSON, MAX_COMMENT_OUT_BYTES, MAX_LABELS, MAX_LIST_ITEMS, MAX_PR_BODY_OUT_BYTES,
    MAX_TITLE_OUT_BYTES, RawUser, bounded_optional, decode, endpoint, github_json_request,
    has_next_link, http_failed, invalid_input, invalid_response, login_out, percent_encode,
    send_get, status_error, timestamp, truncate_text, validate_body, validate_issue_type,
    validate_label, validate_login, validate_milestone, validate_number, validate_page,
    validate_repo,
};

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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ListInput {
    owner: String,
    repo: String,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    assignee: Option<String>,
    #[serde(default)]
    labels: Option<Vec<String>>,
    #[serde(default)]
    milestone: Option<String>,
    #[serde(default)]
    mention: Option<String>,
    #[serde(rename = "type", default)]
    issue_type: Option<String>,
    #[serde(default)]
    page: Option<u32>,
    #[serde(default)]
    per_page: Option<u32>,
    #[serde(default)]
    endpoint: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PagedInput {
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
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CommentInput {
    owner: String,
    repo: String,
    number: u32,
    body: String,
    #[serde(default)]
    endpoint: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawLabel {
    name: String,
}

#[derive(Debug, Deserialize)]
struct RawIssue {
    number: u32,
    title: String,
    state: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    user: Option<RawUser>,
    #[serde(default)]
    labels: Vec<RawLabel>,
    #[serde(default)]
    comments: Option<u64>,
    #[serde(default)]
    pull_request: Option<Value>,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Deserialize)]
struct RawComment {
    id: u64,
    #[serde(default)]
    user: Option<RawUser>,
    #[serde(default)]
    body: Option<String>,
    created_at: String,
}

fn validate_state(state: Option<&str>) -> Result<&'static str, ProviderError> {
    match state {
        None | Some("open") => Ok("open"),
        Some("closed") => Ok("closed"),
        Some("all") => Ok("all"),
        Some(_) => Err(invalid_input()),
    }
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

    let uri = format!(
        "{endpoint}/repos/{}/{}/issues/{}",
        percent_encode(&input.owner),
        percent_encode(&input.repo),
        input.number,
    );
    let response = send_get(send, uri, ACCEPT_JSON)?;
    let issue = decode::<RawIssue>(&response.body)?;
    if issue.number != input.number {
        return Err(invalid_response());
    }

    let (title, _) = truncate_text(&issue.title, MAX_TITLE_OUT_BYTES);
    let (body, body_truncated) = bounded_optional(issue.body.as_deref(), MAX_PR_BODY_OUT_BYTES);
    let labels = issue
        .labels
        .iter()
        .take(MAX_LABELS)
        .map(|label| truncate_text(&label.name, MAX_TITLE_OUT_BYTES).0)
        .collect::<Vec<_>>();
    let mut output = json!({
        "number": issue.number,
        "title": title,
        "state": issue.state,
        "author": login_out(issue.user.as_ref()),
        "body": body,
        "bodyTruncated": body_truncated,
        "labels": labels,
        "comments": issue.comments,
        "isPullRequest": issue.pull_request.is_some(),
        "createdAt": timestamp(&issue.created_at)?,
        "updatedAt": timestamp(&issue.updated_at)?,
    });

    if input.comments == Some(true) {
        let (items, truncated) = fetch_and_project_comments(
            send,
            &endpoint,
            &input.owner,
            &input.repo,
            input.number,
            1,
            MAX_LIST_ITEMS as u32,
        )?;
        let map = output
            .as_object_mut()
            .expect("issue output is always a JSON object");
        // Named distinctly from the pre-existing numeric "comments" count field above (GitHub's
        // total comment count on the issue) so embedding the actual comments never clobbers it.
        map.insert("recentComments".to_owned(), Value::Array(items));
        map.insert("recentCommentsTruncated".to_owned(), Value::Bool(truncated));
    }

    Ok(output)
}

pub(crate) fn list(
    input: Value,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let input = serde_json::from_value::<ListInput>(input).map_err(|_| invalid_input())?;
    validate_login(&input.owner)?;
    validate_repo(&input.repo)?;
    let state = validate_state(input.state.as_deref())?;
    if let Some(author) = &input.author {
        validate_login(author)?;
    }
    if let Some(assignee) = &input.assignee {
        validate_login(assignee)?;
    }
    if let Some(labels) = &input.labels {
        for label in labels {
            validate_label(label)?;
        }
    }
    if let Some(milestone) = &input.milestone {
        validate_milestone(milestone)?;
    }
    if let Some(mention) = &input.mention {
        validate_login(mention)?;
    }
    if let Some(issue_type) = &input.issue_type {
        validate_issue_type(issue_type)?;
    }
    let (page, per_page) = validate_page(input.page, input.per_page)?;
    let per_page = per_page.unwrap_or(30);
    let endpoint = endpoint(input.endpoint.as_deref())?;

    let mut uri = format!(
        "{endpoint}/repos/{}/{}/issues?state={state}&page={page}&per_page={per_page}",
        percent_encode(&input.owner),
        percent_encode(&input.repo),
    );
    if let Some(author) = &input.author {
        uri.push_str(&format!("&creator={}", percent_encode(author)));
    }
    if let Some(assignee) = &input.assignee {
        uri.push_str(&format!("&assignee={}", percent_encode(assignee)));
    }
    if let Some(labels) = &input.labels {
        uri.push_str(&format!("&labels={}", percent_encode(&labels.join(","))));
    }
    if let Some(milestone) = &input.milestone {
        uri.push_str(&format!("&milestone={}", percent_encode(milestone)));
    }
    if let Some(mention) = &input.mention {
        uri.push_str(&format!("&mentioned={}", percent_encode(mention)));
    }
    if let Some(issue_type) = &input.issue_type {
        uri.push_str(&format!("&type={}", percent_encode(issue_type)));
    }
    let response = send_get(send, uri, ACCEPT_JSON)?;
    let issues = decode::<Vec<RawIssue>>(&response.body)?;
    let raw_len = issues.len();
    let has_more = has_next_link(&response) || raw_len > MAX_LIST_ITEMS;

    let items = issues
        .into_iter()
        .take(MAX_LIST_ITEMS)
        .map(|issue| {
            let (title, _) = truncate_text(&issue.title, MAX_TITLE_OUT_BYTES);
            Ok(json!({
                "number": issue.number,
                "title": title,
                "state": issue.state,
                "author": login_out(issue.user.as_ref()),
                "comments": issue.comments,
                "isPullRequest": issue.pull_request.is_some(),
                "createdAt": timestamp(&issue.created_at)?.to_owned(),
                "updatedAt": timestamp(&issue.updated_at)?.to_owned(),
            }))
        })
        .collect::<Result<Vec<_>, ProviderError>>()?;

    Ok(json!({
        "issues": items,
        "page": page,
        "hasMore": has_more,
    }))
}

/// Fetches one page of an issue's (or pull request's) comments and projects them into the
/// bounded output shape shared by `comments()` and the optional embedding in `read()`.
///
/// Returns the bounded, projected comment objects plus whether the page was truncated, either by
/// GitHub's own `Link: rel="next"` header or because a single request returned more raw rows than
/// this provider ever keeps.
fn fetch_and_project_comments(
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
    endpoint: &str,
    owner: &str,
    repo: &str,
    number: u32,
    page: u32,
    per_page: u32,
) -> Result<(Vec<Value>, bool), ProviderError> {
    let uri = format!(
        "{endpoint}/repos/{}/{}/issues/{}/comments?page={page}&per_page={per_page}",
        percent_encode(owner),
        percent_encode(repo),
        number,
    );
    let response = send_get(send, uri, ACCEPT_JSON)?;
    let has_next = has_next_link(&response);
    let comments = decode::<Vec<RawComment>>(&response.body)?;
    let raw_len = comments.len();

    let items = comments
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

    let has_more = has_next || raw_len > MAX_LIST_ITEMS;
    Ok((items, has_more))
}

pub(crate) fn comments(
    input: Value,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let input = serde_json::from_value::<PagedInput>(input).map_err(|_| invalid_input())?;
    validate_login(&input.owner)?;
    validate_repo(&input.repo)?;
    validate_number(input.number)?;
    let (page, per_page) = validate_page(input.page, input.per_page)?;
    let per_page = per_page.unwrap_or(20);
    let endpoint = endpoint(input.endpoint.as_deref())?;

    let (items, has_more) = fetch_and_project_comments(
        send,
        &endpoint,
        &input.owner,
        &input.repo,
        input.number,
        page,
        per_page,
    )?;

    Ok(json!({
        "comments": items,
        "page": page,
        "hasMore": has_more,
    }))
}

pub(crate) fn comment(
    input: Value,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let input = serde_json::from_value::<CommentInput>(input).map_err(|_| invalid_input())?;
    validate_login(&input.owner)?;
    validate_repo(&input.repo)?;
    validate_number(input.number)?;
    validate_body(&input.body)?;
    let endpoint = endpoint(input.endpoint.as_deref())?;

    let uri = format!(
        "{endpoint}/repos/{}/{}/issues/{}/comments",
        percent_encode(&input.owner),
        percent_encode(&input.repo),
        input.number,
    );
    let body = json!({"body": &input.body});
    let response =
        send(github_json_request(method::POST, uri, &body)?).map_err(|_| http_failed())?;
    if response.status != 201 {
        return Err(status_error(&response));
    }

    let comment = decode::<RawComment>(&response.body)?;
    if comment.id == 0 {
        return Err(invalid_response());
    }
    Ok(json!({
        "commentId": comment.id,
        "issueNumber": input.number,
        "author": login_out(comment.user.as_ref()),
        "createdAt": timestamp(&comment.created_at)?,
    }))
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use crate::invoke_with;
    use crate::testutil::{capability, json_response, scripted, step};

    fn issue(number: u32, pull: bool) -> Value {
        let mut value = json!({
            "number": number,
            "title": "Something is broken",
            "state": "open",
            "body": "details",
            "user": {"login": "reporter"},
            "labels": [{"name": "bug"}, {"name": "p1"}],
            "comments": 3,
            "created_at": "2026-08-01T00:00:00Z",
            "updated_at": "2026-08-02T00:00:00Z",
        });
        if pull {
            value["pull_request"] = json!({"url": "https://api.github.com/..."});
        }
        value
    }

    #[test]
    fn read_projects_labels_and_the_pull_flag() {
        let output = invoke_with(
            &capability("gh.issue.read"),
            json!({"owner": "octo", "repo": "hello", "number": 5}),
            scripted(vec![step(
                |request| {
                    assert_eq!(
                        request.uri,
                        "https://api.github.com/repos/octo/hello/issues/5"
                    );
                },
                json_response(200, &issue(5, false)),
            )]),
        )
        .expect("issue read succeeds");

        assert_eq!(output["number"], 5);
        assert_eq!(output["labels"], json!(["bug", "p1"]));
        assert_eq!(output["isPullRequest"], false);
        assert_eq!(output["author"], "reporter");
    }

    #[test]
    fn read_embeds_a_bounded_page_of_comments_when_requested() {
        let output = invoke_with(
            &capability("gh.issue.read"),
            json!({"owner": "octo", "repo": "hello", "number": 5, "comments": true}),
            scripted(vec![
                step(
                    |request| {
                        assert_eq!(
                            request.uri,
                            "https://api.github.com/repos/octo/hello/issues/5"
                        );
                    },
                    json_response(200, &issue(5, false)),
                ),
                step(
                    |request| {
                        assert_eq!(
                            request.uri,
                            format!(
                                "https://api.github.com/repos/octo/hello/issues/5/comments?page=1&per_page={}",
                                crate::MAX_LIST_ITEMS
                            )
                        );
                    },
                    json_response(
                        200,
                        &json!([
                            {"id": 11, "user": {"login": "a"}, "body": "first", "created_at": "2026-08-01T00:00:00Z"},
                        ]),
                    ),
                ),
            ]),
        )
        .expect("issue read with comments succeeds");

        assert_eq!(output["recentComments"][0]["commentId"], 11);
        assert_eq!(output["recentComments"][0]["body"], "first");
        assert_eq!(output["recentCommentsTruncated"], false);
        // The pre-existing numeric comment count must survive untouched alongside the embedded page.
        assert_eq!(output["comments"], 3);
    }

    #[test]
    fn read_skips_the_comments_call_when_not_requested() {
        let output = invoke_with(
            &capability("gh.issue.read"),
            json!({"owner": "octo", "repo": "hello", "number": 5, "comments": false}),
            scripted(vec![step(
                |request| {
                    assert_eq!(
                        request.uri,
                        "https://api.github.com/repos/octo/hello/issues/5"
                    );
                },
                json_response(200, &issue(5, false)),
            )]),
        )
        .expect("issue read succeeds");

        // The scripted harness above has exactly one step, so a second HTTP call here would have
        // panicked on "unexpected extra HTTP request" — reaching this assertion is itself part of
        // the proof that `comments: false` never triggers the embedded fetch.
        assert_eq!(output["comments"], 3);
        assert!(output.get("recentComments").is_none());
        assert!(output.get("recentCommentsTruncated").is_none());
    }

    #[test]
    fn list_flags_pull_requests_hiding_among_issues() {
        let output = invoke_with(
            &capability("gh.issue.list"),
            json!({"owner": "octo", "repo": "hello", "state": "all"}),
            scripted(vec![step(
                |request| {
                    assert_eq!(
                        request.uri,
                        "https://api.github.com/repos/octo/hello/issues?state=all&page=1&per_page=30"
                    );
                },
                json_response(200, &json!([issue(1, false), issue(2, true)])),
            )]),
        )
        .expect("issue list succeeds");

        let items = output["issues"].as_array().expect("items");
        assert_eq!(items[0]["isPullRequest"], false);
        assert_eq!(items[1]["isPullRequest"], true);
    }

    #[test]
    fn list_translates_author_to_the_creator_query_param() {
        let output = invoke_with(
            &capability("gh.issue.list"),
            json!({"owner": "octo", "repo": "hello", "author": "mona"}),
            scripted(vec![step(
                |request| {
                    assert_eq!(
                        request.uri,
                        "https://api.github.com/repos/octo/hello/issues?state=open&page=1&per_page=30&creator=mona"
                    );
                },
                json_response(200, &json!([])),
            )]),
        )
        .expect("issue list with author succeeds");

        assert_eq!(output["issues"], json!([]));
    }

    #[test]
    fn list_appends_assignee_labels_milestone_mention_and_type() {
        let output = invoke_with(
            &capability("gh.issue.list"),
            json!({
                "owner": "octo",
                "repo": "hello",
                "assignee": "hubot",
                "labels": ["bug", "p1"],
                "milestone": "*",
                "mention": "octocat",
                "type": "task",
            }),
            scripted(vec![step(
                |request| {
                    assert_eq!(
                        request.uri,
                        "https://api.github.com/repos/octo/hello/issues?state=open&page=1&per_page=30&assignee=hubot&labels=bug%2Cp1&milestone=%2A&mentioned=octocat&type=task"
                    );
                },
                json_response(200, &json!([])),
            )]),
        )
        .expect("issue list with combined filters succeeds");

        assert_eq!(output["issues"], json!([]));
    }

    #[test]
    fn list_reports_has_more_when_one_page_exceeds_the_list_cap() {
        let issues = (1..=(crate::MAX_LIST_ITEMS as u32 + 1))
            .map(|number| issue(number, false))
            .collect::<Vec<_>>();
        let output = invoke_with(
            &capability("gh.issue.list"),
            json!({"owner": "octo", "repo": "hello"}),
            scripted(vec![step(
                |request| {
                    assert_eq!(
                        request.uri,
                        "https://api.github.com/repos/octo/hello/issues?state=open&page=1&per_page=30"
                    );
                },
                // No `Link` header at all: truncation must be inferred purely from the raw count
                // exceeding MAX_LIST_ITEMS, since MAX_PER_PAGE now lets one request return more
                // raw rows than a single invocation ever keeps.
                json_response(200, &Value::Array(issues)),
            )]),
        )
        .expect("issue list succeeds");

        assert_eq!(output["hasMore"], true);
        assert_eq!(
            output["issues"].as_array().expect("items").len(),
            crate::MAX_LIST_ITEMS
        );
    }

    #[test]
    fn list_rejects_out_of_range_milestone_and_type_without_http() {
        for input in [
            json!({"owner": "octo", "repo": "hello", "milestone": "sprint-42"}),
            json!({"owner": "octo", "repo": "hello", "type": "bug\u{7}"}),
        ] {
            let error = invoke_with(&capability("gh.issue.list"), input.clone(), |_| {
                unreachable!("invalid input must not call HTTP: {input}")
            })
            .expect_err("invalid filter fails");
            assert_eq!(error.code(), "invalid-input", "{input}");
        }
    }

    #[test]
    fn comments_are_bounded_projections() {
        let output = invoke_with(
            &capability("gh.issue-comments.read"),
            json!({"owner": "octo", "repo": "hello", "number": 5, "perPage": 2}),
            scripted(vec![step(
                |request| {
                    assert_eq!(
                        request.uri,
                        "https://api.github.com/repos/octo/hello/issues/5/comments?page=1&per_page=2"
                    );
                },
                json_response(
                    200,
                    &json!([
                        {"id": 11, "user": {"login": "a"}, "body": "first", "created_at": "2026-08-01T00:00:00Z"},
                    ]),
                ),
            )]),
        )
        .expect("comments succeed");

        assert_eq!(output["comments"][0]["commentId"], 11);
        assert_eq!(output["comments"][0]["body"], "first");
    }

    #[test]
    fn comment_posts_the_body_and_projects_the_echo() {
        let output = invoke_with(
            &capability("gh.issue.comment"),
            json!({"owner": "octo", "repo": "hello", "number": 5, "body": "on it"}),
            scripted(vec![step(
                |request| {
                    assert_eq!(request.method, "POST");
                    assert_eq!(
                        request.uri,
                        "https://api.github.com/repos/octo/hello/issues/5/comments"
                    );
                    let body: Value = serde_json::from_slice(&request.body).expect("body is JSON");
                    assert_eq!(body, json!({"body": "on it"}));
                },
                json_response(
                    201,
                    &json!({"id": 77, "user": {"login": "xavier"}, "created_at": "2026-08-16T00:00:00Z"}),
                ),
            )]),
        )
        .expect("comment succeeds");

        assert_eq!(output["commentId"], 77);
        assert_eq!(output["issueNumber"], 5);
        assert_eq!(output["author"], "xavier");
    }

    #[test]
    fn comment_requires_a_nonempty_bounded_body() {
        for body in ["", &"x".repeat(crate::MAX_BODY_IN_BYTES + 1)] {
            let error = invoke_with(
                &capability("gh.issue.comment"),
                json!({"owner": "octo", "repo": "hello", "number": 5, "body": body}),
                |_| unreachable!("invalid body must not call HTTP"),
            )
            .expect_err("invalid body fails");
            assert_eq!(error.code(), "invalid-input");
        }
    }
}
