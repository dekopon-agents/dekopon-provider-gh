use crate::error::ProviderError;
use crate::{invalid_input, invalid_response};
use dekopon_provider_sdk::provider::endpoint::Base;
use dekopon_provider_sdk::provider::{HttpError, Request, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Input {
    owner: String,
    repo: String,
    number: u32,
    body: Option<String>,
}

#[derive(Serialize)]
struct Lookup<'a> {
    owner: &'a str,
    name: &'a str,
    number: u32,
    before: Option<String>,
}

#[derive(Serialize)]
struct Edit<'a> {
    id: &'a str,
    body: &'a str,
}
#[derive(Serialize)]
struct Delete<'a> {
    id: &'a str,
}

#[derive(Deserialize)]
struct Author {
    login: String,
}
#[derive(Deserialize)]
struct Comment {
    id: String,
    author: Option<Author>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    has_previous_page: bool,
    start_cursor: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Comments {
    nodes: Vec<Comment>,
    page_info: PageInfo,
}
#[derive(Deserialize)]
struct Subject {
    id: String,
    comments: Comments,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Repository {
    issue_or_pull_request: Subject,
}
#[derive(Deserialize)]
struct Data {
    viewer: Author,
    repository: Repository,
}

const LOOKUP: &str = "query LastIssueComment($owner: String!, $name: String!, $number: Int!, $before: String) { viewer { login } repository(owner: $owner, name: $name) { issueOrPullRequest(number: $number) { ... on Issue { id comments(last: 100, before: $before) { nodes { id author { login } } pageInfo { hasPreviousPage startCursor } } } ... on PullRequest { id comments(last: 100, before: $before) { nodes { id author { login } } pageInfo { hasPreviousPage startCursor } } } } } }";
const EDIT: &str = "mutation EditIssueComment($id: ID!, $body: String!) { updateIssueComment(input: {id: $id, body: $body}) { issueComment { id url } } }";
const DELETE: &str = "mutation DeleteIssueComment($id: ID!) { deleteIssueComment(input: {id: $id}) { clientMutationId } }";
const CREATE: &str = "mutation CreateIssueComment($id: ID!, $body: String!) { addComment(input: {subjectId: $id, body: $body}) { commentEdge { node { id url } } } }";

#[derive(Clone, Copy)]
enum Action {
    Edit,
    Delete,
    EditOrCreate,
}

pub(crate) fn edit(
    input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    run(Action::Edit, input, base, send)
}
pub(crate) fn delete(
    input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    run(Action::Delete, input, base, send)
}
pub(crate) fn edit_or_create(
    input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    run(Action::EditOrCreate, input, base, send)
}

fn node_id(value: &str) -> Result<(), ProviderError> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(invalid_response());
    }
    Ok(())
}

fn run(
    action: Action,
    input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    match action {
        Action::Delete => {
            if input.get("body").is_some() {
                return Err(invalid_input());
            }
        }
        Action::Edit | Action::EditOrCreate => {}
    }
    let input: Input = serde_json::from_value(input).map_err(|_| invalid_input())?;
    crate::validate_login(&input.owner)?;
    crate::validate_repo(&input.repo)?;
    crate::validate_number(input.number)?;
    match action {
        Action::Delete => {
            if input.body.is_some() {
                return Err(invalid_input());
            }
        }
        Action::Edit | Action::EditOrCreate => {
            crate::validate_body(input.body.as_deref().ok_or_else(invalid_input)?)?
        }
    }
    let mut lookup = Lookup {
        owner: &input.owner,
        name: &input.repo,
        number: input.number,
        before: None,
    };
    let mut cursors = std::collections::HashSet::new();
    for _ in 0..10 {
        let data: Data = crate::graphql::execute(LOOKUP, &lookup, base, send)?;
        if data.viewer.login.is_empty()
            || data.viewer.login.len() > 64
            || data.viewer.login.chars().any(char::is_control)
        {
            return Err(invalid_response());
        }
        let subject = data.repository.issue_or_pull_request;
        node_id(&subject.id)?;
        if subject.comments.nodes.len() > 100 {
            return Err(invalid_response());
        }
        let comment = subject.comments.nodes.into_iter().rev().find(|comment| {
            comment
                .author
                .as_ref()
                .is_some_and(|author| author.login.eq_ignore_ascii_case(&data.viewer.login))
        });
        if let Some(comment) = comment {
            node_id(&comment.id)?;
            let result: Value = match action {
                Action::Delete => {
                    crate::graphql::execute(DELETE, &Delete { id: &comment.id }, base, send)?
                }
                Action::Edit | Action::EditOrCreate => crate::graphql::execute(
                    EDIT,
                    &Edit {
                        id: &comment.id,
                        body: input.body.as_deref().ok_or_else(invalid_input)?,
                    },
                    base,
                    send,
                )?,
            };
            return match action {
                Action::Delete => {
                    if !result["deleteIssueComment"].is_object() {
                        return Err(invalid_response());
                    }
                    Ok(json!({"commentId":comment.id,"issueNumber":input.number,"deleted":true}))
                }
                Action::Edit | Action::EditOrCreate => {
                    let node = &result["updateIssueComment"]["issueComment"];
                    if node["id"] != comment.id {
                        return Err(invalid_response());
                    }
                    Ok(json!({"commentId":comment.id,"issueNumber":input.number,"url":node["url"]}))
                }
            };
        }
        if !subject.comments.page_info.has_previous_page {
            return match action {
                Action::Edit | Action::Delete => Err(ProviderError::new(
                    "not-found",
                    "no comment by the current user was found",
                )),
                Action::EditOrCreate => {
                    let result: Value = crate::graphql::execute(
                        CREATE,
                        &Edit {
                            id: &subject.id,
                            body: input.body.as_deref().ok_or_else(invalid_input)?,
                        },
                        base,
                        send,
                    )?;
                    let node = &result["addComment"]["commentEdge"]["node"];
                    node_id(node["id"].as_str().ok_or_else(invalid_response)?)?;
                    Ok(json!({"commentId":node["id"],"issueNumber":input.number,"url":node["url"]}))
                }
            };
        }
        let cursor = subject
            .comments
            .page_info
            .start_cursor
            .ok_or_else(invalid_response)?;
        node_id(&cursor)?;
        if !cursors.insert(cursor.clone()) {
            return Err(invalid_response());
        }
        lookup.before = Some(cursor);
    }
    Err(ProviderError::new(
        "request-ceiling",
        "comment lookup exceeded ten requests",
    ))
}
