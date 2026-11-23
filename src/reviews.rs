//! Write capabilities: review submission (approve, comment, request changes) and merge.
//!
//! Every write pre-reads its pull request and pins the observed head SHA into the write body.
//! The pre-read is what makes these `conditional` rather than `non-idempotent`: a retry against
//! an unchanged head converges on the same review or merge state. A caller-supplied expected
//! head is checked at pre-read; a push between that read and review submission is not prevented.

use crate::error::ProviderError;
use crate::graphql;
use dekopon_provider_sdk::provider::endpoint::Base;
use dekopon_provider_sdk::provider::{HttpError, Request, Response, method};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::pulls::{RawPull, fetch_pull};
use crate::{
    MAX_MESSAGE_OUT_BYTES, RawUser, decode, github_json_request, http_failed, invalid_input,
    invalid_response, is_sha, login_out, percent_encode, status_error, truncate_text, url,
    validate_body, validate_expected_sha, validate_login, validate_number, validate_repo,
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ReviewInput {
    owner: String,
    repo: String,
    number: u32,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    expected_head_sha: Option<String>,
    comments: Option<Vec<LineComment>>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct LineComment {
    path: String,
    line: u32,
    #[serde(default)]
    side: Side,
    #[serde(skip_serializing_if = "Option::is_none")]
    start_line: Option<u32>,
    body: String,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "UPPERCASE")]
enum Side {
    Left,
    #[default]
    Right,
}

fn review_body(input: &ReviewInput, event: &str, sha: &str) -> Result<Value, ProviderError> {
    let mut body = json!({"event": event, "commit_id": sha});
    if let Some(text) = &input.body {
        body["body"] = json!(text);
    }
    if let Some(comments) = &input.comments {
        let items = comments.iter().map(|comment| {
            let mut item = json!({"path": comment.path, "line": comment.line, "side": comment.side, "body": comment.body});
            if let Some(start) = comment.start_line {
                item["start_line"] = json!(start);
                item["start_side"] = json!(comment.side);
            }
            item
        }).collect::<Vec<_>>();
        body["comments"] = json!(items);
        if serde_json::to_vec(&body)
            .map_err(|_| invalid_input())?
            .len()
            > 16384
        {
            return Err(ProviderError::new(
                "request-too-large",
                "the review request would exceed 16384 bytes",
            ));
        }
    }
    Ok(body)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct MergeInput {
    owner: String,
    repo: String,
    number: u32,
    merge_method: MergeMethod,
    body: Option<String>,
    subject: Option<String>,
    #[serde(default)]
    expected_head_sha: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

impl MergeMethod {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::Squash => "squash",
            Self::Rebase => "rebase",
        }
    }
}

/// One review event, fixed per capability so policy separates the three authority levels.
#[derive(Clone, Copy)]
enum ReviewEvent {
    Approve,
    Comment,
    RequestChanges,
}

impl ReviewEvent {
    fn body_required(self) -> bool {
        !matches!(self, Self::Approve)
    }
}

#[derive(Debug, Deserialize)]
struct RawReview {
    id: u64,
    state: String,
    commit_id: String,
    #[serde(default)]
    user: Option<RawUser>,
    #[serde(default)]
    submitted_at: Option<String>,
}

const PULL_FOR_APPROVAL: &str = "query PullForApproval($owner: String!, $name: String!, $number: Int!) { repository(owner: $owner, name: $name) { pullRequest(number: $number) { id number isDraft state headRefOid } } }";
const APPROVE_PULL_REQUEST: &str = "mutation ApprovePullRequest($pullRequestId: ID!, $commitOID: GitObjectID!, $body: String) { addPullRequestReview(input: {pullRequestId: $pullRequestId, commitOID: $commitOID, event: APPROVE, body: $body}) { pullRequestReview { fullDatabaseId state commit { oid } author { login } submittedAt } } }";

#[derive(Serialize)]
struct ApprovalLookup<'a> {
    owner: &'a str,
    name: &'a str,
    number: u32,
}

#[derive(Deserialize)]
struct ApprovalData {
    repository: ApprovalRepository,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApprovalRepository {
    pull_request: ApprovalPull,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApprovalPull {
    id: String,
    number: u32,
    is_draft: bool,
    state: String,
    head_ref_oid: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ApprovalMutation<'a> {
    pull_request_id: &'a str,
    #[serde(rename = "commitOID")]
    commit_oid: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<&'a str>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApprovalResult {
    add_pull_request_review: ApprovalPayload,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApprovalPayload {
    pull_request_review: ApprovalReview,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApprovalReview {
    full_database_id: String,
    state: String,
    commit: ReviewCommit,
    author: Option<RawUser>,
    submitted_at: Option<String>,
}

#[derive(Deserialize)]
struct ReviewCommit {
    oid: String,
}

#[derive(Debug, Deserialize)]
struct RawMerge {
    sha: String,
    merged: bool,
    #[serde(default)]
    message: Option<String>,
}

pub(crate) fn approve(
    input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    submit_review(input, base, send, ReviewEvent::Approve)
}

pub(crate) fn comment(
    input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    submit_review(input, base, send, ReviewEvent::Comment)
}

pub(crate) fn request_changes(
    input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    submit_review(input, base, send, ReviewEvent::RequestChanges)
}

fn submit_review(
    input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
    event: ReviewEvent,
) -> Result<Value, ProviderError> {
    if input.get("comments").is_some() {
        match event {
            ReviewEvent::Comment => {}
            ReviewEvent::Approve | ReviewEvent::RequestChanges => return Err(invalid_input()),
        }
    }
    let input = serde_json::from_value::<ReviewInput>(input).map_err(|_| invalid_input())?;
    validate_login(&input.owner)?;
    validate_repo(&input.repo)?;
    validate_number(input.number)?;
    validate_expected_sha(input.expected_head_sha.as_deref())?;
    if let Some(comments) = &input.comments {
        match event {
            ReviewEvent::Comment => {}
            ReviewEvent::Approve | ReviewEvent::RequestChanges => return Err(invalid_input()),
        }
        if !(1..=50).contains(&comments.len()) {
            return Err(invalid_input());
        }
        for comment in comments {
            crate::validate_path(&comment.path)?;
            if comment.path.is_empty()
                || comment.line == 0
                || comment
                    .start_line
                    .is_some_and(|start| start == 0 || start >= comment.line)
            {
                return Err(invalid_input());
            }
            validate_body(&comment.body)?;
        }
    }
    match input.body.as_deref() {
        Some(body) => validate_body(body)?,
        None if event.body_required() && input.comments.is_none() => return Err(invalid_input()),
        None => {}
    }

    let (wire_event, expected_state) = match event {
        ReviewEvent::Approve => return submit_approval(&input, base, send),
        ReviewEvent::Comment => ("COMMENT", "COMMENTED"),
        ReviewEvent::RequestChanges => ("REQUEST_CHANGES", "CHANGES_REQUESTED"),
    };
    if input.comments.is_some() {
        review_body(&input, wire_event, &"0".repeat(40))?;
    }

    let pull = fetch_pull(send, base, &input.owner, &input.repo, input.number)?;
    require_open(&pull)?;
    require_expected_head(&pull.head.sha, input.expected_head_sha.as_deref())?;

    let body = review_body(&input, wire_event, &pull.head.sha)?;
    let uri = url(
        base,
        &format!(
            "/repos/{}/{}/pulls/{}/reviews",
            percent_encode(&input.owner),
            percent_encode(&input.repo),
            input.number,
        ),
    )?;
    let response =
        send(github_json_request(method::POST, uri, &body)?).map_err(|_| http_failed())?;
    if response.status != 200 {
        return Err(status_error(&response));
    }

    review_output(
        decode(&response.body)?,
        pull.number,
        &pull.head.sha,
        expected_state,
    )
}

fn submit_approval(
    input: &ReviewInput,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let data: ApprovalData = graphql::execute(
        PULL_FOR_APPROVAL,
        &ApprovalLookup {
            owner: &input.owner,
            name: &input.repo,
            number: input.number,
        },
        base,
        send,
    )?;
    let pull = data.repository.pull_request;
    if pull.number != input.number
        || !is_sha(&pull.head_ref_oid)
        || pull.id.is_empty()
        || pull.id.len() > 256
        || pull.id.chars().any(char::is_control)
    {
        return Err(invalid_response());
    }
    if pull.state != "OPEN" {
        return Err(closed_pull());
    }
    if pull.is_draft {
        return Err(ProviderError::new(
            "pr-draft",
            "the pull request is a draft; approving a draft is refused",
        ));
    }
    require_expected_head(&pull.head_ref_oid, input.expected_head_sha.as_deref())?;

    let data: ApprovalResult = graphql::execute(
        APPROVE_PULL_REQUEST,
        &ApprovalMutation {
            pull_request_id: &pull.id,
            commit_oid: &pull.head_ref_oid,
            body: input.body.as_deref(),
        },
        base,
        send,
    )?;
    let review = data.add_pull_request_review.pull_request_review;
    review_output(
        RawReview {
            id: review
                .full_database_id
                .parse()
                .map_err(|_| invalid_response())?,
            state: review.state,
            commit_id: review.commit.oid,
            user: review.author,
            submitted_at: review.submitted_at,
        },
        pull.number,
        &pull.head_ref_oid,
        "APPROVED",
    )
}

fn review_output(
    review: RawReview,
    pull_number: u32,
    head_sha: &str,
    expected_state: &str,
) -> Result<Value, ProviderError> {
    if review.state != expected_state || !review.commit_id.eq_ignore_ascii_case(head_sha) {
        return Err(invalid_response());
    }
    Ok(json!({
        "reviewId": review.id,
        "state": review.state,
        "pullNumber": pull_number,
        "headSha": head_sha,
        "author": login_out(review.user.as_ref()),
        "submittedAt": review.submitted_at,
    }))
}

pub(crate) fn merge(
    input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let input = serde_json::from_value::<MergeInput>(input).map_err(|_| invalid_input())?;
    validate_login(&input.owner)?;
    validate_repo(&input.repo)?;
    validate_number(input.number)?;
    validate_expected_sha(input.expected_head_sha.as_deref())?;
    if let Some(body) = &input.body {
        validate_body(body)?;
    }
    if let Some(subject) = &input.subject {
        validate_body(subject)?;
    }
    let pull = pinned_pull(
        send,
        base,
        &input.owner,
        &input.repo,
        input.number,
        input.expected_head_sha.as_deref(),
    )?;

    let uri = url(
        base,
        &format!(
            "/repos/{}/{}/pulls/{}/merge",
            percent_encode(&input.owner),
            percent_encode(&input.repo),
            input.number,
        ),
    )?;
    let mut body = json!({
        "sha": pull.head.sha,
        "merge_method": input.merge_method.as_str(),
    });
    if let Some(text) = &input.body {
        body["commit_message"] = json!(text);
    }
    if let Some(text) = &input.subject {
        body["commit_title"] = json!(text);
    }
    let response =
        send(github_json_request(method::PUT, uri, &body)?).map_err(|_| http_failed())?;
    // GitHub refuses a blocked or conflicting merge with 405, and a stale head with 409. The
    // sha pin makes 409 reachable only in the race window between pre-read and merge.
    if matches!(response.status, 405 | 409) {
        return Err(ProviderError::new(
            "merge-conflict",
            "the merge was refused as conflicting or blocked",
        ));
    }
    if response.status != 200 {
        return Err(status_error(&response));
    }

    let merged = decode::<RawMerge>(&response.body)?;
    if !merged.merged {
        return Err(invalid_response());
    }
    let (message, _) = truncate_text(
        merged.message.as_deref().unwrap_or(""),
        MAX_MESSAGE_OUT_BYTES,
    );

    Ok(json!({
        "merged": true,
        "sha": merged.sha,
        "pullNumber": pull.number,
        "headSha": pull.head.sha,
        "message": message,
    }))
}

pub(crate) fn pinned_pull(
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
    base: &Base,
    owner: &str,
    repo: &str,
    number: u32,
    expected: Option<&str>,
) -> Result<RawPull, ProviderError> {
    let pull = fetch_pull(send, base, owner, repo, number)?;
    require_open(&pull)?;
    require_expected_head(&pull.head.sha, expected)?;
    Ok(pull)
}

/// Refuses closed or merged pull requests before any write is constructed.
fn require_open(pull: &RawPull) -> Result<(), ProviderError> {
    if pull.merged.unwrap_or(false) || pull.state != "open" {
        return Err(closed_pull());
    }
    Ok(())
}

fn closed_pull() -> ProviderError {
    ProviderError::new("pr-closed", "the pull request is closed or already merged")
}

/// Refuses a head that moved past the caller's expectation.
fn require_expected_head(head_sha: &str, expected: Option<&str>) -> Result<(), ProviderError> {
    if let Some(expected) = expected
        && !head_sha.eq_ignore_ascii_case(expected)
    {
        return Err(ProviderError::new(
            "head-changed",
            "the pull request head no longer matches expectedHeadSha",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use crate::invoke_with;
    use crate::testutil::{capability, json_response, scripted, step};

    fn pull(state: &str, draft: bool, merged: bool) -> Value {
        json!({
            "number": 7,
            "title": "A change",
            "state": state,
            "draft": draft,
            "merged": merged,
            "user": {"login": "cpetersen"},
            "head": {"ref": "feature/x", "sha": "a".repeat(40)},
            "base": {"ref": "main", "sha": "b".repeat(40)},
            "created_at": "2026-08-01T00:00:00Z",
            "updated_at": "2026-08-02T00:00:00Z",
        })
    }

    fn review_echo(state: &str) -> Value {
        json!({
            "id": 4242,
            "state": state,
            "commit_id": "a".repeat(40),
            "user": {"login": "xavier"},
            "submitted_at": "2026-08-16T00:00:00Z",
        })
    }

    fn approval_pull(state: &str, draft: bool) -> Value {
        json!({"data":{"repository":{"pullRequest":{
            "id": "PR_fixture",
            "number": 7,
            "state": state,
            "isDraft": draft,
            "headRefOid": "a".repeat(40)
        }}}})
    }

    fn approval_echo(state: &str, commit: &str) -> Value {
        json!({"data":{"addPullRequestReview":{"pullRequestReview":{
            "fullDatabaseId": "4242424242",
            "state": state,
            "commit": {"oid": commit},
            "author": {"login":"review-bot"},
            "submittedAt": "2026-08-16T00:00:00Z"
        }}}})
    }

    fn approval_request(request: &dekopon_provider_sdk::provider::Request) -> Value {
        assert_eq!(request.method, "POST");
        assert_eq!(request.uri, "https://api.github.com/graphql");
        assert!(
            !request
                .headers
                .iter()
                .any(|header| header.name.eq_ignore_ascii_case("authorization"))
        );
        serde_json::from_slice(&request.body).expect("GraphQL request body")
    }

    fn assert_approval_lookup(request: &dekopon_provider_sdk::provider::Request) {
        let body = approval_request(request);
        assert_eq!(body["query"], super::PULL_FOR_APPROVAL);
        assert_eq!(
            body["variables"],
            json!({"owner":"octo","name":"hello","number":7})
        );
    }

    fn assert_approval_mutation(
        request: &dekopon_provider_sdk::provider::Request,
        body: Option<&str>,
    ) {
        let actual = approval_request(request);
        assert_eq!(actual["query"], super::APPROVE_PULL_REQUEST);
        let mut variables = json!({"pullRequestId":"PR_fixture","commitOID":"a".repeat(40)});
        if let Some(body) = body {
            variables["body"] = json!(body);
        }
        assert_eq!(actual["variables"], variables);
    }

    #[test]
    fn native_approve_pins_the_observed_head_in_one_graphql_mutation_after_the_query() {
        let output = invoke_with(
            &capability("gh.pull-request.approve"),
            json!({"owner":"octo","repo":"hello","number":7,"body":"lgtm"}),
            scripted(vec![
                step(
                    assert_approval_lookup,
                    json_response(200, &approval_pull("OPEN", false)),
                ),
                step(
                    |request| assert_approval_mutation(request, Some("lgtm")),
                    json_response(200, &approval_echo("APPROVED", &"a".repeat(40))),
                ),
            ]),
        )
        .expect("approve succeeds");

        assert_eq!(
            output,
            json!({
                "reviewId": 4242424242_u64,
                "state": "APPROVED",
                "pullNumber": 7,
                "headSha": "a".repeat(40),
                "author": "review-bot",
                "submittedAt": "2026-08-16T00:00:00Z"
            })
        );
    }

    #[test]
    fn native_approve_refuses_closed_or_merged_pulls_before_the_mutation() {
        for state in ["CLOSED", "MERGED"] {
            let error = invoke_with(
                &capability("gh.pull-request.approve"),
                json!({"owner":"octo","repo":"hello","number":7}),
                scripted(vec![step(
                    assert_approval_lookup,
                    json_response(200, &approval_pull(state, false)),
                )]),
            )
            .expect_err("closed pull refuses");
            assert_eq!(error.code(), "pr-closed");
        }
    }

    #[test]
    fn native_approve_refuses_drafts_before_the_mutation() {
        let error = invoke_with(
            &capability("gh.pull-request.approve"),
            json!({"owner":"octo","repo":"hello","number":7}),
            scripted(vec![step(
                assert_approval_lookup,
                json_response(200, &approval_pull("OPEN", true)),
            )]),
        )
        .expect_err("draft refuses");
        assert_eq!(error.code(), "pr-draft");
    }

    #[test]
    fn native_approve_refuses_a_moved_head_before_the_mutation() {
        let error = invoke_with(
            &capability("gh.pull-request.approve"),
            json!({"owner":"octo","repo":"hello","number":7,"expectedHeadSha":"c".repeat(40)}),
            scripted(vec![step(
                assert_approval_lookup,
                json_response(200, &approval_pull("OPEN", false)),
            )]),
        )
        .expect_err("moved head refuses");
        assert_eq!(error.code(), "head-changed");
    }

    #[test]
    fn native_approve_rejects_an_echo_with_the_wrong_commit_or_state() {
        for (state, commit) in [("APPROVED", "d".repeat(40)), ("COMMENTED", "a".repeat(40))] {
            let error = invoke_with(
                &capability("gh.pull-request.approve"),
                json!({"owner":"octo","repo":"hello","number":7}),
                scripted(vec![
                    step(
                        assert_approval_lookup,
                        json_response(200, &approval_pull("OPEN", false)),
                    ),
                    step(
                        |request| assert_approval_mutation(request, None),
                        json_response(200, &approval_echo(state, &commit)),
                    ),
                ]),
            )
            .expect_err("contradictory echo fails");
            assert_eq!(error.code(), "invalid-response");
        }
    }

    #[test]
    fn native_approval_mutation_errors_keep_graphql_error_without_another_request() {
        for kind in ["FORBIDDEN", "RATE_LIMITED"] {
            let mut response = approval_echo("APPROVED", &"a".repeat(40));
            response["errors"] = json!([{"type":kind,"message":"untrusted error text"}]);
            let error = invoke_with(
                &capability("gh.pull-request.approve"),
                json!({"owner":"octo","repo":"hello","number":7}),
                scripted(vec![
                    step(
                        assert_approval_lookup,
                        json_response(200, &approval_pull("OPEN", false)),
                    ),
                    step(
                        |request| assert_approval_mutation(request, None),
                        json_response(200, &response),
                    ),
                ]),
            )
            .expect_err("GraphQL refusal fails");
            assert_eq!(error.code(), "graphql-error");
            assert_eq!(error.message(), "GitHub GraphQL reported an error");
        }
    }

    #[test]
    fn comment_and_request_changes_require_a_body_and_use_their_event() {
        for (capability_id, event, state) in [
            ("gh.pull-request.comment", "COMMENT", "COMMENTED"),
            (
                "gh.pull-request.request-changes",
                "REQUEST_CHANGES",
                "CHANGES_REQUESTED",
            ),
        ] {
            let error = invoke_with(
                &capability(capability_id),
                json!({"owner": "octo", "repo": "hello", "number": 7}),
                |_| unreachable!("missing body must not call HTTP"),
            )
            .expect_err("missing body fails");
            assert_eq!(error.code(), "invalid-input", "{capability_id}");

            let output = invoke_with(
                &capability(capability_id),
                json!({"owner": "octo", "repo": "hello", "number": 7, "body": "needs work"}),
                scripted(vec![
                    step(|_| {}, json_response(200, &pull("open", false, false))),
                    step(
                        move |request| {
                            let body: Value =
                                serde_json::from_slice(&request.body).expect("body is JSON");
                            assert_eq!(body["event"], event);
                        },
                        json_response(200, &review_echo(state)),
                    ),
                ]),
            )
            .expect("review succeeds");
            assert_eq!(output["state"], state, "{capability_id}");
        }
    }

    #[test]
    fn comment_reviews_are_allowed_on_drafts() {
        // Only approval refuses drafts; a COMMENT review of a draft is ordinary feedback.
        invoke_with(
            &capability("gh.pull-request.comment"),
            json!({"owner": "octo", "repo": "hello", "number": 7, "body": "early note"}),
            scripted(vec![
                step(|_| {}, json_response(200, &pull("open", true, false))),
                step(|_| {}, json_response(200, &review_echo("COMMENTED"))),
            ]),
        )
        .expect("draft comment succeeds");
    }

    #[test]
    fn merge_without_a_method_refuses_before_http() {
        let error = invoke_with(
            &capability("gh.pull-request.merge"),
            json!({"owner": "octo", "repo": "hello", "number": 7}),
            |_| unreachable!("missing merge method cannot call HTTP"),
        )
        .expect_err("method required");
        assert_eq!(error.code(), "invalid-input");
    }

    #[test]
    fn merge_pins_the_sha_and_maps_conflicts() {
        let output = invoke_with(
            &capability("gh.pull-request.merge"),
            json!({"owner": "octo", "repo": "hello", "number": 7, "mergeMethod": "squash"}),
            scripted(vec![
                step(|_| {}, json_response(200, &pull("open", false, false))),
                step(
                    |request| {
                        assert_eq!(request.method, "PUT");
                        assert_eq!(
                            request.uri,
                            "https://api.github.com/repos/octo/hello/pulls/7/merge"
                        );
                        let body: Value =
                            serde_json::from_slice(&request.body).expect("body is JSON");
                        assert_eq!(body["sha"], "a".repeat(40));
                        assert_eq!(body["merge_method"], "squash");
                    },
                    json_response(
                        200,
                        &json!({"sha": "e".repeat(40), "merged": true, "message": "Pull Request successfully merged"}),
                    ),
                ),
            ]),
        )
        .expect("merge succeeds");
        assert_eq!(output["merged"], true);
        assert_eq!(output["sha"], "e".repeat(40));

        for conflict_status in [405_u16, 409] {
            let error = invoke_with(
                &capability("gh.pull-request.merge"),
                json!({"owner": "octo", "repo": "hello", "number": 7, "mergeMethod": "merge"}),
                scripted(vec![
                    step(|_| {}, json_response(200, &pull("open", false, false))),
                    step(|_| {}, json_response(conflict_status, &json!({}))),
                ]),
            )
            .expect_err("conflict fails");
            assert_eq!(error.code(), "merge-conflict", "status {conflict_status}");
        }
    }

    #[test]
    fn merge_refuses_closed_and_moved_heads_before_any_write() {
        let error = invoke_with(
            &capability("gh.pull-request.merge"),
            json!({"owner": "octo", "repo": "hello", "number": 7, "mergeMethod": "merge"}),
            scripted(vec![step(
                |_| {},
                json_response(200, &pull("closed", false, false)),
            )]),
        )
        .expect_err("closed refuses");
        assert_eq!(error.code(), "pr-closed");

        let error = invoke_with(
            &capability("gh.pull-request.merge"),
            json!({
                "owner": "octo",
                "repo": "hello",
                "number": 7,
                "expectedHeadSha": "f".repeat(40),
                "mergeMethod": "merge",
            }),
            scripted(vec![step(
                |_| {},
                json_response(200, &pull("open", false, false)),
            )]),
        )
        .expect_err("moved head refuses");
        assert_eq!(error.code(), "head-changed");
    }
    #[test]
    fn native_bad_line_items_keep_the_invalid_input_code_without_http() {
        let error = invoke_with(&capability("gh.pull-request.comment"), json!({"owner":"octo","repo":"hello","number":7,"comments":[{"path":"../lib.rs","line":1,"body":"review"}]}), |_| panic!("invalid line items cannot send HTTP")).expect_err("invalid item");
        assert_eq!(error.code(), "invalid-input");
    }
}
