use dekopon_provider_sdk::provider::endpoint::Base;
use dekopon_provider_sdk::provider::{HttpError, Request, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::ProviderError;
use crate::{invalid_input, invalid_response, percent_encode, validate_login, validate_repo};

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SearchInput {
    #[serde(default)]
    query: String,
    owner: String,
    repo: Option<String>,
    state: Option<String>,
    author: Option<String>,
    #[serde(rename = "_cliLimit")]
    limit: Option<usize>,
    #[serde(rename = "_cliOutput")]
    output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    order: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    search_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    include_prs: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    app: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    archived: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    assignee: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    closed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    commenter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    comments: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    r#match: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    interactions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    involves: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    visibility: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    label: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    locked: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mentions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    milestone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    no_assignee: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    no_label: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    no_milestone: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    no_project: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reactions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    team_mentions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    updated: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    draft: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    head: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    merged_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    merged: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    review: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    review_requested: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reviewed_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    checks: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    followers: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    include_forks: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    forks: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    good_first_issues: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    help_wanted_issues: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    license: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    size: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stars: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    topic: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    number_topics: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    extension: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    filename: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    author_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    author_email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    author_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    committer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    committer_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    committer_email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    committer_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    merge: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tree: Option<String>,
}

macro_rules! search_handler {
    ($name:ident, $id:expr) => {
        pub(crate) fn $name(
            input: Value,
            base: &Base,
            send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
        ) -> Result<Value, ProviderError> {
            run($id, input, base, send)
        }
    };
}
search_handler!(prs, crate::ids::SEARCH_PRS);
search_handler!(issues, crate::ids::SEARCH_ISSUES);
search_handler!(repos, crate::ids::SEARCH_REPOS);
search_handler!(code, crate::ids::SEARCH_CODE);
search_handler!(commits, crate::ids::SEARCH_COMMITS);

pub(crate) fn run(
    id: &str,
    input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let command = match id {
        crate::ids::SEARCH_PRS => "search prs",
        crate::ids::SEARCH_ISSUES => "search issues",
        crate::ids::SEARCH_REPOS => "search repos",
        crate::ids::SEARCH_CODE => "search code",
        crate::ids::SEARCH_COMMITS => "search commits",
        _ => return Err(invalid_input()),
    };
    crate::qualifiers::validate(command, &input).map_err(|_| invalid_input())?;
    for other in [
        "search prs",
        "search issues",
        "search repos",
        "search code",
        "search commits",
    ] {
        for field in crate::qualifiers::fields(other) {
            if crate::qualifiers::unsupported(command, field, &input) {
                return Err(invalid_input());
            }
        }
    }
    let extra = input.clone();
    let input: SearchInput = serde_json::from_value(input).map_err(|_| invalid_input())?;
    validate_login(&input.owner)?;
    if let Some(repo) = &input.repo {
        validate_repo(repo)?;
    }
    if let Some(author) = &input.author {
        validate_login(author)?;
    }
    if input
        .state
        .as_deref()
        .is_some_and(|s| !matches!(s, "open" | "closed"))
        || (input.query.is_empty() && !crate::qualifiers::has_values(command, &extra))
        || input.query.len() > 512
        || input.query.chars().any(char::is_control)
        || input.query.split_whitespace().any(|word| {
            let word = word.to_ascii_lowercase();
            ["repo:", "org:", "user:", "owner:"]
                .iter()
                .any(|q| word.contains(q))
                || word == "or"
        })
        || input.limit.is_some_and(|n| !(1..=100).contains(&n))
        || (input.output.is_none() && input.limit.is_some())
    {
        return Err(invalid_input());
    }
    let (endpoint, fields) = match id {
        crate::ids::SEARCH_PRS => ("issues", crate::output::SEARCH_PR),
        crate::ids::SEARCH_ISSUES => ("issues", crate::output::SEARCH_ISSUE),
        crate::ids::SEARCH_REPOS => ("repositories", crate::output::SEARCH_REPO),
        crate::ids::SEARCH_CODE => ("code", crate::output::SEARCH_CODE),
        crate::ids::SEARCH_COMMITS => ("commits", crate::output::SEARCH_COMMIT),
        _ => return Err(invalid_input()),
    };
    if (id == crate::ids::SEARCH_REPOS
        && (input.repo.is_some() || input.state.is_some() || input.author.is_some()))
        || (id == crate::ids::SEARCH_CODE && (input.state.is_some() || input.author.is_some()))
        || (id == crate::ids::SEARCH_COMMITS && input.state.is_some())
        || (matches!(id, crate::ids::SEARCH_PRS | crate::ids::SEARCH_ISSUES)
            && input.repo.is_none()
            && input.owner.is_empty())
    {
        return Err(invalid_input());
    }
    let scope = if let Some(repo) = &input.repo {
        format!("repo:{}/{}", input.owner, repo)
    } else {
        format!("user:{}", input.owner)
    };
    let mut q = if input.query.is_empty() {
        scope
    } else {
        format!("{} {}", input.query, scope)
    };
    if id == crate::ids::SEARCH_PRS {
        q.push_str(" is:pr");
    } else if id == crate::ids::SEARCH_ISSUES && input.include_prs != Some(true) {
        q.push_str(" is:issue");
    }
    if let Some(state) = &input.state {
        q.push_str(&format!(" state:{state}"));
    }
    if let Some(author) = &input.author {
        q.push_str(&format!(" author:{author}"));
    }
    for term in crate::qualifiers::terms(command, &extra) {
        q.push(' ');
        q.push_str(&term);
    }
    let mut path = format!("/search/{endpoint}?q={}", percent_encode(&q));
    if let Some(sort) = &input.sort
        && sort != "best-match"
    {
        path.push_str(&format!("&sort={}", percent_encode(sort)));
    }
    if let Some(order) = &input.order
        && input.sort.is_some()
    {
        path.push_str(&format!("&order={order}"));
    }
    if let Some(kind) = &input.search_type
        && kind != "lexical"
    {
        path.push_str(&format!("&search_type={kind}"));
    }
    let mut total = None;
    let mut received = 0_u64;
    let single_page = input
        .search_type
        .as_deref()
        .is_some_and(|kind| kind == "semantic" || kind == "hybrid");
    let mut bounded_send = |request| {
        let mut response = send(request)?;
        if single_page {
            response
                .headers
                .retain(|header| !header.name.eq_ignore_ascii_case("link"));
        }
        Ok(response)
    };
    let rows = crate::collect_pages_with(
        &mut bounded_send,
        base,
        &path,
        input.limit.unwrap_or(30),
        |body| {
            let page: Value = crate::decode(body)?;
            if page["incomplete_results"] != false {
                return Err(ProviderError::new(
                    "incomplete-results",
                    "search indexing is incomplete; results are not exhaustive",
                ));
            }
            let count = page["total_count"].as_u64().ok_or_else(invalid_response)?;
            total = Some(count);
            let items = page["items"]
                .as_array()
                .cloned()
                .ok_or_else(invalid_response)?;
            received += items.len() as u64;
            Ok(items)
        },
        |item| result_key(id, item),
        |item| project(id, fields, item),
    )?;
    if !single_page
        && rows.len() < input.limit.unwrap_or(30)
        && total.is_some_and(|count| count > received)
    {
        return Err(ProviderError::new(
            "index-ceiling",
            "GitHub search exposes only the first 1000 indexed results",
        ));
    }
    Ok(Value::Array(rows))
}

fn project(id: &str, fields: &str, item: Value) -> Result<Option<Value>, ProviderError> {
    let mut row = serde_json::Map::new();
    for field in fields.split(',') {
        let upstream = match field {
            "fullName" => "full_name",
            "defaultBranch" => "default_branch",
            "isArchived" => "archived",
            "isFork" => "fork",
            "isPrivate" => "private",
            "stargazersCount" => "stargazers_count",
            "createdAt" => "created_at",
            "updatedAt" => "updated_at",
            "pushedAt" => "pushed_at",
            "isDraft" => "draft",
            "closedAt" => "closed_at",
            _ => field,
        };
        let value = if field == "repository"
            && matches!(id, crate::ids::SEARCH_PRS | crate::ids::SEARCH_ISSUES)
        {
            let path = item["repository_url"]
                .as_str()
                .ok_or_else(invalid_response)?;
            let (_, names) = path.rsplit_once("/repos/").ok_or_else(invalid_response)?;
            let (owner, name) = names.split_once('/').ok_or_else(invalid_response)?;
            crate::validate_login(owner).map_err(|_| invalid_response())?;
            crate::validate_repo(name).map_err(|_| invalid_response())?;
            json!({"name": name, "nameWithOwner": format!("{owner}/{name}")})
        } else {
            item.get(upstream).cloned().unwrap_or(Value::Null)
        };
        let value = if matches!(field, "author" | "owner") {
            if value.is_null() {
                Value::Null
            } else {
                json!({"login": value.get("login").and_then(Value::as_str)})
            }
        } else if field == "assignees" {
            if let Some(users) = value.as_array() {
                Value::Array(
                    users
                        .iter()
                        .take(100)
                        .map(|user| json!({"login": user["login"]}))
                        .collect(),
                )
            } else {
                Value::Null
            }
        } else {
            bounded(value, 0)
        };
        row.insert(field.to_owned(), value);
    }
    if id == crate::ids::SEARCH_PRS || id == crate::ids::SEARCH_ISSUES {
        row.insert(
            "author".into(),
            if item["user"].is_null() {
                Value::Null
            } else {
                json!({"login": item["user"]["login"]})
            },
        );
    }
    Ok(Some(Value::Object(row)))
}

fn result_key(id: &str, item: &Value) -> Option<String> {
    fn text(value: &Value) -> Option<&str> {
        value.as_str().filter(|text| !text.is_empty())
    }
    match id {
        crate::ids::SEARCH_REPOS => text(&item["full_name"]).map(str::to_owned),
        crate::ids::SEARCH_PRS | crate::ids::SEARCH_ISSUES => {
            if let (Some(repo), Some(number)) =
                (text(&item["repository_url"]), item["number"].as_u64())
            {
                Some(json!([repo, number]).to_string())
            } else {
                item["id"].as_u64().map(|id| id.to_string())
            }
        }
        crate::ids::SEARCH_CODE => Some(
            json!([
                text(&item["repository"]["full_name"])?,
                text(&item["path"])?,
                text(&item["sha"])?
            ])
            .to_string(),
        ),
        crate::ids::SEARCH_COMMITS => text(&item["sha"]).map(str::to_owned),
        _ => None,
    }
}

fn bounded(value: Value, depth: usize) -> Value {
    if depth >= 4 {
        return Value::Null;
    }
    match value {
        Value::String(text) => {
            Value::String(crate::marked_text(&text, crate::MAX_PR_BODY_OUT_BYTES))
        }
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .take(100)
                .map(|item| bounded(item, depth + 1))
                .collect(),
        ),
        Value::Object(fields) => Value::Object(
            fields
                .into_iter()
                .take(100)
                .filter(|(key, _)| key.len() <= 128)
                .map(|(key, value)| (key, bounded(value, depth + 1)))
                .collect(),
        ),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use crate::testutil::{json_response, response_with_headers, scripted, step};
    use crate::{GITHUB_API, ids};
    use serde_json::json;

    #[test]
    fn native_search_collects_two_pages_and_refuses_inconsistent_index() {
        let path = "/search/code?q=fix%20repo%3Aocto%2Fhello";
        let first = format!("https://api.github.com{path}&page=1&per_page=2");
        let second = format!("https://api.github.com{path}&page=2&per_page=2");
        let link = format!("<{second}>; rel=\"next\"");
        let input =
            json!({"owner":"octo","repo":"hello","query":"fix","_cliLimit":2,"_cliOutput":"sha"});
        let mut send = scripted(vec![
            step(
                {
                    let first = first.clone();
                    move |req| assert_eq!(req.uri, first)
                },
                response_with_headers(
                    200,
                    &[("link", &link)],
                    &json!({"total_count":2,"incomplete_results":false,"items":[{"repository":{"full_name":"octo/hello"},"path":"src/a.rs","sha":"a"}]}),
                ),
            ),
            step(
                move |req| assert_eq!(req.uri, second),
                json_response(
                    200,
                    &json!({"total_count":2,"incomplete_results":false,"items":[{"repository":{"full_name":"octo/hello"},"path":"src/b.rs","sha":"b"}]}),
                ),
            ),
        ]);
        let rows =
            super::run(ids::SEARCH_CODE, input.clone(), &GITHUB_API, &mut send).expect("two pages");
        assert_eq!(rows.as_array().expect("rows").len(), 2);
        let mut send = scripted(vec![step(
            move |req| assert_eq!(req.uri, first),
            json_response(
                200,
                &json!({"total_count":1001,"incomplete_results":false,"items":[{"repository":{"full_name":"octo/hello"},"path":"src/a.rs","sha":"a"}]}),
            ),
        )]);
        let error = super::run(ids::SEARCH_CODE, input, &GITHUB_API, &mut send)
            .expect_err("index not exhausted");
        assert_eq!(error.code(), "index-ceiling");
    }

    #[test]
    fn search_prs_and_issues_derive_repository_from_rest_url() {
        for (id, kind) in [(ids::SEARCH_PRS, "pr"), (ids::SEARCH_ISSUES, "issue")] {
            let uri = format!(
                "https://api.github.com/search/issues?q=fix%20repo%3Aocto%2Fhello%20is%3A{kind}&page=1&per_page=30"
            );
            let mut send = scripted(vec![step(
                move |request| assert_eq!(request.uri, uri),
                json_response(
                    200,
                    &json!({"total_count":1,"incomplete_results":false,"items":[{
                        "repository_url":"https://api.github.com/repos/octo/hello","number":7
                    }]}),
                ),
            )]);
            let rows = super::run(
                id,
                json!({"owner":"octo","repo":"hello","query":"fix","_cliOutput":"repository"}),
                &GITHUB_API,
                &mut send,
            )
            .expect("repository projection");
            assert_eq!(
                rows[0]["repository"],
                json!({"name":"hello","nameWithOwner":"octo/hello"})
            );
        }
    }

    #[test]
    fn native_commit_search_deduplicates_sha_without_numeric_id() {
        let uri = "https://api.github.com/search/commits?q=fix%20user%3Adekopon-agents&page=1&per_page=30";
        let mut send = scripted(vec![step(
            move |request| assert_eq!(request.uri, uri),
            json_response(
                200,
                &json!({"total_count":2,"incomplete_results":false,"items":[
                    {"sha":"abc","repository":{"full_name":"dekopon-agents/dekopon"}},
                    {"sha":"abc","repository":{"full_name":"dekopon-agents/dekopon"}}
                ]}),
            ),
        )]);
        let rows = super::run(
            ids::SEARCH_COMMITS,
            json!({"owner":"dekopon-agents","query":"fix","_cliOutput":"sha"}),
            &GITHUB_API,
            &mut send,
        )
        .expect("commit result without id");
        assert_eq!(
            rows,
            json!([{"sha":"abc","author":null,"commit":null,"committer":null,"parents":null,"repository":{"full_name":"dekopon-agents/dekopon"},"url":null}])
        );
    }
}
