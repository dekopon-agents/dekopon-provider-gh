use base64::Engine as _;
use dekopon_broker_host::{
    BrokerHostError, BrokerHostLimits, BrokerHostOptions, BrokerProviderRegistry, PlaintextHosts,
    Streams, TestImports, asset::AssetInputs,
};
use dekopon_capability::{
    ExecutionConstraints, HttpConstraints, ProposedInvocation, broker::AuthorizationGate,
};
use dekopon_core::{Actor, TraceId};
use dekopon_gh_provider::Gh;
use dekopon_provider_sdk::{CommandRunOutcome, provider::Response};
use dekopon_provider_sdk_testkit::{Harness, HttpScript};
use serde_json::{Value, json};
use std::{io::Read, sync::Arc};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

static REGISTRY: tokio::sync::OnceCell<BrokerProviderRegistry> = tokio::sync::OnceCell::const_new();

fn component() -> std::path::PathBuf {
    std::env::var_os("DEKOPON_PROVIDER_COMPONENT")
        .expect("DEKOPON_PROVIDER_COMPONENT must name the freshly built component")
        .into()
}

async fn registry() -> &'static BrokerProviderRegistry {
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

fn bytes(body: &Value) -> Vec<u8> {
    if body.is_null() {
        Vec::new()
    } else if let Some(value) = body.get("json") {
        serde_json::to_vec(value).expect("JSON body")
    } else if let Some(value) = body.get("text") {
        value.as_str().expect("text").as_bytes().to_vec()
    } else {
        base64::engine::general_purpose::STANDARD
            .decode(body["base64"].as_str().expect("base64"))
            .expect("body bytes")
    }
}

async fn invoke(id: &str, input: Value, steps: Vec<Value>) -> (u8, String, Vec<u8>) {
    invoke_with_stdin(id, input, steps, None).await
}

async fn invoke_with_stdin(
    id: &str,
    input: Value,
    steps: Vec<Value>,
    stdin: Option<Vec<u8>>,
) -> (u8, String, Vec<u8>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let authority = listener.local_addr().expect("address").to_string();
    let base = format!("http://{authority}/gh");
    let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = Arc::clone(&seen);
    let expected = steps.len();
    let server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let index = count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let step = steps
                .get(index)
                .expect("no HTTP before validation or beyond the expected request count");
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).await.expect("request headers");
                head.push(byte[0]);
            }
            let head = String::from_utf8(head).expect("UTF-8 headers");
            let request = &step["request"];
            let query = request["query"]
                .as_str()
                .map(|value| format!("?{value}"))
                .unwrap_or_default();
            assert_eq!(
                head.lines().next().expect("request line"),
                format!(
                    "{} /gh{}{query} HTTP/1.1",
                    request["method"].as_str().expect("method"),
                    request["path"].as_str().expect("path")
                )
            );
            assert!(
                !head
                    .lines()
                    .any(|line| line.to_ascii_lowercase().starts_with("authorization:"))
            );
            let headers = head
                .lines()
                .skip(1)
                .filter_map(|line| line.split_once(':'))
                .map(|(key, value)| (key.to_ascii_lowercase(), value.trim().to_owned()))
                .collect::<std::collections::BTreeMap<_, _>>();
            assert_eq!(headers.get("host"), Some(&authority));
            assert_eq!(
                headers.get("accept").map(String::as_str),
                request["headers"]["accept"].as_str()
            );
            let length = headers
                .get("content-length")
                .map(|value| value.parse::<usize>().expect("length"))
                .unwrap_or(0);
            let mut body = vec![0; length];
            stream.read_exact(&mut body).await.expect("request body");
            if request["body"].is_null() {
                assert!(body.is_empty());
            } else if request["body"].get("json").is_some() {
                assert_eq!(
                    serde_json::from_slice::<Value>(&body).expect("JSON request"),
                    request["body"]["json"]
                );
            } else {
                assert_eq!(body, bytes(&request["body"]));
            }
            let response = &step["response"];
            let body = bytes(&response["body"]);
            let mut reply = format!(
                "HTTP/1.1 {} OK\r\nContent-Length: {}\r\nConnection: close\r\n",
                response["status"].as_u64().expect("status"),
                body.len()
            );
            for (name, value) in response["headers"].as_object().expect("response headers") {
                if ["content-length", "connection", "transfer-encoding"]
                    .contains(&name.to_ascii_lowercase().as_str())
                {
                    continue;
                }
                let values = value
                    .as_array()
                    .map(|values| values.iter().collect::<Vec<_>>())
                    .unwrap_or_else(|| vec![value]);
                for value in values {
                    reply.push_str(&format!(
                        "{name}: {}\r\n",
                        value.as_str().expect("header value")
                    ));
                }
            }
            reply.push_str("\r\n");
            stream
                .write_all(reply.as_bytes())
                .await
                .expect("response head");
            stream.write_all(&body).await.expect("response body");
        }
    });
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
            None,
            None,
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
    server.abort();
    let error = server
        .await
        .expect_err("the script waits for any unexpected request");
    assert!(error.is_cancelled(), "HTTP script failed: {error}");
    assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), expected);
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

fn exchange(path: &str, method: &str, body: Value, response: Value) -> Value {
    let (path, query) = path
        .split_once('?')
        .map_or((path, None), |(path, query)| (path, Some(query)));
    json!({"request":{"method":method,"path":path,"query":query,"headers":{"accept":"application/vnd.github+json"},"body": if body.is_null() { Value::Null } else { json!({"json":body}) }},"response":{"status":200,"headers":{},"body":{"json":response}}})
}

#[tokio::test(flavor = "multi_thread")]
async fn recorded_have_flags_replay_through_production_capabilities_with_exact_requests() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cassettes");
    let cases: Value =
        serde_json::from_slice(&std::fs::read(root.join("have-flags.json")).expect("cases"))
            .expect("JSON cases");
    let mut covered = std::collections::BTreeSet::new();
    for case in cases.as_array().expect("case array") {
        let steps = case["files"]
            .as_array()
            .expect("files")
            .iter()
            .map(|name| {
                let name = name.as_str().expect("filename");
                covered.insert(name.to_owned());
                serde_json::from_slice(
                    &std::fs::read(root.join("gh").join(name)).expect("recording"),
                )
                .expect("exchange")
            })
            .collect();
        let (status, stderr, stdout) = invoke(
            case["capability"].as_str().expect("id"),
            case["input"].clone(),
            steps,
        )
        .await;
        assert_eq!(
            status,
            case["status"].as_u64().unwrap_or(0) as u8,
            "{}: {stderr}",
            case["name"]
        );
        if case["status"] == 3 {
            assert_eq!(
                stderr,
                "search indexing is incomplete; results are not exhaustive\n"
            );
        }
        if case["name"] == "search issues" {
            let rows: Value = serde_json::from_slice(&stdout).expect("results");
            for row in rows.as_array().expect("array") {
                assert_eq!(
                    row["repository"]["nameWithOwner"],
                    "dekopon-agents/gh-parity-scratch"
                );
            }
            assert!(
                rows.as_array().expect("array").is_empty(),
                "the literal nonexistent label cannot broaden scope"
            );
        }
    }
    for path in std::fs::read_dir(root.join("gh")).expect("recordings") {
        let name = path.expect("path").file_name().into_string().expect("name");
        assert!(
            name.starts_with("0001-") || covered.contains(&name),
            "unreplayed exchange {name}"
        );
    }
}

#[test]
fn every_recording_redacts_authorization_and_token_windows() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cassettes");
    let token = std::env::var("GH_CASSETTE_TOKEN").ok();
    for entry in std::fs::read_dir(root.join("gh")).expect("recordings") {
        let path = entry.expect("path").path();
        let bytes = std::fs::read(path).expect("recorded bytes");
        let value: Value = serde_json::from_slice(&bytes).expect("recorded JSON");
        for side in ["request", "response"] {
            for (name, value) in value[side]["headers"].as_object().expect("headers") {
                if name.eq_ignore_ascii_case("authorization") {
                    assert_eq!(value, "[redacted]");
                }
            }
        }
        if let Some(token) = &token {
            assert!(
                !token
                    .as_bytes()
                    .windows(8)
                    .any(|window| bytes.windows(8).any(|recorded| recorded == window)),
                "recording contains credential bytes"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_line_items_and_oversized_reviews_refuse_before_any_request() {
    let mut base = json!({"owner":"octo","repo":"hello","number":7,"comments":[{"path":"src/lib.rs","line":2,"body":"review"}]});
    for item in [
        json!({"path":"../lib.rs","line":2,"body":"review"}),
        json!({"path":"lib.rs","line":0,"body":"review"}),
        json!({"path":"lib.rs","line":2,"startLine":2,"body":"review"}),
        json!({"path":"lib.rs","line":2,"side":"UP","body":"review"}),
        json!({"path":"lib.rs","line":2,"body":""}),
    ] {
        base["comments"] = json!([item]);
        let (status, stderr, stdout) =
            invoke("gh.pull-request.comment", base.clone(), vec![]).await;
        assert_eq!(status, 2);
        assert_eq!(stderr, "input does not match the capability contract\n");
        assert!(stdout.is_empty());
    }
    base["comments"] = json!(
        (0..5)
            .map(|_| json!({"path":"lib.rs","line":2,"body":"x".repeat(4096)}))
            .collect::<Vec<_>>()
    );
    let (status, stderr, stdout) = invoke("gh.pull-request.comment", base, vec![]).await;
    assert_eq!(status, 2);
    assert_eq!(stderr, "the review request would exceed 16384 bytes\n");
    assert!(stdout.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn moved_heads_refuse_all_review_events_and_line_comments_after_one_get() {
    let pull = json!({"number":7,"title":"Moved","state":"open","draft":false,"merged":false,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-02T00:00:00Z","head":{"ref":"feature","sha":"a".repeat(40)},"base":{"ref":"main","sha":"c".repeat(40)}});
    for (id, extra) in [
        ("gh.pull-request.approve", json!({})),
        ("gh.pull-request.comment", json!({"body":"review"})),
        ("gh.pull-request.request-changes", json!({"body":"review"})),
        (
            "gh.pull-request.comment",
            json!({"comments":[{"path":"lib.rs","line":1,"body":"review"}]}),
        ),
    ] {
        let mut input =
            json!({"owner":"octo","repo":"hello","number":7,"expectedHeadSha":"b".repeat(40)});
        input
            .as_object_mut()
            .expect("input")
            .extend(extra.as_object().expect("extra").clone());
        let (status, stderr, stdout) = invoke(
            id,
            input,
            vec![exchange(
                "/repos/octo/hello/pulls/7",
                "GET",
                Value::Null,
                pull.clone(),
            )],
        )
        .await;
        assert_eq!(status, 1);
        assert_eq!(
            stderr,
            "the pull request head no longer matches expectedHeadSha\n"
        );
        assert!(stdout.is_empty());
    }
}

#[test]
fn every_search_and_list_flag_proposes_typed_fields_and_sends_one_exact_request() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let cases: Value =
        serde_json::from_str(include_str!("have-flags-matrix.json")).expect("flag cases");
    for case in cases.as_array().expect("cases") {
        let argv = case["argv"]
            .as_array()
            .expect("argv")
            .iter()
            .map(|v| v.as_str().expect("word").to_owned())
            .collect::<Vec<_>>();
        let outcome = runtime
            .block_on(async { registry().await.run_command("gh", &argv, false).await })
            .expect("command");
        let CommandRunOutcome::Proposed {
            capability, input, ..
        } = outcome
        else {
            panic!("{} did not propose: {outcome:?}", case["name"])
        };
        assert_eq!(
            capability.as_str(),
            case["capability"].as_str().expect("id"),
            "{}",
            case["name"]
        );
        assert_eq!(input, case["input"], "{}", case["name"]);
        let response = if case["search"] == true {
            json!({"total_count":0,"incomplete_results":false,"items":[]})
        } else {
            json!([])
        };
        let harness = Harness::<Gh>::get(component()).http(HttpScript::new(
            "localhost",
            "GET",
            Response {
                status: 200,
                headers: vec![],
                body: serde_json::to_vec(&response).expect("response"),
            },
        ));
        let base = format!("{}/prefix", harness.origin().expect("origin"));
        let output = harness
            .settings(json!({"baseUrl":base}))
            .call(capability.as_str(), input)
            .expect("component invocation");
        assert_eq!(output.status, 0, "{}: {}", case["name"], output.stderr);
        assert_eq!(output.http_calls.len(), 1, "{}", case["name"]);
        let request = output.http_request.expect("request");
        assert_eq!(request.method, "GET");
        assert_eq!(
            request.uri,
            format!("{base}{}", case["path"].as_str().expect("path")),
            "{}",
            case["name"]
        );
        assert!(
            !request
                .headers
                .iter()
                .any(|header| header.name.eq_ignore_ascii_case("authorization"))
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn dispatch_refusals_preserve_exact_messages_and_never_propose_http_authority() {
    let cases = [
        (
            vec!["issue", "comment", "7", "-R", "o/r", "--delete-last"],
            "gh issue comment: --delete-last requires --yes",
        ),
        (
            vec!["issue", "comment", "7", "-R", "o/r", "--create-if-none"],
            "gh issue comment: --create-if-none requires --edit-last",
        ),
        (
            vec![
                "issue",
                "comment",
                "7",
                "-R",
                "o/r",
                "--edit-last",
                "--delete-last",
                "--yes",
            ],
            "gh issue comment: --edit-last and --delete-last cannot be used together",
        ),
        (
            vec!["issue", "comment", "7", "-R", "o/r", "--yes", "--body", "x"],
            "gh issue comment: --yes requires --delete-last",
        ),
        (
            vec![
                "issue",
                "comment",
                "7",
                "-R",
                "o/r",
                "--delete-last",
                "--yes",
                "--body",
                "x",
            ],
            "gh issue comment: --delete-last and --body cannot be used together",
        ),
        (
            vec![
                "issue",
                "comment",
                "7",
                "-R",
                "o/r",
                "--delete-last",
                "--yes",
                "--body-file",
                "-",
            ],
            "gh issue comment: --delete-last and --body-file cannot be used together",
        ),
        (
            vec!["repo", "list", "o", "--source", "--fork"],
            "gh repo list: --source and --fork cannot be used together",
        ),
        (
            vec!["repo", "list", "o", "--archived", "--no-archived"],
            "gh repo list: --archived and --no-archived cannot be used together",
        ),
        (
            vec!["repo", "list", "o", "--private", "--public"],
            "gh repo list: --private and --public cannot be used together",
        ),
        (
            vec!["repo", "list", "o", "--visibility", "private", "--private"],
            "gh repo list: --visibility and --private cannot be used together",
        ),
        (
            vec!["repo", "list", "o", "--visibility", "public", "--public"],
            "gh repo list: --visibility and --public cannot be used together",
        ),
        (
            vec!["pr", "diff", "7", "-R", "o/r", "--patch", "--name-only"],
            "gh pr diff: --patch and --name-only cannot be used together",
        ),
        (
            vec!["pr", "diff", "7", "-R", "o/r", "--exclude", "["],
            "gh pr diff --exclude: invalid value",
        ),
        (
            vec!["pr", "list", "-R", "o/r", "--search", "repo:elsewhere/repo"],
            "gh pr list --search: query cannot change the structural scope",
        ),
        (
            vec!["issue", "list", "-R", "o/r", "-S", "term OR other"],
            "gh issue list --search: query cannot change the structural scope",
        ),
        (
            vec!["pr", "list", "-R", "o/r", "--assignee", "bad/login"],
            "gh pr list --assignee: invalid value",
        ),
        (
            vec!["issue", "list", "-R", "o/r", "--label", "bad\"label"],
            "gh issue list --label: invalid value",
        ),
        (
            vec![
                "search",
                "repos",
                "term",
                "--owner",
                "o",
                "--created",
                "2020-02-30",
            ],
            "gh search repos --created: invalid value",
        ),
        (
            vec!["search", "repos", "term", "--owner", "o", "--stars", ">=x"],
            "gh search repos --stars: invalid value",
        ),
        (
            vec!["search", "commits", "term", "-R", "o/r", "--hash", "xyz"],
            "gh search commits --hash: invalid value",
        ),
        (
            vec![
                "pr",
                "review",
                "7",
                "-R",
                "o/r",
                "--approve",
                "--match-head-commit",
                "bad",
            ],
            "gh pr review --match-head-commit: invalid value",
        ),
        (
            vec!["pr", "checks", "7", "-R", "o/r", "--required"],
            "gh: --required is not supported: required-check data is GraphQL-only",
        ),
        (
            vec!["pr", "list", "-R", "o/r", "--author", "bad/login"],
            "gh: invalid author login",
        ),
        (
            vec![
                "pr",
                "list",
                "-R",
                "o/r",
                "--author",
                "octocat",
                "--app",
                "github-actions",
            ],
            "gh pr list: --author and --app cannot be used together",
        ),
        (
            vec![
                "issue",
                "list",
                "-R",
                "o/r",
                "--author",
                "octocat",
                "--app",
                "github-actions",
            ],
            "gh issue list: --author and --app cannot be used together",
        ),
        (
            vec![
                "search",
                "issues",
                "term",
                "-R",
                "o/r",
                "--search-type",
                "semantic",
                "--include-prs",
            ],
            "gh search issues: --search-type and --include-prs cannot be used together",
        ),
        (
            vec![
                "search",
                "issues",
                "term",
                "-R",
                "o/r",
                "--search-type",
                "hybrid",
                "--sort",
                "created",
            ],
            "gh search issues: --search-type and --sort cannot be used together",
        ),
        (
            vec![
                "search",
                "issues",
                "term",
                "-R",
                "o/r",
                "--search-type",
                "semantic",
                "--order",
                "asc",
            ],
            "gh search issues: --search-type and --order cannot be used together",
        ),
    ];
    let argv = [
        "pr",
        "list",
        "-R",
        "o/r",
        "--search",
        "repo:other/repo",
        "--app",
        "bad\"app",
    ]
    .map(str::to_owned);
    let CommandRunOutcome::Failed { error } = registry()
        .await
        .run_command("gh", &argv, false)
        .await
        .expect("rewrite")
    else {
        panic!("invalid app must refuse")
    };
    assert_eq!(error.message, "gh pr list --app: invalid value");
    for (argv, expected) in cases {
        let argv = argv.into_iter().map(str::to_owned).collect::<Vec<_>>();
        let CommandRunOutcome::Failed { error } = registry()
            .await
            .run_command("gh", &argv, true)
            .await
            .expect("command")
        else {
            panic!("must refuse {argv:?}")
        };
        assert_eq!(error.code, "usage");
        assert_eq!(error.message, expected, "{argv:?}");
    }
    for (argv, first) in [
        (
            vec!["pr", "list", "-R", "o/r", "--web", "--state", "bogus"],
            "error: invalid value 'bogus' for '--state <STATE>'",
        ),
        (
            vec!["repo", "list", "o", "--visibility", "bogus"],
            "error: invalid value 'bogus' for '--visibility <VALUE>'",
        ),
        (
            vec!["pr", "view", "7", "-R", "o/r", "--commments"],
            "error: unexpected argument '--commments' found",
        ),
        (
            vec!["search", "repos", "--owner", "o"],
            "error: the following required arguments were not provided:",
        ),
        (
            vec!["pr", "create"],
            "error: unrecognized subcommand 'create'",
        ),
        (
            vec![
                "repo",
                "read-file",
                "README.md",
                "-R",
                "o/r",
                "--output",
                "x",
            ],
            "error: unexpected argument '--output' found",
        ),
        (
            vec!["repo", "view", "o/r", "--branch", "main"],
            "error: unexpected argument '--branch' found",
        ),
        (
            vec![
                "pr",
                "merge",
                "7",
                "-R",
                "o/r",
                "--merge",
                "--author-email",
                "x@y",
            ],
            "error: unexpected argument '--author-email' found",
        ),
    ] {
        let argv = argv.into_iter().map(str::to_owned).collect::<Vec<_>>();
        let CommandRunOutcome::Rendered {
            stdout,
            stderr,
            status,
        } = registry()
            .await
            .run_command("gh", &argv, false)
            .await
            .expect("command")
        else {
            panic!("must render clap error {argv:?}")
        };
        assert_eq!(status, 2);
        assert!(stdout.is_empty());
        assert_eq!(stderr.lines().next(), Some(first));
        assert!(stderr.ends_with('\n'));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn range_comments_keep_one_pinned_post_and_merge_reads_its_piped_body_before_http() {
    let pull = json!({"number":7,"title":"Review","state":"open","draft":false,"merged":false,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-02T00:00:00Z","head":{"ref":"feature","sha":"a".repeat(40)},"base":{"ref":"main","sha":"c".repeat(40)}});
    let comment = json!({"path":"src/lib.rs","line":5,"startLine":3,"side":"LEFT","body":"range"});
    let body = json!({"event":"COMMENT","commit_id":"a".repeat(40),"comments":[{"path":"src/lib.rs","line":5,"start_line":3,"side":"LEFT","start_side":"LEFT","body":"range"}]});
    let steps = vec![
        exchange(
            "/repos/octo/hello/pulls/7",
            "GET",
            Value::Null,
            pull.clone(),
        ),
        exchange(
            "/repos/octo/hello/pulls/7/reviews",
            "POST",
            body,
            json!({"id":1,"state":"COMMENTED","commit_id":"a".repeat(40)}),
        ),
    ];
    let (status, stderr, _) = invoke(
        "gh.pull-request.comment",
        json!({"owner":"octo","repo":"hello","number":7,"comments":[comment]}),
        steps,
    )
    .await;
    assert_eq!(status, 0, "{stderr}");
    let argv = [
        "pr",
        "merge",
        "7",
        "-R",
        "octo/hello",
        "--squash",
        "--subject",
        "Subject",
        "-F",
        "-",
    ]
    .map(str::to_owned);
    let CommandRunOutcome::Proposed {
        capability, input, ..
    } = registry()
        .await
        .run_command("gh", &argv, true)
        .await
        .expect("rewrite")
    else {
        panic!("piped merge must propose")
    };
    assert_eq!(capability.as_str(), "gh.pull-request.merge");
    assert_eq!(input["_stdinBody"], true);
    let steps = vec![
        exchange("/repos/octo/hello/pulls/7", "GET", Value::Null, pull),
        exchange(
            "/repos/octo/hello/pulls/7/merge",
            "PUT",
            json!({"sha":"a".repeat(40),"merge_method":"squash","commit_message":"Piped body","commit_title":"Subject"}),
            json!({"merged":true,"sha":"d".repeat(40),"message":"merged"}),
        ),
    ];
    let (status, stderr, _) = invoke_with_stdin(
        capability.as_str(),
        input,
        steps,
        Some(b"Piped body".to_vec()),
    )
    .await;
    assert_eq!(status, 0, "{stderr}");
}

#[test]
fn escape_sequence_flags_preserve_content_and_diff_output_bytes() {
    for (id,input,response,path) in [
        ("gh.pull-request.diff",json!({"owner":"octo","repo":"hello","number":7}),Response {status:200,headers:vec![],body:b"diff --git a/x b/x\n+\x1b[31mred\x1b[0m\n".to_vec()},"/repos/octo/hello/pulls/7"),
        ("gh.content.read",json!({"owner":"octo","repo":"hello","path":"x","_cliOutput":"","_cliKind":"read-file"}),Response {status:200,headers:vec![],body:serde_json::to_vec(&json!({"type":"file","path":"x","sha":"a".repeat(40),"size":5,"content":base64::engine::general_purpose::STANDARD.encode(b"\x1b[31m"),"encoding":"base64"})).expect("response")},"/repos/octo/hello/contents/x"),
    ] {
        let mut outputs=Vec::new();
        for allow in [false,true] {
            let mut input=input.clone();if allow { input["allowEscapeSequences"]=json!(true); }
            let harness=Harness::<Gh>::get(component()).http(HttpScript::new("localhost","GET",response.clone()));
            let base=format!("{}/prefix",harness.origin().expect("origin"));
            let output=harness.settings(json!({"baseUrl":base})).call(id,input).expect("component call");
            assert_eq!(output.status,0,"{}",output.stderr);assert_eq!(output.http_calls.len(),1);
            let request=output.http_request.expect("request");assert_eq!(request.method,"GET");assert_eq!(request.uri,format!("{base}{path}"));assert!(!request.headers.iter().any(|header|header.name.eq_ignore_ascii_case("authorization")));
            outputs.push(output.stdout);
        }
        assert_eq!(outputs[0],outputs[1]);
        if id=="gh.content.read" {assert!(outputs[0].contains(&27));} else {let value:Value=serde_json::from_slice(&outputs[0]).expect("JSON");assert!(value["diff"].as_str().expect("diff").contains('\x1b'));}
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn new_comment_arrays_are_closed_and_the_serialized_byte_limit_counts_escapes() {
    for (id, input) in [
        (
            "gh.pull-request.approve",
            json!({"owner":"octo","repo":"hello","number":7,"comments":null}),
        ),
        (
            "gh.pull-request.request-changes",
            json!({"owner":"octo","repo":"hello","number":7,"body":"review","comments":[]}),
        ),
        (
            "gh.pull-request.comment",
            json!({"owner":"octo","repo":"hello","number":7,"comments":[]}),
        ),
        (
            "gh.pull-request.comment",
            json!({"owner":"octo","repo":"hello","number":7,"comments":(0..51).map(|_|json!({"path":"x","line":1,"body":"review"})).collect::<Vec<_>>()}),
        ),
        (
            "gh.issue.comment-delete",
            json!({"owner":"octo","repo":"hello","number":7,"body":null}),
        ),
    ] {
        let (status, stderr, stdout) = invoke(id, input, vec![]).await;
        assert_eq!(status, 2);
        assert_eq!(stderr, "input does not match the capability contract\n");
        assert!(stdout.is_empty());
    }
    let input = json!({"owner":"octo","repo":"hello","number":7,"comments":(0..3).map(|_|json!({"path":"x","line":1,"body":"\u{0001}".repeat(1000)})).collect::<Vec<_>>()});
    let (status, stderr, stdout) = invoke("gh.pull-request.comment", input, vec![]).await;
    assert_eq!(status, 2);
    assert_eq!(stderr, "the review request would exceed 16384 bytes\n");
    assert!(stdout.is_empty());
}

#[test]
fn diff_globs_match_go_path_rules_and_name_only_uses_the_one_diff_get() {
    let text = "diff --git a/src/a.rs b/src/a.rs\n+one\ndiff --git a/src/nested/a.rs b/src/nested/a.rs\n+two\ndiff --git a/a.rs b/a.rs\n+three\ndiff --git a/b.rs b/b.rs\n+four\ndiff --git a/!.rs b/!.rs\n+five\ndiff --git a/*.rs b/*.rs\n+six\n";
    for (pattern, expected) in [
        ("src/**", "src/nested/a.rs\na.rs\nb.rs\n!.rs\n*.rs\n"),
        ("[^a]*.rs", "src/a.rs\nsrc/nested/a.rs\na.rs\n"),
        ("[!a].rs", "b.rs\n*.rs\n"),
        (r"\*.rs", "src/a.rs\nsrc/nested/a.rs\na.rs\nb.rs\n!.rs\n"),
    ] {
        let harness = Harness::<Gh>::get(component()).http(HttpScript::new(
            "localhost",
            "GET",
            Response {
                status: 200,
                headers: vec![],
                body: text.as_bytes().to_vec(),
            },
        ));
        let base = format!("{}/prefix", harness.origin().expect("origin"));
        let output=harness.settings(json!({"baseUrl":base})).call("gh.pull-request.diff",json!({"owner":"octo","repo":"hello","number":7,"nameOnly":true,"exclude":[pattern]})).expect("diff");
        assert_eq!(output.status, 0, "{}", output.stderr);
        assert_eq!(output.http_calls.len(), 1);
        let request = output.http_request.expect("request");
        assert_eq!(request.method, "GET");
        assert_eq!(request.uri, format!("{base}/repos/octo/hello/pulls/7"));
        assert!(
            !request
                .headers
                .iter()
                .any(|header| header.name.eq_ignore_ascii_case("authorization"))
        );
        let body: Value = serde_json::from_slice(&output.stdout).expect("JSON diff");
        assert_eq!(body["diff"], expected, "{pattern}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn graphql_errors_remain_typed_on_production_comment_capabilities() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cassettes");
    let cases: Value =
        serde_json::from_str(include_str!("cassettes/have-flags.json")).expect("cases");
    let case = &cases.as_array().expect("array")[0];
    let mut steps = case["files"]
        .as_array()
        .expect("files")
        .iter()
        .map(|name| {
            serde_json::from_slice::<Value>(
                &std::fs::read(root.join("gh").join(name.as_str().expect("name")))
                    .expect("recording"),
            )
            .expect("exchange")
        })
        .collect::<Vec<_>>();
    steps[1]["response"]["body"]["json"] =
        json!({"data":{"addComment":null},"errors":[{"message":"partial failure"}]});
    let (status, stderr, stdout) = invoke(
        case["capability"].as_str().expect("capability"),
        case["input"].clone(),
        steps,
    )
    .await;
    assert_eq!(status, 3);
    assert_eq!(stderr, "GitHub GraphQL reported an error\n");
    assert!(stdout.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn write_and_view_flag_proposals_keep_existing_ids_and_separate_comment_effects() {
    for (argv, id, fields) in [
        (
            vec!["pr", "view", "7", "-R", "o/r", "--comments"],
            "gh.pull-request.read",
            json!({"comments":true}),
        ),
        (
            vec!["issue", "view", "7", "-R", "o/r", "--comments"],
            "gh.issue.read",
            json!({"comments":true}),
        ),
        (
            vec![
                "pr",
                "diff",
                "7",
                "-R",
                "o/r",
                "--patch",
                "--exclude",
                "*.md",
                "--allow-escape-sequences",
            ],
            "gh.pull-request.diff",
            json!({"patch":true,"exclude":["*.md"],"allowEscapeSequences":true}),
        ),
        (
            vec!["pr", "diff", "7", "-R", "o/r", "--name-only"],
            "gh.pull-request.diff",
            json!({"nameOnly":true}),
        ),
        (
            vec![
                "pr",
                "merge",
                "7",
                "-R",
                "o/r",
                "--squash",
                "--body",
                "Message",
                "--subject",
                "Title",
            ],
            "gh.pull-request.merge",
            json!({"mergeMethod":"squash","body":"Message","subject":"Title"}),
        ),
        (
            vec![
                "issue",
                "comment",
                "7",
                "-R",
                "o/r",
                "--edit-last",
                "--body",
                "Edit",
            ],
            "gh.issue.comment-edit",
            json!({"body":"Edit"}),
        ),
        (
            vec![
                "issue",
                "comment",
                "7",
                "-R",
                "o/r",
                "--delete-last",
                "--yes",
            ],
            "gh.issue.comment-delete",
            json!({}),
        ),
        (
            vec![
                "issue",
                "comment",
                "7",
                "-R",
                "o/r",
                "--edit-last",
                "--create-if-none",
                "--body",
                "Create",
            ],
            "gh.issue.comment-edit-or-create",
            json!({"body":"Create"}),
        ),
    ] {
        let argv = argv.into_iter().map(str::to_owned).collect::<Vec<_>>();
        let CommandRunOutcome::Proposed {
            capability, input, ..
        } = registry()
            .await
            .run_command("gh", &argv, false)
            .await
            .expect("rewrite")
        else {
            panic!("must propose {argv:?}")
        };
        assert_eq!(capability.as_str(), id);
        let mut expected = json!({"owner":"o","repo":"r","number":7});
        expected
            .as_object_mut()
            .expect("object")
            .extend(fields.as_object().expect("fields").clone());
        if id == "gh.pull-request.read" || id == "gh.issue.read" {
            expected["_cliOutput"] = json!("");
        }
        assert_eq!(input, expected);
    }
    for (event, id) in [
        ("--approve", "gh.pull-request.approve"),
        ("--comment", "gh.pull-request.comment"),
        ("--request-changes", "gh.pull-request.request-changes"),
    ] {
        let sha = "b".repeat(40);
        let argv = [
            "pr",
            "review",
            "7",
            "-R",
            "o/r",
            event,
            "--body",
            "Review",
            "--match-head-commit",
            &sha,
        ]
        .map(str::to_owned);
        let CommandRunOutcome::Proposed {
            capability, input, ..
        } = registry()
            .await
            .run_command("gh", &argv, false)
            .await
            .expect("rewrite")
        else {
            panic!("review must propose")
        };
        assert_eq!(capability.as_str(), id);
        assert_eq!(
            input,
            json!({"owner":"o","repo":"r","number":7,"body":"Review","expectedHeadSha":sha})
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn legacy_review_bodies_keep_their_base_validation_without_line_comments() {
    let body = "\u{0001}".repeat(4096);
    let pull = json!({"number":7,"title":"Review","state":"open","draft":false,"merged":false,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-02T00:00:00Z","head":{"ref":"feature","sha":"a".repeat(40)},"base":{"ref":"main","sha":"c".repeat(40)}});
    let request = json!({"event":"COMMENT","commit_id":"a".repeat(40),"body":body});
    assert!(
        serde_json::to_vec(&request)
            .expect("serialized request")
            .len()
            > 16384
    );
    let steps = vec![
        exchange("/repos/octo/hello/pulls/7", "GET", Value::Null, pull),
        exchange(
            "/repos/octo/hello/pulls/7/reviews",
            "POST",
            request,
            json!({"id":1,"state":"COMMENTED","commit_id":"a".repeat(40)}),
        ),
    ];
    let (status, stderr, _) = invoke(
        "gh.pull-request.comment",
        json!({"owner":"octo","repo":"hello","number":7,"body":body}),
        steps,
    )
    .await;
    assert_eq!(status, 0, "{stderr}");
    let argv = [
        "repo",
        "read-file",
        "x",
        "-R",
        "o/r",
        "--allow-escape-sequences",
    ]
    .map(str::to_owned);
    let CommandRunOutcome::Proposed {
        capability, input, ..
    } = registry()
        .await
        .run_command("gh", &argv, false)
        .await
        .expect("rewrite")
    else {
        panic!("file flag must propose")
    };
    assert_eq!(capability.as_str(), "gh.content.read");
    assert_eq!(
        input,
        json!({"owner":"o","repo":"r","path":"x","allowEscapeSequences":true,"_cliOutput":"","_cliKind":"read-file"})
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn query_budgets_count_supplied_terms_without_tightening_legacy_searches() {
    let raw = "x".repeat(512);
    let input = json!({"owner":"o","repo":"r","query":raw,"author":"octocat","state":"closed","_cliOutput":"","_cliLimit":30});
    let path = format!(
        "/search/issues?q={raw}%20repo%3Ao%2Fr%20is%3Apr%20state%3Aclosed%20author%3Aoctocat&page=1&per_page=100"
    );
    let (status, stderr, _) = invoke(
        "gh.search.prs",
        input,
        vec![exchange(
            &path,
            "GET",
            Value::Null,
            json!({"total_count":0,"incomplete_results":false,"items":[]}),
        )],
    )
    .await;
    assert_eq!(status, 0, "{stderr}");
    let (status,stderr,_) = invoke("gh.issue.list",json!({"owner":"o","repo":"r","label":["a,b"],"_cliOutput":"","_cliLimit":30}),vec![exchange("/search/issues?q=label%3A%22a%2Cb%22%20repo%3Ao%2Fr%20is%3Aissue%20is%3Aopen&page=1&per_page=100","GET",Value::Null,json!({"total_count":0,"incomplete_results":false,"items":[]}))]).await;
    assert_eq!(status, 0, "{stderr}");
    for (id, input) in [
        (
            "gh.search.prs",
            json!({"owner":"o","repo":"r","query":"x".repeat(490),"author":"octocat","state":"closed","label":["x"],"_cliOutput":"","_cliLimit":30}),
        ),
        (
            "gh.pull-request.list",
            json!({"owner":"o","repo":"r","search":"x".repeat(505),"state":"closed","_cliOutput":"","_cliLimit":30}),
        ),
        (
            "gh.issue.list",
            json!({"owner":"o","repo":"r","search":"x".repeat(505),"state":"closed","_cliOutput":"","_cliLimit":30}),
        ),
    ] {
        let (status, stderr, stdout) = invoke(id, input, vec![]).await;
        assert_eq!(status, 2);
        assert_eq!(stderr, "input does not match the capability contract\n");
        assert!(stdout.is_empty());
    }
}
