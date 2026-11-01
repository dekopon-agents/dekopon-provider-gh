use dekopon_broker::{
    Attestation, AttestorGrant, AuthenticatedContext, Broker, BrokerLimits, CapabilityRoute,
    ConstraintCatalog, ConstraintSet, CredentialStore, IdentityDirectory, InMemoryAuditLog,
    InvocationRequest, PolicyEngine, PolicyWorld,
};
use dekopon_broker_host::{
    BrokerHostLimits, BrokerHostOptions, BrokerProviderRegistry, PlaintextHosts, Streams,
    asset::AssetInputs,
};
use dekopon_capability::{
    EffectKind, ExecutionConstraints, HttpConstraints, InvocationOutcome, InvocationResult,
};
use dekopon_core::{Actor, RiskLevel};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

#[path = "support/fixture.rs"]
mod fixture;
#[path = "support/github.rs"]
mod github;

fn component() -> PathBuf {
    dekopon_broker_host::enable_test_component_memoization();
    std::env::var_os("DEKOPON_PROVIDER_COMPONENT")
        .expect("DEKOPON_PROVIDER_COMPONENT must name the freshly built component")
        .into()
}
fn input() -> Value {
    json!({"owner":"octo","repo":"config","path":"config/mutable/agent.yaml","branch":"runs/content-123","message":"update","mode":"create","body":""})
}
fn response(status: u16) -> Value {
    json!({"status":status,"headers":{"content-type":"application/json"},"body":{"json":{"content":{"path":"config/mutable/agent.yaml","type":"file","sha":"b".repeat(40)},"commit":{"sha":"c".repeat(40)}}}})
}
fn step(status: u16) -> Value {
    json!({"request":{"method":"PUT","path":"/repos/octo/config/contents/config/mutable/agent.yaml","headers":{"accept":"application/vnd.github+json"},"body":{"json":{"branch":"runs/content-123","message":"update","content":""}}},"response":response(status)})
}

async fn broker(path: &Path, base: &str) -> Broker<InMemoryAuditLog> {
    let registry = BrokerProviderRegistry::load_with_options(
        [path],
        BrokerHostLimits::default(),
        None,
        &BrokerHostOptions {
            plaintext_hosts: PlaintextHosts::new(["127.0.0.1"]).expect("loopback"),
            provider_settings: Arc::new(BTreeMap::from([(
                "gh".parse().expect("id"),
                json!({"baseUrl":base}).to_string(),
            )])),
            ..Default::default()
        },
    )
    .await
    .expect("registry");
    let policy = include_str!("../examples/content-write/policies.cedar")
        .split("@id(\"run-file-read\")")
        .next()
        .expect("write policy")
        .replace("api.github.com", "127.0.0.1")
        .replace("/repos/octo/", "/gh/repos/octo/");
    let world = PolicyWorld::new(
        ["content-writer", "other"].map(|s| s.parse().expect("principal")),
        ["gh.content.write", "gh.content.read"].map(|s| {
            (
                s.parse().expect("capability"),
                "gh".parse().expect("provider"),
            )
        }),
    )
    .expect("world");
    let constraints = ExecutionConstraints {
        timeout_ms: 10000,
        http: Some(HttpConstraints {
            allowed_hosts: vec![
                base.strip_prefix("http://")
                    .expect("HTTP")
                    .strip_suffix("/gh")
                    .expect("prefix")
                    .into(),
            ],
            allowed_methods: vec!["PUT".into()],
            max_requests: 1,
            max_request_bytes: 16384,
            max_response_bytes: 16384,
            allow_plaintext_loopback: true,
            propagate_trace: false,
            signed_download: None,
            request_templates: vec![],
        }),
        asset: None,
        storage: None,
        secret_use: None,
    };
    let catalog = ConstraintCatalog::new([(
        "gh.content.write".parse().expect("id"),
        ConstraintSet {
            provider: "gh".parse().expect("provider"),
            effect: EffectKind::ExternalWrite,
            risk: RiskLevel::Medium,
            credential: None,
            default_secret_use: None,
            route: CapabilityRoute::Generic,
            constraints,
        },
    )])
    .expect("catalog");
    Broker::new(
        registry,
        "broker-test".parse().expect("principal"),
        PolicyEngine::new(&policy, &world).expect("policy"),
        catalog,
        CredentialStore::empty(),
        IdentityDirectory::new([
            (
                "slack.t0123abc.ucontentwriter".parse().expect("subject"),
                "content-writer".parse().expect("principal"),
            ),
            (
                "slack.t0123abc.uother".parse().expect("subject"),
                "other".parse().expect("principal"),
            ),
        ])
        .expect("identities"),
        Arc::new(InMemoryAuditLog::new(128).expect("audit")),
        BrokerLimits::default(),
    )
    .expect("broker")
}

async fn invoke(
    broker: &Broker<InMemoryAuditLog>,
    principal: &str,
    agent: &str,
    input: Value,
) -> (InvocationResult, Vec<u8>) {
    let peer = AuthenticatedContext::new(
        "content-write-gateway".parse().expect("principal"),
        Actor::Service {
            principal: "content-write-gateway".parse().expect("principal"),
        },
    )
    .expect("context");
    let subject = match principal {
        "content-writer" => "slack.t0123abc.ucontentwriter",
        "other" => "slack.t0123abc.uother",
        _ => panic!("unknown fixture principal"),
    };
    let attestation = Attestation::for_subject(
        subject.parse().expect("subject"),
        agent.parse().expect("agent"),
    )
    .bound_to("content-write-test".parse().expect("invocation"));
    let request = InvocationRequest {
        id: "content-write-test".parse().expect("invocation"),
        capability: "gh.content.write".parse().expect("capability"),
        trace_parent: "00-0000000000000000000000000000f1c7-00000000000000f1-00"
            .parse()
            .expect("trace"),
        input,
        secret_use: None,
    };
    let (writer, mut reader) = std::os::unix::net::UnixStream::pair().expect("stdout");
    let capture = tokio::task::spawn_blocking(move || {
        let mut bytes = vec![];
        reader.read_to_end(&mut bytes).expect("capture");
        bytes
    });
    let result = broker
        .invoke(
            &peer,
            Some(&AttestorGrant { namespaces: None }),
            Some(&attestation),
            request,
            AssetInputs {
                streams: Some(Streams {
                    stdin: None,
                    stdout: writer.into(),
                }),
                ..Default::default()
            },
        )
        .await
        .expect("accounted invocation");
    (result.result, capture.await.expect("capture joined"))
}

#[tokio::test(flavor = "multi_thread")]
async fn real_broker_pins_identity_file_and_branch_before_any_egress() {
    let server = github::GitHub::start(vec![step(201)]).await;
    let broker = broker(&component(), &server.base).await;
    for (principal, agent, field, value) in [
        ("other", "content-writer", "owner", "octo"),
        ("content-writer", "other", "owner", "octo"),
        ("content-writer", "content-writer", "repo", "other"),
        (
            "content-writer",
            "content-writer",
            "path",
            "config/mutable/other.yaml",
        ),
        ("content-writer", "content-writer", "branch", "runs/other"),
    ] {
        let mut input = input();
        input[field] = json!(value);
        let (result, stdout) = invoke(&broker, principal, agent, input).await;
        assert_ne!(result.outcome, InvocationOutcome::Succeeded);
        assert!(stdout.is_empty());
        assert_eq!(server.egress(), 0, "{principal}/{agent}/{field}");
    }
    let (result, stdout) = invoke(&broker, "content-writer", "content-writer", input()).await;
    assert_eq!(result.outcome, InvocationOutcome::Succeeded, "{result:?}");
    assert_eq!(server.egress(), 1);
    assert_eq!(
        serde_json::from_slice::<Value>(&stdout).expect("output")["commitSha"],
        "c".repeat(40)
    );
    server.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn real_broker_denies_opaque_malformed_missing_and_query_requests_with_zero_egress() {
    let fixture = fixture::build(Some("content_write_fence"));
    let server = github::GitHub::start(vec![step(201)]).await;
    let broker = broker(&fixture, &server.base).await;
    let uri = format!(
        "{}/repos/octo/config/contents/config/mutable/agent.yaml",
        server.base
    );
    for (content_type, body) in [
        ("application/json", "".to_owned()),
        ("application/json", "{}".to_owned()),
        ("application/json", "{".to_owned()),
        ("application/json", "[]".to_owned()),
        ("text/plain", r#"{"branch":"runs/content-123"}"#.to_owned()),
        ("application/json", r#"{"branch":null}"#.to_owned()),
        ("application/json", r#"{"branch":7}"#.to_owned()),
        (
            "application/json",
            r#"{"branch":"runs/content-123","branch":"runs/content-123"}"#.to_owned(),
        ),
        (
            "application/json",
            format!(
                "{{\"branch\":\"runs/content-123\",\"extra\":\"{}\"}}",
                "x".repeat(16384)
            ),
        ),
    ] {
        let (result, stdout) = invoke(
            &broker,
            "content-writer",
            "content-writer",
            json!({"uri":uri,"contentType":content_type,"body":body}),
        )
        .await;
        assert_ne!(
            result.outcome,
            InvocationOutcome::Succeeded,
            "{content_type}"
        );
        assert!(stdout.is_empty());
        assert_eq!(server.egress(), 0);
    }
    let body = json!({"branch":"runs/content-123","message":"update","content":""}).to_string();
    for target in [
        format!("{uri}?ref=runs%2Fcontent-123"),
        uri.replace("/config/contents/", "/other/contents/"),
    ] {
        let (result, _) = invoke(
            &broker,
            "content-writer",
            "content-writer",
            json!({"uri":target,"contentType":"application/json","body":body}),
        )
        .await;
        assert_ne!(result.outcome, InvocationOutcome::Succeeded);
        assert_eq!(server.egress(), 0);
    }
    let (result, _) = invoke(
        &broker,
        "content-writer",
        "content-writer",
        json!({"uri":uri,"contentType":"application/json","body":body}),
    )
    .await;
    assert_eq!(result.outcome, InvocationOutcome::Succeeded, "{result:?}");
    assert_eq!(server.egress(), 1);
    server.finish().await;
}

#[path = "support/content_write_outcomes.rs"]
mod outcomes;

#[tokio::test(flavor = "multi_thread")]
async fn real_broker_write_failures_keep_status_and_message_without_retry() {
    let mut statuses = Vec::new();
    for case in outcomes::CASES {
        let server = github::GitHub::start(vec![step(case.http_status)]).await;
        let broker = broker(&component(), &server.base).await;
        let (result, stdout) = invoke(&broker, "content-writer", "content-writer", input()).await;
        assert_ne!(result.outcome, InvocationOutcome::Succeeded);
        assert!(stdout.is_empty());
        let exit_status = result.exit_status.expect("provider exit status").get();
        assert_eq!(exit_status, case.exit_status);
        statuses.push(exit_status);
        assert_eq!(
            result.detail.expect("provider message").message,
            case.stderr
        );
        assert_eq!(server.egress(), 1);
        server.finish().await;
    }
    let unknown = statuses.pop().expect("transport outcome");
    assert!(statuses[..4].iter().all(|status| *status != unknown));
    assert!(statuses[4..].iter().all(|status| *status == unknown));
    assert_ne!(statuses[0], statuses[1]);
    assert_ne!(statuses[0], statuses[2]);
}
