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
    assert_eq!(manifest.capabilities.len(), 19);
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
