mod matcher;
mod merge;

pub(crate) use self::matcher::{BotLogin, PinSet};
pub(crate) use self::merge::merge;

use self::matcher::{File, Files, MAX_FILE_BYTES, MAX_PINS, NAME_PATTERN, Repo, Values, rewrite};
use crate::error::ProviderError;
use crate::{
    ACCEPT_JSON, Base, GhSettings, HttpError, Request, Response, content::decode_base64, decode,
    encode_path, github_json_request, http_failed, invalid_input, invalid_response, is_sha,
    number_property, object_schema, required_sha_property, send_get, status_error, url,
    validate_body,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use dekopon_provider_sdk::provider::method;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

const UPDATE_MAX_REQUESTS: usize = 24;
const MAIN: &str = "main";
type Send<'a> = dyn FnMut(Request) -> Result<Response, HttpError> + 'a;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateInput {
    pins: Vec<Change>,
    title: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    name: String,
    value: String,
}

pub(crate) fn update_schema() -> Value {
    object_schema(
        json!({
            "pins": {
                "type": "array", "minItems": 1, "maxItems": MAX_PINS,
                "items": object_schema(json!({
                    "name": {"type": "string", "pattern": NAME_PATTERN},
                    "value": {"type": "string", "minLength": 1, "maxLength": MAX_FILE_BYTES}
                }), &["name", "value"])
            },
            "title": crate::body_property("Pull request title.")
        }),
        &["pins", "title"],
    )
}

pub(crate) fn merge_schema() -> Value {
    object_schema(
        json!({
            "repo": {"type": "string", "description": "Configured owner/repository."},
            "number": number_property(),
            "expectedHeadSha": required_sha_property()
        }),
        &["repo", "number", "expectedHeadSha"],
    )
}

#[derive(Deserialize)]
struct ObjectSha {
    sha: String,
}

#[derive(Deserialize)]
struct GitRef {
    #[serde(rename = "ref")]
    name: String,
    object: ObjectSha,
}

#[derive(Deserialize)]
struct Content {
    #[serde(rename = "type")]
    kind: String,
    path: String,
    sha: String,
    size: usize,
    encoding: String,
    content: String,
}

#[derive(Deserialize)]
struct Written {
    content: WrittenContent,
    commit: WrittenCommit,
}

#[derive(Deserialize)]
struct WrittenContent {
    path: String,
    sha: String,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct WrittenCommit {
    sha: String,
    parents: Vec<ObjectSha>,
}

#[derive(Deserialize)]
struct NewPull {
    number: u32,
    head: crate::pulls::RawRef,
    base: crate::pulls::RawRef,
}

impl Repo {
    fn path(&self, suffix: &str) -> String {
        format!("/repos/{}/{}{}", self.owner, self.name, suffix)
    }
}

struct Api<'a, 'send> {
    base: &'a Base,
    repo: &'a Repo,
    send: &'a mut Send<'send>,
}

impl Api<'_, '_> {
    fn get<T: serde::de::DeserializeOwned>(&mut self, suffix: &str) -> Result<T, ProviderError> {
        let response = send_get(
            self.send,
            url(self.base, &self.repo.path(suffix))?,
            ACCEPT_JSON,
        )?;
        decode(&response.body)
    }

    fn write(
        &mut self,
        method: &'static str,
        suffix: &str,
        body: &Value,
    ) -> Result<Response, ProviderError> {
        (self.send)(github_json_request(
            method,
            url(self.base, &self.repo.path(suffix))?,
            body,
        )?)
        .map_err(|_| http_failed())
    }

    fn file(&mut self, path: &str, reference: &str) -> Result<File, ProviderError> {
        let file: Content =
            self.get(&format!("/contents/{}?ref={reference}", encode_path(path)))?;
        if file.kind != "file"
            || file.path != path
            || file.encoding != "base64"
            || !is_sha(&file.sha)
            || file.size > MAX_FILE_BYTES
        {
            return Err(invalid_response());
        }
        let bytes = decode_base64(&file.content)?;
        if bytes.len() != file.size {
            return Err(invalid_response());
        }
        let text = String::from_utf8(bytes).map_err(|_| invalid_response())?;
        Ok(File {
            sha: file.sha,
            text,
        })
    }

    fn create_branch(&mut self, branch: &str, head: &str) -> Result<(), ProviderError> {
        let name = format!("refs/heads/{branch}");
        let response = self.write(
            method::POST,
            "/git/refs",
            &json!({"ref": name, "sha": head}),
        )?;
        if response.status != 201 {
            return Err(status_error(&response));
        }
        let created: GitRef = decode(&response.body)?;
        if created.name != name || created.object.sha != head {
            return Err(invalid_response());
        }
        Ok(())
    }

    fn put_file(
        &mut self,
        path: &str,
        file: &File,
        text: &str,
        branch: &str,
        title: &str,
        parent: &str,
    ) -> Result<String, ProviderError> {
        let response = self.write(method::PUT, &format!("/contents/{}", encode_path(path)), &json!({
            "message": title, "branch": branch, "sha": file.sha, "content": STANDARD.encode(text),
        }))?;
        if response.status != 200 {
            return Err(status_error(&response));
        }
        let written: Written = decode(&response.body)?;
        if written.content.path != path
            || written.content.kind != "file"
            || !is_sha(&written.content.sha)
            || !is_sha(&written.commit.sha)
            || !matches!(written.commit.parents.as_slice(), [p] if p.sha == parent)
        {
            return Err(invalid_response());
        }
        Ok(written.commit.sha)
    }
}

pub(crate) fn update(
    input: Value,
    settings: &GhSettings,
    send: &mut Send<'_>,
) -> Result<Value, ProviderError> {
    let input: UpdateInput = serde_json::from_value(input).map_err(|_| invalid_input())?;
    validate_body(&input.title)?;
    let first = input.pins.first().ok_or_else(invalid_input)?;
    let first_name = first.name.clone();
    let repo = &settings
        .pins
        .0
        .get(&first_name)
        .ok_or_else(invalid_input)?
        .repo;
    if input.pins.len() > MAX_PINS {
        return Err(invalid_input());
    }
    let mut values = Values::new();
    let mut paths = BTreeSet::new();
    for change in input.pins {
        let pin = settings
            .pins
            .0
            .get(&change.name)
            .ok_or_else(invalid_input)?;
        if &pin.repo != repo {
            return Err(invalid_input());
        }
        pin.check_value(&change.name, &change.value)?;
        if values.insert(change.name, change.value).is_some() {
            return Err(invalid_input());
        }
        paths.extend(pin.sites.iter().map(|site| site.path.as_str()));
    }
    if paths.len() > UPDATE_MAX_REQUESTS - 3 {
        return Err(request_ceiling());
    }

    let mut api = Api {
        base: settings.base(),
        repo,
        send,
    };
    let main: GitRef = api.get("/git/ref/heads/main")?;
    if main.name != "refs/heads/main" || !is_sha(&main.object.sha) {
        return Err(invalid_response());
    }
    let head = main.object.sha;
    let reads = paths.len();
    let mut files = Files::new();
    for path in paths {
        files.insert(path.to_owned(), api.file(path, &head)?);
    }
    let changed = rewrite(&settings.pins, &files, &values)?;
    if changed.is_empty() {
        return Err(ProviderError::new(
            "no-change",
            "all requested pins have their requested value",
        ));
    }
    if reads + changed.len() + 3 > UPDATE_MAX_REQUESTS {
        return Err(request_ceiling());
    }

    let encoded = serde_json::to_vec(&values).map_err(|_| invalid_input())?;
    let hash = format!("{:x}", Sha256::digest(encoded));
    let branch = format!("pin/{first_name}-{}", &hash[..8]);
    api.create_branch(&branch, &head)?;
    let mut commit = head;
    for (path, text) in &changed {
        commit = api.put_file(path, &files[path], text, &branch, &input.title, &commit)?;
    }
    let response = api.write(
        method::POST,
        "/pulls",
        &json!({
            "title": input.title, "head": branch, "base": MAIN, "maintainer_can_modify": false,
        }),
    )?;
    if response.status != 201 {
        return Err(status_error(&response));
    }
    let pull: NewPull = decode(&response.body)?;
    if pull.number == 0
        || pull.head.name != branch
        || pull.head.sha != commit
        || pull.base.name != MAIN
    {
        return Err(invalid_response());
    }
    Ok(json!({
        "number": pull.number, "headSha": commit, "branch": branch,
        "files": changed.keys().collect::<Vec<_>>(),
    }))
}

fn request_ceiling() -> ProviderError {
    ProviderError::new(
        "request-ceiling",
        "the pin operation exceeds its HTTP request budget",
    )
}

#[cfg(test)]
mod tests;
