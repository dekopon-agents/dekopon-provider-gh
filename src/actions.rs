use crate::{
    Base, HttpError, Request, Response, error::ProviderError, invalid_input, invalid_response,
};
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    io::Write,
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

pub(crate) const LIST_FIELDS: &str = "databaseId,attempt,headSha,headBranch,event,status,conclusion,workflowName,displayTitle,createdAt,startedAt,updatedAt,url,workflowDatabaseId,wallClockSeconds";
pub(crate) const READ_FIELDS: &str = "databaseId,attempt,headSha,headBranch,event,status,conclusion,workflowName,displayTitle,createdAt,startedAt,updatedAt,url,workflowDatabaseId,wallClockSeconds,jobs,latestAttempt,existingAttempts,completedJobDurationSeconds";
const STATUSES: &[&str] = &[
    "queued",
    "in_progress",
    "completed",
    "requested",
    "waiting",
    "pending",
    "action_required",
    "cancelled",
    "failure",
    "neutral",
    "skipped",
    "stale",
    "startup_failure",
    "success",
    "timed_out",
];
const CONCLUSIONS: &[&str] = &[
    "action_required",
    "cancelled",
    "failure",
    "neutral",
    "skipped",
    "stale",
    "startup_failure",
    "success",
    "timed_out",
];

#[derive(Debug, PartialEq)]
enum ActionError {
    NotFoundOrNotAccessible,
    Permission,
    RateLimit,
    Incomplete,
    RunFailed,
}
impl From<ActionError> for ProviderError {
    fn from(value: ActionError) -> Self {
        let (code, message) = match value {
            ActionError::NotFoundOrNotAccessible => {
                ("not-found-or-not-accessible", "not found or not accessible")
            }
            ActionError::Permission => (
                "forbidden",
                "credential is not permitted to read Actions or Checks",
            ),
            ActionError::RateLimit => ("rate-limited", "endpoint rate limit is exhausted"),
            ActionError::Incomplete => (
                "incomplete-results",
                "Actions results are incomplete; inspect coverage",
            ),
            ActionError::RunFailed => ("run-failed", "the selected run attempt did not succeed"),
        };
        Self::new(code, message)
    }
}

#[derive(Clone, Copy, Deserialize)]
#[serde(transparent)]
struct RunId(u64);
#[derive(Clone, Copy, Deserialize)]
#[serde(transparent)]
struct JobId(u64);
#[derive(Clone, Copy, Deserialize)]
#[serde(transparent)]
struct Attempt(u32);
impl std::fmt::Display for RunId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::fmt::Display for JobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Workflow {
    Id(u64),
    File(String),
}
impl Workflow {
    fn validate(&self) -> Result<(), ProviderError> {
        match self {
            Self::Id(id) if *id > 0 => Ok(()),
            Self::File(file)
                if file.len() <= 255
                    && (file.ends_with(".yml") || file.ends_with(".yaml"))
                    && file
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                    && !file.starts_with('.') =>
            {
                Ok(())
            }
            _ => Err(invalid_input()),
        }
    }
    fn matches(&self, row: &Value) -> bool {
        match self {
            Self::Id(id) => row["id"].as_u64() == Some(*id),
            Self::File(name) => {
                row["path"].as_str().and_then(|p| p.rsplit('/').next()) == Some(name)
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListInput {
    owner: String,
    repo: String,
    #[serde(default = "default_limit")]
    limit: usize,
    workflow: Option<Workflow>,
    branch: Option<String>,
    user: Option<String>,
    event: Option<String>,
    created: Option<String>,
    commit: Option<String>,
    status: Option<String>,
    #[serde(default)]
    all: bool,
    #[serde(rename = "_cliOutput")]
    output: Option<String>,
}
fn default_limit() -> usize {
    20
}
impl ListInput {
    const MAX_REQUESTS: usize = 21;
    const MAX_WORKFLOW_PAGES: usize = 10;
    const MAX_RUN_PAGES: usize = 10;
    fn validate(&self) -> Result<(), ProviderError> {
        validate_repo(&self.owner, &self.repo)?;
        if !(1..=100).contains(&self.limit) {
            return Err(invalid_input());
        }
        if let Some(w) = &self.workflow {
            w.validate()?;
        }
        if let Some(b) = &self.branch {
            crate::validate_ref(b)?;
        }
        if let Some(u) = &self.user {
            crate::validate_login(u)?;
        }
        if let Some(e) = &self.event
            && (e.is_empty()
                || e.len() > 64
                || !e.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
        {
            return Err(invalid_input());
        }
        if self.commit.as_ref().is_some_and(|s| !crate::is_sha(s))
            || self
                .status
                .as_ref()
                .is_some_and(|s| !STATUSES.contains(&s.as_str()))
            || self.created.as_ref().is_some_and(|s| !date_range(s))
        {
            return Err(invalid_input());
        }
        selection(self.output.as_deref(), LIST_FIELDS)
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReadInput {
    owner: String,
    repo: String,
    run_id: RunId,
    attempt: Option<Attempt>,
    job: Option<JobId>,
    #[serde(default)]
    #[serde(rename = "verbose")]
    _verbose: bool,
    #[serde(default)]
    #[serde(rename = "exitStatus")]
    _exit_status: bool,
    #[serde(rename = "_cliOutput")]
    output: Option<String>,
}
impl ReadInput {
    const MAX_REQUESTS: usize = 29;
    const MAX_JOB_PAGES: usize = 10;
    const MAX_ANNOTATION_REQUESTS: usize = 16;
    const MAX_SELECTED_JOB_REQUESTS: usize = 21;
    fn validate(&self) -> Result<(), ProviderError> {
        validate_repo(&self.owner, &self.repo)?;
        if self.run_id.0 == 0
            || self.attempt.is_some_and(|v| v.0 == 0)
            || self.job.is_some_and(|v| v.0 == 0)
        {
            return Err(invalid_input());
        }
        selection(self.output.as_deref(), READ_FIELDS)
    }
}
fn selection(value: Option<&str>, allowed: &str) -> Result<(), ProviderError> {
    if let Some(value) = value {
        let mut seen = HashSet::new();
        if value.len() > 512
            || (!value.is_empty()
                && value
                    .split(',')
                    .any(|f| !allowed.split(',').any(|a| a == f) || !seen.insert(f)))
        {
            return Err(invalid_input());
        }
    }
    Ok(())
}
fn validate_repo(owner: &str, repo: &str) -> Result<(), ProviderError> {
    crate::validate_login(owner)?;
    crate::validate_repo(repo)
}
fn date_range(s: &str) -> bool {
    fn day(s: &str) -> bool {
        s.len() == 10
            && time::format_description::parse_borrowed::<2>("[year]-[month]-[day]")
                .is_ok_and(|f| time::Date::parse(s, &f).is_ok())
    }
    if let Some((a, b)) = s.split_once("..") {
        return (a != "*" || b != "*")
            && (a == "*" || day(a))
            && (b == "*" || day(b))
            && (a == "*" || b == "*" || a <= b);
    }
    day([">=", "<=", ">", "<"]
        .iter()
        .find_map(|p| s.strip_prefix(p))
        .unwrap_or(s))
}

struct Reads<'a> {
    base: &'a Base,
    send: &'a mut dyn FnMut(Request) -> Result<Response, HttpError>,
    used: usize,
    max: usize,
}
impl Reads<'_> {
    fn get(&mut self, path: &str) -> Result<Response, ProviderError> {
        if self.used >= self.max {
            return Err(ProviderError::new(
                "request-ceiling",
                "Actions request ceiling reached",
            ));
        }
        self.used += 1;
        let response = (self.send)(crate::github_request(
            "GET",
            crate::url(self.base, path)?,
            crate::ACCEPT_JSON,
        )?)
        .map_err(|_| crate::http_failed())?;
        if response.status == 200 {
            return Ok(response);
        }
        if let Some(error) = action_status(&response) {
            return Err(error.into());
        }
        Err(crate::status_error(&response))
    }

    fn value(&mut self, path: &str) -> Result<Value, ProviderError> {
        crate::decode(&self.get(path)?.body)
    }
}

fn action_status(response: &Response) -> Option<ActionError> {
    match response.status {
        404 => Some(ActionError::NotFoundOrNotAccessible),
        403 if crate::rate_limit_exhausted(response) => Some(ActionError::RateLimit),
        403 => Some(ActionError::Permission),
        429 => Some(ActionError::RateLimit),
        _ => None,
    }
}

#[derive(Default)]
struct Coverage {
    returned: usize,
    total: Option<u64>,
    reasons: Vec<&'static str>,
    notes: Vec<&'static str>,
    page_error: Option<String>,
}
impl Coverage {
    fn mark(&mut self, reason: &'static str) {
        if !self.reasons.contains(&reason) {
            self.reasons.push(reason);
        }
    }
    fn json(&self) -> Value {
        json!({"returnedCount":self.returned,"total_count":self.total,"incomplete":!self.reasons.is_empty(),"reasons":self.reasons,"notes":self.notes,"pageError":self.page_error})
    }
}
fn pages(
    reads: &mut Reads<'_>,
    path: &str,
    field: &str,
    bound: usize,
    limit: usize,
) -> Result<(Vec<Value>, Coverage), ProviderError> {
    pages_with(reads, path, field, bound, limit, false, |value| {
        Ok(Some(value.clone()))
    })
}
fn pages_with(
    reads: &mut Reads<'_>,
    path: &str,
    field: &str,
    bound: usize,
    limit: usize,
    limit_note: bool,
    mut project: impl FnMut(&Value) -> Result<Option<Value>, ProviderError>,
) -> Result<(Vec<Value>, Coverage), ProviderError> {
    let mut result = Vec::new();
    let mut coverage = Coverage::default();
    let mut seen = HashSet::new();
    for page in 1..=bound {
        let response = match reads.get(&format!(
            "{path}{}page={page}&per_page=100",
            if path.ends_with('?') { "" } else { "&" }
        )) {
            Ok(response) => response,
            Err(error) if limit_note && coverage.total.is_some() => {
                coverage.mark("page-failed");
                coverage.page_error = Some(
                    dekopon_provider_sdk::provider::Failure::code(&error)
                        .as_str()
                        .to_owned(),
                );
                break;
            }
            Err(error) => return Err(error),
        };
        let value: Value = crate::decode(&response.body)?;
        let total = value["total_count"].as_u64().ok_or_else(invalid_response)?;
        let items = value[field].as_array().ok_or_else(invalid_response)?;
        if items.len() > 100 || total < (items.len() as u64) {
            return Err(invalid_response());
        }
        if coverage
            .total
            .replace(total)
            .is_some_and(|old| old != total)
        {
            coverage.mark("count-changed");
        }
        for item in items {
            let id = positive(item, "id")?;
            if !seen.insert(id) {
                coverage.mark("duplicate-id");
                continue;
            }
            if result.len() < limit {
                if let Some(row) = project(item)? {
                    result.push(row);
                }
                if limit_note && result.len() == limit {
                    break;
                }
            } else {
                coverage.mark("limit");
            }
        }
        let next = crate::has_next_link(&response);
        if result.len() >= limit {
            if limit_note {
                if next || total > seen.len() as u64 {
                    coverage.notes.push("limit");
                }
                if total < seen.len() as u64 || (next && total == seen.len() as u64) {
                    coverage.mark("count-mismatch");
                }
            } else if next || total > result.len() as u64 {
                coverage.mark("limit");
            }
            break;
        }
        if !next {
            if total != seen.len() as u64 {
                coverage.mark("count-mismatch");
            }
            break;
        }
        if page == bound {
            coverage.mark("page-bound");
        }
    }
    coverage.returned = result.len();
    Ok((result, coverage))
}
fn root(owner: &str, repo: &str) -> String {
    format!(
        "/repos/{}/{}",
        crate::percent_encode(owner),
        crate::percent_encode(repo)
    )
}
fn repository(reads: &mut Reads<'_>, root: &str) -> Result<(), ProviderError> {
    let repo = reads.value(root)?;
    positive(&repo, "id")?;
    Ok(())
}

pub(crate) fn list(
    input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let input: ListInput = serde_json::from_value(input).map_err(|_| invalid_input())?;
    input.validate()?;
    let mut reads = Reads {
        base,
        send,
        used: 0,
        max: ListInput::MAX_REQUESTS,
    };
    let root = root(&input.owner, &input.repo);
    repository(&mut reads, &root)?;
    let (workflows, workflow_coverage) = pages(
        &mut reads,
        &format!("{root}/actions/workflows?"),
        "workflows",
        ListInput::MAX_WORKFLOW_PAGES,
        1000,
    )?;
    let mut states = BTreeMap::new();
    let mut selected = None;
    for workflow in &workflows {
        let id = positive(workflow, "id")?;
        let state = string(workflow, "state")?;
        if ![
            "active",
            "deleted",
            "disabled_fork",
            "disabled_inactivity",
            "disabled_manually",
        ]
        .contains(&state)
        {
            return Err(invalid_response());
        }
        if input.workflow.as_ref().is_some_and(|w| w.matches(workflow)) {
            selected = Some(id);
        }
        states.insert(id, state);
    }
    let mut coverage_reasons = workflow_coverage.reasons.clone();
    let target = match (&input.workflow, selected) {
        (Some(_), Some(id)) => format!("{root}/actions/workflows/{id}/runs?"),
        (Some(_), None) if !workflow_coverage.reasons.is_empty() => {
            return Ok(
                json!({"runs":[],"coverage":{"returnedCount":0,"total_count":null,"incomplete":true,"reasons":["workflow-unresolved"]},"workflowCoverage":workflow_coverage.json(),"requestsUsed":reads.used}),
            );
        }
        (Some(_), None) => return Err(ActionError::NotFoundOrNotAccessible.into()),
        (None, _) => format!("{root}/actions/runs?"),
    };
    let mut path = target;
    for (key, value) in [
        ("branch", input.branch.as_ref()),
        ("actor", input.user.as_ref()),
        ("event", input.event.as_ref()),
        ("created", input.created.as_ref()),
        ("head_sha", input.commit.as_ref()),
        ("status", input.status.as_ref()),
    ] {
        if let Some(v) = value {
            path.push_str(&format!(
                "{}{key}={}",
                if path.ends_with('?') { "" } else { "&" },
                crate::percent_encode(v)
            ));
        }
    }
    let (output, mut coverage) = pages_with(
        &mut reads,
        &path,
        "workflow_runs",
        ListInput::MAX_RUN_PAGES,
        input.limit,
        true,
        |run| {
            let row = run_row(run, None, None)?;
            let workflow = positive(run, "workflow_id")?;
            let eligible = if input.all {
                true
            } else {
                match states.get(&workflow) {
                    Some(state) => *state == "active",
                    None => {
                        coverage_reasons.push("workflow-unresolved");
                        false
                    }
                }
            };
            Ok(eligible.then_some(row))
        },
    )?;
    coverage.returned = output.len();
    for reason in coverage_reasons {
        coverage.mark(reason);
    }
    Ok(
        json!({"runs":output,"coverage":coverage.json(),"workflowCoverage":workflow_coverage.json(),"requestsUsed":reads.used}),
    )
}

fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str, ProviderError> {
    v[key].as_str().ok_or_else(invalid_response)
}
fn positive(v: &Value, key: &str) -> Result<u64, ProviderError> {
    v[key]
        .as_u64()
        .filter(|id| *id > 0)
        .ok_or_else(invalid_response)
}
fn clean(s: &str, max: usize) -> String {
    crate::marked_text(
        &s.chars().filter(|c| !c.is_control()).collect::<String>(),
        max,
    )
}
fn text(v: &Value, key: &str, max: usize) -> Result<String, ProviderError> {
    Ok(clean(string(v, key)?, max))
}
fn stamp(v: &Value, key: &str) -> Result<Value, ProviderError> {
    match &v[key] {
        Value::Null => Ok(Value::Null),
        Value::String(s) if s.len() <= 64 => {
            OffsetDateTime::parse(s, &Rfc3339).map_err(|_| invalid_response())?;
            Ok(json!(s))
        }
        _ => Err(invalid_response()),
    }
}
fn duration(start: &Value, end: &Value) -> Result<Option<i64>, ProviderError> {
    let (Some(start), Some(end)) = (start.as_str(), end.as_str()) else {
        return Ok(None);
    };
    let start = OffsetDateTime::parse(start, &Rfc3339).map_err(|_| invalid_response())?;
    let end = OffsetDateTime::parse(end, &Rfc3339).map_err(|_| invalid_response())?;
    let d = end
        .unix_timestamp()
        .checked_sub(start.unix_timestamp())
        .filter(|d| *d >= 0)
        .ok_or_else(invalid_response)?;
    Ok(Some(d))
}
fn state(v: &Value) -> Result<(&str, Value), ProviderError> {
    let status = string(v, "status")?;
    if !STATUSES[..6].contains(&status) {
        return Err(invalid_response());
    }
    let conclusion = match &v["conclusion"] {
        Value::Null => Value::Null,
        Value::String(s) if CONCLUSIONS.contains(&s.as_str()) => json!(s),
        _ => return Err(invalid_response()),
    };
    if status == "completed" && conclusion.is_null() {
        return Err(invalid_response());
    }
    Ok((status, conclusion))
}
fn run_row(
    run: &Value,
    expected_id: Option<u64>,
    expected_attempt: Option<u32>,
) -> Result<Value, ProviderError> {
    let id = positive(run, "id")?;
    let attempt = positive(run, "run_attempt")?;
    if attempt > u32::MAX as u64
        || expected_id.is_some_and(|v| v != id)
        || expected_attempt.is_some_and(|v| u64::from(v) != attempt)
        || !crate::is_sha(string(run, "head_sha")?)
    {
        return Err(invalid_response());
    }
    let (status, conclusion) = state(run)?;
    let start = stamp(run, "run_started_at")?;
    let updated = stamp(run, "updated_at")?;
    let wall = if status == "completed" {
        duration(&start, &updated)?
    } else {
        None
    };
    let branch = match &run["head_branch"] {
        Value::Null => Value::Null,
        Value::String(s) => json!(clean(s, crate::MAX_REF_BYTES)),
        _ => return Err(invalid_response()),
    };
    Ok(
        json!({"databaseId":id,"attempt":attempt,"run_attempt":attempt,"headSha":string(run,"head_sha")?,"headBranch":branch,"event":text(run,"event",64)?,"status":status,"conclusion":conclusion,"workflowName":text(run,"name",256)?,"displayTitle":text(run,"display_title",256)?,"workflowDatabaseId":positive(run,"workflow_id")?,"createdAt":stamp(run,"created_at")?,"startedAt":start,"updatedAt":updated,"wallClockSeconds":wall,"url":text(run,"html_url",2048)?}),
    )
}
fn job_row(job: &Value, run: &Value) -> Result<Value, ProviderError> {
    let id = positive(job, "id")?;
    if job["run_id"] != run["databaseId"]
        || job["run_attempt"] != run["attempt"]
        || job["head_sha"] != run["headSha"]
    {
        return Err(invalid_response());
    }
    let (status, conclusion) = state(job)?;
    let start = stamp(job, "started_at")?;
    let end = stamp(job, "completed_at")?;
    let elapsed = if status == "completed" {
        duration(&start, &end)?
    } else {
        None
    };
    let steps = job["steps"].as_array().ok_or_else(invalid_response)?;
    if steps.len() > 1000 {
        return Err(invalid_response());
    }
    let mut projected = Vec::new();
    let mut seen = HashSet::new();
    for step in steps {
        let number = positive(step, "number")?;
        if !seen.insert(number) {
            return Err(invalid_response());
        }
        let (status, conclusion) = state(step)?;
        projected.push(json!({"number":number,"name":text(step,"name",256)?,"status":status,"conclusion":conclusion,"startedAt":stamp(step,"started_at")?,"completedAt":stamp(step,"completed_at")?}));
    }
    Ok(
        json!({"databaseId":id,"runId":run["databaseId"],"attempt":run["attempt"],"run_attempt":run["attempt"],"headSha":run["headSha"],"name":text(job,"name",256)?,"status":status,"conclusion":conclusion,"startedAt":start,"completedAt":end,"durationSeconds":elapsed,"steps":projected,"annotations":[],"annotationCoverage":{"returnedCount":0,"total_count":0,"incomplete":false,"reasons":[]}}),
    )
}

pub(crate) fn read(
    input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let input: ReadInput = serde_json::from_value(input).map_err(|_| invalid_input())?;
    input.validate()?;
    let max = if input.job.is_some() {
        ReadInput::MAX_SELECTED_JOB_REQUESTS
    } else {
        ReadInput::MAX_REQUESTS
    };
    let mut reads = Reads {
        base,
        send,
        used: 0,
        max,
    };
    let root = root(&input.owner, &input.repo);
    repository(&mut reads, &root)?;
    let latest = reads.value(&format!("{root}/actions/runs/{}", input.run_id))?;
    let latest_row = run_row(&latest, Some(input.run_id.0), None)?;
    let latest_attempt = latest_row["attempt"]
        .as_u64()
        .ok_or_else(invalid_response)? as u32;
    let attempt = input.attempt.map_or(latest_attempt, |v| v.0);
    if attempt > latest_attempt {
        return Err(ActionError::NotFoundOrNotAccessible.into());
    }
    let mut run = if input.attempt.is_some() {
        let value = reads.value(&format!(
            "{root}/actions/runs/{}/attempts/{attempt}",
            input.run_id
        ))?;
        let row = run_row(&value, Some(input.run_id.0), Some(attempt))?;
        if row["headSha"] != latest_row["headSha"] {
            return Err(invalid_response());
        }
        row
    } else {
        latest_row
    };
    let jobs_path = format!(
        "{root}/actions/runs/{}/attempts/{attempt}/jobs?",
        input.run_id
    );
    let (raw_jobs, mut coverage) = if let Some(job) = input.job {
        let value = reads.value(&format!("{root}/actions/jobs/{job}"))?;
        if positive(&value, "id")? != job.0 {
            return Err(invalid_response());
        }
        job_row(&value, &run)?;
        let count = reads.value(&format!("{jobs_path}per_page=1&page=1"))?;
        let total = positive(&count, "total_count")?;
        let rows = count["jobs"].as_array().ok_or_else(invalid_response)?;
        if rows.len() != 1 {
            return Err(invalid_response());
        }
        job_row(&rows[0], &run)?;
        if total == 1 && rows[0]["id"] != value["id"] {
            return Err(invalid_response());
        }
        let c = Coverage {
            returned: 1,
            total: Some(total),
            reasons: Vec::new(),
            ..Default::default()
        };
        (vec![value], c)
    } else {
        pages(
            &mut reads,
            &jobs_path,
            "jobs",
            ReadInput::MAX_JOB_PAGES,
            1000,
        )?
    };
    let mut jobs = Vec::new();
    let mut sum = 0i64;
    let mut completed = 0usize;
    let mut annotation_requests = 0;
    for raw in raw_jobs {
        let mut job = job_row(&raw, &run)?;
        if let Some(seconds) = job["durationSeconds"].as_i64() {
            sum = sum.checked_add(seconds).ok_or_else(invalid_response)?;
            completed += 1;
        }
        if job["conclusion"] == "failure"
            || job["conclusion"] == "timed_out"
            || job["conclusion"] == "startup_failure"
        {
            let (annotations, ac) =
                annotations(&mut reads, &root, &raw, &run, &mut annotation_requests)?;
            if !ac.reasons.is_empty() {
                coverage.mark("annotations-incomplete");
            }
            job["annotations"] = json!(annotations);
            job["annotationCoverage"] = ac.json();
        }
        jobs.push(job);
    }
    let partial = coverage.total != Some(completed as u64);
    run["latestAttempt"] = json!(latest_attempt);
    run["existingAttempts"] = json!({"first":1,"last":latest_attempt});
    run["jobs"] = json!(jobs);
    run["completedJobDurationSeconds"] = json!(sum);
    run["jobDurationCoverage"] = json!({"completedJobs":completed,"jobsRead":coverage.returned,"totalJobs":coverage.total,"partial":partial});
    run["coverage"] = coverage.json();
    run["coverage"]["scope"] = json!(if input.job.is_some() {
        "selected-job"
    } else {
        "attempt-jobs"
    });
    run["requestsUsed"] = json!(reads.used);
    Ok(run)
}
fn check_id(raw: &Value, root: &str) -> Result<u64, ProviderError> {
    let value = string(raw, "check_run_url")?;
    let (prefix, id) = value.rsplit_once('/').ok_or_else(invalid_response)?;
    let id = id
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(invalid_response)?;
    if !prefix
        .to_ascii_lowercase()
        .ends_with(&format!("{root}/check-runs").to_ascii_lowercase())
        || value.len() > 2048
        || value.chars().any(char::is_control)
    {
        return Err(invalid_response());
    }
    Ok(id)
}
fn annotations(
    reads: &mut Reads<'_>,
    root: &str,
    raw: &Value,
    run: &Value,
    used: &mut usize,
) -> Result<(Vec<Value>, Coverage), ProviderError> {
    let mut coverage = Coverage::default();
    let mut result = Vec::new();
    if *used >= ReadInput::MAX_ANNOTATION_REQUESTS {
        coverage.mark("annotation-bound");
        return Ok((result, coverage));
    }
    let id = check_id(raw, root)?;
    *used += 1;
    let check = reads.value(&format!("{root}/check-runs/{id}"))?;
    if positive(&check, "id")? != id || check["head_sha"] != run["headSha"] {
        return Err(invalid_response());
    }
    let total = check["output"]["annotations_count"]
        .as_u64()
        .ok_or_else(invalid_response)?;
    coverage.total = Some(total);
    let mut page = 1;
    while (result.len() as u64) < total {
        if *used >= ReadInput::MAX_ANNOTATION_REQUESTS {
            coverage.mark("annotation-bound");
            break;
        }
        *used += 1;
        let response = reads.get(&format!(
            "{root}/check-runs/{id}/annotations?per_page=100&page={page}"
        ))?;
        let rows: Vec<Value> = crate::decode(&response.body)?;
        if rows.len() > 100 {
            return Err(invalid_response());
        }
        for row in rows {
            let start = positive(&row, "start_line")?;
            let end = positive(&row, "end_line")?;
            if end < start {
                return Err(invalid_response());
            }
            let level = string(&row, "annotation_level")?;
            if !["notice", "warning", "failure"].contains(&level) {
                return Err(invalid_response());
            }
            result.push(json!({"untrusted":true,"path":text(&row,"path",1024)?,"startLine":start,"endLine":end,"level":level,"message":text(&row,"message",4096)?,"title":match &row["title"] {Value::Null=>Value::Null,Value::String(s)=>json!(clean(s,256)),_=>return Err(invalid_response())}}));
        }
        if !crate::has_next_link(&response) {
            break;
        }
        if result.len() as u64 >= total {
            coverage.mark("count-mismatch");
            break;
        }
        page += 1;
    }
    coverage.returned = result.len();
    if result.len() as u64 != total {
        coverage.mark("count-mismatch");
    }
    Ok((result, coverage))
}

pub(crate) fn execute(
    id: &str,
    mut input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
    out: &mut dyn Write,
) -> Result<(), ProviderError> {
    let selection = input
        .get("_cliOutput")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let verbose = input["verbose"] == true;
    let exit = input["exitStatus"] == true;
    if let Some(object) = input.as_object_mut() {
        object.remove("_cliOutput");
    }
    let value = if id == crate::ids::RUN_LIST {
        list(input, base, send)?
    } else {
        read(input, base, send)?
    };
    let rendered = match selection.as_deref() {
        Some("") => render(&value, id == crate::ids::RUN_LIST, verbose)?,
        Some(fields) => {
            serde_json::to_string(&selected(&value, fields, id == crate::ids::RUN_LIST))
                .map_err(|_| invalid_response())?
                + "\n"
        }
        None => serde_json::to_string(&value).map_err(|_| invalid_response())? + "\n",
    };
    out.write_all(rendered.as_bytes())
        .map_err(|_| invalid_response())?;
    let reasons = value["coverage"]["reasons"]
        .as_array()
        .ok_or_else(invalid_response)?;
    if !reasons.is_empty() {
        return Err(ActionError::Incomplete.into());
    }
    if exit
        && value["status"] == "completed"
        && ![json!("success"), json!("neutral"), json!("skipped")].contains(&value["conclusion"])
    {
        return Err(ActionError::RunFailed.into());
    }
    Ok(())
}
fn selected(value: &Value, fields: &str, list: bool) -> Value {
    let mut value = value.clone();
    let project = |row: &Value| {
        let mut selected = Map::new();
        for field in fields.split(',').chain([
            "databaseId",
            "attempt",
            "run_attempt",
            "headSha",
            "headBranch",
            "event",
            "status",
            "conclusion",
            "workflowName",
            "coverage",
            "jobDurationCoverage",
        ]) {
            if let Some(v) = row.get(field) {
                selected.insert(field.into(), v.clone());
            }
        }
        Value::Object(selected)
    };
    if list {
        value["runs"] = Value::Array(
            value["runs"]
                .as_array()
                .expect("projected runs")
                .iter()
                .map(project)
                .collect(),
        );
        value
    } else {
        let mut row = project(&value);
        for f in ["latestAttempt", "existingAttempts", "requestsUsed"] {
            row[f] = value[f].clone();
        }
        row
    }
}
fn render(value: &Value, list: bool, verbose: bool) -> Result<String, ProviderError> {
    let rows = if list {
        value["runs"]
            .as_array()
            .ok_or_else(invalid_response)?
            .as_slice()
    } else {
        std::slice::from_ref(value)
    };
    let mut text = String::new();
    for row in rows {
        text.push_str(&format!(
            "{}\tattempt {}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            row["databaseId"],
            row["attempt"],
            row["headSha"].as_str().unwrap_or(""),
            row["headBranch"].as_str().unwrap_or(""),
            row["event"].as_str().unwrap_or(""),
            row["status"].as_str().unwrap_or(""),
            row["conclusion"].as_str().unwrap_or(""),
            row["workflowName"].as_str().unwrap_or("")
        ));
    }
    if !list {
        text.push_str(&format!("Latest attempt: {}; existing attempts: 1..{}\nWall-clock seconds: {}\nCompleted job duration seconds: {} ({} of {} jobs; partial: {})\n",value["latestAttempt"],value["latestAttempt"],value["wallClockSeconds"],value["completedJobDurationSeconds"],value["jobDurationCoverage"]["completedJobs"],value["jobDurationCoverage"]["totalJobs"],value["jobDurationCoverage"]["partial"]));
        for job in value["jobs"].as_array().ok_or_else(invalid_response)? {
            text.push_str(&format!(
                "Job {}\trun {}\tattempt {}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                job["databaseId"],
                job["runId"],
                job["attempt"],
                job["headSha"].as_str().unwrap_or(""),
                job["status"].as_str().unwrap_or(""),
                job["conclusion"].as_str().unwrap_or(""),
                job["startedAt"],
                job["completedAt"],
                job["name"].as_str().unwrap_or("")
            ));
            for step in job["steps"].as_array().ok_or_else(invalid_response)? {
                if verbose || step["conclusion"] == "failure" {
                    text.push_str(&format!(
                        "  Step {}\t{}\t{}\n",
                        step["number"],
                        step["conclusion"].as_str().unwrap_or(""),
                        step["name"].as_str().unwrap_or("")
                    ));
                }
            }
            for annotation in job["annotations"].as_array().ok_or_else(invalid_response)? {
                text.push_str(&format!(
                    "  [untrusted annotation] {}:{}-{}: {}\n",
                    annotation["path"].as_str().unwrap_or(""),
                    annotation["startLine"],
                    annotation["endLine"],
                    annotation["message"].as_str().unwrap_or("")
                ));
            }
            text.push_str(&format!(
                "Annotation coverage: {}\n",
                job["annotationCoverage"]
            ));
        }
    }
    text.push_str(&format!("Coverage: {}\n", value["coverage"]));
    Ok(text)
}

pub(crate) fn schema(read: bool) -> Value {
    let mut properties = json!({"_cliOutput":{"type":"string","maxLength":512}});
    if read {
        properties["runId"] = json!({"type":"integer","minimum":1});
        properties["attempt"] = json!({"type":"integer","minimum":1,"maximum":u32::MAX});
        properties["job"] = json!({"type":"integer","minimum":1});
        for flag in ["verbose", "exitStatus"] {
            properties[flag] = json!({"type":"boolean"});
        }
    } else {
        properties["limit"] = json!({"type":"integer","minimum":1,"maximum":100});
        properties["workflow"] =
            json!({"anyOf":[{"type":"integer","minimum":1},{"type":"string","maxLength":255}]});
        for flag in ["branch", "user", "event", "created", "commit"] {
            properties[flag] = json!({"type":"string"});
        }
        properties["status"] = json!({"type":"string","enum":STATUSES});
        properties["all"] = json!({"type":"boolean"});
    }
    crate::repo_schema(properties, if read { &["runId"] } else { &[] })
}
pub(crate) fn commands() -> Command {
    let repo = || Arg::new("repo").long("repo").short('R').required(true);
    let boolean = |name: &'static str, short: Option<char>| {
        let a = Arg::new(name).long(name).action(ArgAction::SetTrue);
        if let Some(s) = short { a.short(s) } else { a }
    };
    let mut list = Command::new("list")
        .about("Read bounded Actions run history")
        .arg(repo())
        .arg(
            Arg::new("json")
                .long("json")
                .num_args(0..=1)
                .default_missing_value(""),
        )
        .arg(
            Arg::new("limit")
                .long("limit")
                .short('L')
                .value_parser(clap::value_parser!(u64).range(1..=100)),
        )
        .arg(boolean("all", Some('a')));
    for (name, short) in [
        ("workflow", Some('w')),
        ("branch", Some('b')),
        ("user", Some('u')),
        ("event", Some('e')),
        ("created", None),
        ("commit", Some('c')),
        ("status", Some('s')),
    ] {
        let arg = Arg::new(name).long(name);
        list = list.arg(if let Some(s) = short {
            arg.short(s)
        } else {
            arg
        });
    }
    let view = Command::new("view")
        .about("Inspect a run, attempt, jobs and extracted annotations")
        .arg(repo())
        .arg(
            Arg::new("runId")
                .required(true)
                .value_parser(clap::value_parser!(u64).range(1..)),
        )
        .arg(
            Arg::new("attempt")
                .long("attempt")
                .short('a')
                .value_parser(clap::value_parser!(u32).range(1..)),
        )
        .arg(
            Arg::new("job")
                .long("job")
                .short('j')
                .value_parser(clap::value_parser!(u64).range(1..)),
        )
        .arg(boolean("verbose", Some('v')))
        .arg(boolean("exit-status", None))
        .arg(
            Arg::new("json")
                .long("json")
                .num_args(0..=1)
                .default_missing_value(""),
        );
    Command::new("run")
        .about("Inspect Actions runs and jobs")
        .subcommand_required(true)
        .subcommand(list)
        .subcommand(view)
}
pub(crate) fn dispatch(
    verb: &str,
    matches: &ArgMatches,
    input: &mut Map<String, Value>,
) -> Result<(), ProviderError> {
    if verb == "list" {
        input.insert(
            "limit".into(),
            json!(matches.get_one::<u64>("limit").copied().unwrap_or(20)),
        );
        for name in ["branch", "user", "event", "created", "commit", "status"] {
            if let Some(s) = matches.get_one::<String>(name) {
                input.insert(name.into(), json!(s));
            }
        }
        if let Some(s) = matches.get_one::<String>("workflow") {
            input.insert(
                "workflow".into(),
                s.parse::<u64>().map_or_else(|_| json!(s), |n| json!(n)),
            );
        }
        if matches.get_flag("all") {
            input.insert("all".into(), json!(true));
        }
        let value: ListInput = serde_json::from_value(json!(input)).map_err(|_| invalid_input())?;
        value.validate()?;
    } else {
        input.insert(
            "runId".into(),
            json!(matches.get_one::<u64>("runId").expect("required run")),
        );
        if let Some(a) = matches.get_one::<u32>("attempt") {
            input.insert("attempt".into(), json!(a));
        }
        if let Some(j) = matches.get_one::<u64>("job") {
            input.insert("job".into(), json!(j));
        }
        for (flag, field) in [("verbose", "verbose"), ("exit-status", "exitStatus")] {
            if matches.get_flag(flag) {
                input.insert(field.into(), json!(true));
            }
        }
        let value: ReadInput = serde_json::from_value(json!(input)).map_err(|_| invalid_input())?;
        value.validate()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dekopon_provider_sdk::provider::Failure;
    #[test]
    fn native_action_error_variants_preserve_distinct_codes_and_statuses() {
        for (variant, code, status) in [
            (
                ActionError::NotFoundOrNotAccessible,
                "not-found-or-not-accessible",
                3,
            ),
            (ActionError::Permission, "forbidden", 3),
            (ActionError::RateLimit, "rate-limited", 3),
            (ActionError::Incomplete, "incomplete-results", 3),
            (ActionError::RunFailed, "run-failed", 1),
        ] {
            let error: ProviderError = variant.into();
            assert_eq!(error.code(), code);
            assert_eq!(Failure::code(&error).status().get(), status);
        }
        for (status, headers, variant) in [
            (404, vec![], ActionError::NotFoundOrNotAccessible),
            (403, vec![], ActionError::Permission),
            (429, vec![], ActionError::RateLimit),
        ] {
            assert_eq!(
                action_status(&Response {
                    status,
                    headers: headers.clone(),
                    body: vec![]
                }),
                Some(variant)
            );
            let expected_code = action_status(&Response {
                status,
                headers: headers.clone(),
                body: vec![],
            })
            .map(ProviderError::from)
            .expect("typed error");
            let calls = std::cell::Cell::new(0);
            let mut send = |request: Request| {
                calls.set(calls.get() + 1);
                assert_eq!(request.method, "GET");
                assert_eq!(request.uri, "https://api.github.com/repos/o/r");
                assert!(
                    !request
                        .headers
                        .iter()
                        .any(|header| header.name.eq_ignore_ascii_case("authorization"))
                );
                Ok(Response {
                    status,
                    headers: headers.clone(),
                    body: vec![],
                })
            };
            let mut reads = Reads {
                base: &crate::GITHUB_API,
                send: &mut send,
                used: 0,
                max: 1,
            };
            let error = reads.get("/repos/o/r").expect_err("typed failure");
            assert_eq!(error.code(), expected_code.code());
            assert_eq!(calls.get(), 1);
        }
    }
}
