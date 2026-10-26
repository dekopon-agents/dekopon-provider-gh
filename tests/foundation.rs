use dekopon_broker_host::{BrokerHostLimits, BrokerProviderRegistry};
use dekopon_gh_provider::Gh;
use dekopon_provider_sdk::provider::Response;
use dekopon_provider_sdk::{CommandRunOutcome, provider};
use dekopon_provider_sdk_testkit::{Harness, HttpScript};
use serde_json::{Value, json};

#[path = "fixtures/foundation_provider.rs"]
mod fixture;
#[expect(
    dead_code,
    reason = "This test consumes the registry data; the production parser owns registration."
)]
#[path = "../src/unavailable.rs"]
mod unavailable;

fn component() -> std::path::PathBuf {
    std::env::var_os("DEKOPON_PROVIDER_COMPONENT")
        .expect("DEKOPON_PROVIDER_COMPONENT must name the freshly built component")
        .into()
}

fn fixture_component() -> std::path::PathBuf {
    static COMPONENT: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    COMPONENT
        .get_or_init(|| {
            let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let target = root.join("target/foundation-fixture");
            let mut flags = std::env::var("CARGO_ENCODED_RUSTFLAGS").unwrap_or_else(|_| {
                std::env::var("RUSTFLAGS")
                    .unwrap_or_default()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join("\x1f")
            });
            if !flags.is_empty() {
                flags.push('\x1f');
            }
            flags.push_str("--cfg=gh_foundation_fixture");
            let status = std::process::Command::new("cargo")
                .env("CARGO_ENCODED_RUSTFLAGS", flags)
                .current_dir(&root)
                .args([
                    "build",
                    "--locked",
                    "--manifest-path",
                    "tests/fixtures/Cargo.toml",
                    "--target",
                    "wasm32-unknown-unknown",
                    "--release",
                    "--target-dir",
                ])
                .arg(&target)
                .status()
                .expect("build the isolated foundation fixture");
            assert!(status.success(), "fixture build failed");
            let core = target.join("wasm32-unknown-unknown/release/gh_foundation_fixture.wasm");
            let component = target.join("foundation-fixture.wasm");
            let status = std::process::Command::new("wasm-tools")
                .args(["component", "new"])
                .arg(core)
                .arg("-o")
                .arg(&component)
                .status()
                .expect("encode foundation fixture");
            assert!(status.success(), "fixture encoding failed");
            component
        })
        .clone()
}

fn valid_refusal_argv(command: &str) -> Vec<String> {
    let mut words: Vec<_> = command.split_whitespace().map(str::to_owned).collect();
    let args: &[&str] = match command {
        "run list" | "pr list" | "issue list" | "repo read-dir" | "pr edit" | "issue edit" => {
            &["-R", "octo/hello"]
        }
        "run view" | "pr view" | "pr checks" | "pr diff" | "issue view" | "issue comment" => {
            &["7", "-R", "octo/hello"]
        }
        "repo read-file" => &["README.md", "-R", "octo/hello"],
        "repo view" => &["octo/hello"],
        "repo list" => &["octo"],
        "search prs" | "search issues" | "search repos" | "search code" | "search commits" => {
            &["term", "--owner", "octo"]
        }
        "api" => &["user"],
        _ => &[],
    };
    words.extend(args.iter().map(|word| (*word).to_owned()));
    words
}

fn whole_command(kind: unavailable::Kind) -> bool {
    match kind {
        unavailable::Kind::Command => true,
        unavailable::Kind::Flag | unavailable::Kind::UnscopedSearch | unavailable::Kind::Syntax => {
            false
        }
    }
}

#[test]
fn every_inventory_refusal_is_known_to_the_real_component_without_http_imports()
-> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Runtime::new()?;
    let registry = runtime.block_on(BrokerProviderRegistry::load(
        [component()],
        BrokerHostLimits::default(),
    ))?;
    let mut cases = 0;
    for entry in unavailable::entries() {
        match entry.kind {
            unavailable::Kind::Syntax => continue,
            unavailable::Kind::Command
            | unavailable::Kind::Flag
            | unavailable::Kind::UnscopedSearch => {}
        }
        let mut variants = vec![];
        if whole_command(entry.kind) {
            variants.push(None);
        }
        for flag in entry.flags() {
            let value = match flag.arity {
                unavailable::Arity::Boolean => None,
                unavailable::Arity::Value => Some("value"),
            };
            variants.push(Some((format!("--{}", flag.name), flag.name, value)));
            if let Some(short) = flag.short {
                variants.push(Some((format!("-{short}"), flag.name, value)));
            }
        }
        for flag in variants {
            let mut argv = valid_refusal_argv(entry.command);
            let subject = match &flag {
                Some((spelling, name, value)) => {
                    argv.push(spelling.clone());
                    if let Some(value) = value {
                        argv.push((*value).to_owned());
                    }
                    match entry.kind {
                        unavailable::Kind::Command => String::new(),
                        unavailable::Kind::Flag => format!(" --{name}"),
                        unavailable::Kind::UnscopedSearch | unavailable::Kind::Syntax => {
                            unreachable!()
                        }
                    }
                }
                None => String::new(),
            };
            let outcome = runtime.block_on(registry.run_command("gh", &argv, false))?;
            let CommandRunOutcome::Failed { error } = outcome else {
                panic!("{argv:?}: {outcome:?}")
            };
            let reason = unavailable::entries()
                .find(|row| row.command == entry.command && whole_command(row.kind))
                .map_or(entry.reason, |row| row.reason);
            assert_eq!(error.code, "usage", "{argv:?}");
            assert_eq!(
                error.message,
                format!(
                    "gh {}{subject}: not available in this environment ({reason})",
                    entry.command
                ),
                "{argv:?}"
            );
            cases += 1;
        }
    }
    assert_eq!(
        unavailable::entries()
            .filter(|entry| whole_command(entry.kind))
            .count(),
        51
    );
    assert!(
        cases > 400,
        "every long and short refusal is exercised: {cases}"
    );
    for argv in [
        vec!["--", "pr", "view", "7", "-R", "octo/hello"],
        vec!["auth", "login", "--definitely-a-typo"],
        vec!["codespace", "ports", "bogus"],
        vec!["codespace", "ports", "-", "-c", "demo"],
        vec!["cs", "ports", "-c", "demo", "bogus"],
        vec![
            "pr",
            "create",
            "--definitely-a-typo",
            "--web",
            "-R",
            "octo/hello",
        ],
        vec!["pr", "create", "-wx", "-R", "octo/hello"],
        vec!["--", "pr", "view", "--web", "7", "-R", "octo/hello"],
        vec!["pr", "--", "create", "--web"],
        vec!["definitely-a-typo"],
        vec!["pr", "view", "--definitely-a-typo"],
    ] {
        let outcome = runtime.block_on(registry.run_command(
            "gh",
            &argv.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
            false,
        ))?;
        let CommandRunOutcome::Rendered {
            stdout,
            stderr,
            status,
        } = outcome
        else {
            panic!("clap usage expected")
        };
        assert_eq!(status, 2);
        assert!(stdout.is_empty());
        assert!(stderr.contains("error:"));
    }
    for (argv, subject, reason) in [
        (
            vec![
                "issue",
                "comment",
                "7",
                "-R",
                "octo/hello",
                "--attach",
                "before.png",
                "--attach",
                "after.png",
            ],
            "issue comment --attach",
            "needs local file access",
        ),
        (
            vec![
                "pr", "create", "--label", "bug", "--label", "feature", "--web",
            ],
            "pr create --web",
            "needs a browser or local application",
        ),
        (
            vec![
                "pr",
                "create",
                "--reviewer",
                "alice",
                "--reviewer",
                "bob",
                "--web",
            ],
            "pr create --web",
            "needs a browser or local application",
        ),
        (
            vec!["pr", "-R", "octo/hello", "view", "7", "--web"],
            "pr view --web",
            "needs a browser or local application",
        ),
        (
            vec!["pr", "-R", "octo/hello", "ls", "--web"],
            "pr list --web",
            "needs a browser or local application",
        ),
        (
            vec!["pr", "-R", "octo/hello", "checkout", "7"],
            "pr checkout",
            "needs a local git checkout",
        ),
        (
            vec!["pr", "-R", "octo/hello", "co", "7"],
            "pr checkout",
            "needs a local git checkout",
        ),
        (
            vec!["pr", "co", "7", "-R", "octo/hello"],
            "pr checkout",
            "needs a local git checkout",
        ),
        (
            vec!["run", "-R", "octo/hello", "watch", "7"],
            "run watch",
            "needs a long-lived invocation",
        ),
        (
            vec!["run", "watch", "7", "-R", "octo/hello"],
            "run watch",
            "needs a long-lived invocation",
        ),
        (
            vec![
                "codespace",
                "ports",
                "-c",
                "demo",
                "forward",
                "8080:8080",
                "--all-interfaces",
            ],
            "codespace ports forward",
            "needs a dev tunnel or websocket",
        ),
        (
            vec![
                "codespace",
                "ports",
                "forward",
                "-c",
                "demo",
                "8080:8080",
                "--all-interfaces",
            ],
            "codespace ports forward",
            "needs a dev tunnel or websocket",
        ),
        (
            vec![
                "cs",
                "ports",
                "--codespace=forward",
                "forward",
                "8080:8080",
                "--all-interfaces",
            ],
            "codespace ports forward",
            "needs a dev tunnel or websocket",
        ),
        (
            vec!["cs", "ports", "-cdemo", "visibility", "8080:private"],
            "codespace ports visibility",
            "needs a dev tunnel or websocket",
        ),
        (
            vec!["cs", "ssh", "-c", "demo"],
            "codespace ssh",
            "needs a dev tunnel or websocket",
        ),
        (
            vec!["extension", "ls"],
            "extension list",
            "needs local executable support",
        ),
        (
            vec!["ext", "ls"],
            "extension list",
            "needs local executable support",
        ),
        (
            vec!["extensions", "uninstall", "label"],
            "extension remove",
            "needs local executable support",
        ),
        (
            vec!["pr", "co", "7"],
            "pr checkout",
            "needs a local git checkout",
        ),
        (
            vec!["copilot", "-p", "x"],
            "copilot",
            "needs local executable support",
        ),
        (
            vec!["extension", "exec", "label", "--foo"],
            "extension exec",
            "needs local executable support",
        ),
        (
            vec![
                "issue",
                "comment",
                "7",
                "-R",
                "octo/hello",
                "--attach",
                "a.png",
            ],
            "issue comment --attach",
            "needs local file access",
        ),
    ] {
        let CommandRunOutcome::Failed { error } = runtime.block_on(registry.run_command(
            "gh",
            &argv.into_iter().map(str::to_owned).collect::<Vec<_>>(),
            false,
        ))?
        else {
            panic!("inventory refusal expected")
        };
        assert_eq!(error.code, "usage");
        assert_eq!(
            error.message,
            format!("gh {subject}: not available in this environment ({reason})")
        );
    }
    for value in [
        "true", "false", "1", "t", "T", "TRUE", "True", "0", "f", "F", "FALSE", "False",
    ] {
        for (words, flag, expected) in [
            (
                vec!["pr", "view", "7", "-R", "octo/hello"],
                "--web",
                "gh pr view --web: not available in this environment (needs a browser or local application)",
            ),
            (
                vec!["pr", "view", "7", "-R", "octo/hello"],
                "-w",
                "gh pr view --web: not available in this environment (needs a browser or local application)",
            ),
            (
                vec!["auth", "status"],
                "--show-token",
                "gh auth status --show-token: not available in this environment (by policy)",
            ),
        ] {
            let mut argv: Vec<_> = words.into_iter().map(str::to_owned).collect();
            argv.push(format!("{flag}={value}"));
            let CommandRunOutcome::Failed { error } =
                runtime.block_on(registry.run_command("gh", &argv, false))?
            else {
                panic!("explicit boolean must refuse: {argv:?}")
            };
            assert_eq!(error.code, "usage", "{argv:?}");
            assert_eq!(error.message, expected, "{argv:?}");
        }
    }
    let argv = ["pr", "view", "7", "-R", "octo/hello", "--jq", "--web"].map(str::to_owned);
    let CommandRunOutcome::Failed { error } =
        runtime.block_on(registry.run_command("gh", &argv, false))?
    else {
        panic!("value-taking flag must consume its value")
    };
    assert_eq!(
        error.message,
        "gh pr view --jq: not available in this environment (by policy)"
    );
    for (words, reason) in [
        (
            vec!["pr", "create", "-dw", "-R", "octo/hello"],
            "needs a browser or local application",
        ),
        (
            vec!["pr", "create", "--title", "--", "--web", "-R", "octo/hello"],
            "needs a browser or local application",
        ),
        (
            vec!["codespace", "ports", "forward", "8080:8080", "-c", "foo"],
            "needs a dev tunnel or websocket",
        ),
        (
            vec![
                "codespace",
                "ports",
                "forward",
                "8080:8080",
                "-R",
                "octo/hello",
            ],
            "needs a dev tunnel or websocket",
        ),
        (
            vec![
                "codespace",
                "ports",
                "visibility",
                "8080:private",
                "--repo-owner",
                "octo",
            ],
            "needs a dev tunnel or websocket",
        ),
        (
            vec!["pr", "view", "--web", "7", "-R", "octo/hello"],
            "needs a browser or local application",
        ),
        (
            vec!["pr", "checks", "--watch", "7", "-R", "octo/hello"],
            "needs a long-lived invocation",
        ),
        (
            vec!["pr", "create", "-R", "octo/hello", "--title", "x", "--web"],
            "needs a browser or local application",
        ),
        (
            vec![
                "attestation",
                "verify",
                "f",
                "-R",
                "octo/hello",
                "--bundle-from-oci",
            ],
            "needs an OCI registry token exchange",
        ),
        (
            vec!["pr", "checkout", "7", "--worktree", "--web"],
            "needs a local git checkout",
        ),
        (
            vec!["pr", "checkout", "7", "-R", "octo/hello"],
            "needs a local git checkout",
        ),
        (
            vec!["repo", "deploy-key", "add", "key.pub", "-R", "octo/hello"],
            "by policy",
        ),
        (
            vec!["run", "watch", "7", "-R", "octo/hello"],
            "needs a long-lived invocation",
        ),
        (
            vec!["run", "download", "7", "-R", "octo/hello"],
            "needs redirects to rotating storage hosts",
        ),
        (
            vec!["pr", "view", "7", "-R", "octo/hello", "-w"],
            "needs a browser or local application",
        ),
        (
            vec!["issue", "create", "-R", "octo/hello", "--editor"],
            "needs an interactive terminal",
        ),
        (
            vec!["release", "download", "-D/tmp/out"],
            "needs a local filesystem",
        ),
        (
            vec!["project", "view", "7", "--web"],
            "needs a browser or local application",
        ),
    ] {
        let command = match words[0] {
            "repo" => "repo deploy-key add",
            "pr" if words[1] == "view" => "pr view --web",
            "pr" if words[1] == "checks" => "pr checks --watch",
            "pr" if words[1] == "create" => "pr create --web",
            "attestation" => "attestation verify --bundle-from-oci",
            "codespace" if words[2] == "forward" => "codespace ports forward",
            "codespace" => "codespace ports visibility",
            "pr" => "pr checkout",
            "issue" => "issue create --editor",
            "project" => "project view --web",
            "release" => "release download --dir",
            "run" if words[1] == "watch" => "run watch",
            _ => "run download",
        };
        let CommandRunOutcome::Failed { error } = runtime.block_on(registry.run_command(
            "gh",
            &words.into_iter().map(str::to_owned).collect::<Vec<_>>(),
            false,
        ))?
        else {
            panic!("valid upstream refusal expected")
        };
        assert_eq!(
            error.message,
            format!("gh {command}: not available in this environment ({reason})")
        );
    }
    for name in ["version", "licenses", "actions", "accessibility"] {
        let CommandRunOutcome::Rendered {
            stdout,
            stderr,
            status,
        } = runtime.block_on(registry.run_command("gh", &[name.into()], false))?
        else {
            panic!("pure output expected for {name}")
        };
        assert_eq!(status, 0, "{name}: {stderr}");
        assert!(stderr.is_empty());
        match name {
            "version" => assert_eq!(
                stdout,
                concat!("dekopon-provider-gh ", env!("CARGO_PKG_VERSION"), "\n")
            ),
            "licenses" => {
                assert!(stdout.contains("Permission is hereby granted"));
                assert!(stdout.contains("Apache License"));
            }
            "actions" => assert!(stdout.contains("GitHub Actions: workflows define automation")),
            "accessibility" => assert!(stdout.contains("plain text and JSON over pipes")),
            _ => unreachable!(),
        }
    }
    Ok(())
}

#[test]
fn auth_empty_input_and_its_grant_are_unchanged_while_json_is_optional()
-> Result<(), Box<dyn std::error::Error>> {
    let manifest = provider::manifest::<Gh>()?;
    let auth = manifest
        .capabilities
        .iter()
        .find(|cap| cap.id.as_str() == "gh.auth.status")
        .expect("auth capability");
    assert_eq!(auth.input_schema["required"], json!([]));
    assert_eq!(
        auth.input_schema["properties"]
            .as_object()
            .expect("properties")
            .len(),
        1
    );
    assert!(auth.input_schema["properties"].get("_cliOutput").is_some());
    for (words, expected) in [
        (vec!["auth", "status"], json!({})),
        (vec!["auth", "status", "-a"], json!({})),
        (
            vec!["auth", "status", "--json", "remaining,limit"],
            json!({"_cliOutput":"remaining,limit"}),
        ),
    ] {
        let CommandRunOutcome::Proposed {
            capability, input, ..
        } = provider::command::<Gh>(
            &words.iter().map(|word| (*word).into()).collect::<Vec<_>>(),
            false,
        )
        else {
            panic!("auth proposal")
        };
        assert_eq!(capability.as_str(), "gh.auth.status");
        assert_eq!(input, expected);
        let inputs =
            std::iter::once(input).chain((words.len() == 2).then(|| json!({"_cliOutput":""})));
        for input in inputs {
            let harness = Harness::<Gh>::get(component()).http(HttpScript::new(
                "localhost",
                "GET",
                Response {
                    status: 200,
                    headers: vec![],
                    body: json!({"resources":{"core":{"limit":5000,"remaining":4999,"reset":42}}})
                        .to_string()
                        .into_bytes(),
                },
            ));
            let base = format!("{}/prefix", harness.origin().expect("origin"));
            let output = harness
                .settings(json!({"baseUrl":base}))
                .call(capability.as_str(), input)?;
            assert_eq!(output.status, 0, "{}", output.stderr);
            let expected = if expected == json!({}) {
                json!({"limit":5000,"remaining":4999,"reset":42})
            } else {
                json!({"remaining":4999,"limit":5000})
            };
            assert_eq!(serde_json::from_slice::<Value>(&output.stdout)?, expected);
            assert_eq!(output.http_calls.len(), 1);
            let request = output.http_request.expect("script observed request");
            assert_eq!(request.uri, format!("{base}/rate_limit"));
            assert_eq!(request.method, "GET");
            assert!(
                !request
                    .headers
                    .iter()
                    .any(|header| header.name.eq_ignore_ascii_case("authorization"))
            );
        }
    }
    Ok(())
}

#[test]
fn fixed_graphql_documents_project_data_and_refuse_errors_in_a_real_component()
-> Result<(), Box<dyn std::error::Error>> {
    for response in [
        json!({"data":{"repository":{"id":"R_fixed","secret":"discarded"}}}),
        json!({"data":{"repository":{"id":"R_fixed"}},"errors":[{"message":"sensitive upstream text"}]}),
        json!({"errors":[{"message":"sensitive upstream text"}]}),
        json!({"data":null}),
    ] {
        let harness =
            Harness::<fixture::Foundation>::get(fixture_component()).http(HttpScript::new(
                "localhost",
                "POST",
                Response {
                    status: 200,
                    headers: vec![],
                    body: response.to_string().into_bytes(),
                },
            ));
        let base = format!("{}/prefix", harness.origin().expect("origin"));
        let output = harness.settings(json!({"baseUrl":base})).call(
            "gh.foundation.read-repository-node",
            json!({"owner":"octo","name":"hello"}),
        )?;
        if response.get("errors").is_some() {
            assert_eq!(output.status, 3);
            assert_eq!(output.stderr, "GitHub GraphQL reported an error\n");
            assert!(output.stdout.is_empty());
        } else if response["data"].is_null() {
            assert_ne!(output.status, 0);
            assert!(output.stdout.is_empty());
        } else {
            assert_eq!(output.status, 0, "{}", output.stderr);
            assert_eq!(
                serde_json::from_slice::<Value>(&output.stdout)?,
                json!({"id":"R_fixed"})
            );
        }
        assert_eq!(output.http_calls.len(), 1);
        let request = output.http_request.expect("script request");
        assert_eq!(request.method, "POST");
        assert_eq!(request.uri, format!("{base}/graphql"));
        assert!(
            !request
                .headers
                .iter()
                .any(|header| header.name.eq_ignore_ascii_case("authorization"))
        );
        let body: Value = serde_json::from_slice(&request.body)?;
        assert_eq!(
            body,
            json!({"query":"query RepositoryNode($owner: String!, $name: String!) { repository(owner: $owner, name: $name) { id } }", "variables":{"owner":"octo","name":"hello"}})
        );
    }
    Ok(())
}

#[test]
fn moved_heads_refuse_all_existing_pr_writes_after_one_read()
-> Result<(), Box<dyn std::error::Error>> {
    for (id, fields) in [
        ("gh.pull-request.approve", json!({})),
        ("gh.pull-request.comment", json!({"body":"review"})),
        ("gh.pull-request.request-changes", json!({"body":"review"})),
        ("gh.pull-request.merge", json!({"mergeMethod":"merge"})),
    ] {
        let mut input =
            json!({"owner":"octo","repo":"hello","number":7,"expectedHeadSha":"b".repeat(40)});
        input
            .as_object_mut()
            .expect("object")
            .extend(fields.as_object().expect("fields").clone());
        let harness = Harness::<Gh>::get(component()).http(HttpScript::new("localhost", "GET", Response {status:200, headers:vec![], body:json!({"number":7,"title":"Moved","state":"open","draft":false,"merged":false,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-02T00:00:00Z","head":{"ref":"feature","sha":"a".repeat(40)},"base":{"ref":"main","sha":"c".repeat(40)}}).to_string().into_bytes()}));
        let base = format!("{}/prefix", harness.origin().expect("origin"));
        let output = harness.settings(json!({"baseUrl":base})).call(id, input)?;
        assert_eq!(output.status, 1);
        assert_eq!(
            output.stderr,
            "the pull request head no longer matches expectedHeadSha\n"
        );
        assert!(output.stdout.is_empty());
        assert_eq!(output.http_calls.len(), 1);
        let request = output.http_request.expect("script observed read");
        assert_eq!(request.method, "GET");
        assert_eq!(request.uri, format!("{base}/repos/octo/hello/pulls/7"));
        assert!(
            !request
                .headers
                .iter()
                .any(|header| header.name.eq_ignore_ascii_case("authorization"))
        );
    }
    Ok(())
}

#[test]
fn upstream_shorthands_and_four_finite_routes_keep_their_typed_proposals_in_the_component()
-> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Runtime::new()?;
    let registry = runtime.block_on(BrokerProviderRegistry::load(
        [component()],
        BrokerHostLimits::default(),
    ))?;
    for (short, long) in [
        (
            vec!["pr", "-R", "octo/hello", "ls"],
            vec!["pr", "list", "-R", "octo/hello"],
        ),
        (
            vec!["pr", "--repo=octo/hello", "view", "7"],
            vec!["pr", "view", "7", "-R", "octo/hello"],
        ),
        (
            vec!["issue", "comment", "7", "-R", "octo/hello", "-F", "-"],
            vec![
                "issue",
                "comment",
                "7",
                "--repo",
                "octo/hello",
                "--body-file",
                "-",
            ],
        ),
        (
            vec!["pr", "review", "7", "-R", "octo/hello", "-a"],
            vec!["pr", "review", "7", "--repo", "octo/hello", "--approve"],
        ),
        (
            vec![
                "pr",
                "review",
                "7",
                "-R",
                "octo/hello",
                "-c",
                "-b",
                "review",
            ],
            vec![
                "pr",
                "review",
                "7",
                "--repo",
                "octo/hello",
                "--comment",
                "--body",
                "review",
            ],
        ),
        (
            vec!["pr", "review", "7", "-R", "octo/hello", "-r", "-F", "-"],
            vec![
                "pr",
                "review",
                "7",
                "--repo",
                "octo/hello",
                "--request-changes",
                "--body-file",
                "-",
            ],
        ),
        (
            vec!["pr", "merge", "7", "-R", "octo/hello", "-m"],
            vec!["pr", "merge", "7", "--repo", "octo/hello", "--merge"],
        ),
        (
            vec!["pr", "merge", "7", "-R", "octo/hello", "-r"],
            vec!["pr", "merge", "7", "--repo", "octo/hello", "--rebase"],
        ),
        (
            vec!["pr", "merge", "7", "-R", "octo/hello", "-s"],
            vec!["pr", "merge", "7", "--repo", "octo/hello", "--squash"],
        ),
        (
            vec![
                "pr",
                "list",
                "-R",
                "octo/hello",
                "-s",
                "closed",
                "-A",
                "octo",
                "-L",
                "5",
            ],
            vec![
                "pr",
                "list",
                "--repo",
                "octo/hello",
                "--state",
                "closed",
                "--author",
                "octo",
                "--limit",
                "5",
            ],
        ),
        (
            vec!["issue", "list", "-R", "octo/hello", "-s", "closed"],
            vec!["issue", "list", "--repo", "octo/hello", "--state", "closed"],
        ),
    ] {
        let run = |words: Vec<&str>| {
            runtime.block_on(registry.run_command(
                "gh",
                &words.into_iter().map(str::to_owned).collect::<Vec<_>>(),
                true,
            ))
        };
        let CommandRunOutcome::Proposed {
            capability: short_id,
            input: short_input,
            ..
        } = run(short.clone())?
        else {
            panic!("shorthand must propose: {short:?}")
        };
        let CommandRunOutcome::Proposed {
            capability: long_id,
            input: long_input,
            ..
        } = run(long)?
        else {
            panic!("long form must propose")
        };
        assert_eq!(short_id, long_id);
        assert_eq!(short_input, long_input);
    }
    for verb in ["prs", "issues", "repos", "code", "commits"] {
        let CommandRunOutcome::Failed { error } = runtime.block_on(registry.run_command(
            "gh",
            &["search".into(), verb.into(), "term".into()],
            false,
        ))?
        else {
            panic!("unscoped search must refuse")
        };
        assert_eq!(
            error.message,
            format!(
                "gh search {verb} without --owner or -R: not available in this environment (by policy)"
            )
        );
    }
    for words in [
        vec!["auth", "status", "--json"],
        vec!["auth", "status", "--json", "unknown"],
        vec!["auth", "status", "--json", "limit,limit"],
    ] {
        let CommandRunOutcome::Failed { error } = runtime.block_on(registry.run_command(
            "gh",
            &words.into_iter().map(str::to_owned).collect::<Vec<_>>(),
            false,
        ))?
        else {
            panic!("invalid auth JSON fields must refuse")
        };
        assert_eq!(error.code, "usage");
    }
    let CommandRunOutcome::Failed { error } = runtime.block_on(registry.run_command(
        "gh",
        &["api".into(), "users/bad..login".into()],
        false,
    ))?
    else {
        panic!("invalid login must refuse")
    };
    assert_eq!(error.message, "gh api: invalid login");
    for words in [
        vec!["pr", "create", "--title", "--web", "-R", "octo/hello"],
        vec!["pr", "create", "-t", "--web", "-R", "octo/hello"],
        vec!["pr", "create", "--title=--web", "-R", "octo/hello"],
        vec!["pr", "create", "-tweb", "-R", "octo/hello"],
        vec!["release", "view", "--", "--web"],
        vec!["pr", "create"],
        vec!["pr", "close", "7", "-R", "octo/hello"],
        vec!["label", "create", "--color", "ff0000"],
        vec!["label", "edit", "--color", "ff0000"],
    ] {
        let CommandRunOutcome::Rendered {
            stdout,
            stderr,
            status,
        } = runtime.block_on(
            registry.run_command(
                "gh",
                &words
                    .iter()
                    .map(|word| (*word).to_owned())
                    .collect::<Vec<_>>(),
                false,
            ),
        )?
        else {
            panic!("unbuilt command must retain clap error: {words:?}")
        };
        assert_eq!(status, 2);
        assert!(stdout.is_empty());
        assert!(
            stderr.starts_with("error: unrecognized subcommand"),
            "{stderr}"
        );
        assert!(!stderr.contains("not implemented"));
    }
    let CommandRunOutcome::Rendered { stdout, status, .. } = runtime
        .block_on(registry.run_command("gh", &["pr".into(), "view".into(), "-h".into()], false))?
    else {
        panic!("help must render")
    };
    assert_eq!(status, 0);
    assert!(
        stdout.contains("Usage: gh pr view [OPTIONS] --repo <OWNER/REPO> <NUMBER>"),
        "{stdout}"
    );
    for (route, id, expected) in [
        ("user", "gh.user-self.read", json!({})),
        ("users/octo", "gh.user.read", json!({"login":"octo"})),
        (
            "repos/octo/hello/branches/main",
            "gh.branch.read",
            json!({"owner":"octo","repo":"hello","branch":"main"}),
        ),
        (
            "repos/octo/hello/commits/main",
            "gh.commit.read",
            json!({"owner":"octo","repo":"hello","ref":"main"}),
        ),
    ] {
        let CommandRunOutcome::Proposed {
            capability, input, ..
        } = runtime.block_on(registry.run_command("gh", &["api".into(), route.into()], false))?
        else {
            panic!("finite route must propose")
        };
        assert_eq!(capability.as_str(), id);
        assert_eq!(input, expected);
    }
    for route in [
        "graphql",
        "repos/octo/hello/pulls",
        "user?x=1",
        "https://other.example/user",
    ] {
        let CommandRunOutcome::Failed { error } =
            runtime.block_on(registry.run_command("gh", &["api".into(), route.into()], false))?
        else {
            panic!("arbitrary API route must refuse")
        };
        assert_eq!(
            error.message,
            "gh api: not available in this environment (by policy)"
        );
    }
    Ok(())
}
