use crate::error::ProviderError;
use crate::{
    Base, HttpError, Request, Response, graphql, invalid_input, invalid_response,
    reviews::pinned_pull, validate_expected_sha, validate_login, validate_number, validate_repo,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const ENQUEUE: &str = "mutation EnqueuePullRequest($pullRequestId: ID!, $expectedHeadOid: GitObjectID!) { enqueuePullRequest(input: {pullRequestId: $pullRequestId, expectedHeadOid: $expectedHeadOid}) { mergeQueueEntry { id position } } }";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Input {
    owner: String,
    repo: String,
    number: u32,
    expected_head_sha: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Variables<'a> {
    pull_request_id: &'a str,
    expected_head_oid: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EnqueueData {
    enqueue_pull_request: EnqueuePayload,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EnqueuePayload {
    merge_queue_entry: QueueEntry,
}

#[derive(Deserialize)]
struct QueueEntry {
    id: String,
    position: u32,
}

fn valid_node_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

pub(crate) fn enqueue(
    input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let input = serde_json::from_value::<Input>(input).map_err(|_| invalid_input())?;
    validate_login(&input.owner)?;
    validate_repo(&input.repo)?;
    validate_number(input.number)?;
    validate_expected_sha(Some(&input.expected_head_sha))?;
    let pull = pinned_pull(
        send,
        base,
        &input.owner,
        &input.repo,
        input.number,
        Some(&input.expected_head_sha),
    )?;
    let node_id = pull
        .node_id
        .as_deref()
        .filter(|id| valid_node_id(id))
        .ok_or_else(invalid_response)?;
    let result: EnqueueData = graphql::execute(
        ENQUEUE,
        &Variables {
            pull_request_id: node_id,
            expected_head_oid: &pull.head.sha,
        },
        base,
        send,
    )?;
    let entry = result.enqueue_pull_request.merge_queue_entry;
    if !valid_node_id(&entry.id) {
        return Err(invalid_response());
    }
    Ok(json!({
        "number": pull.number,
        "headSha": pull.head.sha,
        "mergeQueueEntryId": entry.id,
        "position": entry.position,
        "outcome": "enqueued",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{capability, json_response, scripted, step};
    use crate::{Gh, invoke_with};
    use dekopon_provider_sdk_testkit::{HttpScript, Native};

    fn pull(state: &str) -> Value {
        json!({
            "number": 7, "node_id": "PR_queue", "title": "Queue", "state": state,
            "head": {"ref": "topic", "sha": "a".repeat(40)},
            "base": {"ref": "main", "sha": "b".repeat(40)},
            "created_at": "2026-11-23T00:00:00Z", "updated_at": "2026-11-23T00:00:00Z",
        })
    }

    fn input(head: &str) -> Value {
        json!({"owner":"octo","repo":"hello","number":7,"expectedHeadSha":head.repeat(40)})
    }

    #[test]
    fn native_enqueue_refuses_closed_or_moved_heads_before_the_mutation() {
        let base = "https://api.github.com/example";
        for (state, head, message) in [
            (
                "closed",
                "a",
                "the pull request is closed or already merged",
            ),
            (
                "open",
                "b",
                "the pull request head no longer matches expectedHeadSha",
            ),
        ] {
            let native =
                Native::<Gh>::new()
                    .settings(json!({"baseUrl":base}))
                    .http(HttpScript::new(
                        "api.github.com",
                        "GET",
                        json_response(200, &pull(state)).unwrap(),
                    ));
            let output = native.call("gh.pull-request.enqueue", &input(head).to_string());
            assert_ne!(output.status, 0);
            assert_eq!(output.stderr, format!("{message}\n"));
            let sent = native.requests();
            assert_eq!(sent.len(), 1);
            assert_eq!(sent[0].method, "GET");
            assert_eq!(sent[0].uri, format!("{base}/repos/octo/hello/pulls/7"));
            crate::testutil::assert_standard_headers(&sent[0]);
        }
    }

    #[test]
    fn native_enqueue_uses_the_observed_node_and_oid_with_one_fixed_mutation() {
        for (reply, error) in [
            (
                json!({"data":{"enqueuePullRequest":{"mergeQueueEntry":{"id":"MQE_queue","position":3}}}}),
                None,
            ),
            (
                json!({"errors":[{"message":"Permission denied"}]}),
                Some("graphql-error"),
            ),
        ] {
            let mut script = scripted(vec![
                step(
                    |request| {
                        assert_eq!(request.method, "GET");
                        assert_eq!(
                            request.uri,
                            "https://api.github.com/repos/octo/hello/pulls/7"
                        );
                    },
                    json_response(200, &pull("open")),
                ),
                step(
                    |request| {
                        assert_eq!(request.method, "POST");
                        assert_eq!(request.uri, "https://api.github.com/graphql");
                        let body: Value =
                            serde_json::from_slice(&request.body).expect("request JSON");
                        assert_eq!(
                            body,
                            json!({
                                "query": ENQUEUE,
                                "variables": {"pullRequestId": "PR_queue", "expectedHeadOid": "a".repeat(40)}
                            })
                        );
                    },
                    json_response(200, &reply),
                ),
            ]);
            let mut count = 0;
            let result = invoke_with(
                &capability("gh.pull-request.enqueue"),
                input("A"),
                |request| {
                    count += 1;
                    script(request)
                },
            );
            assert_eq!(count, 2);
            if let Some(code) = error {
                assert_eq!(result.unwrap_err().code(), code);
            } else {
                assert_eq!(
                    result.unwrap(),
                    json!({
                        "number":7,"headSha":"a".repeat(40),"mergeQueueEntryId":"MQE_queue","position":3,"outcome":"enqueued"
                    })
                );
            }
        }
    }
}
