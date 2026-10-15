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
        ("repo", "view") => REPO,
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
    let list = matches!(id, "gh.pull-request.list" | "gh.issue.list")
        || (id == "gh.content.read" && input.get("_cliKind") == Some(&json!("read-dir")));
    let source = match id {
        "gh.pull-request.list" => &value["pullRequests"],
        "gh.issue.list" => &value["issues"],
        "gh.content.read" if list => &value["entries"],
        _ => value,
    };
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
            "gh.repo.read" => &["nameWithOwner", "description"],
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
                        .filter_map(Value::as_str)
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
