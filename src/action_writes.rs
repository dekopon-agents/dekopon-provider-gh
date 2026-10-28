use crate::actions::{self, Reads, Workflow};
use crate::{
    Base, HttpError, Request, Response, error::ProviderError, invalid_input, invalid_response,
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Permission,
    NotFound,
    Conflict,
    Validation,
    RateLimit,
    Incomplete,
}
impl From<Outcome> for ProviderError {
    fn from(value: Outcome) -> Self {
        let (code, message) = match value {
            Outcome::Permission => (
                "forbidden",
                "repository is invisible or Actions permission is insufficient",
            ),
            Outcome::NotFound => ("not-found", "Actions target was not found"),
            Outcome::Conflict => (
                "actions-conflict",
                "operation does not apply to the observed target state",
            ),
            Outcome::Validation => ("unprocessable", "GitHub rejected the workflow inputs"),
            Outcome::RateLimit => ("rate-limited", "Actions rate limit is exhausted"),
            Outcome::Incomplete => (
                "incomplete-results",
                "Actions discovery is incomplete; no further writes attempted",
            ),
        };
        Self::new(code, message)
    }
}
pub(crate) fn status(response: &Response, repository: bool) -> Option<Outcome> {
    match response.status {
        403 if crate::rate_limit_exhausted(response)
            || crate::header_text(response, "retry-after")
                .is_some_and(|v| !v.trim().is_empty()) =>
        {
            Some(Outcome::RateLimit)
        }
        403 => Some(Outcome::Permission),
        404 if repository => Some(Outcome::Permission),
        404 => Some(Outcome::NotFound),
        409 => Some(Outcome::Conflict),
        422 => Some(Outcome::Validation),
        429 => Some(Outcome::RateLimit),
        _ => None,
    }
}
pub(crate) fn get(
    reads: &mut Reads<'_>,
    path: &str,
    repository: bool,
) -> Result<Value, ProviderError> {
    let response = send(reads, "GET", path, None)?;
    if response.status != 200 {
        return Err(status(&response, repository)
            .map(ProviderError::from)
            .unwrap_or_else(|| crate::status_error(&response)));
    }
    crate::decode(&response.body)
}
pub(crate) fn send(
    reads: &mut Reads<'_>,
    method: &'static str,
    path: &str,
    body: Option<Value>,
) -> Result<Response, ProviderError> {
    if reads.used >= reads.max {
        return Err(Outcome::Incomplete.into());
    }
    let uri = crate::url(reads.base, path)?;
    let request = match body {
        Some(body) => crate::github_json_request(method, uri, &body)?,
        None => crate::github_request(method, uri, crate::ACCEPT_JSON)?,
    };
    reads.used += 1;
    (reads.send)(request).map_err(|_| crate::http_failed())
}
pub(crate) fn write(
    reads: &mut Reads<'_>,
    method: &'static str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Result<(), ProviderError> {
    let response = send(reads, method, path, body)?;
    if response.status == expected {
        return Ok(());
    }
    Err(status(&response, false)
        .map(ProviderError::from)
        .unwrap_or_else(|| crate::status_error(&response)))
}
pub(crate) fn repository(reads: &mut Reads<'_>, root: &str) -> Result<Value, ProviderError> {
    let value = get(reads, root, true)?;
    actions::positive(&value, "id")?;
    Ok(value)
}
pub(crate) fn workflow_path(workflow: &Workflow) -> String {
    match workflow {
        Workflow::Id(id) => id.to_string(),
        Workflow::File(file) => crate::percent_encode(file),
    }
}

#[derive(Clone, Copy, Deserialize)]
#[serde(transparent)]
struct RunId(u64);
#[derive(Clone, Copy, Deserialize)]
#[serde(transparent)]
struct JobId(u64);
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RunInput {
    owner: String,
    repo: String,
    run_id: RunId,
    expected_head_sha: Option<String>,
    #[serde(default)]
    failed: bool,
    job: Option<JobId>,
    #[serde(default)]
    debug: bool,
}
impl RunInput {
    fn validate(&self, id: &str) -> Result<(), ProviderError> {
        crate::validate_login(&self.owner)?;
        crate::validate_repo(&self.repo)?;
        crate::validate_expected_sha(self.expected_head_sha.as_deref())?;
        if self.run_id.0 == 0
            || self.job.is_some_and(|j| j.0 == 0)
            || (self.failed && self.job.is_some())
            || (id != crate::ids::RUN_RERUN && (self.failed || self.job.is_some() || self.debug))
        {
            return Err(invalid_input());
        }
        Ok(())
    }
}
pub(crate) fn validate(id: &str, input: Value) -> Result<(), ProviderError> {
    let input: RunInput = serde_json::from_value(input).map_err(|_| invalid_input())?;
    input.validate(id)
}
pub(crate) fn run(
    id: &str,
    input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let input: RunInput = serde_json::from_value(input).map_err(|_| invalid_input())?;
    input.validate(id)?;
    let mut reads = Reads {
        base,
        send,
        used: 0,
        max: 4,
        target_errors: true,
    };
    let root = actions::root(&input.owner, &input.repo);
    repository(&mut reads, &root)?;
    let path = format!("{root}/actions/runs/{}", input.run_id.0);
    let raw = get(&mut reads, &path, false)?;
    let run = actions::run_row(&raw, Some(input.run_id.0), None)?;
    if input.expected_head_sha.as_ref().is_some_and(|sha| {
        !run["headSha"]
            .as_str()
            .is_some_and(|head| head.eq_ignore_ascii_case(sha))
    }) {
        return Err(ProviderError::new(
            "head-changed",
            "run head differs from expected SHA",
        ));
    }
    let completed = run["status"] == "completed";
    if input.failed && run["conclusion"] == "success" {
        return Err(Outcome::Conflict.into());
    }
    if ((id == crate::ids::RUN_CANCEL || id == crate::ids::RUN_FORCE_CANCEL) && completed)
        || ((id == crate::ids::RUN_RERUN || id == crate::ids::RUN_DELETE) && !completed)
    {
        return Err(Outcome::Conflict.into());
    }
    let (method, target, expected, body) = match id {
        crate::ids::RUN_CANCEL => ("POST", format!("{path}/cancel"), 202, None),
        crate::ids::RUN_FORCE_CANCEL => ("POST", format!("{path}/force-cancel"), 202, None),
        crate::ids::RUN_DELETE => ("DELETE", path, 204, None),
        crate::ids::RUN_RERUN => {
            let target = if let Some(job) = input.job {
                let job_path = format!("{root}/actions/jobs/{}", job.0);
                let raw = get(&mut reads, &job_path, false)?;
                if actions::positive(&raw, "id")? != job.0
                    || raw["run_id"] != run["databaseId"]
                    || raw["run_attempt"] != run["attempt"]
                    || raw["head_sha"] != run["headSha"]
                {
                    return Err(invalid_response());
                }
                if raw["status"] != "completed" {
                    return Err(Outcome::Conflict.into());
                }
                format!("{job_path}/rerun")
            } else {
                format!(
                    "{path}/{}",
                    if input.failed {
                        "rerun-failed-jobs"
                    } else {
                        "rerun"
                    }
                )
            };
            (
                "POST",
                target,
                201,
                Some(json!({"enable_debug_logging":input.debug})),
            )
        }
        _ => return Err(invalid_input()),
    };
    write(&mut reads, method, &target, body, expected)?;
    Ok(
        json!({"runId":input.run_id.0,"attempt":run["attempt"],"headSha":run["headSha"],"status":run["status"],"outcome":"accepted","requestsUsed":reads.used}),
    )
}
pub(crate) fn schema(id: &str) -> Value {
    let mut properties = json!({"runId":{"type":"integer","minimum":1},"expectedHeadSha":{"type":"string","pattern":"^[a-fA-F0-9]{40}$"}});
    if id == crate::ids::RUN_RERUN {
        properties["failed"] = json!({"type":"boolean"});
        properties["debug"] = json!({"type":"boolean"});
        properties["job"] = json!({"type":"integer","minimum":1});
    }
    crate::repo_schema(properties, &["runId"])
}

pub(crate) fn unused(
    _: Value,
    _: &Base,
    _: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    Err(invalid_input())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_actions_outcomes_match_variants_without_printed_error_dispatch() {
        for (code, repository, expected) in [
            (403, false, Outcome::Permission),
            (404, true, Outcome::Permission),
            (404, false, Outcome::NotFound),
            (409, false, Outcome::Conflict),
            (422, false, Outcome::Validation),
            (429, false, Outcome::RateLimit),
        ] {
            assert_eq!(
                status(
                    &Response {
                        status: code,
                        headers: vec![],
                        body: vec![]
                    },
                    repository
                ),
                Some(expected)
            );
        }
    }
}
