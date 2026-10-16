use serde_json::{Map, Value, json};

use crate::error::ProviderError;

pub(crate) const PR: &str = "assignees,author,baseRefName,baseRefOid,body,closedAt,createdAt,headRefName,headRefOid,isDraft,labels,mergedAt,number,state,title,updatedAt,url";
pub(crate) const ISSUE: &str =
    "assignees,author,body,closedAt,createdAt,labels,number,state,title,updatedAt,url";
pub(crate) const REPO: &str = "createdAt,defaultBranchRef,description,isArchived,isFork,isPrivate,name,nameWithOwner,owner,pushedAt,updatedAt,url,visibility";
pub(crate) const FILE: &str =
    "name,path,gitSHA,size,url,htmlUrl,gitUrl,downloadUrl,type,encoding,content";
pub(crate) const DIR: &str = "name,path,type,gitSHA,size";
pub(crate) const SEARCH_PR: &str = "assignees,author,body,closedAt,createdAt,isDraft,labels,number,repository,state,title,updatedAt,url";
pub(crate) const SEARCH_ISSUE: &str =
    "assignees,author,body,closedAt,createdAt,labels,number,repository,state,title,updatedAt,url";
pub(crate) const SEARCH_REPO: &str = "createdAt,defaultBranch,description,fullName,isArchived,isFork,isPrivate,language,name,owner,pushedAt,stargazersCount,updatedAt,url,visibility";
pub(crate) const SEARCH_CODE: &str = "path,repository,sha,url";
pub(crate) const SEARCH_COMMIT: &str = "author,commit,committer,parents,repository,sha,url";
pub(crate) const CHECKS: &str =
    "bucket,completedAt,description,event,link,name,startedAt,state,workflow";

pub(crate) fn fields(area: &str, verb: &str) -> Option<String> {
    let common = match (area, verb) {
        ("pr", "list" | "view") => PR,
        ("issue", "list" | "view") => ISSUE,
        ("repo", "view" | "list") => REPO,
        ("repo", "read-file") => FILE,
        ("repo", "read-dir") => DIR,
        ("search", "prs") => SEARCH_PR,
        ("search", "issues") => SEARCH_ISSUE,
        ("search", "repos") => SEARCH_REPO,
        ("search", "code") => SEARCH_CODE,
        ("search", "commits") => SEARCH_COMMIT,
        ("pr", "checks") => CHECKS,
        _ => return None,
    };
    let mut result = common.to_owned();
    match (area, verb) {
        ("pr", "view") => result.push_str(",files,reviews,latestReviews,comments"),
        ("issue", "view") => result.push_str(",comments"),
        _ => {}
    }
    Some(result)
}

pub(crate) fn validate_selection(
    id: &str,
    selection: Option<&Value>,
    kind: Option<&Value>,
) -> Result<(), ProviderError> {
    if selection.is_none() && kind.is_some() {
        return Err(crate::invalid_input());
    }
    let Some(selection) = selection else {
        return Ok(());
    };
    let Value::String(selection) = selection else {
        return Err(crate::invalid_input());
    };
    let (area, verb) = match (id, kind.and_then(Value::as_str)) {
        ("gh.repo.read", None) => ("repo", "view"),
        ("gh.repo.list", None) => ("repo", "list"),
        ("gh.content.read", Some("read-file")) => ("repo", "read-file"),
        ("gh.content.read", Some("read-dir")) => ("repo", "read-dir"),
        ("gh.pull-request.read", None) => ("pr", "view"),
        ("gh.pull-request.status", None) => ("pr", "checks"),
        ("gh.pull-request.list", None) => ("pr", "list"),
        ("gh.issue.read", None) => ("issue", "view"),
        ("gh.issue.list", None) => ("issue", "list"),
        ("gh.search.prs", None) => ("search", "prs"),
        ("gh.search.issues", None) => ("search", "issues"),
        ("gh.search.repos", None) => ("search", "repos"),
        ("gh.search.code", None) => ("search", "code"),
        ("gh.search.commits", None) => ("search", "commits"),
        _ => return Err(crate::invalid_input()),
    };
    let allowed = fields(area, verb).expect("CLI fields exist");
    if selection.len() > 512
        || (!selection.is_empty() && {
            let mut seen = std::collections::HashSet::new();
            selection
                .split(',')
                .any(|field| !allowed.split(',').any(|entry| field == entry) || !seen.insert(field))
        })
    {
        return Err(crate::invalid_input());
    }
    Ok(())
}

pub(crate) fn select(value: &Value, fields: &str, list: bool) -> Value {
    let select_one = |item: &Value| {
        let mut selected = Map::new();
        for field in fields.split(',') {
            let alias = match field {
                "headRefName" => "headRef",
                "headRefOid" => "headSha",
                "baseRefName" => "baseRef",
                "baseRefOid" => "baseSha",
                "isDraft" => "draft",
                _ => field,
            };
            let mut value = item
                .get(field)
                .or_else(|| item.get(alias))
                .cloned()
                .unwrap_or(Value::Null);
            if matches!(field, "author" | "owner") && value.is_string() {
                value = json!({"login": value});
            }
            selected.insert(field.to_owned(), value);
        }
        Value::Object(selected)
    };
    if list {
        Value::Array(
            value
                .as_array()
                .map_or_else(Vec::new, |rows| rows.iter().map(select_one).collect()),
        )
    } else {
        select_one(value)
    }
}

pub(crate) fn render(
    id: &str,
    input: &Value,
    value: &Value,
    selection: &str,
) -> Result<String, ProviderError> {
    if id == crate::ids::PR_STATUS {
        let checks = value["checks"]
            .as_array()
            .ok_or_else(crate::invalid_response)?;
        if !selection.is_empty() {
            return serde_json::to_string(&select(&Value::Array(checks.clone()), selection, true))
                .map(|text| format!("{text}\n"))
                .map_err(|_| crate::invalid_response());
        }
        let mut text = String::new();
        for check in checks {
            let bucket = check["bucket"]
                .as_str()
                .ok_or_else(crate::invalid_response)?;
            let printed_bucket = if bucket == "cancel" { "fail" } else { bucket };
            text.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\n",
                check["name"].as_str().ok_or_else(crate::invalid_response)?,
                printed_bucket,
                check_duration(check)?,
                check["link"].as_str().ok_or_else(crate::invalid_response)?,
                check["description"]
                    .as_str()
                    .ok_or_else(crate::invalid_response)?
            ));
        }
        return Ok(text);
    }
    let list = matches!(
        id,
        "gh.pull-request.list"
            | "gh.issue.list"
            | "gh.repo.list"
            | "gh.search.prs"
            | "gh.search.issues"
            | "gh.search.repos"
            | "gh.search.code"
            | "gh.search.commits"
    ) || (id == "gh.content.read" && input.get("_cliKind") == Some(&json!("read-dir")));
    if id == "gh.content.read" {
        let expected = if list { "dir" } else { "file" };
        if value["type"] != expected {
            return Err(crate::invalid_response());
        }
    }
    let source = match id {
        "gh.pull-request.list" => &value["pullRequests"],
        "gh.issue.list" => &value["issues"],
        "gh.repo.list" => &value["repositories"],
        "gh.content.read" if list => &value["entries"],
        _ => value,
    };
    if list && !source.is_array() {
        return Err(crate::invalid_response());
    }
    if !selection.is_empty() {
        return serde_json::to_string(&select(source, selection, list))
            .map(|text| format!("{text}\n"))
            .map_err(|_| crate::invalid_response());
    }
    if id == "gh.content.read" && !list {
        let text = source["content"]
            .as_str()
            .ok_or_else(crate::invalid_response)?;
        return Ok(text.to_owned());
    }
    let rows = if list {
        source
            .as_array()
            .ok_or_else(crate::invalid_response)?
            .as_slice()
    } else {
        std::slice::from_ref(source)
    };
    let mut output = String::new();
    for row in rows {
        let cells: &[&str] = match id {
            "gh.pull-request.list" => &["number", "title", "headRefName", "createdAt"],
            "gh.issue.list" => &["number", "title", "labels", "updatedAt"],
            "gh.repo.read" | "gh.repo.list" => &["nameWithOwner", "description"],
            "gh.search.prs" | "gh.search.issues" => &["number", "title", "repository", "state"],
            "gh.search.repos" => &["fullName", "description"],
            "gh.search.code" => &["path", "repository", "sha"],
            "gh.search.commits" => &["sha", "commit", "repository"],
            "gh.pull-request.read" => &["title", "body"],
            "gh.issue.read" => &["title", "body"],
            "gh.content.read" => &["name", "type", "size"],
            _ => return Err(crate::invalid_input()),
        };
        for (index, field) in cells.iter().enumerate() {
            if index != 0 {
                output.push('\t');
            }
            let value = if *field == "headRefName" && row.get("headRefName").is_none() {
                &row["headRef"]
            } else {
                &row[*field]
            };
            match value {
                Value::String(text) => output.push_str(text),
                Value::Null => {}
                Value::Array(items) => output.push_str(
                    &items
                        .iter()
                        .filter_map(|item| item.as_str().or_else(|| item["name"].as_str()))
                        .collect::<Vec<_>>()
                        .join(","),
                ),
                other => output.push_str(&other.to_string()),
            }
        }
        output.push('\n');
    }
    Ok(output)
}

fn check_duration(row: &Value) -> Result<String, ProviderError> {
    use time::{OffsetDateTime, format_description::well_known::Rfc3339};
    let started = row["startedAt"]
        .as_str()
        .ok_or_else(crate::invalid_response)?;
    let completed = row["completedAt"]
        .as_str()
        .ok_or_else(crate::invalid_response)?;
    if started == "0001-01-01T00:00:00Z" || completed == "0001-01-01T00:00:00Z" {
        return Ok("0".to_owned());
    }
    let start = OffsetDateTime::parse(started, &Rfc3339).map_err(|_| crate::invalid_response())?;
    let end = OffsetDateTime::parse(completed, &Rfc3339).map_err(|_| crate::invalid_response())?;
    let elapsed = end
        .unix_timestamp_nanos()
        .saturating_sub(start.unix_timestamp_nanos());
    if elapsed <= 0 {
        return Ok("0".to_owned());
    }
    let seconds = elapsed / 1_000_000_000;
    let fraction = elapsed % 1_000_000_000;
    let hours = seconds / 3600;
    let minutes = (seconds / 60) % 60;
    let remainder = seconds % 60;
    if seconds > 0 {
        let suffix = if fraction == 0 {
            String::new()
        } else {
            format!(".{fraction:09}").trim_end_matches('0').to_owned()
        };
        if hours > 0 {
            Ok(format!("{hours}h{minutes}m{remainder}{suffix}s"))
        } else if minutes > 0 {
            Ok(format!("{minutes}m{remainder}{suffix}s"))
        } else {
            Ok(format!("{remainder}{suffix}s"))
        }
    } else {
        let (unit, factor, digits) = if elapsed >= 1_000_000 {
            ("ms", 1_000_000, 6)
        } else if elapsed >= 1_000 {
            ("µs", 1_000, 3)
        } else {
            ("ns", 1, 0)
        };
        let whole = elapsed / factor;
        let decimal = if digits == 0 || elapsed % factor == 0 {
            String::new()
        } else {
            format!(".{:0digits$}", elapsed % factor)
                .trim_end_matches('0')
                .to_owned()
        };
        Ok(format!("{whole}{decimal}{unit}"))
    }
}

pub(crate) fn require_checks(value: &Value) -> Result<(), ProviderError> {
    let checks = value["checks"]
        .as_array()
        .ok_or_else(crate::invalid_response)?;
    if checks.is_empty() {
        return Err(ProviderError::new(
            "checks-empty",
            format!(
                "no checks reported on the '{}' branch",
                value["headRefName"]
                    .as_str()
                    .ok_or_else(crate::invalid_response)?
            ),
        ));
    }
    Ok(())
}

pub(crate) fn checks_verdict(value: &Value) -> Result<(), ProviderError> {
    let checks = value["checks"]
        .as_array()
        .ok_or_else(crate::invalid_response)?;
    if checks.iter().any(|row| row["bucket"] == "fail") {
        return Err(ProviderError::new(
            "checks-failed",
            "one or more checks failed",
        ));
    }
    if checks.iter().any(|row| row["bucket"] == "pending") {
        return Err(ProviderError::new("checks-pending", "checks are pending"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn checks_render_bucket_duration_and_json_selection_without_losing_verdict() {
        use dekopon_provider_sdk::provider::Failure;
        let input = json!({"checks":[{"name":"third-party","bucket":"fail","state":"FAILURE",
            "startedAt":"2026-08-10T00:00:00Z","completedAt":"2026-08-10T00:02:05Z",
            "link":"https://example.test/check","description":"broken","workflow":"","event":""}]});
        let text = super::render("gh.pull-request.status", &json!({}), &input, "").expect("text");
        assert_eq!(
            text,
            "third-party\tfail\t2m5s\thttps://example.test/check\tbroken\n"
        );
        let selected = super::render("gh.pull-request.status", &json!({}), &input, "name,bucket")
            .expect("json");
        assert_eq!(
            selected,
            "[{\"bucket\":\"fail\",\"name\":\"third-party\"}]\n"
        );
        let error = super::checks_verdict(&input).expect_err("failed check");
        assert_eq!(error.code(), "checks-failed");
        assert_eq!(Failure::code(&error).status().get(), 1);
    }

    #[test]
    fn checks_verdict_and_provider_failures_have_distinct_status_codes() {
        use dekopon_provider_sdk::provider::Failure;
        for (bucket, code, status) in [
            ("pending", "checks-pending", 8),
            ("fail", "checks-failed", 1),
        ] {
            let error = super::checks_verdict(&json!({"checks":[{"bucket":bucket}]}))
                .expect_err("nonpassing verdict");
            assert_eq!(error.code(), code);
            assert_eq!(Failure::code(&error).status().get(), status);
        }
        let empty = super::require_checks(&json!({"checks":[],"headRefName":"topic"}))
            .expect_err("no checks on branch");
        assert_eq!(empty.code(), "checks-empty");
        assert_eq!(Failure::code(&empty).status().get(), 1);
        assert_eq!(empty.message(), "no checks reported on the 'topic' branch");
        for (code, status) in [
            ("request-ceiling", 3),
            ("invalid-response", 3),
            ("forbidden", 3),
            ("not-found", 3),
            ("unprocessable", 3),
            ("rate-limited", 3),
            ("http-failed", 3),
            ("usage", 2),
        ] {
            let error = crate::error::ProviderError::new(code, "safe failure");
            assert_eq!(Failure::code(&error).as_str(), code);
            assert_eq!(Failure::code(&error).status().get(), status);
        }
        assert!(super::checks_verdict(&json!({"checks":[{"bucket":"cancel"}]})).is_ok());
        assert!(super::checks_verdict(&json!({"checks":[{"bucket":"skipping"}]})).is_ok());
        let failed_wins = super::checks_verdict(&json!({"checks":[
            {"bucket":"pending"},{"bucket":"fail"}
        ]}))
        .expect_err("failed check takes precedence over pending");
        assert_eq!(failed_wins.code(), "checks-failed");
    }

    #[test]
    fn native_invalid_selection_has_typed_invalid_input_code() {
        let error = super::validate_selection("gh.repo.read", Some(&json!("unknown")), None)
            .expect_err("unknown field must fail before HTTP");
        assert_eq!(error.code(), "invalid-input");
    }
}
