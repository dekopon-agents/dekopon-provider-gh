use dekopon_provider_sdk::provider::{self, Capability, Http, Proposal, Settings, Stdout};
use dekopon_provider_sdk::{EffectKind, RiskLevel};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{Read, Write};

use crate::{Gh, GhSettings, error::ProviderError};

// The established per-capability schemas remain the model-facing contract. Each typed input
// has its own schema and keeps the existing native validation at the HTTP boundary.
macro_rules! capability {
    ($name:ident, $input:ident, $id:expr, $short:expr, $description:expr, $handler:path, $effect:expr, $risk:expr) => {
        #[derive(Serialize)]
        #[serde(transparent)]
        pub struct $input(Value);
        impl<'de> Deserialize<'de> for $input {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let input = Value::deserialize(deserializer)?;
                if (matches!($id, crate::ids::USER_SELF_READ) && input != Value::Object(Default::default()))
                    || input.get("_cli").is_some()
                    || crate::output::validate_selection($id, input.get("_cliOutput"), input.get("_cliKind")).is_err()
                {
                    return Err(serde::de::Error::custom("invalid gh output selection"));
                }
                Ok(Self(input))
            }
        }
        impl JsonSchema for $input {
            fn schema_name() -> std::borrow::Cow<'static, str> {
                stringify!($input).into()
            }
            fn json_schema(_: &mut SchemaGenerator) -> Schema {
                let mut schema = crate::capabilities()
                    .into_iter()
                    .find(|c| c.id.as_str() == $id)
                    .expect("declared gh capability")
                    .input_schema;
                if matches!($id, "gh.auth.status" | "gh.pull-request.list" | "gh.pull-request.read" | "gh.issue.list" | "gh.issue.read" | "gh.repo.read" | "gh.repo.list" | "gh.content.read" | "gh.search.prs" | "gh.search.issues" | "gh.search.repos" | "gh.search.code" | "gh.search.commits" | "gh.pull-request.status") {
                    schema["properties"]["_cliOutput"] = serde_json::json!({"type": "string", "maxLength": 512});
                    if matches!($id, "gh.pull-request.list" | "gh.issue.list" | "gh.repo.list" | "gh.search.prs" | "gh.search.issues" | "gh.search.repos" | "gh.search.code" | "gh.search.commits") {
                        schema["properties"]["_cliLimit"] = serde_json::json!({"type": "integer", "minimum": 1, "maximum": 100});
                    }
                    if $id == "gh.content.read" {
                        schema["properties"]["_cliKind"] = serde_json::json!({"type": "string", "enum": ["read-file", "read-dir"]});
                    }
                }
                if has_piped_body($id) {
                    schema["properties"][STDIN_BODY_FIELD] = serde_json::json!({
                        "type": "boolean", "const": true,
                        "description": "Internal marker for --body-file -; set by command rewrite, not literal body text."
                    });
                }
                serde_json::from_value(schema).expect("valid gh schema")
            }
        }
        pub struct $name;
        impl $name {
            pub(crate) fn run_with(
                input: $input,
                base: &crate::Base,
                send: &mut dyn FnMut(crate::Request) -> Result<crate::Response, crate::HttpError>,
                out: &mut dyn Write,
            ) -> Result<(), ProviderError> {
                let mut input = input.0;
                if input.get(STDIN_BODY_FIELD).is_some() {
                    if !has_piped_body($id)
                        || input.get(STDIN_BODY_FIELD) != Some(&Value::Bool(true))
                        || input.get("body").and_then(Value::as_str) != Some(STDIN_BODY)
                    {
                        return Err(crate::invalid_input());
                    }
                    input.as_object_mut().expect("input is an object").remove(STDIN_BODY_FIELD);
                    let mut reader = provider::stdin().ok_or_else(|| {
                        ProviderError::new(
                            "usage",
                            "gh: --body-file -: nothing was piped into the word",
                        )
                    })?;
                    let mut body = Vec::new();
                    reader
                        .by_ref()
                        .take((crate::MAX_BODY_IN_BYTES + 1) as u64)
                        .read_to_end(&mut body)
                        .map_err(|_| {
                            ProviderError::new("invalid-input", "could not read piped body")
                        })?;
                    if body.len() > crate::MAX_BODY_IN_BYTES {
                        return Err(crate::invalid_input());
                    }
                    let body = String::from_utf8(body).map_err(|_| crate::invalid_input())?;
                    input["body"] = Value::String(body);
                }
                let selection = input.as_object_mut().and_then(|object| object.remove("_cliOutput"));
                let kind = input.as_object_mut().and_then(|object| object.remove("_cliKind"));
                let mut render_input = input.clone();
                if let Some(kind) = kind { render_input["_cliKind"] = kind; }
                if let Some(Value::String(fields)) = selection.as_ref().filter(|_| $id != crate::ids::AUTH_STATUS) {
                    if matches!($id, "gh.repo.read" | "gh.content.read") {
                        input["_cli"] = Value::Bool(true);
                    } else if $id.starts_with("gh.search.") {
                        input["_cliOutput"] = Value::String(fields.clone());
                    } else {
                        input["_cli"] = Value::String(fields.clone());
                    }
                }
                let result = $handler(input, base, send)?;
                if $id == crate::ids::PR_STATUS {
                    crate::output::require_checks(&result)?;
                }
                if let Some(Value::String(selection)) = selection {
                    let rendered = crate::output::render($id, &render_input, &result, &selection)?;
                    out.write_all(rendered.as_bytes()).map_err(|_| crate::invalid_response())?;
                    if $id == crate::ids::PR_STATUS && selection.is_empty() {
                        return crate::output::checks_verdict(&result);
                    }
                    return Ok(());
                }
                serde_json::to_writer(&mut *out, &result).map_err(|_| {
                    ProviderError::new("invalid-response", "could not write output")
                })?;
                out.write_all(b"\n").map_err(|_| {
                    ProviderError::new("invalid-response", "could not write output")
                })?;
                Ok(())
            }
        }
        impl Capability for $name {
            type Provider = Gh;
            const NAME: &'static str = $short;
            const DESCRIPTION: &'static str = $description;
            const EFFECT: EffectKind = $effect;
            const RISK: RiskLevel = $risk;
            type Input = $input;
            type Needs = (Settings<GhSettings>, Http);
            type Error = ProviderError;
            fn run(
                input: Self::Input,
                (settings, http): Self::Needs,
                out: &mut Stdout,
            ) -> Result<(), Self::Error> {
                let base = settings.into_inner().base();
                Self::run_with(input, &base, &mut |request| http.send(request), out)
            }
        }
    };
}

// Only the internal flag together with this body value requests stdin. A literal body of these
// bytes, from either argv or a direct invocation, remains literal even when stdin is piped.
pub(crate) const STDIN_BODY: &str = "\u{0}gh:body-file:-\u{0}";
pub(crate) const STDIN_BODY_FIELD: &str = "_stdinBody";

fn has_piped_body(id: &str) -> bool {
    matches!(
        id,
        "gh.issue.comment"
            | "gh.issue.comment-edit"
            | "gh.issue.comment-edit-or-create"
            | "gh.pull-request.merge"
            | "gh.pull-request.approve"
            | "gh.pull-request.comment"
            | "gh.pull-request.request-changes"
    )
}

capability!(
    AuthStatus,
    AuthStatusInput,
    crate::ids::AUTH_STATUS,
    "auth.status",
    "Reads the credential core quota, token expiration, and scopes",
    crate::auth::status,
    EffectKind::ReadOnly,
    RiskLevel::Low
);
capability!(
    ContentRead,
    ContentReadInput,
    "gh.content.read",
    "content.read",
    "Reads one file or directory listing at a path and optional ref",
    crate::content::read,
    EffectKind::ReadOnly,
    RiskLevel::Low
);
capability!(
    PrList,
    PrListInput,
    "gh.pull-request.list",
    "pull-request.list",
    "Lists pull requests with optional state and author filters",
    crate::pulls::list,
    EffectKind::ReadOnly,
    RiskLevel::Low
);
capability!(
    PrRead,
    PrReadInput,
    "gh.pull-request.read",
    "pull-request.read",
    "Reads one pull request's metadata, state, and head/base",
    crate::pulls::read,
    EffectKind::ReadOnly,
    RiskLevel::Low
);
capability!(
    PrDiff,
    PrDiffInput,
    "gh.pull-request.diff",
    "pull-request.diff",
    "Reads one pull request's unified diff, truncated with a marker",
    crate::pulls::diff,
    EffectKind::ReadOnly,
    RiskLevel::Low
);
capability!(
    PrStatus,
    PrStatusInput,
    "gh.pull-request.status",
    "pull-request.status",
    "Reads real check runs, legacy statuses, and Actions names at one pull request's head",
    crate::pulls::status,
    EffectKind::ReadOnly,
    RiskLevel::Low
);
capability!(
    PrApprove,
    PrApproveInput,
    "gh.pull-request.approve",
    "pull-request.approve",
    "Submits an APPROVE review pinned to the verified head SHA",
    crate::reviews::approve,
    EffectKind::ExternalWrite,
    RiskLevel::High
);
capability!(
    PrComment,
    PrCommentInput,
    "gh.pull-request.comment",
    "pull-request.comment",
    "Submits a COMMENT review pinned to the verified head SHA",
    crate::reviews::comment,
    EffectKind::ExternalWrite,
    RiskLevel::Medium
);
capability!(
    PrRequestChanges,
    PrRequestChangesInput,
    "gh.pull-request.request-changes",
    "pull-request.request-changes",
    "Submits a REQUEST_CHANGES review pinned to the verified head SHA",
    crate::reviews::request_changes,
    EffectKind::ExternalWrite,
    RiskLevel::Medium
);
capability!(
    PrMerge,
    PrMergeInput,
    "gh.pull-request.merge",
    "pull-request.merge",
    "Merges one pull request, pinned to the verified head SHA",
    crate::reviews::merge,
    EffectKind::ExternalWrite,
    RiskLevel::High
);
capability!(
    RepoRead,
    RepoReadInput,
    "gh.repo.read",
    "repo.read",
    "Reads repository metadata: default branch, visibility, and flags",
    crate::repos::repo,
    EffectKind::ReadOnly,
    RiskLevel::Low
);
capability!(
    RepoList,
    RepoListInput,
    crate::ids::REPO_LIST,
    "repo.list",
    "Lists repositories belonging to a named owner",
    crate::repos::list,
    EffectKind::ReadOnly,
    RiskLevel::Low
);
capability!(
    SearchPrs,
    SearchPrsInput,
    crate::ids::SEARCH_PRS,
    "search.prs",
    "Search scoped pull requests",
    crate::search::prs,
    EffectKind::ReadOnly,
    RiskLevel::Low
);
capability!(
    SearchIssues,
    SearchIssuesInput,
    crate::ids::SEARCH_ISSUES,
    "search.issues",
    "Search scoped issues",
    crate::search::issues,
    EffectKind::ReadOnly,
    RiskLevel::Low
);
capability!(
    SearchRepos,
    SearchReposInput,
    crate::ids::SEARCH_REPOS,
    "search.repos",
    "Search repositories for an owner",
    crate::search::repos,
    EffectKind::ReadOnly,
    RiskLevel::Low
);
capability!(
    SearchCode,
    SearchCodeInput,
    crate::ids::SEARCH_CODE,
    "search.code",
    "Search scoped code",
    crate::search::code,
    EffectKind::ReadOnly,
    RiskLevel::Low
);
capability!(
    SearchCommits,
    SearchCommitsInput,
    crate::ids::SEARCH_COMMITS,
    "search.commits",
    "Search scoped commits",
    crate::search::commits,
    EffectKind::ReadOnly,
    RiskLevel::Low
);
capability!(
    BranchRead,
    BranchReadInput,
    "gh.branch.read",
    "branch.read",
    "Reads one branch's bounded GitHub API response",
    crate::repos::branch,
    EffectKind::ReadOnly,
    RiskLevel::Low
);
capability!(
    CommitRead,
    CommitReadInput,
    "gh.commit.read",
    "commit.read",
    "Reads one commit's bounded GitHub API response",
    crate::repos::commit,
    EffectKind::ReadOnly,
    RiskLevel::Low
);
capability!(
    UserSelfRead,
    UserSelfReadInput,
    crate::ids::USER_SELF_READ,
    "user-self.read",
    "Reads the credential's bounded GitHub user identity",
    crate::repos::self_user,
    EffectKind::ReadOnly,
    RiskLevel::Low
);
capability!(
    UserRead,
    UserReadInput,
    "gh.user.read",
    "user.read",
    "Reads one user's bounded GitHub API response",
    crate::repos::user,
    EffectKind::ReadOnly,
    RiskLevel::Low
);
capability!(
    IssueRead,
    IssueReadInput,
    "gh.issue.read",
    "issue.read",
    "Reads one issue with a bounded body",
    crate::issues::read,
    EffectKind::ReadOnly,
    RiskLevel::Low
);
capability!(
    IssueList,
    IssueListInput,
    "gh.issue.list",
    "issue.list",
    "Lists issues (GitHub includes pull requests; each item is flagged)",
    crate::issues::list,
    EffectKind::ReadOnly,
    RiskLevel::Low
);
capability!(
    IssueComment,
    IssueCommentInput,
    "gh.issue.comment",
    "issue.comment",
    "Posts one comment on an issue or pull request",
    crate::issues::comment,
    EffectKind::ExternalWrite,
    RiskLevel::Medium
);

capability!(
    IssueCommentEdit,
    IssueCommentEditInput,
    crate::ids::ISSUE_COMMENT_EDIT,
    "issue.comment-edit",
    "Edits the current user's last issue comment",
    crate::issue_comments::edit,
    EffectKind::ExternalWrite,
    RiskLevel::Medium
);
capability!(
    IssueCommentDelete,
    IssueCommentDeleteInput,
    crate::ids::ISSUE_COMMENT_DELETE,
    "issue.comment-delete",
    "Deletes the current user's last issue comment",
    crate::issue_comments::delete,
    EffectKind::ExternalWrite,
    RiskLevel::Medium
);
capability!(
    IssueCommentEditOrCreate,
    IssueCommentEditOrCreateInput,
    crate::ids::ISSUE_COMMENT_EDIT_OR_CREATE,
    "issue.comment-edit-or-create",
    "Edits the current user's last issue comment or creates one",
    crate::issue_comments::edit_or_create,
    EffectKind::ExternalWrite,
    RiskLevel::Medium
);

pub(crate) fn proposal(id: &str, input: Value) -> Proposal<Gh> {
    match id {
        crate::ids::AUTH_STATUS => Proposal::to::<AuthStatus>(AuthStatusInput(input)),
        "gh.content.read" => Proposal::to::<ContentRead>(ContentReadInput(input)),
        "gh.pull-request.list" => Proposal::to::<PrList>(PrListInput(input)),
        "gh.pull-request.read" => Proposal::to::<PrRead>(PrReadInput(input)),
        "gh.pull-request.diff" => Proposal::to::<PrDiff>(PrDiffInput(input)),
        "gh.pull-request.status" => Proposal::to::<PrStatus>(PrStatusInput(input)),
        "gh.pull-request.approve" => Proposal::to::<PrApprove>(PrApproveInput(input)),
        "gh.pull-request.comment" => Proposal::to::<PrComment>(PrCommentInput(input)),
        "gh.pull-request.request-changes" => {
            Proposal::to::<PrRequestChanges>(PrRequestChangesInput(input))
        }
        "gh.pull-request.merge" => Proposal::to::<PrMerge>(PrMergeInput(input)),
        "gh.repo.read" => Proposal::to::<RepoRead>(RepoReadInput(input)),
        crate::ids::REPO_LIST => Proposal::to::<RepoList>(RepoListInput(input)),
        crate::ids::SEARCH_PRS => Proposal::to::<SearchPrs>(SearchPrsInput(input)),
        crate::ids::SEARCH_ISSUES => Proposal::to::<SearchIssues>(SearchIssuesInput(input)),
        crate::ids::SEARCH_REPOS => Proposal::to::<SearchRepos>(SearchReposInput(input)),
        crate::ids::SEARCH_CODE => Proposal::to::<SearchCode>(SearchCodeInput(input)),
        crate::ids::SEARCH_COMMITS => Proposal::to::<SearchCommits>(SearchCommitsInput(input)),
        "gh.branch.read" => Proposal::to::<BranchRead>(BranchReadInput(input)),
        "gh.commit.read" => Proposal::to::<CommitRead>(CommitReadInput(input)),
        "gh.user.read" => Proposal::to::<UserRead>(UserReadInput(input)),
        crate::ids::USER_SELF_READ => Proposal::to::<UserSelfRead>(UserSelfReadInput(input)),
        "gh.issue.read" => Proposal::to::<IssueRead>(IssueReadInput(input)),
        "gh.issue.list" => Proposal::to::<IssueList>(IssueListInput(input)),
        crate::ids::ISSUE_COMMENT_EDIT => {
            Proposal::to::<IssueCommentEdit>(IssueCommentEditInput(input))
        }
        crate::ids::ISSUE_COMMENT_DELETE => {
            Proposal::to::<IssueCommentDelete>(IssueCommentDeleteInput(input))
        }
        crate::ids::ISSUE_COMMENT_EDIT_OR_CREATE => {
            Proposal::to::<IssueCommentEditOrCreate>(IssueCommentEditOrCreateInput(input))
        }
        "gh.issue.comment" => Proposal::to::<IssueComment>(IssueCommentInput(input)),
        _ => unreachable!("the command dispatch uses only declared IDs"),
    }
}
