//! Repository, branch, commit, and user read capabilities.

use crate::error::ProviderError;
use dekopon_provider_sdk::provider::endpoint::Base;
use dekopon_provider_sdk::provider::{HttpError, Request, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    ACCEPT_JSON, MAX_DESCRIPTION_OUT_BYTES, RawUser, bounded_optional, decode, encode_path,
    invalid_input, invalid_response, is_sha, login_out, marked_optional, percent_encode, send_get,
    timestamp, url, validate_login, validate_ref, validate_repo,
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RepoInput {
    owner: String,
    repo: String,
    #[serde(default, rename = "_cli")]
    cli: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RepoListInput {
    owner: String,
    #[serde(default, rename = "_cli")]
    cli: Option<String>,
    #[serde(default, rename = "_cliLimit")]
    cli_limit: Option<usize>,
    source: Option<bool>,
    fork: Option<bool>,
    language: Option<String>,
    topic: Option<Vec<String>>,
    visibility: Option<String>,
    archived: Option<bool>,
    no_archived: Option<bool>,
    private: Option<bool>,
    public: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct BranchInput {
    owner: String,
    repo: String,
    branch: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CommitInput {
    owner: String,
    repo: String,
    #[serde(rename = "ref")]
    reference: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct UserInput {
    login: String,
}

#[derive(Debug, Deserialize)]
struct RawRepo {
    name: String,
    #[serde(default)]
    owner: Option<RawUser>,
    private: bool,
    #[serde(default)]
    visibility: Option<String>,
    default_branch: String,
    fork: bool,
    #[serde(default)]
    archived: bool,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    pushed_at: Option<String>,
    updated_at: String,
}

pub(crate) fn repo(
    input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let input = serde_json::from_value::<RepoInput>(input).map_err(|_| invalid_input())?;
    validate_login(&input.owner)?;
    validate_repo(&input.repo)?;

    let uri = url(
        base,
        &format!(
            "/repos/{}/{}",
            percent_encode(&input.owner),
            percent_encode(&input.repo),
        ),
    )?;
    let response = send_get(send, uri, ACCEPT_JSON)?;
    let raw = decode::<Value>(&response.body)?;
    let repo = serde_json::from_value::<RawRepo>(raw.clone()).map_err(|_| invalid_response())?;
    // The name echo binds the response to the request without being case-brittle: GitHub
    // canonicalizes repository casing.
    if !repo.name.eq_ignore_ascii_case(&input.repo) {
        return Err(invalid_response());
    }

    let (description, description_truncated) =
        bounded_optional(repo.description.as_deref(), MAX_DESCRIPTION_OUT_BYTES);
    if !input.cli {
        return Ok(json!({
            "name": repo.name,
            "owner": login_out(repo.owner.as_ref()),
            "private": repo.private,
            "visibility": repo.visibility,
            "defaultBranch": repo.default_branch,
            "fork": repo.fork,
            "archived": repo.archived,
            "description": description,
            "descriptionTruncated": description_truncated,
            "pushedAt": repo.pushed_at,
            "updatedAt": timestamp(&repo.updated_at)?,
        }));
    }
    let owner = login_out(repo.owner.as_ref());
    let full_name = raw["full_name"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{}/{}", owner.as_str().unwrap_or(&input.owner), repo.name));
    Ok(json!({
        "name": repo.name,
        "owner": owner,
        "nameWithOwner": full_name,
        "url": raw["html_url"],
        "createdAt": raw["created_at"],
        "isPrivate": repo.private,
        "isFork": repo.fork,
        "isArchived": repo.archived,
        "defaultBranchRef": {"name": repo.default_branch},
        "private": repo.private,
        "visibility": repo.visibility,
        "fork": repo.fork,
        "archived": repo.archived,
        "defaultBranch": repo.default_branch,
        "description": marked_optional(repo.description.as_deref(), MAX_DESCRIPTION_OUT_BYTES),
        "descriptionTruncated": description_truncated,
        "pushedAt": repo.pushed_at,
        "updatedAt": timestamp(&repo.updated_at)?,
    }))
}

pub(crate) fn list(
    input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let input = serde_json::from_value::<RepoListInput>(input).map_err(|_| invalid_input())?;
    validate_login(&input.owner)?;
    if input.cli.is_none() && input.cli_limit.is_some() {
        return Err(invalid_input());
    }
    if (input.source == Some(true) && input.fork == Some(true))
        || (input.archived == Some(true) && input.no_archived == Some(true))
        || (input.private == Some(true) && input.public == Some(true))
        || (input.visibility.is_some()
            && (input.private == Some(true) || input.public == Some(true)))
        || input
            .visibility
            .as_deref()
            .is_some_and(|v| !["public", "private", "internal"].contains(&v))
    {
        return Err(invalid_input());
    }
    for value in input.language.iter().chain(input.topic.iter().flatten()) {
        crate::qualifiers::free_text(value)?;
    }
    let visibility = input
        .visibility
        .as_deref()
        .or(if input.private == Some(true) {
            Some("private")
        } else if input.public == Some(true) {
            Some("public")
        } else {
            None
        });
    let path = format!("/users/{}/repos?type=owner", percent_encode(&input.owner));
    let projection = crate::Projection::drops_rows_when(
        input.source == Some(true)
            || input.fork == Some(true)
            || input.archived == Some(true)
            || input.no_archived == Some(true)
            || visibility.is_some()
            || input.language.is_some()
            || input.topic.is_some(),
    );
    let rows = crate::collect_pages(
        send,
        base,
        &path,
        input.cli_limit.unwrap_or(30),
        projection,
        |raw| {
            let repo =
                serde_json::from_value::<RawRepo>(raw.clone()).map_err(|_| invalid_response())?;
            validate_repo(&repo.name).map_err(|_| invalid_response())?;
            if !raw["owner"]["login"]
                .as_str()
                .is_some_and(|login| login.eq_ignore_ascii_case(&input.owner))
                || !raw["full_name"].as_str().is_some_and(|name| {
                    name.split_once('/').is_some_and(|(owner, name)| {
                        owner.eq_ignore_ascii_case(&input.owner)
                            && name.eq_ignore_ascii_case(&repo.name)
                    })
                })
            {
                return Err(invalid_response());
            }
            if (input.source == Some(true) && repo.fork)
                || (input.fork == Some(true) && !repo.fork)
                || (input.archived == Some(true) && !repo.archived)
                || (input.no_archived == Some(true) && repo.archived)
                || visibility.is_some_and(|visibility| {
                    repo.visibility.as_deref().unwrap_or(if repo.private {
                        "private"
                    } else {
                        "public"
                    }) != visibility
                })
                || input.language.as_deref().is_some_and(|language| {
                    !raw["language"]
                        .as_str()
                        .is_some_and(|value| value.eq_ignore_ascii_case(language))
                })
                || input.topic.as_ref().is_some_and(|topics| {
                    topics.iter().any(|topic| {
                        !raw["topics"].as_array().is_some_and(|values| {
                            values.iter().any(|value| {
                                value
                                    .as_str()
                                    .is_some_and(|value| value.eq_ignore_ascii_case(topic))
                            })
                        })
                    })
                })
            {
                return Ok(None);
            }
            Ok(Some(json!({
                "name": repo.name, "owner": raw["owner"]["login"],
                "nameWithOwner": raw["full_name"], "url": raw["html_url"],
                "createdAt": raw["created_at"], "isPrivate": repo.private,
                "isFork": repo.fork, "isArchived": repo.archived,
                "defaultBranchRef": {"name": repo.default_branch},
                "description": marked_optional(repo.description.as_deref(), MAX_DESCRIPTION_OUT_BYTES),
                "pushedAt": repo.pushed_at, "updatedAt": timestamp(&repo.updated_at)?,
                "visibility": repo.visibility,
            })))
        },
    )?;
    Ok(json!({"repositories": rows}))
}

pub(crate) fn branch(
    input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let input = serde_json::from_value::<BranchInput>(input).map_err(|_| invalid_input())?;
    validate_login(&input.owner)?;
    validate_repo(&input.repo)?;
    validate_ref(&input.branch)?;

    let uri = url(
        base,
        &format!(
            "/repos/{}/{}/branches/{}",
            percent_encode(&input.owner),
            percent_encode(&input.repo),
            encode_path(&input.branch),
        ),
    )?;
    let response = send_get(send, uri, ACCEPT_JSON)?;
    let body = decode::<Value>(&response.body)?;
    if body["name"].as_str() != Some(&input.branch)
        || !body["commit"]["sha"].as_str().is_some_and(is_sha)
    {
        return Err(invalid_response());
    }
    Ok(body)
}

pub(crate) fn commit(
    input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let input = serde_json::from_value::<CommitInput>(input).map_err(|_| invalid_input())?;
    validate_login(&input.owner)?;
    validate_repo(&input.repo)?;
    validate_ref(&input.reference)?;

    let uri = url(
        base,
        &format!(
            "/repos/{}/{}/commits/{}",
            percent_encode(&input.owner),
            percent_encode(&input.repo),
            encode_path(&input.reference),
        ),
    )?;
    let response = send_get(send, uri, ACCEPT_JSON)?;
    let body = decode::<Value>(&response.body)?;
    if !body["sha"].as_str().is_some_and(is_sha) {
        return Err(invalid_response());
    }
    Ok(body)
}

pub(crate) fn self_user(
    _input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let response = send_get(send, url(base, "/user")?, ACCEPT_JSON)?;
    let body = decode::<Value>(&response.body)?;
    if !body["login"]
        .as_str()
        .is_some_and(|login| !login.is_empty())
    {
        return Err(invalid_response());
    }
    Ok(body)
}

pub(crate) fn user(
    input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let input = serde_json::from_value::<UserInput>(input).map_err(|_| invalid_input())?;
    validate_login(&input.login)?;

    let uri = url(base, &format!("/users/{}", percent_encode(&input.login)))?;
    let response = send_get(send, uri, ACCEPT_JSON)?;
    let body = decode::<Value>(&response.body)?;
    if !body["login"]
        .as_str()
        .is_some_and(|login| login.eq_ignore_ascii_case(&input.login))
    {
        return Err(invalid_response());
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::invoke_with;
    use crate::testutil::{capability, json_response, scripted, step};

    #[test]
    fn repo_read_projects_metadata() {
        let output = invoke_with(
            &capability("gh.repo.read"),
            json!({"owner": "octo", "repo": "Hello"}),
            scripted(vec![step(
                |request| {
                    assert_eq!(request.method, "GET");
                    assert_eq!(request.uri, "https://api.github.com/repos/octo/Hello");
                },
                json_response(
                    200,
                    &json!({
                        "name": "hello",
                        "owner": {"login": "octo"},
                        "private": true,
                        "visibility": "private",
                        "default_branch": "main",
                        "fork": false,
                        "archived": false,
                        "description": "A demo",
                        "pushed_at": "2026-08-10T00:00:00Z",
                        "updated_at": "2026-08-11T00:00:00Z",
                    }),
                ),
            )]),
        )
        .expect("repo read succeeds");

        assert_eq!(output["defaultBranch"], "main");
        assert_eq!(output["private"], true);
        assert_eq!(output["owner"], "octo");
        assert_eq!(output["descriptionTruncated"], false);
    }

    #[test]
    fn branch_read_validates_the_echo_and_sha() {
        let output = invoke_with(
            &capability("gh.branch.read"),
            json!({"owner": "octo", "repo": "hello", "branch": "release/1.2"}),
            scripted(vec![step(
                |request| {
                    assert_eq!(
                        request.uri,
                        "https://api.github.com/repos/octo/hello/branches/release/1.2"
                    );
                },
                json_response(
                    200,
                    &json!({
                        "name": "release/1.2",
                        "commit": {"sha": "a".repeat(40)},
                        "protected": true,
                    }),
                ),
            )]),
        )
        .expect("branch read succeeds");

        assert_eq!(output["commit"]["sha"], "a".repeat(40));
        assert_eq!(output["protected"], true);

        let error = invoke_with(
            &capability("gh.branch.read"),
            json!({"owner": "octo", "repo": "hello", "branch": "main"}),
            scripted(vec![step(
                |_| {},
                json_response(
                    200,
                    &json!({"name": "other", "commit": {"sha": "a".repeat(40)}, "protected": false}),
                ),
            )]),
        )
        .expect_err("echo mismatch fails");
        assert_eq!(error.code(), "invalid-response");
    }

    #[test]
    fn native_commit_read_preserves_upstream_nested_fields() {
        let output = invoke_with(
            &capability("gh.commit.read"),
            json!({"owner": "octo", "repo": "hello", "ref": "main"}),
            scripted(vec![step(
                |request| {
                    assert_eq!(
                        request.uri,
                        "https://api.github.com/repos/octo/hello/commits/main"
                    );
                },
                json_response(
                    200,
                    &json!({
                        "sha": "c".repeat(40),
                        "html_url": "https://github.com/octo/hello/commit/c",
                        "committer": null,
                        "commit": {
                            "message": "fix: everything",
                            "author": {"name": "Xavier", "date": "2026-08-15T00:00:00Z"},
                        },
                        "author": {"login": "xavier"},
                        "stats": {"additions": 3, "deletions": 1, "total": 4},
                        "files": [
                            {"filename": "src/lib.rs", "status": "modified", "additions": 3, "deletions": 1},
                        ],
                    }),
                ),
            )]),
        )
        .expect("commit read succeeds");

        assert_eq!(output["sha"], "c".repeat(40));
        assert_eq!(output["html_url"], "https://github.com/octo/hello/commit/c");
        assert!(output["committer"].is_null());
        assert_eq!(output["author"]["login"], "xavier");
        assert_eq!(output["commit"]["author"]["name"], "Xavier");
        assert_eq!(output["stats"]["total"], 4);
        assert_eq!(output["files"][0]["filename"], "src/lib.rs");
    }

    #[test]
    fn user_read_matches_login_case_insensitively() {
        let output = invoke_with(
            &capability("gh.user.read"),
            json!({"login": "CPetersen"}),
            scripted(vec![step(
                |request| {
                    assert_eq!(request.uri, "https://api.github.com/users/CPetersen");
                },
                json_response(
                    200,
                    &json!({
                        "login": "cpetersen",
                        "type": "User",
                        "name": "Chris Petersen",
                        "public_repos": 42,
                        "followers": 7,
                        "created_at": "2010-01-01T00:00:00Z",
                    }),
                ),
            )]),
        )
        .expect("user read succeeds");

        assert_eq!(output["login"], "cpetersen");
        assert_eq!(output["type"], "User");
        assert_eq!(output["name"], "Chris Petersen");
    }
}
