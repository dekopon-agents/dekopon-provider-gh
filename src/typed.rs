use dekopon_provider_sdk::provider::{self, Capability, Http, Proposal, Stdout};
use dekopon_provider_sdk::{EffectKind, RiskLevel};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{Read, Write};

use crate::{Gh, error::ProviderError};

// The established per-capability schemas remain the model-facing contract. Each typed input
// has its own schema and keeps the existing native validation at the HTTP boundary.
macro_rules! capability {
    ($name:ident, $input:ident, $id:expr, $short:expr, $handler:path, $effect:expr, $risk:expr) => {
        #[derive(Deserialize, Serialize)]
        #[serde(transparent)]
        pub struct $input(Value);
        impl JsonSchema for $input {
            fn schema_name() -> std::borrow::Cow<'static, str> {
                stringify!($input).into()
            }
            fn json_schema(_: &mut SchemaGenerator) -> Schema {
                let schema = crate::capabilities()
                    .into_iter()
                    .find(|c| c.id.as_str() == $id)
                    .expect("declared gh capability")
                    .input_schema;
                serde_json::from_value(schema).expect("valid gh schema")
            }
        }
        pub struct $name;
        impl Capability for $name {
            type Provider = Gh;
            const NAME: &'static str = $short;
            const DESCRIPTION: &'static str = $id;
            const EFFECT: EffectKind = $effect;
            const RISK: RiskLevel = $risk;
            type Input = $input;
            type Needs = Http;
            type Error = ProviderError;
            fn run(input: Self::Input, http: Http, out: &mut Stdout) -> Result<(), Self::Error> {
                let mut input = input.0;
                if input.get("body").and_then(Value::as_str) == Some(STDIN_BODY) {
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
                let result = $handler(input, &mut |request| http.send(request))?;
                serde_json::to_writer(&mut *out, &result).map_err(|_| {
                    ProviderError::new("invalid-response", "could not write output")
                })?;
                out.write_all(b"\n").map_err(|_| {
                    ProviderError::new("invalid-response", "could not write output")
                })?;
                Ok(())
            }
        }
    };
}

// Marker is only emitted by the pure argv proposal. A direct caller still needs the same
// write-capability grant; an explicit body of this value is treated as stdin for that call.
pub(crate) const STDIN_BODY: &str = "\u{0}gh:body-file:-\u{0}";

capability!(
    ContentRead,
    ContentReadInput,
    "gh.content.read",
    "content.read",
    crate::content::read,
    EffectKind::ReadOnly,
    RiskLevel::Low
);
capability!(
    PrList,
    PrListInput,
    "gh.pull-request.list",
    "pull-request.list",
    crate::pulls::list,
    EffectKind::ReadOnly,
    RiskLevel::Low
);
capability!(
    PrRead,
    PrReadInput,
    "gh.pull-request.read",
    "pull-request.read",
    crate::pulls::read,
    EffectKind::ReadOnly,
    RiskLevel::Low
);
capability!(
    PrFiles,
    PrFilesInput,
    "gh.pull-request.files",
    "pull-request.files",
    crate::pulls::files,
    EffectKind::ReadOnly,
    RiskLevel::Low
);
capability!(
    PrDiff,
    PrDiffInput,
    "gh.pull-request.diff",
    "pull-request.diff",
    crate::pulls::diff,
    EffectKind::ReadOnly,
    RiskLevel::Low
);
capability!(
    PrReviews,
    PrReviewsInput,
    "gh.pull-request.reviews",
    "pull-request.reviews",
    crate::pulls::reviews,
    EffectKind::ReadOnly,
    RiskLevel::Low
);
capability!(
    PrStatus,
    PrStatusInput,
    "gh.pull-request.status",
    "pull-request.status",
    crate::pulls::status,
    EffectKind::ReadOnly,
    RiskLevel::Low
);
capability!(
    PrApprove,
    PrApproveInput,
    "gh.pull-request.approve",
    "pull-request.approve",
    crate::reviews::approve,
    EffectKind::ExternalWrite,
    RiskLevel::High
);
capability!(
    PrComment,
    PrCommentInput,
    "gh.pull-request.comment",
    "pull-request.comment",
    crate::reviews::comment,
    EffectKind::ExternalWrite,
    RiskLevel::Medium
);
capability!(
    PrRequestChanges,
    PrRequestChangesInput,
    "gh.pull-request.request-changes",
    "pull-request.request-changes",
    crate::reviews::request_changes,
    EffectKind::ExternalWrite,
    RiskLevel::Medium
);
capability!(
    PrMerge,
    PrMergeInput,
    "gh.pull-request.merge",
    "pull-request.merge",
    crate::reviews::merge,
    EffectKind::ExternalWrite,
    RiskLevel::High
);
capability!(
    RepoRead,
    RepoReadInput,
    "gh.repo.read",
    "repo.read",
    crate::repos::repo,
    EffectKind::ReadOnly,
    RiskLevel::Low
);
capability!(
    BranchRead,
    BranchReadInput,
    "gh.branch.read",
    "branch.read",
    crate::repos::branch,
    EffectKind::ReadOnly,
    RiskLevel::Low
);
capability!(
    CommitRead,
    CommitReadInput,
    "gh.commit.read",
    "commit.read",
    crate::repos::commit,
    EffectKind::ReadOnly,
    RiskLevel::Low
);
capability!(
    UserRead,
    UserReadInput,
    "gh.user.read",
    "user.read",
    crate::repos::user,
    EffectKind::ReadOnly,
    RiskLevel::Low
);
capability!(
    IssueRead,
    IssueReadInput,
    "gh.issue.read",
    "issue.read",
    crate::issues::read,
    EffectKind::ReadOnly,
    RiskLevel::Low
);
capability!(
    IssueList,
    IssueListInput,
    "gh.issue.list",
    "issue.list",
    crate::issues::list,
    EffectKind::ReadOnly,
    RiskLevel::Low
);
capability!(
    IssueCommentsRead,
    IssueCommentsReadInput,
    "gh.issue-comments.read",
    "issue-comments.read",
    crate::issues::comments,
    EffectKind::ReadOnly,
    RiskLevel::Low
);
capability!(
    IssueComment,
    IssueCommentInput,
    "gh.issue.comment",
    "issue.comment",
    crate::issues::comment,
    EffectKind::ExternalWrite,
    RiskLevel::Medium
);

pub(crate) fn proposal(id: &str, input: Value) -> Proposal<Gh> {
    match id {
        "gh.content.read" => Proposal::to::<ContentRead>(ContentReadInput(input)),
        "gh.pull-request.list" => Proposal::to::<PrList>(PrListInput(input)),
        "gh.pull-request.read" => Proposal::to::<PrRead>(PrReadInput(input)),
        "gh.pull-request.files" => Proposal::to::<PrFiles>(PrFilesInput(input)),
        "gh.pull-request.diff" => Proposal::to::<PrDiff>(PrDiffInput(input)),
        "gh.pull-request.reviews" => Proposal::to::<PrReviews>(PrReviewsInput(input)),
        "gh.pull-request.status" => Proposal::to::<PrStatus>(PrStatusInput(input)),
        "gh.pull-request.approve" => Proposal::to::<PrApprove>(PrApproveInput(input)),
        "gh.pull-request.comment" => Proposal::to::<PrComment>(PrCommentInput(input)),
        "gh.pull-request.request-changes" => {
            Proposal::to::<PrRequestChanges>(PrRequestChangesInput(input))
        }
        "gh.pull-request.merge" => Proposal::to::<PrMerge>(PrMergeInput(input)),
        "gh.repo.read" => Proposal::to::<RepoRead>(RepoReadInput(input)),
        "gh.branch.read" => Proposal::to::<BranchRead>(BranchReadInput(input)),
        "gh.commit.read" => Proposal::to::<CommitRead>(CommitReadInput(input)),
        "gh.user.read" => Proposal::to::<UserRead>(UserReadInput(input)),
        "gh.issue.read" => Proposal::to::<IssueRead>(IssueReadInput(input)),
        "gh.issue.list" => Proposal::to::<IssueList>(IssueListInput(input)),
        "gh.issue-comments.read" => {
            Proposal::to::<IssueCommentsRead>(IssueCommentsReadInput(input))
        }
        "gh.issue.comment" => Proposal::to::<IssueComment>(IssueCommentInput(input)),
        _ => unreachable!("the command dispatch uses only declared IDs"),
    }
}
