use dekopon_gh_provider::Gh;
use dekopon_provider_sdk::provider::{Header, Http, ImportSet, Needs, Provider, Response};
use dekopon_provider_sdk::{CommandRunOutcome, EffectKind, RiskLevel, provider};
use dekopon_provider_sdk_testkit::{Harness, HttpScript, Native, conformance};
use serde_json::{Value, json};

fn component() -> std::path::PathBuf {
    std::env::var_os("DEKOPON_PROVIDER_COMPONENT")
        .expect("DEKOPON_PROVIDER_COMPONENT must name the freshly built component")
        .into()
}

#[test]
fn manifest_keeps_read_and_write_grants_independent() {
    let manifest = provider::manifest::<Gh>().expect("closed manifest");
    assert_eq!(manifest.capabilities.len(), 17);
    for removed in [
        "gh.pull-request.files",
        "gh.pull-request.reviews",
        "gh.issue-comments.read",
    ] {
        assert!(
            !manifest
                .capabilities
                .iter()
                .any(|cap| cap.id.as_str() == removed)
        );
    }
    for cap in &manifest.capabilities {
        assert_eq!(cap.input_schema["additionalProperties"], false);
        assert!(cap.id.as_str().starts_with("gh."));
    }
    assert_eq!(
        manifest
            .capabilities
            .iter()
            .filter(|c| c.effect == EffectKind::ExternalWrite)
            .count(),
        5
    );
    assert_eq!(
        manifest
            .capabilities
            .iter()
            .filter(|c| c.risk == RiskLevel::High)
            .count(),
        2
    );
    assert!((<Http as Needs>::IMPORTS).contains(ImportSet::HTTP));
    assert_eq!(<Gh as Provider>::COMMAND_WORDS, &["gh"]);
    for capability in &manifest.capabilities {
        let marker = capability.input_schema["properties"].get("_stdinBody");
        if [
            "gh.issue.comment",
            "gh.pull-request.approve",
            "gh.pull-request.comment",
            "gh.pull-request.request-changes",
        ]
        .contains(&capability.id.as_str())
        {
            assert_eq!(marker.expect("piped body marker")["const"], true);
        } else {
            assert!(marker.is_none(), "{} cannot read piped body", capability.id);
        }
    }
}

#[test]
fn removed_ids_cannot_run_and_existing_api_reads_require_their_own_grants() {
    for removed in [
        "gh.pull-request.files",
        "gh.pull-request.reviews",
        "gh.issue-comments.read",
    ] {
        let native = Native::<Gh>::new();
        let result = native.call(
            removed,
            &json!({"owner":"octo","repo":"hello","number":7}).to_string(),
        );
        assert_ne!(result.status, 0, "{removed}");
        assert!(native.requests().is_empty(), "{removed}");
    }
    for (id, input) in [
        ("gh.user.read", json!({"login":"octo"})),
        (
            "gh.branch.read",
            json!({"owner":"octo","repo":"hello","branch":"main"}),
        ),
        (
            "gh.commit.read",
            json!({"owner":"octo","repo":"hello","ref":"main"}),
        ),
        (
            "gh.content.read",
            json!({"owner":"octo","repo":"hello","path":"README.md"}),
        ),
    ] {
        let denied = Harness::<Gh>::get(component()).call(id, input);
        assert!(denied.is_err(), "{id} must have its own HTTP grant");
    }
}

#[test]
fn api_user_read_uses_only_the_existing_user_grant() -> Result<(), Box<dyn std::error::Error>> {
    let args = ["api", "users/octo"].map(str::to_owned);
    let CommandRunOutcome::Proposed {
        capability, input, ..
    } = provider::command::<Gh>(&args, false)
    else {
        panic!("finite route must propose")
    };
    assert_eq!(capability.as_str(), "gh.user.read");
    let output = scripted_component_get(
        Response { status: 200, headers: vec![], body: br#"{"login":"octo","type":"User","public_repos":1,"followers":2,"created_at":"2026-01-01T00:00:00Z"}"#.to_vec() },
        capability.as_str(),
        input,
        "/users/octo",
    )?;
    assert_eq!(output.status, 0, "{}", output.stderr);
    Ok(())
}

#[test]
fn body_file_is_only_read_during_authorized_invocation() {
    let args = [
        "issue",
        "comment",
        "9",
        "-R",
        "octo/hello",
        "--body-file",
        "-",
    ];
    let words = args.map(str::to_owned);
    let CommandRunOutcome::Proposed {
        capability, input, ..
    } = provider::command::<Gh>(&words, true)
    else {
        panic!("body-file must propose without reading bytes")
    };
    assert_eq!(capability.as_str(), "gh.issue.comment");
    assert_ne!(input["body"], "posted at invoke");
    assert_eq!(input["_stdinBody"], true);
    let no_pipe = provider::command::<Gh>(&words, false);
    assert!(matches!(no_pipe, CommandRunOutcome::Failed { .. }));
    let native = Native::<Gh>::new().stdin(b"posted at invoke".to_vec()).http(HttpScript::new(
        "api.github.com", "POST", Response { status: 201, headers: vec![], body: br#"{"id":1,"body":"posted at invoke","user":{"login":"octo"},"created_at":"2026-01-01T00:00:00Z"}"#.to_vec() }
    ));
    // A scripted write is allowed only after the input has been read and validated.
    let result = native.call(capability.as_str(), &input.to_string());
    assert_ne!(result.status, 2, "{}", result.stderr);
    let sent = native.requests();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].method, "POST");
    assert!(String::from_utf8_lossy(&sent[0].body).contains("posted at invoke"));
    assert!(
        !sent[0]
            .headers
            .iter()
            .any(|h| h.name.eq_ignore_ascii_case("authorization"))
    );
    let mismatched = Native::<Gh>::new();
    let bad_marker =
        json!({"owner":"octo","repo":"hello","number":9,"body":"literal","_stdinBody":true});
    let refused = mismatched.call("gh.issue.comment", &bad_marker.to_string());
    assert_ne!(refused.status, 0);
    assert!(mismatched.requests().is_empty());
    let too_large = Native::<Gh>::new().stdin(vec![b'a'; 4097]);
    let refused = too_large.call(capability.as_str(), &input.to_string());
    assert_ne!(refused.status, 0);
    assert!(too_large.requests().is_empty());
}

#[test]
fn marker_bytes_in_literal_and_direct_bodies_never_read_stdin() {
    let marker = "\u{0}gh:body-file:-\u{0}";
    let args = [
        "issue",
        "comment",
        "9",
        "-R",
        "octo/hello",
        "--body",
        marker,
    ];
    let CommandRunOutcome::Proposed { input: literal, .. } =
        provider::command::<Gh>(&args.map(str::to_owned), true)
    else {
        panic!("literal body proposes")
    };
    assert_eq!(literal["body"], marker);
    assert!(literal.get("_stdinBody").is_none());
    let direct = json!({"owner":"octo","repo":"hello","number":9,"body":marker});
    let reply = Response {
        status: 201,
        headers: vec![],
        body: br#"{"id":1,"body":"literal","user":{"login":"octo"},"created_at":"2026-01-01T00:00:00Z"}"#.to_vec(),
    };
    for (name, input, stdin) in [
        (
            "literal --body",
            literal,
            Some(b"different piped bytes".to_vec()),
        ),
        ("direct invocation", direct, None),
    ] {
        let mut native =
            Native::<Gh>::new().http(HttpScript::new("api.github.com", "POST", reply.clone()));
        if let Some(stdin) = stdin {
            native = native.stdin(stdin);
        }
        let output = native.call("gh.issue.comment", &input.to_string());
        assert_eq!(output.status, 0, "{name}: {}", output.stderr);
        let requests = native.requests();
        assert_eq!(requests.len(), 1, "{name}");
        let posted: Value = serde_json::from_slice(&requests[0].body).expect("JSON request");
        assert_eq!(posted["body"], marker, "{name}");
    }
}

#[test]
fn real_component_conforms_and_reaches_only_the_owner_base_url()
-> Result<(), Box<dyn std::error::Error>> {
    conformance::<Gh>(component())?;
    let denied = Harness::<Gh>::get(component())
        .call("gh.repo.read", json!({"owner":"octo","repo":"hello"}));
    assert!(denied.is_err(), "no HTTP grant must not reach GitHub");
    let response = Response { status: 200, headers: vec![Header::text("x-ratelimit-remaining", "10")?], body: json!({"name":"hello","full_name":"octo/hello","private":false,"default_branch":"main","archived":false,"fork":false,"updated_at":"2026-01-01T00:00:00Z"}).to_string().into_bytes() };
    let model_origin =
        Harness::<Gh>::get(component()).http(HttpScript::new("localhost", "GET", response.clone()));
    let origin = model_origin.origin().expect("script origin").to_owned();
    let refused = model_origin.call(
        "gh.repo.read",
        json!({"owner":"octo","repo":"hello","endpoint":origin}),
    )?;
    assert_ne!(refused.status, 0);
    assert!(refused.stdout.is_empty());
    assert!(refused.http_calls.is_empty());
    let invalid = Harness::<Gh>::get(component())
        .settings(json!({"baseUrl": "https://user@localhost"}))
        .call("gh.repo.read", json!({"owner":"octo","repo":"hello"}))?;
    assert_ne!(invalid.status, 0);
    assert!(invalid.stderr.contains("settings"), "{}", invalid.stderr);
    assert!(invalid.http_calls.is_empty());
    let null_base = Harness::<Gh>::get(component())
        .settings(json!({"baseUrl": null}))
        .call("gh.repo.read", json!({"owner":"octo","repo":"hello"}))?;
    assert_ne!(null_base.status, 0);
    assert!(
        null_base.stderr.contains("settings"),
        "{}",
        null_base.stderr
    );
    assert!(null_base.http_calls.is_empty());
    let owner_base =
        Harness::<Gh>::get(component()).http(HttpScript::new("localhost", "GET", response.clone()));
    let origin = owner_base.origin().expect("script origin").to_owned();
    let reached = owner_base
        .settings(json!({"baseUrl": format!("{origin}/")}))
        .call("gh.repo.read", json!({"owner":"octo","repo":"hello"}))?;
    assert_eq!(reached.status, 0, "{}", reached.stderr);
    assert_eq!(reached.http_calls.len(), 1);
    assert_eq!(
        serde_json::from_slice::<Value>(&reached.stdout)?["name"],
        "hello"
    );
    let native = Native::<Gh>::new().http(HttpScript::new("api.github.com", "GET", response));
    let output = native.call(
        "gh.repo.read",
        &json!({"owner":"octo","repo":"hello"}).to_string(),
    );
    assert_eq!(output.status, 0, "{}", output.stderr);
    assert_eq!(native.requests().len(), 1);
    let value: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(value["name"], "hello");
    Ok(())
}

fn scripted_component_get(
    response: Response,
    capability: &str,
    input: Value,
    path: &str,
) -> Result<dekopon_provider_sdk_testkit::ComponentOutput, Box<dyn std::error::Error>> {
    let harness =
        Harness::<Gh>::get(component()).http(HttpScript::new("localhost", "GET", response));
    let base = format!("{}/gh", harness.origin().expect("script origin"));
    let output = harness
        .settings(json!({"baseUrl": base}))
        .call(capability, input)?;
    assert_eq!(output.http_calls.len(), 1);
    let request = output.http_request.as_ref().expect("recorded request");
    assert_eq!(request.method, "GET");
    assert_eq!(request.uri, format!("{base}{path}"));
    assert!(
        !request
            .headers
            .iter()
            .any(|header| header.name.eq_ignore_ascii_case("authorization"))
    );
    Ok(output)
}

#[test]
fn forbidden_names_the_accepted_permission() -> Result<(), Box<dyn std::error::Error>> {
    let output = scripted_component_get(
        Response {
            status: 403,
            headers: vec![
                Header::text("X-Accepted-GitHub-Permissions", "contents=read")?,
                Header::text("x-accepted-oauth-scopes", "repo")?,
            ],
            body: br#"{"message":"untrusted response body"}"#.to_vec(),
        },
        "gh.repo.read",
        json!({"owner":"octo","repo":"hello"}),
        "/repos/octo/hello",
    )?;
    assert_ne!(output.status, 0);
    assert_eq!(
        output.stderr,
        "credential lacks a permission this call needs; GitHub accepts: contents=read\n"
    );
    Ok(())
}

#[test]
fn forbidden_names_classic_scopes_against_held_scopes() -> Result<(), Box<dyn std::error::Error>> {
    let output = scripted_component_get(
        Response {
            status: 403,
            headers: vec![
                Header::text("x-accepted-oauth-scopes", "repo")?,
                Header::text("x-oauth-scopes", "read:org")?,
            ],
            body: br#"{"message":"untrusted response body"}"#.to_vec(),
        },
        "gh.repo.read",
        json!({"owner":"octo","repo":"hello"}),
        "/repos/octo/hello",
    )?;
    assert_ne!(output.status, 0);
    assert_eq!(
        output.stderr,
        "credential lacks a scope; GitHub accepts: repo; credential has: read:org\n"
    );
    Ok(())
}

#[test]
fn long_headers_are_bounded_without_rejecting_the_call() -> Result<(), Box<dyn std::error::Error>> {
    let long = "x".repeat(5000);
    let output = scripted_component_get(
        Response {
            status: 403,
            headers: vec![Header::text("x-accepted-github-permissions", &long)?],
            body: vec![],
        },
        "gh.repo.read",
        json!({"owner":"octo","repo":"hello"}),
        "/repos/octo/hello",
    )?;
    assert_ne!(output.status, 0);
    assert_eq!(output.stderr.len(), 4097);
    assert!(
        output
            .stderr
            .starts_with("credential lacks a permission this call needs; GitHub accepts: ")
    );

    let output = scripted_component_get(
        Response {
            status: 200,
            headers: vec![
                Header::text("github-authentication-token-expiration", &long)?,
                Header::text("x-oauth-scopes", format!("future:scope,{long}"))?,
            ],
            body: json!({"resources":{"core":{"limit":1,"remaining":0,"reset":1}}})
                .to_string()
                .into_bytes(),
        },
        "gh.auth.status",
        json!({}),
        "/rate_limit",
    )?;
    assert_eq!(output.status, 0, "{}", output.stderr);
    let value: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(value["tokenExpiration"].as_str().unwrap().len(), 64);
    assert_eq!(value["scopes"].as_str().unwrap().len(), 4096);
    assert!(
        value["scopes"]
            .as_str()
            .unwrap()
            .starts_with("future:scope,")
    );
    Ok(())
}

#[test]
fn empty_permission_header_falls_back_to_classic_scopes() -> Result<(), Box<dyn std::error::Error>>
{
    for empty in ["", "   "] {
        let output = scripted_component_get(
            Response {
                status: 403,
                headers: vec![
                    Header::text("x-accepted-github-permissions", empty)?,
                    Header::text("x-accepted-oauth-scopes", "repo")?,
                ],
                body: vec![],
            },
            "gh.repo.read",
            json!({"owner":"octo","repo":"hello"}),
            "/repos/octo/hello",
        )?;
        assert_ne!(output.status, 0);
        assert_eq!(
            output.stderr,
            "credential lacks a scope; GitHub accepts: repo; credential has: not reported\n"
        );
    }
    Ok(())
}

#[test]
fn not_found_does_not_assert_existence() -> Result<(), Box<dyn std::error::Error>> {
    let output = scripted_component_get(
        Response {
            status: 404,
            headers: vec![],
            body: vec![],
        },
        "gh.repo.read",
        json!({"owner":"octo","repo":"hello"}),
        "/repos/octo/hello",
    )?;
    assert_ne!(output.status, 0);
    assert_eq!(
        output.stderr,
        "not found, or not visible to this credential\n"
    );
    Ok(())
}

#[test]
fn unauthorized_says_the_credential_was_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let output = scripted_component_get(
        Response {
            status: 401,
            headers: vec![],
            body: vec![],
        },
        "gh.repo.read",
        json!({"owner":"octo","repo":"hello"}),
        "/repos/octo/hello",
    )?;
    assert_ne!(output.status, 0);
    assert_eq!(
        output.stderr,
        "credential rejected: expired, revoked or malformed\n"
    );
    Ok(())
}

#[test]
fn auth_status_reports_quota_and_expiry() -> Result<(), Box<dyn std::error::Error>> {
    let CommandRunOutcome::Proposed {
        capability, input, ..
    } = provider::command::<Gh>(&["auth".to_owned(), "status".to_owned()], false)
    else {
        panic!("auth status must propose its capability")
    };
    assert_eq!(capability.as_str(), "gh.auth.status");
    assert_eq!(input, json!({}));
    for headers_present in [true, false] {
        let headers = if headers_present {
            vec![
                Header::text(
                    "GitHub-Authentication-Token-Expiration",
                    "2026-10-13 00:00:00 UTC",
                )?,
                Header::text("X-OAuth-Scopes", "repo, read:org")?,
            ]
        } else {
            vec![]
        };
        let output = scripted_component_get(
            Response {
                status: 200,
                headers,
                body: json!({
                    "resources": {
                        "core": {"limit": 5000, "remaining": 4999, "reset": 1791853200, "used": 1},
                        "search": {"limit": 30, "remaining": 29, "reset": 1791850000}
                    },
                    "rate": {"limit": 60, "remaining": 59, "reset": 1}
                })
                .to_string()
                .into_bytes(),
            },
            capability.as_str(),
            input.clone(),
            "/rate_limit",
        )?;
        assert_eq!(output.status, 0, "{}", output.stderr);
        let mut expected = json!({"limit": 5000, "remaining": 4999, "reset": 1791853200});
        if headers_present {
            expected["tokenExpiration"] = json!("2026-10-13 00:00:00 UTC");
            expected["scopes"] = json!("repo, read:org");
        }
        assert_eq!(serde_json::from_slice::<Value>(&output.stdout)?, expected);
    }
    Ok(())
}

#[test]
fn forbidden_without_permission_headers_stays_generic() -> Result<(), Box<dyn std::error::Error>> {
    let output = scripted_component_get(
        Response {
            status: 403,
            headers: vec![],
            body: br#"{"message":"untrusted response body"}"#.to_vec(),
        },
        "gh.repo.read",
        json!({"owner":"octo","repo":"hello"}),
        "/repos/octo/hello",
    )?;
    assert_ne!(output.status, 0);
    assert_eq!(
        output.stderr,
        "credential is not permitted to make this call\n"
    );
    Ok(())
}
