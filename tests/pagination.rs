use dekopon_broker_host::{
    BrokerHostError, BrokerHostLimits, BrokerHostOptions, BrokerProviderRegistry, PlaintextHosts,
    Streams, TestImports, asset::AssetInputs,
};
use dekopon_capability::{
    ExecutionConstraints, HttpConstraints, ProposedInvocation, broker::AuthorizationGate,
};
use dekopon_core::{Actor, TraceId};
use serde_json::{Value, json};
use std::{io::Read, sync::Arc};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

static REGISTRY: tokio::sync::OnceCell<BrokerProviderRegistry> = tokio::sync::OnceCell::const_new();

const CANONICAL: &str = "/gh/repositories/1403954647/pulls/7/files?per_page=100";

const FILES: &str = "/gh/repos/octo/hello/pulls/7/files?per_page=100";

#[derive(Clone, Copy)]
enum Case {
    Complete,
    Origin,
    Path,
    Oversized,
    Requests,
    Cap,
    ForeignId,
    Budget,
}

async fn run(case: Case) -> Result<(), Box<dyn std::error::Error>> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let authority = listener.local_addr()?.to_string();
    let origin = format!("http://{authority}");
    let response_origin = origin.clone();
    let expected = match case {
        Case::Complete | Case::Requests => 4,
        Case::Cap => 31,
        Case::Budget => 32,
        Case::Origin | Case::Path | Case::Oversized | Case::ForeignId => 3,
    };
    let page_path = match case {
        Case::Cap => FILES,
        Case::Complete
        | Case::Origin
        | Case::Path
        | Case::Oversized
        | Case::Requests
        | Case::ForeignId
        | Case::Budget => CANONICAL,
    };
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = Arc::clone(&seen);
    let server = tokio::spawn(async move {
        for index in 0..expected {
            let (mut stream, _) = listener.accept().await?;
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                if stream.read(&mut byte).await? == 0 {
                    return Err(std::io::Error::other("incomplete request"));
                }
                request.push(byte[0]);
            }
            let request = String::from_utf8(request).expect("HTTP request");
            let path = request
                .lines()
                .next()
                .expect("request line")
                .split_whitespace()
                .nth(1)
                .expect("request path");
            let expected_path = match index {
                0 => "/gh/repos/octo/hello/pulls/7".to_owned(),
                1 => FILES.to_owned(),
                31 if matches!(case, Case::Budget) => {
                    "/gh/repos/octo/hello/pulls/7/reviews?per_page=100".to_owned()
                }
                _ => format!("{page_path}&page={index}"),
            };
            assert_eq!(path, expected_path);
            assert!(request.starts_with("GET "));
            let host = request
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("host").then_some(value.trim())
                })
                .expect("Host header");
            assert_eq!(format!("http://{host}"), response_origin);
            assert!(
                !request
                    .lines()
                    .any(|line| line.to_ascii_lowercase().starts_with("authorization:"))
            );
            captured
                .lock()
                .expect("capture lock")
                .push(format!("{response_origin}{path}"));
            let body = if index == 0 {
                json!({"number":7,"title":"Ship","state":"open","head":{"ref":"topic","sha":"a".repeat(40)},"base":{"ref":"main","sha":"b".repeat(40),"repo":{"id":1403954647}},"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-02T00:00:00Z"}).to_string()
            } else if index == 31 && matches!(case, Case::Budget) {
                "[]".to_owned()
            } else if matches!(case, Case::Oversized) && index == 2 {
                " ".repeat(1024 * 1024 + 1)
            } else {
                json!((0..100).map(|file| json!({"filename":format!("{index}/{file}"),"status":"modified","additions":1,"deletions":0,"patch": if matches!(case, Case::Cap) { "x".repeat(25000) } else { "must not escape".to_owned() }})).collect::<Vec<_>>()).to_string()
            };
            let next = if index == 0
                || (index >= 30 && matches!(case, Case::Budget))
                || (index + 1 == expected
                    && !matches!(
                        case,
                        Case::Cap | Case::Origin | Case::Path | Case::ForeignId
                    )) {
                None
            } else if index == 2 && matches!(case, Case::ForeignId) {
                Some(format!(
                    "{response_origin}/gh/repositories/1403954648/pulls/7/files?per_page=100&page=3"
                ))
            } else if index == 2 && matches!(case, Case::Origin) {
                Some(format!("http://other.example{FILES}&page=3"))
            } else if index == 2 && matches!(case, Case::Path) {
                Some(format!(
                    "{response_origin}/gh/repos/octo/other/pulls/7/files?per_page=100&page=3"
                ))
            } else {
                Some(format!("{response_origin}{page_path}&page={}", index + 1))
            };
            let link = next
                .map(|next| format!("Link: <{next}>; rel=\"next\"\r\n"))
                .unwrap_or_default();
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n{link}Connection: close\r\n\r\n{body}",
                body.len()
            );
            if stream.write_all(reply.as_bytes()).await.is_err() && !matches!(case, Case::Oversized)
            {
                return Err(std::io::Error::other("response write failed"));
            }
        }
        Ok::<_, std::io::Error>(())
    });
    let component = std::env::var_os("DEKOPON_PROVIDER_COMPONENT")
        .expect("DEKOPON_PROVIDER_COMPONENT must name the freshly built component");
    let registry = REGISTRY
        .get_or_try_init(|| async {
            BrokerProviderRegistry::load_with_options(
                [std::path::PathBuf::from(component)],
                BrokerHostLimits::default(),
                None,
                &BrokerHostOptions {
                    plaintext_hosts: PlaintextHosts::new(["127.0.0.1"])
                        .expect("loopback fixture host"),
                    ..Default::default()
                },
            )
            .await
        })
        .await?;
    let proposal = ProposedInvocation::new(
        "pagination-test".parse()?,
        "gh.pull-request.read".parse()?,
        Actor::Agent {
            agent: "pagination-test".parse()?,
        },
        TraceId::new(*b"pagination-test!")?,
        json!({"owner":"octo","repo":"hello","number":7,"_cliOutput": match case { Case::Budget => "files,reviews,comments", _ => "files" }}),
    );
    let authorized = AuthorizationGate::new().authorize(
        proposal,
        "gh".parse()?,
        "pagination-decision".to_owned(),
        "test-broker".parse()?,
        "a".repeat(64),
        ExecutionConstraints {
            timeout_ms: 30000,
            http: Some(HttpConstraints {
                allowed_hosts: vec![authority],
                allowed_methods: vec!["GET".to_owned()],
                max_requests: if matches!(case, Case::Requests) {
                    3
                } else {
                    32
                },
                max_request_bytes: 65536,
                max_response_bytes: match case {
                    Case::Cap => 4 * 1024 * 1024,
                    _ => 1024 * 1024,
                },
                allow_plaintext_loopback: true,
                propagate_trace: false,
                request_templates: vec![],
            }),
            asset: None,
            storage: None,
            secret_use: None,
        },
    )?;
    let (writer, mut reader) = std::os::unix::net::UnixStream::pair()?;
    let capture = tokio::task::spawn_blocking(move || {
        let mut stdout = Vec::new();
        reader.read_to_end(&mut stdout).map(|_| stdout)
    });
    let imports = TestImports {
        settings: Some(json!({"baseUrl":format!("{origin}/gh")}).to_string()),
        ..Default::default()
    };
    let result = registry
        .invoke_with_test_imports(
            authorized,
            None,
            None,
            AssetInputs {
                streams: Some(Streams {
                    stdin: None,
                    stdout: writer.into(),
                }),
                ..Default::default()
            },
            Some(&imports),
        )
        .await;
    let stdout = capture.await??;
    match case {
        Case::Complete => {
            let output = result?;
            assert_eq!(output.http_calls.len(), expected);
            let value: Value = serde_json::from_slice(&stdout)?;
            let files = value["files"].as_array().expect("files array");
            assert_eq!(files.len(), (expected - 1) * 100);
            for (index, file) in files.iter().enumerate() {
                assert_eq!(
                    *file,
                    json!({"path":format!("{}/{}", index / 100 + 1, index % 100),"status":"modified","additions":1,"deletions":0})
                );
            }
        }
        Case::Origin
        | Case::Path
        | Case::Oversized
        | Case::Requests
        | Case::Cap
        | Case::ForeignId
        | Case::Budget => {
            let failure = result.expect_err("pagination must refuse");
            assert!(stdout.is_empty());
            assert_eq!(
                failure.http_calls.len(),
                if matches!(case, Case::Requests) {
                    3
                } else {
                    expected
                }
            );
            match (case, *failure.error) {
                (Case::Oversized, BrokerHostError::HostCallRejected { reason, .. }) => {
                    assert_eq!(reason, "byte-limit");
                }
                (
                    Case::Requests | Case::Budget,
                    BrokerHostError::HostCallRejected { reason, .. },
                ) => {
                    assert_eq!(reason, "host-call-limit");
                }
                (
                    Case::Origin | Case::Path | Case::Cap | Case::ForeignId,
                    BrokerHostError::ProviderFailure { status, stderr, .. },
                ) => {
                    assert_eq!(status, 3);
                    let message = if matches!(case, Case::Cap) {
                        "pull-request files exceed the page ceiling"
                    } else {
                        "endpoint returned an invalid resource"
                    };
                    assert_eq!(stderr, format!("{message}\n"));
                }
                (_, error) => panic!("unexpected failure: {error:?}"),
            }
        }
    }
    if matches!(case, Case::Requests) {
        server.abort();
        assert!(server.await.expect_err("server cancelled").is_cancelled());
    } else {
        tokio::time::timeout(std::time::Duration::from_secs(5), server).await???;
    }
    let observed = match case {
        Case::Requests => 3,
        Case::Complete
        | Case::Origin
        | Case::Path
        | Case::Oversized
        | Case::Cap
        | Case::ForeignId
        | Case::Budget => expected,
    };
    let expected_uris = (0..observed)
        .map(|index| {
            let path = match index {
                0 => "/gh/repos/octo/hello/pulls/7".to_owned(),
                1 => FILES.to_owned(),
                31 => "/gh/repos/octo/hello/pulls/7/reviews?per_page=100".to_owned(),
                _ => format!("{page_path}&page={index}"),
            };
            format!("{origin}{path}")
        })
        .collect::<Vec<_>>();
    assert_eq!(*seen.lock().expect("capture lock"), expected_uris);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn component_reads_three_file_pages_in_order_with_a_closed_projection()
-> Result<(), Box<dyn std::error::Error>> {
    run(Case::Complete).await
}

#[tokio::test(flavor = "multi_thread")]
async fn component_refuses_a_next_file_page_on_another_origin()
-> Result<(), Box<dyn std::error::Error>> {
    run(Case::Origin).await
}

#[tokio::test(flavor = "multi_thread")]
async fn component_refuses_a_next_file_page_on_another_path()
-> Result<(), Box<dyn std::error::Error>> {
    run(Case::Path).await
}

#[tokio::test(flavor = "multi_thread")]
async fn component_refuses_a_file_page_over_the_response_grant()
-> Result<(), Box<dyn std::error::Error>> {
    run(Case::Oversized).await
}

#[tokio::test(flavor = "multi_thread")]
async fn component_refuses_file_pages_beyond_the_request_grant()
-> Result<(), Box<dyn std::error::Error>> {
    run(Case::Requests).await
}

#[tokio::test(flavor = "multi_thread")]
async fn component_reads_three_thousand_files_and_refuses_a_further_page()
-> Result<(), Box<dyn std::error::Error>> {
    run(Case::Cap).await
}

#[tokio::test(flavor = "multi_thread")]
async fn component_refuses_a_canonical_file_page_for_another_repository()
-> Result<(), Box<dyn std::error::Error>> {
    run(Case::ForeignId).await
}

#[tokio::test(flavor = "multi_thread")]
async fn component_combined_expansions_exhaust_the_default_request_budget_without_output()
-> Result<(), Box<dyn std::error::Error>> {
    run(Case::Budget).await
}
