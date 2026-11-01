#[path = "github.rs"]
mod github;
use dekopon_broker_host::{
    BrokerHostError, BrokerHostLimits, BrokerHostOptions, BrokerProviderRegistry, PlaintextHosts,
    Streams, TestImports, asset::AssetInputs,
};
use dekopon_capability::{
    ExecutionConstraints, HttpConstraints, ProposedInvocation, broker::AuthorizationGate,
};
use dekopon_core::{Actor, TraceId};
use serde_json::{Value, json};
use std::io::Read;
static REGISTRY: tokio::sync::OnceCell<BrokerProviderRegistry> = tokio::sync::OnceCell::const_new();

fn component() -> std::path::PathBuf {
    dekopon_broker_host::enable_test_component_memoization();
    std::env::var_os("DEKOPON_PROVIDER_COMPONENT")
        .expect("DEKOPON_PROVIDER_COMPONENT must name the freshly built component")
        .into()
}

pub async fn registry() -> &'static BrokerProviderRegistry {
    REGISTRY
        .get_or_init(|| async {
            BrokerProviderRegistry::load_with_options(
                [component()],
                BrokerHostLimits::default(),
                None,
                &BrokerHostOptions {
                    plaintext_hosts: PlaintextHosts::new(["127.0.0.1"]).expect("loopback"),
                    ..Default::default()
                },
            )
            .await
            .expect("production registry")
        })
        .await
}

pub async fn invoke_with_stdin(
    id: &str,
    input: Value,
    steps: Vec<Value>,
    stdin: Option<Vec<u8>>,
) -> (u8, String, Vec<u8>) {
    let expected = steps.len();
    let server = github::GitHub::start(steps).await;
    let base = server.base.clone();
    let proposal = ProposedInvocation::new(
        "have-flags".parse().expect("grant"),
        id.parse().expect("capability"),
        Actor::Agent {
            agent: "have-flags".parse().expect("agent"),
        },
        TraceId::new(*b"have-flags-test!").expect("trace"),
        input,
    );
    let authorized = AuthorizationGate::new()
        .authorize(
            proposal,
            "gh".parse().expect("provider"),
            "test-decision".into(),
            "test-broker".parse().expect("broker"),
            "a".repeat(64),
            ExecutionConstraints {
                timeout_ms: 30000,
                http: Some(HttpConstraints {
                    allowed_hosts: vec![
                        base.strip_prefix("http://")
                            .expect("HTTP")
                            .strip_suffix("/gh")
                            .expect("prefix")
                            .to_owned(),
                    ],
                    allowed_methods: vec![
                        "GET".into(),
                        "POST".into(),
                        "PUT".into(),
                        "PATCH".into(),
                        "DELETE".into(),
                    ],
                    max_requests: 32,
                    max_request_bytes: 1024 * 1024,
                    max_response_bytes: 4 * 1024 * 1024,
                    allow_plaintext_loopback: true,
                    propagate_trace: false,
                    signed_download: None,
                    request_templates: vec![],
                }),
                asset: None,
                storage: None,
                secret_use: None,
            },
        )
        .expect("authorize");
    let (writer, mut reader) = std::os::unix::net::UnixStream::pair().expect("stdout pair");
    let capture = tokio::task::spawn_blocking(move || {
        let mut output = Vec::new();
        reader.read_to_end(&mut output).expect("stdout");
        output
    });
    let stdin = stdin.map(|bytes| {
        let (mut writer, reader) = std::os::unix::net::UnixStream::pair().expect("stdin pair");
        std::io::Write::write_all(&mut writer, &bytes).expect("stdin bytes");
        reader.into()
    });
    let result = registry()
        .await
        .invoke_with_test_imports(
            authorized,
            Default::default(),
            AssetInputs {
                streams: Some(Streams {
                    stdin,
                    stdout: writer.into(),
                }),
                ..Default::default()
            },
            Some(&TestImports {
                settings: Some(json!({"baseUrl":base}).to_string()),
                ..Default::default()
            }),
        )
        .await;
    let stdout = capture.await.expect("capture joined");
    server.finish().await;
    match result {
        Ok(output) => {
            assert_eq!(output.http_calls.len(), expected);
            (0, String::new(), stdout)
        }
        Err(failure) => {
            assert_eq!(failure.http_calls.len(), expected);
            match *failure.error {
                BrokerHostError::ProviderFailure { status, stderr, .. } => (status, stderr, stdout),
                other => panic!("unexpected host failure: {other:?}"),
            }
        }
    }
}
