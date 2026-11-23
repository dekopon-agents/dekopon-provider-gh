use super::matcher::{Files, PinSet, Repo, Values, lines, rewrite, unsafe_diff};
use super::{Api, MAIN, Send, request_ceiling};
use crate::error::ProviderError;
use crate::pulls::RawPull;
use crate::{
    GhSettings, MAX_MESSAGE_OUT_BYTES, Response, decode, invalid_input, invalid_response, is_sha,
    reviews::pinned_pull, truncate_text, validate_expected_sha, validate_number,
};
use dekopon_provider_sdk::provider::method;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const MERGE_MAX_REQUESTS: usize = 32;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Input {
    repo: String,
    number: u32,
    expected_head_sha: String,
}

#[derive(Deserialize)]
struct Compare {
    merge_base_commit: CompareCommit,
}

#[derive(Deserialize)]
struct CompareCommit {
    sha: String,
    commit: GitCommit,
}

#[derive(Deserialize)]
struct GitCommit {
    tree: super::ObjectSha,
}

#[derive(Deserialize)]
struct Tree {
    sha: String,
    truncated: bool,
    tree: Vec<TreeEntry>,
}

#[derive(Deserialize)]
struct TreeEntry {
    path: String,
    mode: String,
    #[serde(rename = "type")]
    kind: String,
    sha: String,
}

#[derive(Deserialize)]
struct MergeResult {
    merged: bool,
    sha: Option<String>,
    message: Option<String>,
}

#[derive(Deserialize)]
struct GitHubMessage {
    message: String,
}

fn merge_refused(status: u16, message: Option<&str>) -> ProviderError {
    let message = message
        .filter(|message| !message.is_empty())
        .unwrap_or("merge refused");
    let message = format!("GitHub HTTP {status}: {message}");
    ProviderError::new(
        "github-merge-refused",
        truncate_text(&message, MAX_MESSAGE_OUT_BYTES).0,
    )
}

fn merged(response: Response, pull: &RawPull) -> Result<Value, ProviderError> {
    if response.status != 200 {
        let details = decode::<GitHubMessage>(&response.body).ok();
        return Err(merge_refused(
            response.status,
            details.as_ref().map(|body| body.message.as_str()),
        ));
    }
    let result: MergeResult = decode(&response.body)?;
    if !result.merged {
        return Err(merge_refused(response.status, result.message.as_deref()));
    }
    let sha = result
        .sha
        .filter(|sha| is_sha(sha))
        .ok_or_else(invalid_response)?;
    Ok(json!({
        "number": pull.number,
        "headSha": pull.head.sha,
        "merged": true,
        "sha": sha,
        "message": truncate_text(result.message.as_deref().unwrap_or(""), MAX_MESSAGE_OUT_BYTES).0,
    }))
}

fn verify_pull(pull: &RawPull, slug: &str, settings: &GhSettings) -> Result<(), ProviderError> {
    let same_repo = pull
        .head
        .repo
        .as_ref()
        .is_some_and(|repo| repo.full_name.eq_ignore_ascii_case(slug));
    let app_bot = pull.user.as_ref().is_some_and(|user| {
        user.account_type.as_deref() == Some("Bot")
            && settings
                .pin_bot_login
                .as_ref()
                .is_some_and(|bot| bot.matches(&user.login))
    });
    if pull.base.name != MAIN || !pull.head.name.starts_with("pin/") || !same_repo || !app_bot {
        return Err(unsafe_diff());
    }
    Ok(())
}

fn tree(api: &mut Api<'_, '_>, sha: &str) -> Result<BTreeMap<String, TreeEntry>, ProviderError> {
    if !is_sha(sha) {
        return Err(invalid_response());
    }
    let response: Tree = api.get(&format!("/git/trees/{sha}?recursive=1"))?;
    if response.truncated || response.sha != sha {
        return Err(unsafe_diff());
    }
    let mut entries = BTreeMap::new();
    for entry in response.tree {
        if !is_sha(&entry.sha) {
            return Err(invalid_response());
        }
        if entry.kind == "tree" && entry.mode == "040000" {
            continue;
        }
        if entries.insert(entry.path.clone(), entry).is_some() {
            return Err(invalid_response());
        }
    }
    Ok(entries)
}

fn changed_paths<'a>(
    before: &'a BTreeMap<String, TreeEntry>,
    after: &'a BTreeMap<String, TreeEntry>,
    allowed: &BTreeSet<&str>,
) -> Result<Vec<&'a str>, ProviderError> {
    let mut changed = Vec::new();
    for (path, old) in before {
        let new = after.get(path).ok_or_else(unsafe_diff)?;
        if old.kind != new.kind || old.mode != new.mode {
            return Err(unsafe_diff());
        }
        if old.sha != new.sha {
            if old.kind != "blob"
                || !matches!(old.mode.as_str(), "100644" | "100755")
                || !allowed.contains(path.as_str())
            {
                return Err(unsafe_diff());
            }
            changed.push(path.as_str());
        }
    }
    if before.len() != after.len() || changed.is_empty() {
        return Err(unsafe_diff());
    }
    Ok(changed)
}

fn requested_values(
    before: &Files,
    after: &Files,
    pins: &PinSet,
    repo: &Repo,
) -> Result<Values, ProviderError> {
    let mut values = Values::new();
    for (path, old) in before {
        let new = &after[path];
        let mut old_lines = lines(&old.text);
        let mut new_lines = lines(&new.text);
        for (before, after) in old_lines.by_ref().zip(new_lines.by_ref()) {
            if before.raw == after.raw {
                continue;
            }
            let mut matches = pins
                .0
                .iter()
                .filter(|(_, pin)| &pin.repo == repo)
                .flat_map(|(name, pin)| pin.sites.iter().map(move |site| (name, site)))
                .filter(|(_, site)| site.path == *path)
                .filter_map(|(name, site)| {
                    site.matches_change(before, after)
                        .map(|(value, _)| (name, value))
                });
            let (name, value) = matches.next().ok_or_else(unsafe_diff)?;
            if matches.next().is_some()
                || values
                    .insert(name.clone(), value.to_owned())
                    .is_some_and(|old| old != value)
            {
                return Err(unsafe_diff());
            }
        }
        if old_lines.next().is_some() || new_lines.next().is_some() {
            return Err(unsafe_diff());
        }
    }
    Ok(values)
}

pub(crate) fn merge(
    input: Value,
    settings: &GhSettings,
    send: &mut Send<'_>,
) -> Result<Value, ProviderError> {
    let input: Input = serde_json::from_value(input).map_err(|_| invalid_input())?;
    let repo = Repo::parse(&input.repo)?;
    validate_number(input.number)?;
    validate_expected_sha(Some(&input.expected_head_sha))?;
    let allowed: BTreeSet<_> = settings
        .pins
        .0
        .values()
        .filter(|pin| pin.repo == repo)
        .flat_map(|pin| pin.sites.iter().map(|site| site.path.as_str()))
        .collect();
    if allowed.is_empty() {
        return Err(invalid_input());
    }
    let base = settings.base();
    let pull = pinned_pull(
        send,
        base,
        &repo.owner,
        &repo.name,
        input.number,
        Some(&input.expected_head_sha),
    )?;
    verify_pull(&pull, &input.repo, settings)?;

    let mut api = Api {
        base,
        repo: &repo,
        send,
    };
    let comparison: Compare = api.get(&format!(
        "/compare/{}...{}?per_page=1",
        pull.base.sha, pull.head.sha
    ))?;
    let merge_base = comparison.merge_base_commit;
    if !is_sha(&merge_base.sha) {
        return Err(invalid_response());
    }
    let head: GitCommit = api.get(&format!("/git/commits/{}", pull.head.sha))?;
    let before_tree = tree(&mut api, &merge_base.commit.tree.sha)?;
    let after_tree = tree(&mut api, &head.tree.sha)?;
    let changed = changed_paths(&before_tree, &after_tree, &allowed)?;
    if changed.len() * 2 + 7 > MERGE_MAX_REQUESTS {
        return Err(request_ceiling());
    }

    let mut before = Files::new();
    let mut after = Files::new();
    for path in changed {
        let old = api.file(path, &merge_base.sha)?;
        let new = api.file(path, &pull.head.sha)?;
        if old.sha != before_tree[path].sha || new.sha != after_tree[path].sha {
            return Err(invalid_response());
        }
        before.insert(path.to_owned(), old);
        after.insert(path.to_owned(), new);
    }
    let values = requested_values(&before, &after, &settings.pins, &repo)?;
    let reproduced = rewrite(&settings.pins, &before, &values)?;
    if reproduced.len() != after.len()
        || after
            .iter()
            .any(|(path, file)| reproduced.get(path) != Some(&file.text))
    {
        return Err(unsafe_diff());
    }

    let refreshed = pinned_pull(
        api.send,
        base,
        &repo.owner,
        &repo.name,
        input.number,
        Some(&pull.head.sha),
    )?;
    verify_pull(&refreshed, &input.repo, settings)?;
    if refreshed.base.sha != pull.base.sha || refreshed.head.name != pull.head.name {
        return Err(unsafe_diff());
    }
    let response = api.write(
        method::PUT,
        &format!("/pulls/{}/merge", input.number),
        &json!({"sha": pull.head.sha}),
    )?;
    merged(response, &pull)
}
