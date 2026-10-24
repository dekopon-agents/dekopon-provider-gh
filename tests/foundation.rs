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
            let status = std::process::Command::new("cargo")
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
        let mut variants = vec![];
        if entry.kind == unavailable::Kind::Command {
            variants.push(None);
        }
        for flag in entry.flags.split_whitespace() {
            let mut parts = flag.split('/');
            variants.push(Some(format!("--{}", parts.next().expect("long flag"))));
            if let Some(short) = parts.next() {
                variants.push(Some(format!("-{short}")));
            }
        }
        for flag in variants {
            let mut argv: Vec<_> = entry
                .command
                .split_whitespace()
                .map(str::to_owned)
                .collect();
            if let Some(flag) = flag {
                argv.push(flag);
            }
            let outcome = runtime.block_on(registry.run_command("gh", &argv, false))?;
            let CommandRunOutcome::Failed { error } = outcome else {
                panic!("{argv:?}: {outcome:?}")
            };
            let reason = unavailable::entries()
                .find(|row| row.command == entry.command && row.kind == unavailable::Kind::Command)
                .map_or(entry.reason, |row| row.reason);
            assert_eq!(error.code, "usage", "{argv:?}");
            assert_eq!(
                error.message,
                format!(
                    "gh {}: not available in this environment ({reason})",
                    entry.command
                ),
                "{argv:?}"
            );
            cases += 1;
        }
    }
    assert_eq!(
        unavailable::entries()
            .filter(|entry| entry.kind == unavailable::Kind::Command)
            .count(),
        51
    );
    assert!(
        cases > 400,
        "every long and short refusal is exercised: {cases}"
    );
    for argv in [
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
    for (words, reason) in [
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
            vec!["project", "view", "7", "--web"],
            "needs a browser or local application",
        ),
    ] {
        let command = match words[0] {
            "repo" => "repo deploy-key add",
            "pr" if words[1] == "view" => "pr view",
            "pr" => "pr checkout",
            "issue" => "issue create",
            "project" => "project view",
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
    let CommandRunOutcome::Rendered {
        stdout,
        stderr,
        status,
    } = runtime.block_on(registry.run_command("gh", &["--help".into()], false))?
    else {
        panic!("help expected")
    };
    assert_eq!(status, 0);
    assert!(stderr.is_empty());
    assert_eq!(
        stdout.matches("Not available in this environment:").count(),
        1
    );
    for entry in unavailable::entries() {
        assert!(
            stdout.contains(entry.command),
            "{} missing from help",
            entry.command
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
            "actions" => assert!(stdout.contains("GitHub Actions: workflow and run operations")),
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
        assert_ne!(output.status, 0);
        assert!(
            output
                .stderr
                .contains("head no longer matches expectedHeadSha"),
            "{id}: {}",
            output.stderr
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
            format!("gh search {verb}: not available in this environment (by policy)")
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
        "users/octo?x=1",
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
