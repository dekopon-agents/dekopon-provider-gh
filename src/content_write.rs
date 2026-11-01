use base64::{Engine as _, engine::general_purpose::STANDARD};
use dekopon_provider_sdk::provider::{HttpError, Request, Response, endpoint::Base};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    MAX_BODY_IN_BYTES, MAX_PATH_BYTES, MAX_REF_BYTES, decode, encode_path,
    error::{ProviderError, WriteOutcome},
    github_json_request, invalid_input, invalid_response, percent_encode, repo_schema,
    status_error, url, validate_body, validate_expected_sha, validate_login, validate_path,
    validate_ref, validate_repo,
};

const MAX_REQUEST_BYTES: usize = 16 * 1024;

#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase", deny_unknown_fields)]
enum Input {
    Create {
        owner: String,
        repo: String,
        path: String,
        branch: String,
        message: String,
        body: String,
    },
    Update {
        owner: String,
        repo: String,
        path: String,
        branch: String,
        message: String,
        body: String,
        sha: String,
    },
}

struct File {
    owner: String,
    repo: String,
    path: String,
    branch: String,
    message: String,
    body: String,
}

enum Change {
    Create,
    Update { sha: String },
}

impl Input {
    fn split(self) -> (File, Change) {
        match self {
            Self::Create {
                owner,
                repo,
                path,
                branch,
                message,
                body,
            } => (
                File {
                    owner,
                    repo,
                    path,
                    branch,
                    message,
                    body,
                },
                Change::Create,
            ),
            Self::Update {
                owner,
                repo,
                path,
                branch,
                message,
                body,
                sha,
            } => (
                File {
                    owner,
                    repo,
                    path,
                    branch,
                    message,
                    body,
                },
                Change::Update { sha },
            ),
        }
    }
}

pub(crate) fn schema() -> Value {
    let mut schema = repo_schema(
        json!({
            "path": {"type":"string","minLength":1,"maxLength":MAX_PATH_BYTES,"description":"Canonical repository-relative file path; no percent escapes or backslashes."},
            "branch": {"type":"string","minLength":1,"maxLength":MAX_REF_BYTES,"description":"Explicit existing branch; never defaults to the repository default branch."},
            "message": {"type":"string","minLength":1,"maxLength":MAX_BODY_IN_BYTES,"description":"Commit message, at most 4096 UTF-8 bytes."},
            "mode": {"type":"string","enum":["create","update"]},
            "sha": {"type":"string","pattern":"^[a-fA-F0-9]{40}$","description":"Expected file blob SHA, required only for update; not branch HEAD."},
            "body": {"type":"string","maxLength":MAX_BODY_IN_BYTES,"description":"Raw UTF-8 content, zero to 4096 bytes; base64-encoded by the provider."}
        }),
        &["path", "branch", "message", "mode", "body"],
    );
    schema["oneOf"] = json!([
        {"properties":{"mode":{"const":"create"}},"not":{"required":["sha"]}},
        {"properties":{"mode":{"const":"update"}},"required":["sha"]}
    ]);
    schema
}

pub(crate) fn validate_target(
    path: &str,
    branch: &str,
    message: &str,
) -> Result<(), ProviderError> {
    validate_path(path)?;
    if path.is_empty() || path.contains(['%', '\\']) {
        return Err(invalid_input());
    }
    validate_ref(branch)?;
    if branch == "HEAD"
        || branch.contains("..")
        || branch.ends_with('.')
        || branch
            .split('/')
            .any(|part| part.starts_with('.') || part.ends_with(".lock"))
    {
        return Err(invalid_input());
    }
    validate_body(message)
}

#[derive(Deserialize)]
struct Written {
    content: WrittenFile,
    commit: Commit,
}

#[derive(Deserialize)]
struct WrittenFile {
    path: String,
    sha: String,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct Commit {
    sha: String,
}

pub(crate) fn write(
    input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let input = serde_json::from_value::<Input>(input).map_err(|_| invalid_input())?;
    let (file, change) = input.split();
    validate_login(&file.owner)?;
    validate_repo(&file.repo)?;
    validate_target(&file.path, &file.branch, &file.message)?;
    if file.body.len() > MAX_BODY_IN_BYTES {
        return Err(invalid_input());
    }
    let mut body = json!({"branch":file.branch,"message":file.message,"content":STANDARD.encode(file.body.as_bytes())});
    let expected_status = match change {
        Change::Create => 201,
        Change::Update { sha } => {
            validate_expected_sha(Some(&sha))?;
            body["sha"] = json!(sha);
            200
        }
    };
    let uri = url(
        base,
        &format!(
            "/repos/{}/{}/contents/{}",
            percent_encode(&file.owner),
            percent_encode(&file.repo),
            encode_path(&file.path)
        ),
    )?;
    let request = github_json_request("PUT", uri, &body)?;
    if request.body.len() > MAX_REQUEST_BYTES {
        return Err(ProviderError::new(
            "request-too-large",
            "the file write request would exceed 16384 bytes",
        ));
    }
    let response = send(request).map_err(|_| ProviderError::write(WriteOutcome::Unknown))?;
    if response.status == 409 {
        return Err(ProviderError::write(WriteOutcome::Conflict));
    }
    if response.status != expected_status {
        return Err(status_error(&response));
    }
    let written = decode::<Written>(&response.body)?;
    if written.content.path != file.path
        || written.content.kind != "file"
        || !crate::is_sha(&written.content.sha)
        || !crate::is_sha(&written.commit.sha)
    {
        return Err(invalid_response());
    }
    Ok(
        json!({"owner":file.owner,"repo":file.repo,"path":file.path,"branch":file.branch,"blobSha":written.content.sha,"commitSha":written.commit.sha}),
    )
}
