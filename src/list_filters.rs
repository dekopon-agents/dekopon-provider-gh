use crate::error::ProviderError;
use crate::{invalid_input, invalid_response, percent_encode};
use dekopon_provider_sdk::provider::endpoint::Base;
use dekopon_provider_sdk::provider::{HttpError, Request, Response};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Input {
    owner: String,
    repo: String,
    state: Option<String>,
    author: Option<String>,
    base: Option<String>,
    head: Option<String>,
    label: Option<Vec<String>>,
    assignee: Option<String>,
    search: Option<String>,
    draft: Option<bool>,
    app: Option<String>,
    mention: Option<String>,
    milestone: Option<String>,
    #[serde(rename = "type")]
    issue_type: Option<String>,
    page: Option<u32>,
    per_page: Option<u32>,
    #[serde(rename = "_cli")]
    cli: Option<String>,
    #[serde(rename = "_cliLimit")]
    limit: Option<usize>,
}

pub(crate) fn run(
    command: &str,
    value: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    crate::qualifiers::validate(command, &value).map_err(|_| invalid_input())?;
    let input: Input = serde_json::from_value(value.clone()).map_err(|_| invalid_input())?;
    crate::validate_login(&input.owner)?;
    crate::validate_repo(&input.repo)?;
    if let Some(author) = &input.author {
        crate::validate_login(author)?;
    }
    if input.limit.is_some() && input.cli.is_none() {
        return Err(invalid_input());
    }
    let pr = command == "pr list";
    if !pr && (input.base.is_some() || input.head.is_some() || input.draft.is_some()) {
        return Err(invalid_input());
    }
    if pr && (input.mention.is_some() || input.milestone.is_some() || input.issue_type.is_some()) {
        return Err(invalid_input());
    }
    let state = input.state.as_deref().unwrap_or("open");
    if !["open", "closed", "all"].contains(&state) && !(pr && state == "merged") {
        return Err(invalid_input());
    }
    let search = (!pr
        && input
            .label
            .as_ref()
            .is_some_and(|labels| labels.iter().any(|label| label.contains(','))))
        || input.search.is_some()
        || input.app.is_some()
        || input
            .milestone
            .as_deref()
            .is_some_and(|v| v.parse::<u64>().is_err());
    let mut path;
    if search {
        let mut q = input.search.clone().unwrap_or_default();
        for term in crate::qualifiers::terms(command, &value) {
            if !q.is_empty() {
                q.push(' ');
            }
            q.push_str(&term);
        }
        q.push_str(&format!(
            " repo:{}/{} is:{}",
            input.owner,
            input.repo,
            if pr { "pr" } else { "issue" }
        ));
        if state != "all" {
            q.push_str(&format!(" is:{state}"));
        }
        if pr && let Some(author) = &input.author {
            q.push_str(&format!(" author:{author}"));
        }
        path = format!("/search/issues?q={}", percent_encode(&q));
    } else {
        path = format!(
            "/repos/{}/{}/{}?state={}",
            percent_encode(&input.owner),
            percent_encode(&input.repo),
            if pr { "pulls" } else { "issues" },
            if state == "merged" { "closed" } else { state }
        );
        if pr {
            for (name, value) in [("base", &input.base), ("head", &input.head)] {
                if let Some(value) = value {
                    path.push_str(&format!("&{name}={}", percent_encode(value)));
                }
            }
        } else {
            for (name, value) in [
                ("assignee", &input.assignee),
                ("creator", &input.author),
                ("mentioned", &input.mention),
                ("milestone", &input.milestone),
                ("type", &input.issue_type),
            ] {
                if let Some(value) = value {
                    path.push_str(&format!("&{name}={}", percent_encode(value)));
                }
            }
            if let Some(labels) = &input.label {
                path.push_str(&format!("&labels={}", percent_encode(&labels.join(","))));
            }
        }
    }
    let fields = input.cli.as_deref().unwrap_or("");
    let project = |raw: Value| -> Result<Option<Value>, ProviderError> {
        if !pr && !raw["pull_request"].is_null() {
            return Ok(None);
        }
        if !search && pr {
            if state == "merged" && raw["merged_at"].is_null() && raw["merged"] != true {
                return Ok(None);
            }
            if input.draft.is_some_and(|draft| raw["draft"] != draft) {
                return Ok(None);
            }
            if input.author.as_deref().is_some_and(|author| {
                !raw["user"]["login"]
                    .as_str()
                    .is_some_and(|login| login.eq_ignore_ascii_case(author))
            }) {
                return Ok(None);
            }
            if input.assignee.as_deref().is_some_and(|assignee| {
                !raw["assignees"].as_array().is_some_and(|users| {
                    users.iter().any(|user| {
                        user["login"]
                            .as_str()
                            .is_some_and(|login| login.eq_ignore_ascii_case(assignee))
                    })
                })
            }) {
                return Ok(None);
            }
            if input.label.as_ref().is_some_and(|labels| {
                labels.iter().any(|label| {
                    !raw["labels"].as_array().is_some_and(|values| {
                        values.iter().any(|value| {
                            value["name"]
                                .as_str()
                                .is_some_and(|value| value.eq_ignore_ascii_case(label))
                        })
                    })
                })
            }) {
                return Ok(None);
            }
        }
        if pr && !search {
            let pull = serde_json::from_value::<crate::pulls::RawPull>(raw.clone())
                .map_err(|_| invalid_response())?;
            return crate::pulls::cli_pull(&pull, &raw, fields).map(Some);
        }
        let issue = serde_json::from_value::<crate::issues::RawIssue>(raw.clone())
            .map_err(|_| invalid_response())?;
        let mut row = crate::issues::cli_issue(&issue, &raw, fields)?;
        if pr {
            row["isDraft"] = raw["draft"].clone();
            row["state"] = json!(if !raw["pull_request"]["merged_at"].is_null() {
                "MERGED"
            } else if raw["state"] == "closed" {
                "CLOSED"
            } else {
                "OPEN"
            });
        }
        Ok(Some(row))
    };
    let rows = if input.cli.is_some() {
        if input.page.is_some() || input.per_page.is_some() {
            return Err(invalid_input());
        }
        crate::collect_pages_with(
            send,
            base,
            &path,
            input.limit.unwrap_or(30),
            |body| {
                let raw: Value = crate::decode(body)?;
                if search {
                    if raw["incomplete_results"] != false {
                        return Err(ProviderError::new(
                            "incomplete-results",
                            "search indexing is incomplete; results are not exhaustive",
                        ));
                    }
                    raw["items"]
                        .as_array()
                        .cloned()
                        .ok_or_else(invalid_response)
                } else {
                    raw.as_array().cloned().ok_or_else(invalid_response)
                }
            },
            |row| row["id"].as_u64().map(|id| id.to_string()),
            project,
        )?
    } else {
        let (page, per_page) = crate::validate_page(input.page, input.per_page)?;
        let uri = crate::url(
            base,
            &format!("{path}&page={page}&per_page={}", per_page.unwrap_or(20)),
        )?;
        let response = crate::send_get(send, uri, crate::ACCEPT_JSON)?;
        let raw: Value = crate::decode(&response.body)?;
        let items = if search { &raw["items"] } else { &raw };
        items
            .as_array()
            .ok_or_else(invalid_response)?
            .iter()
            .take(crate::MAX_LIST_ITEMS)
            .cloned()
            .map(project)
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect()
    };
    Ok(if pr {
        json!({"pullRequests":rows})
    } else {
        json!({"issues":rows})
    })
}
