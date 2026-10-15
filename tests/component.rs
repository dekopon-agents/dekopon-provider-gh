use dekopon_gh_provider::Gh;
use dekopon_provider_sdk::provider::{Header, Http, ImportSet, Needs, Provider, Response};
use dekopon_provider_sdk::{CommandRunOutcome, EffectKind, RiskLevel, provider};
use dekopon_provider_sdk_testkit::{Harness, HarnessError, HttpScript, Native, conformance};
use serde_json::{Value, json};

fn component() -> std::path::PathBuf {
    std::env::var_os("DEKOPON_PROVIDER_COMPONENT")
        .expect("DEKOPON_PROVIDER_COMPONENT must name the freshly built component")
        .into()
}

#[test]
fn manifest_keeps_read_and_write_grants_independent() {
    let manifest = provider::manifest::<Gh>().expect("closed manifest");
    assert_eq!(manifest.capabilities.len(), 18);
    assert!(
        manifest
            .capabilities
            .iter()
            .any(|cap| cap.id.as_str() == "gh.repo.list")
    );
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
fn native_removed_ids_cannot_run_without_http() {
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
}

#[test]
fn retained_reads_succeed_only_with_their_own_http_grant() -> Result<(), Box<dyn std::error::Error>>
{
    let cases: &[(&[&str], &str, &str, Value, &str)] = &[
        (
            &["api", "users/octo"],
            "gh.user.read",
            "/users/octo",
            json!({"login":"octo","type":"User","public_repos":1,"followers":2,"created_at":"2026-01-01T00:00:00Z"}),
            "login",
        ),
        (
            &["api", "repos/octo/hello/branches/main"],
            "gh.branch.read",
            "/repos/octo/hello/branches/main",
            json!({"name":"main","commit":{"sha":"a".repeat(40)},"protected":false}),
            "commit",
        ),
        (
            &["api", "repos/octo/hello/commits/main"],
            "gh.commit.read",
            "/repos/octo/hello/commits/main",
            json!({"sha":"c".repeat(40),"commit":{"message":"ship","author":{"name":"Octo","date":"2026-01-01T00:00:00Z"}},"author":{"login":"octo"},"stats":{"additions":1,"deletions":0,"total":1},"files":[]}),
            "sha",
        ),
        (
            &["repo", "read-file", "README.md", "-R", "octo/hello"],
            "gh.content.read",
            "/repos/octo/hello/contents/README.md",
            json!({"type":"file","path":"README.md","sha":"abc","size":3,"encoding":"base64","content":"eWVz"}),
            "content",
        ),
    ];
    for (argv, id, path, body, field) in cases {
        let CommandRunOutcome::Proposed {
            capability, input, ..
        } = provider::command::<Gh>(
            &argv
                .iter()
                .map(|word| (*word).to_owned())
                .collect::<Vec<_>>(),
            false,
        )
        else {
            panic!("{argv:?} must propose")
        };
        assert_eq!(capability.as_str(), *id, "{argv:?}");
        match Harness::<Gh>::get(component()).call(id, input.clone()) {
            Err(HarnessError::Invocation(failure)) => {
                assert!(failure.http_calls.is_empty(), "{id} denial sent HTTP");
            }
            other => panic!("{id} without HTTP authority: {other:?}"),
        }
        let output = scripted_component_get(
            Response {
                status: 200,
                headers: vec![],
                body: body.to_string().into_bytes(),
            },
            id,
            input,
            path,
        )?;
        assert_eq!(output.status, 0, "{id}: {}", output.stderr);
        if *id == "gh.content.read" {
            assert_eq!(output.stdout, b"yes");
        } else {
            let value: Value = serde_json::from_slice(&output.stdout)?;
            assert!(!value[*field].is_null(), "{id} was not executed");
            assert_eq!(
                value, *body,
                "{id} must return the bounded upstream API shape"
            );
        }
    }
    Ok(())
}

#[test]
fn component_repo_list_requires_its_own_grant_and_renders_a_short_array()
-> Result<(), Box<dyn std::error::Error>> {
    let words = [
        "repo",
        "list",
        "octo",
        "-L",
        "100",
        "--json",
        "owner,nameWithOwner",
    ];
    let CommandRunOutcome::Proposed {
        capability, input, ..
    } = provider::command::<Gh>(&words.map(str::to_owned), false)
    else {
        panic!("repo list proposes")
    };
    assert_eq!(capability.as_str(), "gh.repo.list");
    assert_eq!(input["_cliLimit"], 100);
    match Harness::<Gh>::get(component()).call("gh.repo.list", input.clone()) {
        Err(HarnessError::Invocation(failure)) => assert!(failure.http_calls.is_empty()),
        other => panic!("repo list without its grant must be denied: {other:?}"),
    }
    let result = scripted_component_get(
        Response {
            status: 200,
            headers: vec![],
            body: json!([{"id":10,"name":"hello","full_name":"octo/hello","owner":{"login":"octo"},
            "private":false,"default_branch":"main","fork":false,
            "updated_at":"2026-01-01T00:00:00Z"}])
            .to_string()
            .into_bytes(),
        },
        "gh.repo.list",
        input,
        "/users/octo/repos?type=owner&page=1&per_page=100",
    )?;
    assert_eq!(result.status, 0, "{}", result.stderr);
    assert_eq!(
        serde_json::from_slice::<Value>(&result.stdout)?,
        json!([{"owner":{"login":"octo"},"nameWithOwner":"octo/hello"}])
    );
    Ok(())
}

#[test]
fn component_list_rejects_a_bad_cursor_without_partial_stdout()
-> Result<(), Box<dyn std::error::Error>> {
    let words = ["issue", "list", "-R", "octo/hello", "--json", "number"];
    let CommandRunOutcome::Proposed {
        capability, input, ..
    } = provider::command::<Gh>(&words.map(str::to_owned), false)
    else {
        panic!("issue list proposes")
    };
    let result = scripted_component_get(
        Response {
            status: 200,
            headers: vec![Header::text("link", "<not-a-valid-cursor>; rel=\"next\"")?],
            body: json!([{"id":1,"number":1,"title":"Fix","state":"open",
            "created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-02T00:00:00Z"}])
            .to_string()
            .into_bytes(),
        },
        capability.as_str(),
        input,
        "/repos/octo/hello/issues?state=open&page=1&per_page=100",
    )?;
    assert_ne!(result.status, 0);
    assert!(result.stdout.is_empty());
    Ok(())
}

#[test]
fn component_issue_list_exhausts_one_page_short_of_its_limit()
-> Result<(), Box<dyn std::error::Error>> {
    let words = [
        "issue",
        "list",
        "-R",
        "octo/hello",
        "-L",
        "100",
        "--json",
        "number",
    ];
    let CommandRunOutcome::Proposed {
        capability, input, ..
    } = provider::command::<Gh>(&words.map(str::to_owned), false)
    else {
        panic!("issue list proposes")
    };
    let issue = |number: u64, pull: bool| {
        let mut row = json!({"id":number,"number":number,"title":"Fix","state":"open",
            "created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-02T00:00:00Z"});
        if pull {
            row["pull_request"] = json!({"url":"https://api.github.com/pulls/2"});
        }
        row
    };
    let result = scripted_component_get(
        Response {
            status: 200,
            headers: vec![],
            body: json!([issue(1, false), issue(2, true)])
                .to_string()
                .into_bytes(),
        },
        capability.as_str(),
        input,
        "/repos/octo/hello/issues?state=open&page=1&per_page=100",
    )?;
    assert_eq!(result.status, 0, "{}", result.stderr);
    assert_eq!(
        serde_json::from_slice::<Value>(&result.stdout)?,
        json!([{"number":1}])
    );
    Ok(())
}

#[test]
fn component_repo_and_content_render_selected_json_and_default_text()
-> Result<(), Box<dyn std::error::Error>> {
    let cases: &[(&[&str], &str, &str, Value, &str)] = &[
        (
            &[
                "repo",
                "view",
                "octo/hello",
                "--json",
                "owner,description,defaultBranchRef,url",
            ],
            "gh.repo.read",
            "/repos/octo/hello",
            json!({"name":"hello","owner":{"login":"octo"},"private":false,"default_branch":"main","fork":false,"updated_at":"2026-01-01T00:00:00Z"}),
            "json",
        ),
        (
            &["repo", "view", "octo/hello"],
            "gh.repo.read",
            "/repos/octo/hello",
            json!({"name":"hello","owner":{"login":"octo"},"private":false,"default_branch":"main","fork":false,"updated_at":"2026-01-01T00:00:00Z"}),
            "text",
        ),
        (
            &[
                "repo",
                "read-file",
                "README.md",
                "-R",
                "octo/hello",
                "--json",
                "gitSHA,content,htmlUrl",
            ],
            "gh.content.read",
            "/repos/octo/hello/contents/README.md",
            json!({"type":"file","path":"README.md","sha":"abc","size":3,"encoding":"base64","content":"eWVz"}),
            "file-json",
        ),
        (
            &[
                "repo",
                "read-dir",
                "-R",
                "octo/hello",
                "--json",
                "name,gitSHA,path",
            ],
            "gh.content.read",
            "/repos/octo/hello/contents/",
            json!([{"name":"README.md","type":"file","sha":"abc","size":3}]),
            "dir-json",
        ),
        (
            &["repo", "read-dir", "-R", "octo/hello"],
            "gh.content.read",
            "/repos/octo/hello/contents/",
            json!([{"name":"README.md","type":"file","sha":"abc","size":3}]),
            "dir-text",
        ),
    ];
    for (words, id, path, body, shape) in cases {
        let CommandRunOutcome::Proposed {
            capability, input, ..
        } = provider::command::<Gh>(
            &words.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
            false,
        )
        else {
            panic!("must propose {words:?}")
        };
        assert_eq!(capability.as_str(), *id);
        let output = scripted_component_get(
            Response {
                status: 200,
                headers: vec![],
                body: body.to_string().into_bytes(),
            },
            id,
            input,
            path,
        )?;
        assert_eq!(output.status, 0, "{}", output.stderr);
        match *shape {
            "json" => assert_eq!(
                serde_json::from_slice::<Value>(&output.stdout)?,
                json!({"owner":{"login":"octo"},"description":null,"defaultBranchRef":{"name":"main"},"url":null})
            ),
            "text" => assert_eq!(output.stdout, b"octo/hello\t\n"),
            "file-json" => assert_eq!(
                serde_json::from_slice::<Value>(&output.stdout)?,
                json!({"gitSHA":"abc","content":"yes","htmlUrl":null})
            ),
            "dir-json" => assert_eq!(
                serde_json::from_slice::<Value>(&output.stdout)?,
                json!([{"name":"README.md","gitSHA":"abc","path":"README.md"}])
            ),
            "dir-text" => assert_eq!(output.stdout, b"README.md\tfile\t3\n"),
            _ => unreachable!(),
        }
    }
    Ok(())
}

#[test]
fn component_small_binary_read_file_keeps_decodable_base64()
-> Result<(), Box<dyn std::error::Error>> {
    let words = [
        "repo",
        "read-file",
        "logo.bin",
        "-R",
        "octo/hello",
        "--json",
        "encoding,content",
    ];
    let CommandRunOutcome::Proposed {
        capability, input, ..
    } = provider::command::<Gh>(&words.map(str::to_owned), false)
    else {
        panic!("read-file must propose")
    };
    assert_eq!(capability.as_str(), "gh.content.read");
    let output = scripted_component_get(
        Response {
            status: 200,
            headers: vec![],
            body: json!({
                "type":"file","path":"logo.bin","sha":"abc","size":3,
                "encoding":"base64","content":"/wD+"
            })
            .to_string()
            .into_bytes(),
        },
        capability.as_str(),
        input,
        "/repos/octo/hello/contents/logo.bin",
    )?;
    assert_eq!(output.status, 0, "{}", output.stderr);
    let value: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(value, json!({"encoding":"base64","content":"/wD+"}));
    Ok(())
}

#[test]
fn component_list_json_is_an_array_with_nested_logins_and_nulls()
-> Result<(), Box<dyn std::error::Error>> {
    let words = [
        "pr",
        "list",
        "-R",
        "octo/hello",
        "--json",
        "author,number,closedAt",
    ];
    let CommandRunOutcome::Proposed {
        capability, input, ..
    } = provider::command::<Gh>(&words.map(str::to_owned), false)
    else {
        panic!("list must propose")
    };
    assert_eq!(capability.as_str(), "gh.pull-request.list");
    let sha = "a".repeat(40);
    let body = json!([{"number":7,"title":"Ship","state":"open","user":{"login":"octo"},"head":{"ref":"feature","sha":sha},"base":{"ref":"main","sha":"b".repeat(40)},"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-02T00:00:00Z"}]).to_string().into_bytes();
    let output = scripted_component_get(
        Response {
            status: 200,
            headers: vec![],
            body: body.clone(),
        },
        capability.as_str(),
        input,
        "/repos/octo/hello/pulls?state=open&page=1&per_page=100",
    )?;
    assert_eq!(output.status, 0, "{}", output.stderr);
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout)?,
        json!([{"author":{"login":"octo"},"number":7,"closedAt":null}])
    );
    let words = ["pr", "list", "-R", "octo/hello"];
    let CommandRunOutcome::Proposed { input, .. } =
        provider::command::<Gh>(&words.map(str::to_owned), false)
    else {
        panic!("text list must propose")
    };
    let text = scripted_component_get(
        Response {
            status: 200,
            headers: vec![],
            body,
        },
        capability.as_str(),
        input,
        "/repos/octo/hello/pulls?state=open&page=1&per_page=100",
    )?;
    assert_eq!(text.status, 0, "{}", text.stderr);
    assert_eq!(text.stdout, b"7\tShip\tfeature\t2026-01-01T00:00:00Z\n");
    Ok(())
}

#[test]
fn component_pr_and_issue_fields_render_command_shapes() -> Result<(), Box<dyn std::error::Error>> {
    let sha = "a".repeat(40);
    let pr = json!({"number":7,"title":"Ship","state":"open","body":null,"draft":true,"user":{"login":"octo"},"assignees":[{"login":"dev"}],"labels":[{"name":"ready"}],"head":{"ref":"topic","sha":sha},"base":{"ref":"main","sha":"b".repeat(40)},"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-02T00:00:00Z","closed_at":null,"merged_at":null,"html_url":"https://github.com/octo/hello/pull/7"});
    let issue = json!({"number":9,"title":"Fix","state":"closed","body":"details","user":{"login":"octo"},"assignees":[{"login":"dev"}],"labels":[{"name":"bug"}],"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-02T00:00:00Z","closed_at":"2026-01-02T00:00:00Z","html_url":"https://github.com/octo/hello/issues/9"});
    let cases: &[(&[&str], &str, &str, Value, Value)] = &[
        (
            &[
                "pr",
                "list",
                "-R",
                "octo/hello",
                "--json",
                "assignees,author,baseRefOid,body,closedAt,headRefName,isDraft,labels,state,url",
            ],
            "gh.pull-request.list",
            "/repos/octo/hello/pulls?state=open&page=1&per_page=100",
            json!([pr]),
            json!([{"assignees":[{"login":"dev"}],"author":{"login":"octo"},"baseRefOid":"b".repeat(40),"body":null,"closedAt":null,"headRefName":"topic","isDraft":true,"labels":[{"name":"ready"}],"state":"OPEN","url":"https://github.com/octo/hello/pull/7"}]),
        ),
        (
            &[
                "issue",
                "view",
                "9",
                "-R",
                "octo/hello",
                "--json",
                "assignees,author,body,closedAt,labels,state,url",
            ],
            "gh.issue.read",
            "/repos/octo/hello/issues/9",
            issue.clone(),
            json!({"assignees":[{"login":"dev"}],"author":{"login":"octo"},"body":"details","closedAt":"2026-01-02T00:00:00Z","labels":[{"name":"bug"}],"state":"CLOSED","url":"https://github.com/octo/hello/issues/9"}),
        ),
        (
            &["pr", "view", "7", "-R", "octo/hello"],
            "gh.pull-request.read",
            "/repos/octo/hello/pulls/7",
            pr.clone(),
            json!("Ship\t\n"),
        ),
        (
            &["issue", "view", "9", "-R", "octo/hello"],
            "gh.issue.read",
            "/repos/octo/hello/issues/9",
            issue.clone(),
            json!("Fix\tdetails\n"),
        ),
        (
            &[
                "issue",
                "list",
                "-R",
                "octo/hello",
                "--json",
                "assignees,author,body,closedAt,createdAt,labels,number,state,title,updatedAt,url",
            ],
            "gh.issue.list",
            "/repos/octo/hello/issues?state=open&page=1&per_page=100",
            json!([issue.clone()]),
            json!([{"assignees":[{"login":"dev"}],"author":{"login":"octo"},"body":"details","closedAt":"2026-01-02T00:00:00Z","createdAt":"2026-01-01T00:00:00Z","labels":[{"name":"bug"}],"number":9,"state":"CLOSED","title":"Fix","updatedAt":"2026-01-02T00:00:00Z","url":"https://github.com/octo/hello/issues/9"}]),
        ),
        (
            &["issue", "list", "-R", "octo/hello"],
            "gh.issue.list",
            "/repos/octo/hello/issues?state=open&page=1&per_page=100",
            json!([issue]),
            json!("9\tFix\tbug\t2026-01-02T00:00:00Z\n"),
        ),
    ];
    for (words, id, path, response, expected) in cases {
        let CommandRunOutcome::Proposed {
            capability, input, ..
        } = provider::command::<Gh>(
            &words
                .iter()
                .map(|word| (*word).to_owned())
                .collect::<Vec<_>>(),
            false,
        )
        else {
            panic!("must propose {words:?}")
        };
        assert_eq!(capability.as_str(), *id);
        let output = scripted_component_get(
            Response {
                status: 200,
                headers: vec![],
                body: response.to_string().into_bytes(),
            },
            id,
            input,
            path,
        )?;
        assert_eq!(output.status, 0, "{words:?}: {}", output.stderr);
        if expected.is_string() {
            assert_eq!(output.stdout, expected.as_str().expect("text").as_bytes());
        } else {
            assert_eq!(
                serde_json::from_slice::<Value>(&output.stdout)?,
                *expected,
                "{words:?}"
            );
        }
    }
    Ok(())
}

#[test]
fn component_pr_view_full_fields_keep_denial_separate_from_success()
-> Result<(), Box<dyn std::error::Error>> {
    let words = [
        "pr",
        "view",
        "7",
        "-R",
        "octo/hello",
        "--json",
        "assignees,author,baseRefName,baseRefOid,body,closedAt,createdAt,headRefName,headRefOid,isDraft,labels,mergedAt,number,state,title,updatedAt,url",
    ];
    let CommandRunOutcome::Proposed {
        capability, input, ..
    } = provider::command::<Gh>(&words.map(str::to_owned), false)
    else {
        panic!("pr view must propose")
    };
    assert_eq!(capability.as_str(), "gh.pull-request.read");
    match Harness::<Gh>::get(component()).call(capability.as_str(), input.clone()) {
        Err(HarnessError::Invocation(failure)) => assert!(failure.http_calls.is_empty()),
        other => panic!("denial must send zero HTTP: {other:?}"),
    }
    let reply = json!({"number":7,"title":"Ship","state":"closed","merged":true,"draft":false,"body":null,"user":{"login":"octo"},"assignees":[{"login":"dev"}],"labels":[{"name":"ready"}],"head":{"ref":"topic","sha":"a".repeat(40)},"base":{"ref":"main","sha":"b".repeat(40)},"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-02T00:00:00Z","closed_at":"2026-01-02T00:00:00Z","merged_at":"2026-01-02T00:00:00Z","html_url":"https://github.com/octo/hello/pull/7"});
    let output = scripted_component_get(
        Response {
            status: 200,
            headers: vec![],
            body: reply.to_string().into_bytes(),
        },
        capability.as_str(),
        input,
        "/repos/octo/hello/pulls/7",
    )?;
    assert_eq!(output.status, 0, "{}", output.stderr);
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout)?,
        json!({"assignees":[{"login":"dev"}],"author":{"login":"octo"},"baseRefName":"main","baseRefOid":"b".repeat(40),"body":null,"closedAt":"2026-01-02T00:00:00Z","createdAt":"2026-01-01T00:00:00Z","headRefName":"topic","headRefOid":"a".repeat(40),"isDraft":false,"labels":[{"name":"ready"}],"mergedAt":"2026-01-02T00:00:00Z","number":7,"state":"MERGED","title":"Ship","updatedAt":"2026-01-02T00:00:00Z","url":"https://github.com/octo/hello/pull/7"})
    );
    Ok(())
}

#[test]
fn native_failed_view_expansion_cannot_emit_partial_json() {
    let words = ["pr", "view", "7", "-R", "octo/hello", "--json", "files"];
    let CommandRunOutcome::Proposed {
        capability, input, ..
    } = provider::command::<Gh>(&words.map(str::to_owned), false)
    else {
        panic!("files must propose")
    };
    let response = json!({"number":7,"title":"Ship","state":"open","head":{"ref":"topic","sha":"a".repeat(40)},"base":{"ref":"main","sha":"b".repeat(40)},"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-02T00:00:00Z"});
    let native = Native::<Gh>::new().http(HttpScript::new(
        "api.github.com",
        "GET",
        Response {
            status: 200,
            headers: vec![],
            body: response.to_string().into_bytes(),
        },
    ));
    let result = native.call(capability.as_str(), &input.to_string());
    assert_ne!(result.status, 0);
    assert!(result.stdout.is_empty());
    let requests = native.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].uri,
        "https://api.github.com/repos/octo/hello/pulls/7"
    );
    assert_eq!(
        requests[1].uri,
        "https://api.github.com/repos/octo/hello/pulls/7/files?per_page=100"
    );
    assert!(requests.iter().all(|request| {
        !request
            .headers
            .iter()
            .any(|header| header.name.eq_ignore_ascii_case("authorization"))
    }));
}

#[test]
fn component_content_kind_mismatch_never_renders_success() -> Result<(), Box<dyn std::error::Error>>
{
    let mut results = Vec::new();
    for (words, body) in [
        (
            &[
                "repo",
                "read-dir",
                "README.md",
                "-R",
                "octo/hello",
                "--json",
                "name",
            ][..],
            json!({"type":"file","path":"README.md","sha":"abc","size":3,"encoding":"base64","content":"eWVz"}),
        ),
        (
            &[
                "repo",
                "read-file",
                "README.md",
                "-R",
                "octo/hello",
                "--json",
                "content",
            ][..],
            json!([{"name":"entry","type":"file","sha":"abc","size":3}]),
        ),
    ] {
        let CommandRunOutcome::Proposed {
            capability, input, ..
        } = provider::command::<Gh>(
            &words
                .iter()
                .map(|word| (*word).to_owned())
                .collect::<Vec<_>>(),
            false,
        )
        else {
            panic!("must propose {words:?}")
        };
        assert_eq!(capability.as_str(), "gh.content.read");
        let output = scripted_component_get(
            Response {
                status: 200,
                headers: vec![],
                body: body.to_string().into_bytes(),
            },
            capability.as_str(),
            input,
            "/repos/octo/hello/contents/README.md",
        )?;
        results.push((words[1], output.status, output.stdout));
    }
    assert_eq!(
        results
            .iter()
            .map(|(_, status, stdout)| *status != 0 && stdout.is_empty())
            .collect::<Vec<_>>(),
        vec![true, true],
        "read-dir on file and read-file on directory must both fail: {results:?}"
    );
    Ok(())
}

#[test]
fn component_pr_and_issue_body_truncation_is_visible_in_json_and_tsv()
-> Result<(), Box<dyn std::error::Error>> {
    let body = "é".repeat(9000);
    let marker = "\n[dekopon: truncated to 16384 bytes of 18000]";
    let shortened = format!("{}{}", "é".repeat(8192), marker);
    let pr = json!({"number":7,"title":"Ship","state":"open","body":body,"head":{"ref":"topic","sha":"a".repeat(40)},"base":{"ref":"main","sha":"b".repeat(40)},"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-02T00:00:00Z"});
    let issue = json!({"number":9,"title":"Fix","state":"open","body":body,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-02T00:00:00Z"});
    for (area, id, path, reply, title, number) in [
        (
            "pr",
            "gh.pull-request.read",
            "/repos/octo/hello/pulls/7",
            pr,
            "Ship",
            "7",
        ),
        (
            "issue",
            "gh.issue.read",
            "/repos/octo/hello/issues/9",
            issue,
            "Fix",
            "9",
        ),
    ] {
        for fields in [Some("body"), None] {
            let mut words = vec![area, "view", number, "-R", "octo/hello"];
            if let Some(fields) = fields {
                words.extend(["--json", fields]);
            }
            let CommandRunOutcome::Proposed {
                capability, input, ..
            } = provider::command::<Gh>(
                &words
                    .iter()
                    .map(|word| (*word).to_owned())
                    .collect::<Vec<_>>(),
                false,
            )
            else {
                panic!("view must propose")
            };
            assert_eq!(capability.as_str(), id);
            let output = scripted_component_get(
                Response {
                    status: 200,
                    headers: vec![],
                    body: reply.to_string().into_bytes(),
                },
                id,
                input,
                path,
            )?;
            assert_eq!(output.status, 0, "{}", output.stderr);
            if fields.is_some() {
                let value: Value = serde_json::from_slice(&output.stdout)?;
                assert_eq!(value, json!({"body": shortened}));
            } else {
                assert_eq!(
                    output.stdout,
                    format!("{title}\t{shortened}\n").into_bytes()
                );
            }
        }
    }
    Ok(())
}

#[test]
fn component_invalid_typed_selection_refuses_before_settings_grant()
-> Result<(), Box<dyn std::error::Error>> {
    for extra in [
        json!({"_cliOutput":"unknown"}),
        json!({"_cliOutput":"name,,url"}),
        json!({"_cliOutput":5}),
        json!({"_cli":true}),
    ] {
        let mut input = json!({"owner":"octo","repo":"hello"});
        input
            .as_object_mut()
            .expect("input object")
            .extend(extra.as_object().expect("extra object").clone());
        let harness = Harness::<Gh>::get(component()).http(HttpScript::new(
            "localhost",
            "GET",
            Response {
                status: 200,
                headers: vec![],
                body: br#"{"name":"hello"}"#.to_vec(),
            },
        ));
        let output = harness
            .settings(json!({"baseUrl":5}))
            .call("gh.repo.read", input)?;
        assert_eq!(output.status, 2, "{extra}");
        assert_eq!(
            output.stderr,
            "the input does not match the capability's input schema\n"
        );
        assert!(output.stdout.is_empty());
        assert!(output.http_calls.is_empty());
        assert!(output.http_request.is_none());
    }
    Ok(())
}

#[test]
fn native_invalid_cli_selection_never_sends_http() {
    let native = Native::<Gh>::new().http(HttpScript::new(
        "api.github.com",
        "GET",
        Response {
            status: 200,
            headers: vec![],
            body: br#"{"name":"hello"}"#.to_vec(),
        },
    ));
    let result = native.call(
        "gh.repo.read",
        &json!({"owner":"octo","repo":"hello","_cliOutput":"unknown"}).to_string(),
    );
    assert_eq!(result.status, 2);
    assert!(native.requests().is_empty());
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
