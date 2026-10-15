use dekopon_provider_sdk::provider::endpoint::Base;
use dekopon_provider_sdk::provider::{HttpError, Request, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::ProviderError;
use crate::{invalid_input, invalid_response, percent_encode, validate_login, validate_repo};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SearchInput {
    query: String,
    owner: String,
    repo: Option<String>,
    state: Option<String>,
    author: Option<String>,
    #[serde(rename = "_cliLimit")]
    limit: Option<usize>,
    #[serde(rename = "_cliOutput")]
    output: Option<String>,
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
        || input.query.is_empty()
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
    } else if id == crate::ids::SEARCH_REPOS {
        format!("user:{}", input.owner)
    } else {
        format!("org:{}", input.owner)
    };
    let mut q = format!("{} {}", input.query, scope);
    if id == crate::ids::SEARCH_PRS {
        q.push_str(" is:pr");
    } else if id == crate::ids::SEARCH_ISSUES {
        q.push_str(" is:issue");
    }
    if let Some(state) = &input.state {
        q.push_str(&format!(" state:{state}"));
    }
    if let Some(author) = &input.author {
        q.push_str(&format!(" author:{author}"));
    }
    let path = format!("/search/{endpoint}?q={}", percent_encode(&q));
    let mut total = None;
    let mut received = 0_u64;
    let rows = crate::collect_pages_with(
        send,
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
        |item| {
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
                let value = item.get(upstream).cloned().unwrap_or(Value::Null);
                let value = if matches!(field, "author" | "owner") {
                    json!({"login": value.get("login").and_then(Value::as_str)})
                } else {
                    bounded(value, 0)
                };
                row.insert(field.to_owned(), value);
            }
            if id == crate::ids::SEARCH_PRS || id == crate::ids::SEARCH_ISSUES {
                row.insert("author".into(), json!({"login": item["user"]["login"]}));
            }
            Ok(Some(Value::Object(row)))
        },
    )?;
    if rows.len() < input.limit.unwrap_or(30) && total.is_some_and(|count| count > received) {
        return Err(ProviderError::new(
            "index-ceiling",
            "GitHub search exposes only the first 1000 indexed results",
        ));
    }
    Ok(Value::Array(rows))
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
        let first = format!("https://api.github.com{path}&page=1&per_page=100");
        let second = format!("https://api.github.com{path}&page=2&per_page=100");
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
                    &json!({"total_count":2,"incomplete_results":false,"items":[{"id":1,"sha":"a"}]}),
                ),
            ),
            step(
                move |req| assert_eq!(req.uri, second),
                json_response(
                    200,
                    &json!({"total_count":2,"incomplete_results":false,"items":[{"id":2,"sha":"b"}]}),
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
                &json!({"total_count":1001,"incomplete_results":false,"items":[{"id":1,"sha":"a"}]}),
            ),
        )]);
        let error = super::run(ids::SEARCH_CODE, input, &GITHUB_API, &mut send)
            .expect_err("index not exhausted");
        assert_eq!(error.code(), "index-ceiling");
    }
}
