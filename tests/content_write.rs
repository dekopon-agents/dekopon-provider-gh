use base64::{Engine as _, engine::general_purpose::STANDARD};
use dekopon_broker_host::{BrokerHostLimits, BrokerProviderRegistry};
use dekopon_gh_provider::Gh;
use dekopon_provider_sdk::{
    CommandRunOutcome, EffectKind, RiskLevel,
    provider::{self, Response},
};
use dekopon_provider_sdk_testkit::{Harness, HttpScript};
use serde_json::{Value, json};

fn component() -> std::path::PathBuf {
    dekopon_broker_host::enable_test_component_memoization();
    std::env::var_os("DEKOPON_PROVIDER_COMPONENT")
        .expect("DEKOPON_PROVIDER_COMPONENT must name the freshly built component")
        .into()
}

fn input() -> Value {
    json!({"owner":"octo","repo":"config","path":"config/mutable/agent.yaml","branch":"runs/content-123","message":"Update configuration","mode":"create","body":"enabled: true\n"})
}

fn reply(status: u16) -> Response {
    Response {status, headers:vec![], body: json!({"content":{"path":"config/mutable/agent.yaml","type":"file","sha":"b".repeat(40)},"commit":{"sha":"c".repeat(40)},"submittedContentMustNotLeak":"SENTINEL"}).to_string().into_bytes()}
}

#[test]
fn create_and_blob_pinned_update_send_one_exact_put_through_real_cli_and_typed_calls() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let registry = runtime
        .block_on(BrokerProviderRegistry::load(
            [component()],
            BrokerHostLimits::default(),
        ))
        .expect("registry");
    for mode in ["create", "update"] {
        for cli in [false, true] {
            for body in [String::new(), "é".repeat(2048), "\0gh:body-file:-\0".into()] {
                let mut value = input();
                value["mode"] = json!(mode);
                value["body"] = json!(body);
                let mut argv = vec![
                    "repo".into(),
                    "write-file".into(),
                    "config/mutable/agent.yaml".into(),
                    "-R".into(),
                    "octo/config".into(),
                    "--branch".into(),
                    "runs/content-123".into(),
                    "--message".into(),
                    "Update configuration".into(),
                    "--body-file".into(),
                    "-".into(),
                ];
                if mode == "update" {
                    value["sha"] = json!("a".repeat(40));
                    argv.extend(["--sha".into(), "a".repeat(40)]);
                } else {
                    argv.push("--create".into());
                }
                if cli {
                    let outcome = runtime
                        .block_on(registry.run_command("gh", &argv, true))
                        .expect("command");
                    let CommandRunOutcome::Proposed {
                        capability,
                        input: proposal,
                        ..
                    } = outcome
                    else {
                        panic!("did not propose: {outcome:?}")
                    };
                    assert_eq!(capability.as_str(), "gh.content.write");
                    assert_eq!(proposal["_stdinBody"], true);
                    assert_eq!(proposal["mode"], mode);
                    assert_eq!(proposal["sha"], value["sha"]);
                    value = proposal;
                }
                let harness = Harness::<Gh>::get(component()).http(HttpScript::new(
                    "localhost",
                    "PUT",
                    reply(if mode == "create" { 201 } else { 200 }),
                ));
                let base = format!("{}/gh", harness.origin().expect("origin"));
                let output = harness
                    .settings(json!({"baseUrl":base}))
                    .stdin(body.as_bytes())
                    .call("gh.content.write", value)
                    .expect("component");
                assert_eq!(output.status, 0, "{}", output.stderr);
                assert_eq!(output.http_calls.len(), 1);
                let request = output.http_request.expect("request");
                assert_eq!(request.method, "PUT");
                assert_eq!(
                    request.uri,
                    format!("{base}/repos/octo/config/contents/config/mutable/agent.yaml")
                );
                assert!(
                    !request
                        .headers
                        .iter()
                        .any(|h| h.name.eq_ignore_ascii_case("authorization"))
                );
                let mut expected = json!({"branch":"runs/content-123","message":"Update configuration","content":STANDARD.encode(body.as_bytes())});
                if mode == "update" {
                    expected["sha"] = json!("a".repeat(40));
                }
                assert_eq!(
                    serde_json::from_slice::<Value>(&request.body).expect("body"),
                    expected
                );
                assert!(request.body.len() <= 16384);
                assert_eq!(
                    serde_json::from_slice::<Value>(&output.stdout).expect("output"),
                    json!({"owner":"octo","repo":"config","path":"config/mutable/agent.yaml","branch":"runs/content-123","blobSha":"b".repeat(40),"commitSha":"c".repeat(40)})
                );
            }
        }
    }
}

#[test]
fn malformed_closed_inputs_and_byte_bounds_refuse_without_component_http() {
    let mut cases = vec![];
    for (field, values) in [
        (
            "path",
            vec![
                json!(""),
                json!("../x"),
                json!("x//y"),
                json!("/x"),
                json!("x/"),
                json!("x/./y"),
                json!("%2e%2e/x"),
                json!("x\\y"),
                json!("x\n"),
            ],
        ),
        (
            "branch",
            vec![
                json!(""),
                json!("HEAD"),
                json!("x..y"),
                json!("x.lock"),
                json!(".hidden/x"),
                json!("x."),
                json!("x?y"),
                json!("x//y"),
            ],
        ),
        (
            "message",
            vec![json!(""), json!("é".repeat(2049)), json!("\0".repeat(4096))],
        ),
        ("body", vec![Value::Null, json!("é".repeat(2049))]),
        ("mode", vec![json!("other"), Value::Null]),
    ] {
        for value in values {
            let mut case = input();
            case[field] = value;
            cases.push(case);
        }
    }
    for sha in [json!("a".repeat(40)), Value::Null] {
        let mut case = input();
        case["sha"] = sha;
        cases.push(case);
    }
    for sha in [Value::Null, json!("a".repeat(39)), json!("g".repeat(40))] {
        let mut case = input();
        case["mode"] = json!("update");
        case["sha"] = sha;
        cases.push(case);
    }
    let mut missing_sha = input();
    missing_sha["mode"] = json!("update");
    cases.push(missing_sha);
    for extra in ["baseUrl", "origin", "bodyFile", "unexpected", "_cliOutput"] {
        let mut case = input();
        case[extra] = json!("x");
        cases.push(case);
    }
    for field in ["owner", "repo", "path", "branch", "message", "mode", "body"] {
        let mut case = input();
        case.as_object_mut().expect("object").remove(field);
        cases.push(case);
    }
    for case in cases {
        let output = Harness::<Gh>::get(component())
            .http(HttpScript::new("localhost", "PUT", reply(201)))
            .call("gh.content.write", case.clone())
            .expect("component");
        assert_ne!(output.status, 0, "{case}");
        assert!(output.stdout.is_empty());
        assert!(output.http_calls.is_empty(), "{case}");
        assert!(output.http_request.is_none());
    }
    let mut piped = input();
    piped["body"] = json!("\0gh:body-file:-\0");
    piped["_stdinBody"] = json!(true);
    for bytes in [vec![0xff], vec![b'x'; 4097]] {
        let output = Harness::<Gh>::get(component())
            .stdin(bytes)
            .http(HttpScript::new("localhost", "PUT", reply(201)))
            .call("gh.content.write", piped.clone())
            .expect("component");
        assert_ne!(output.status, 0);
        assert!(output.http_calls.is_empty());
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn command_modes_and_local_file_inputs_refuse_before_proposing_authority() {
    let base = [
        "repo",
        "write-file",
        "config/mutable/agent.yaml",
        "-R",
        "octo/config",
        "--branch",
        "runs/content-123",
        "--message",
        "update",
        "--body-file",
        "-",
    ];
    let mut cases = vec![base.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>()];
    let mut both = cases[0].clone();
    both.extend(["--create".into(), "--sha".into(), "a".repeat(40)]);
    cases.push(both);
    let mut local = cases[0].clone();
    *local.last_mut().expect("file") = "local.yaml".into();
    local.push("--create".into());
    cases.push(local);
    for field in ["--branch", "--message", "-R"] {
        let mut argv = cases[0].clone();
        let i = argv.iter().position(|s| s == field).expect("flag");
        argv.drain(i..i + 2);
        argv.push("--create".into());
        cases.push(argv);
    }
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let registry = runtime
        .block_on(BrokerProviderRegistry::load(
            [component()],
            BrokerHostLimits::default(),
        ))
        .expect("registry");
    for argv in cases {
        let result = runtime
            .block_on(registry.run_command("gh", &argv, true))
            .expect("command");
        assert!(
            !matches!(result, CommandRunOutcome::Proposed { .. }),
            "{argv:?}"
        );
    }
    let argv = base
        .into_iter()
        .chain(["--create"])
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert!(!matches!(
        runtime
            .block_on(registry.run_command("gh", &argv, false))
            .expect("command"),
        CommandRunOutcome::Proposed { .. }
    ));
}

#[test]
fn read_back_blob_sha_is_usable_without_widening_the_read_grant() {
    let harness=Harness::<Gh>::get(component()).http(HttpScript::new("localhost","GET",Response{status:200,headers:vec![],body:json!({"type":"file","path":"config/mutable/agent.yaml","sha":"a".repeat(40),"size":0,"content":"","encoding":"base64"}).to_string().into_bytes()}));
    let base = format!("{}/gh", harness.origin().expect("origin"));
    let read=harness.settings(json!({"baseUrl":base})).call("gh.content.read",json!({"owner":"octo","repo":"config","path":"config/mutable/agent.yaml","ref":"runs/content-123"})).expect("read");
    assert_eq!(read.status, 0, "{}", read.stderr);
    assert_eq!(read.http_calls.len(), 1);
    let request = read.http_request.expect("request");
    assert_eq!(request.method, "GET");
    assert_eq!(
        request.uri,
        format!(
            "{base}/repos/octo/config/contents/config/mutable/agent.yaml?ref=runs%2Fcontent-123"
        )
    );
    assert!(
        !request
            .headers
            .iter()
            .any(|h| h.name.eq_ignore_ascii_case("authorization"))
    );
    let mut update = input();
    update["mode"] = json!("update");
    update["sha"] =
        serde_json::from_slice::<Value>(&read.stdout).expect("read output")["sha"].clone();
    let harness =
        Harness::<Gh>::get(component()).http(HttpScript::new("localhost", "PUT", reply(200)));
    let base = format!("{}/gh", harness.origin().expect("origin"));
    let output = harness
        .settings(json!({"baseUrl":base}))
        .call("gh.content.write", update)
        .expect("update");
    assert_eq!(output.status, 0, "{}", output.stderr);
    assert_eq!(output.http_calls.len(), 1);
    let request = output.http_request.expect("request");
    assert_eq!(request.method, "PUT");
    assert_eq!(
        request.uri,
        format!("{base}/repos/octo/config/contents/config/mutable/agent.yaml")
    );
    assert!(
        !request
            .headers
            .iter()
            .any(|h| h.name.eq_ignore_ascii_case("authorization"))
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&request.body).expect("body"),
        json!({"branch":"runs/content-123","message":"Update configuration","content":STANDARD.encode("enabled: true\n"),"sha":"a".repeat(40)})
    );
}

#[test]
fn component_manifest_help_and_embedded_skill_expose_only_the_separate_write() {
    let manifest = provider::manifest::<Gh>().expect("manifest");
    let write = manifest
        .capabilities
        .iter()
        .find(|c| c.id.as_str() == "gh.content.write")
        .expect("write");
    assert_eq!(write.effect, EffectKind::ExternalWrite);
    assert_eq!(write.risk, RiskLevel::Medium);
    assert_eq!(write.input_schema["additionalProperties"], false);
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let registry = runtime
        .block_on(BrokerProviderRegistry::load(
            [component()],
            BrokerHostLimits::default(),
        ))
        .expect("registry");
    let outcome = runtime
        .block_on(registry.run_command(
            "gh",
            &["repo", "write-file", "--help"].map(str::to_owned),
            false,
        ))
        .expect("help");
    let CommandRunOutcome::Rendered { stdout, status, .. } = outcome else {
        panic!("help proposed")
    };
    assert_eq!(status, 0);
    for flag in ["--branch", "--message", "--sha", "--create", "--body-file"] {
        assert!(stdout.contains(flag));
    }
    let skills = dekopon_broker_host::skills::read(
        &"gh".parse().expect("id"),
        &std::fs::read(component()).expect("component bytes"),
    )
    .expect("skills");
    let skill = skills
        .iter()
        .find(|s| s.path == "gh-content-write/SKILL.md")
        .expect("embedded skill");
    assert_eq!(
        skill.bytes,
        include_bytes!("../skills/gh-content-write/SKILL.md")
    );
    assert!(
        !include_str!("../examples/pr-summarizer-linter/policies.cedar")
            .contains("gh.content.write")
    );
}

#[path = "support/content_write_outcomes.rs"]
mod outcomes;

#[test]
fn real_component_http_refusals_keep_status_and_stderr_without_retry() {
    for case in &outcomes::CASES[..4] {
        let harness = Harness::<Gh>::get(component()).http(HttpScript::new(
            "localhost",
            "PUT",
            reply(case.http_status),
        ));
        let base = format!("{}/gh", harness.origin().expect("origin"));
        let output = harness
            .settings(json!({"baseUrl":base}))
            .call("gh.content.write", input())
            .expect("component");
        assert_ne!(output.status, 0);
        assert_eq!(output.status, case.exit_status);
        assert_eq!(output.stderr, case.stderr);
        assert!(output.stdout.is_empty());
        assert_eq!(output.http_calls.len(), 1);
        let request = output.http_request.expect("request");
        assert_eq!(request.method, "PUT");
        assert_eq!(
            request.uri,
            format!("{base}/repos/octo/config/contents/config/mutable/agent.yaml")
        );
        assert!(
            !request
                .headers
                .iter()
                .any(|header| header.name.eq_ignore_ascii_case("authorization"))
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&request.body).expect("request body"),
            json!({"branch":"runs/content-123","message":"Update configuration","content":STANDARD.encode("enabled: true\n")})
        );
    }
}

#[path = "support/component.rs"]
mod component_harness;

#[tokio::test(flavor = "multi_thread")]
async fn real_component_transport_drop_keeps_unknown_status_and_stderr_without_retry() {
    let case = outcomes::CASES.last().expect("transport case");
    let step = json!({"request":{"method":"PUT","path":"/repos/octo/config/contents/config/mutable/agent.yaml","headers":{"accept":"application/vnd.github+json"},"body":{"json":{"branch":"runs/content-123","message":"Update configuration","content":STANDARD.encode("enabled: true\n")}}},"response":{"status":0}});
    let (status, stderr, stdout) =
        component_harness::invoke_with_stdin("gh.content.write", input(), vec![step], None).await;
    assert_ne!(status, 0);
    assert_eq!(status, case.exit_status);
    assert_eq!(stderr, case.stderr);
    assert!(stdout.is_empty());
}
