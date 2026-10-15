//! `gh.content.read`: one file or directory listing at a path and optional ref.

use crate::error::ProviderError;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use dekopon_provider_sdk::provider::endpoint::Base;
use dekopon_provider_sdk::provider::{HttpError, Request, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    ACCEPT_JSON, MAX_CONTENT_OUT_BYTES, MAX_DIR_ENTRIES, decode, encode_path, invalid_input,
    invalid_response, percent_encode, send_get, truncate_text, url, validate_login, validate_path,
    validate_ref, validate_repo,
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Input {
    owner: String,
    repo: String,
    path: String,
    #[serde(default, rename = "ref")]
    reference: Option<String>,
    #[serde(default, rename = "_cli")]
    cli: bool,
}

/// One entry of a directory listing response.
#[derive(Debug, Deserialize)]
struct RawEntry {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    size: Option<u64>,
    sha: String,
}

/// A file, symlink, or submodule response.
#[derive(Debug, Deserialize)]
struct RawContent {
    #[serde(rename = "type")]
    kind: String,
    path: String,
    sha: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    encoding: Option<String>,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    submodule_git_url: Option<String>,
}

pub(crate) fn read(
    input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let input = serde_json::from_value::<Input>(input).map_err(|_| invalid_input())?;
    validate_login(&input.owner)?;
    validate_repo(&input.repo)?;
    validate_path(&input.path)?;
    if let Some(reference) = input.reference.as_deref() {
        validate_ref(reference)?;
    }

    let mut uri = url(
        base,
        &format!(
            "/repos/{}/{}/contents/{}",
            percent_encode(&input.owner),
            percent_encode(&input.repo),
            encode_path(&input.path),
        ),
    )?;
    if let Some(reference) = input.reference.as_deref() {
        uri.push_str("?ref=");
        uri.push_str(&percent_encode(reference));
    }
    let response = send_get(send, uri, ACCEPT_JSON)?;

    // A directory is a JSON array; everything else is one object. Decide on the parsed shape
    // rather than sniffing bytes.
    let value = decode::<Value>(&response.body)?;
    if value.is_array() {
        return project_directory(&input.path, value, input.cli);
    }
    let content =
        serde_json::from_value::<RawContent>(value.clone()).map_err(|_| invalid_response())?;
    let mut projected = project_content(&input.path, content, input.cli)?;
    if input.cli && projected["type"] == "file" {
        let name = input.path.rsplit('/').next().unwrap_or_default();
        projected["name"] = json!(name);
        projected["gitSHA"] = projected["sha"].clone();
        projected["url"] = value["url"].clone();
        projected["htmlUrl"] = value["html_url"].clone();
        projected["gitUrl"] = value["git_url"].clone();
        projected["downloadUrl"] = value["download_url"].clone();
    }
    Ok(projected)
}

fn project_directory(path: &str, value: Value, cli: bool) -> Result<Value, ProviderError> {
    let raw = serde_json::from_value::<Vec<RawEntry>>(value).map_err(|_| invalid_response())?;
    let truncated = raw.len() > MAX_DIR_ENTRIES;
    let entries = raw
        .into_iter()
        .take(MAX_DIR_ENTRIES)
        .map(|entry| {
            if entry.name.is_empty() || entry.name.len() > 512 || entry.sha.len() > 64 {
                return Err(invalid_response());
            }
            let mut row = json!({
                "name": entry.name,
                "type": entry.kind,
                "size": entry.size,
                "sha": entry.sha,
            });
            if cli {
                row["path"] = json!(if path.is_empty() {
                    entry.name.clone()
                } else {
                    format!("{path}/{}", entry.name)
                });
                row["gitSHA"] = row["sha"].clone();
            }
            Ok(row)
        })
        .collect::<Result<Vec<_>, ProviderError>>()?;
    Ok(json!({
        "type": "dir",
        "path": path,
        "entries": entries,
        "entriesTruncated": truncated,
    }))
}

fn project_content(
    requested_path: &str,
    content: RawContent,
    cli: bool,
) -> Result<Value, ProviderError> {
    // The path echo binds the response to the request; a mismatch is a broken or hostile reply.
    if content.path != requested_path {
        return Err(invalid_response());
    }
    match content.kind.as_str() {
        "file" => project_file(content, cli),
        "symlink" | "submodule" => {
            let target = content
                .target
                .or(content.submodule_git_url)
                .filter(|target| !target.is_empty() && target.len() <= 1024);
            Ok(json!({
                "type": content.kind,
                "path": content.path,
                "target": target,
            }))
        }
        _ => Err(invalid_response()),
    }
}

/// Decodes a file body and re-projects it bounded: UTF-8 text when valid, base64 otherwise.
fn project_file(content: RawContent, cli: bool) -> Result<Value, ProviderError> {
    if content.encoding.as_deref() != Some("base64") {
        return Err(invalid_response());
    }
    let raw = content.content.ok_or_else(invalid_response)?;
    // GitHub wraps base64 with newlines; strip all ASCII whitespace before decoding.
    let compact = raw
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect::<Vec<u8>>();
    let bytes = STANDARD.decode(&compact).map_err(|_| invalid_response())?;

    let (encoding, mut text, kept) = match core::str::from_utf8(&bytes) {
        Ok(text) => {
            let (bounded, _) = truncate_text(text, MAX_CONTENT_OUT_BYTES);
            let kept = bounded.len();
            ("utf-8", bounded, kept)
        }
        Err(_) => {
            if cli && bytes.len() > MAX_CONTENT_OUT_BYTES {
                return Err(ProviderError::new(
                    "invalid-response",
                    format!(
                        "gh: file is {} bytes, over the {MAX_CONTENT_OUT_BYTES}-byte read bound; binary content is not truncated",
                        bytes.len()
                    ),
                ));
            }
            let bounded = &bytes[..bytes.len().min(MAX_CONTENT_OUT_BYTES)];
            ("base64", STANDARD.encode(bounded), bounded.len())
        }
    };
    let truncated = kept < bytes.len();
    if cli && encoding == "utf-8" && truncated {
        text.push_str(&crate::truncation_marker(kept, bytes.len()));
    }

    Ok(json!({
        "type": "file",
        "path": content.path,
        "sha": content.sha,
        "size": content.size,
        "encoding": encoding,
        "content": text,
        "contentTruncated": truncated,
    }))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::invoke_with;
    use crate::testutil::{accept_of, capability, json_response, scripted, step};

    #[test]
    fn reads_a_utf8_file_with_ref_and_projects_it() {
        let encoded =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, "fn main() {}\n");
        let output = invoke_with(
            &capability("gh.content.read"),
            json!({
                "owner": "octo",
                "repo": "hello",
                "path": "src/main.rs",
                "ref": "feature/x"
            }),
            scripted(vec![step(
                |request| {
                    assert_eq!(request.method, "GET");
                    assert_eq!(
                        request.uri,
                        "https://api.github.com/repos/octo/hello/contents/src/main.rs?ref=feature%2Fx"
                    );
                    assert_eq!(accept_of(request), b"application/vnd.github+json");
                    assert!(request.body.is_empty());
                },
                json_response(
                    200,
                    &json!({
                        "type": "file",
                        "path": "src/main.rs",
                        "sha": "abc123",
                        "size": 13,
                        "encoding": "base64",
                        "content": format!("{encoded}\n"),
                    }),
                ),
            )]),
        )
        .expect("file read succeeds");

        assert_eq!(output["type"], "file");
        assert_eq!(output["encoding"], "utf-8");
        assert_eq!(output["content"], "fn main() {}\n");
        assert_eq!(output["contentTruncated"], false);
        assert_eq!(output["path"], "src/main.rs");
    }

    #[test]
    fn binary_files_come_back_as_bounded_base64() {
        let bytes = vec![0_u8, 159, 146, 150];
        let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);
        let output = invoke_with(
            &capability("gh.content.read"),
            json!({"owner": "octo", "repo": "hello", "path": "logo.png"}),
            scripted(vec![step(
                |request| {
                    assert_eq!(
                        request.uri,
                        "https://api.github.com/repos/octo/hello/contents/logo.png"
                    );
                },
                json_response(
                    200,
                    &json!({
                        "type": "file",
                        "path": "logo.png",
                        "sha": "abc",
                        "size": 4,
                        "encoding": "base64",
                        "content": encoded,
                    }),
                ),
            )]),
        )
        .expect("binary read succeeds");

        assert_eq!(output["encoding"], "base64");
        assert_eq!(output["contentTruncated"], false);
        let decoded = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            output["content"].as_str().expect("content is a string"),
        )
        .expect("content decodes");
        assert_eq!(decoded, bytes);
    }

    #[test]
    fn oversize_text_truncates_on_the_exact_boundary() {
        let text = "a".repeat(crate::MAX_CONTENT_OUT_BYTES + 1);
        let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &text);
        let output = invoke_with(
            &capability("gh.content.read"),
            json!({"owner": "octo", "repo": "hello", "path": "big.txt"}),
            scripted(vec![step(
                |_| {},
                json_response(
                    200,
                    &json!({
                        "type": "file",
                        "path": "big.txt",
                        "sha": "abc",
                        "size": text.len(),
                        "encoding": "base64",
                        "content": encoded,
                    }),
                ),
            )]),
        )
        .expect("oversize read succeeds");

        assert_eq!(output["contentTruncated"], true);
        assert_eq!(
            output["content"].as_str().expect("string").len(),
            crate::MAX_CONTENT_OUT_BYTES
        );
    }

    #[test]
    fn oversized_binary_cli_refuses_without_corrupting_base64_or_typed_reads() {
        use std::{cell::Cell, rc::Rc};
        let bound = crate::MAX_CONTENT_OUT_BYTES;
        let read = |bytes: &[u8], cli: bool| {
            let seen = Rc::new(Cell::new(0));
            let checked = Rc::clone(&seen);
            let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes);
            let output = invoke_with(
                &capability("gh.content.read"),
                json!({"owner":"octo","repo":"hello","path":"logo.bin","_cli":cli}),
                scripted(vec![step(
                    move |request| {
                        assert_eq!(request.method, "GET");
                        assert_eq!(
                            request.uri,
                            "https://api.github.com/repos/octo/hello/contents/logo.bin"
                        );
                        checked.set(checked.get() + 1);
                    },
                    json_response(
                        200,
                        &json!({"type":"file","path":"logo.bin","sha":"abc","size":bytes.len(),"encoding":"base64","content":encoded}),
                    ),
                )]),
            );
            assert_eq!(seen.get(), 1);
            output
        };
        let big = vec![0xff; bound + 1];
        let error = read(&big, true).expect_err("CLI binary cannot return partial base64");
        assert_eq!(error.code(), "invalid-response");
        assert_eq!(
            error.message(),
            format!(
                "gh: file is {} bytes, over the {bound}-byte read bound; binary content is not truncated",
                big.len()
            )
        );
        let typed = read(&big, false).expect("direct typed envelope keeps truncation metadata");
        assert_eq!(typed["contentTruncated"], true);
        assert_eq!(typed["encoding"], "base64");
        let decoded = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            typed["content"].as_str().expect("base64 string"),
        )
        .expect("typed base64 remains valid");
        assert_eq!(decoded, &big[..bound]);
        let at_bound = read(&big[..bound], true).expect("binary at bound is complete");
        assert_eq!(at_bound["encoding"], "base64");
        assert_eq!(at_bound["contentTruncated"], false);
        assert_eq!(at_bound["content"], typed["content"]);

        let small = [0xff, 0x00, 0xfe];
        let cli = read(&small, true).expect("small CLI binary is unchanged");
        assert_eq!(cli["encoding"], "base64");
        assert_eq!(cli["contentTruncated"], false);
        assert_eq!(
            cli["content"],
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, small)
        );

        let text = "é".repeat(bound / 2 + 1);
        let text_cli = read(text.as_bytes(), true).expect("large UTF-8 file succeeds");
        assert_eq!(text_cli["encoding"], "utf-8");
        assert_eq!(text_cli["contentTruncated"], true);
        let rendered = text_cli["content"].as_str().expect("UTF-8 string");
        assert_eq!(
            rendered,
            format!(
                "{}\n[dekopon: truncated to {bound} bytes of {}]",
                "é".repeat(bound / 2),
                text.len()
            )
        );
        let selection_input = json!({"_cliKind":"read-file"});
        let plain = crate::output::render("gh.content.read", &selection_input, &text_cli, "")
            .expect("text read-file renders");
        assert_eq!(plain, rendered);
        let selected =
            crate::output::render("gh.content.read", &selection_input, &text_cli, "content")
                .expect("selected text content renders");
        let selected: serde_json::Value = serde_json::from_str(&selected).expect("JSON");
        assert_eq!(selected["content"], rendered);
    }

    #[test]
    fn directory_listings_are_bounded_projections() {
        let entries = (0..3)
            .map(|index| {
                json!({
                    "name": format!("file-{index}.rs"),
                    "type": "file",
                    "size": 10,
                    "sha": format!("sha-{index}"),
                    "url": "https://ignored.example/should-not-matter",
                })
            })
            .collect::<Vec<_>>();
        let output = invoke_with(
            &capability("gh.content.read"),
            json!({"owner": "octo", "repo": "hello", "path": "src"}),
            scripted(vec![step(|_| {}, json_response(200, &json!(entries)))]),
        )
        .expect("directory read succeeds");

        assert_eq!(output["type"], "dir");
        assert_eq!(output["entries"].as_array().expect("entries").len(), 3);
        assert_eq!(output["entries"][1]["name"], "file-1.rs");
        assert_eq!(output["entriesTruncated"], false);
    }

    #[test]
    fn empty_path_lists_the_repository_root() {
        invoke_with(
            &capability("gh.content.read"),
            json!({"owner": "octo", "repo": "hello", "path": ""}),
            scripted(vec![step(
                |request| {
                    assert_eq!(
                        request.uri,
                        "https://api.github.com/repos/octo/hello/contents/"
                    );
                },
                json_response(200, &json!([])),
            )]),
        )
        .expect("root listing succeeds");
    }

    #[test]
    fn path_echo_mismatch_is_invalid_response() {
        let error = invoke_with(
            &capability("gh.content.read"),
            json!({"owner": "octo", "repo": "hello", "path": "a.txt"}),
            scripted(vec![step(
                |_| {},
                json_response(
                    200,
                    &json!({
                        "type": "file",
                        "path": "b.txt",
                        "sha": "abc",
                        "size": 1,
                        "encoding": "base64",
                        "content": "eA==",
                    }),
                ),
            )]),
        )
        .expect_err("mismatched echo fails");
        assert_eq!(error.code(), "invalid-response");
    }

    #[test]
    fn missing_files_are_not_found() {
        let error = invoke_with(
            &capability("gh.content.read"),
            json!({"owner": "octo", "repo": "hello", "path": "gone.txt"}),
            scripted(vec![step(|_| {}, json_response(404, &json!({})))]),
        )
        .expect_err("missing file fails");
        assert_eq!(error.code(), "not-found");
    }
}
