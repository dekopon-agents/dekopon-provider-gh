use dekopon_broker_protocol::{
    Attestation, BrokerResponse, DescriptorStream, FrameLimits, InvocationRequest, RequestEnvelope,
    ResponseEnvelope, StreamDescriptors,
};
use dekopon_capability::InvocationOutcome;
use serde_json::{Value, json};
use std::{
    fs,
    io::Read,
    os::{
        fd::AsFd,
        unix::fs::{DirBuilderExt, PermissionsExt},
    },
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::Duration,
};
#[path = "../support/github.rs"]
mod github;
#[path = "../support/content_write_outcomes.rs"]
mod outcomes;

struct Runtime {
    directory: PathBuf,
    child: Option<Child>,
}
impl Drop for Runtime {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.directory);
    }
}

async fn invoke(
    socket: &std::path::Path,
    value: Value,
) -> (dekopon_capability::InvocationResult, Vec<u8>) {
    let mut stream = DescriptorStream::new(
        tokio::net::UnixStream::connect(socket)
            .await
            .expect("broker connection"),
    );
    let (writer, mut reader) = std::os::unix::net::UnixStream::pair().expect("stdout");
    let capture = tokio::task::spawn_blocking(move || {
        let mut bytes = vec![];
        reader.read_to_end(&mut bytes).expect("stdout");
        bytes
    });
    let request = RequestEnvelope::invoke(
        Some(
            Attestation::for_subject(
                "slack.t0123abc.ucontentwriter".parse().expect("subject"),
                "content-writer".parse().expect("agent"),
            )
            .bound_to("content-smoke".parse().expect("invocation")),
        ),
        InvocationRequest {
            id: "content-smoke".parse().expect("id"),
            capability: "gh.content.write".parse().expect("capability"),
            trace_parent: "00-0000000000000000000000000000f1c7-00000000000000f1-00"
                .parse()
                .expect("trace"),
            input: value,
            secret_use: None,
        },
        vec![],
        0,
        Some(StreamDescriptors { stdin: false }),
    );
    stream
        .write_frame(&request, &[writer.as_fd()], FrameLimits::default())
        .await
        .expect("send invocation");
    drop(writer);
    let (response, descriptors): (ResponseEnvelope, _) = stream
        .read_frame(FrameLimits::default())
        .await
        .expect("broker result");
    assert!(descriptors.is_empty());
    let BrokerResponse::Invocation { result, .. } = response.response else {
        panic!("unexpected response: {response:?}")
    };
    (result, capture.await.expect("stdout joined"))
}

#[tokio::main]
async fn main() {
    let brokerd = std::env::args_os()
        .nth(1)
        .expect("usage: content-write-smoke PATH_TO_BROKERD_0_64_0");
    let version = Command::new(&brokerd)
        .arg("--version")
        .output()
        .expect("broker version");
    assert!(version.status.success());
    assert_eq!(
        String::from_utf8(version.stdout).expect("version").trim(),
        "dekopon-brokerd 0.64.0"
    );
    let component = fs::canonicalize(
        std::env::var_os("DEKOPON_PROVIDER_COMPONENT")
            .expect("DEKOPON_PROVIDER_COMPONENT must name the freshly built component"),
    )
    .expect("component");
    let mut steps = vec![];
    let failure_cases = || std::iter::once(&outcomes::CASES[0]).chain(outcomes::CASES[4..].iter());
    for (status, sha) in [(201, None), (200, Some("b".repeat(40)))]
        .into_iter()
        .chain(failure_cases().map(|case| (case.http_status, None)))
    {
        let mut body = json!({"branch":"runs/content-123","message":"smoke","content":""});
        if let Some(sha) = sha {
            body["sha"] = json!(sha);
        }
        steps.push(json!({"request":{"method":"PUT","path":"/repos/octo/config/contents/config/mutable/agent.yaml","headers":{"accept":"application/vnd.github+json"},"brokerAuthorization":"Bearer synthetic-gh-content-write","body":{"json":body}},"response":{"status":status,"headers":{"content-type":"application/json"},"body":{"json":{"content":{"path":"config/mutable/agent.yaml","type":"file","sha":"b".repeat(40)},"commit":{"sha":"c".repeat(40)}}}}}));
    }
    for (status, mode) in [(201, "create"), (200, "update")] {
        for reply in outcomes::unreadable_replies() {
            let mut body = json!({"branch":"runs/content-123","message":"smoke","content":""});
            if mode == "update" {
                body["sha"] = json!("b".repeat(40));
            }
            steps.push(json!({"request":{"method":"PUT","path":"/repos/octo/config/contents/config/mutable/agent.yaml","headers":{"accept":"application/vnd.github+json"},"brokerAuthorization":"Bearer synthetic-gh-content-write","body":{"json":body}},"response":{"status":status,"headers":{"content-type":"application/json"},"body":{"text":reply}}}));
        }
    }
    let server = github::GitHub::start(steps).await;
    let directory =
        std::env::temp_dir().join(format!("gh-content-write-smoke-{}", std::process::id()));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .expect("private runtime directory");
    let mut runtime = Runtime {
        directory: directory.clone(),
        child: None,
    };
    let socket = directory.join("broker.sock");
    let authority = server
        .base
        .strip_prefix("http://")
        .expect("HTTP")
        .strip_suffix("/gh")
        .expect("prefix");
    let policy = include_str!("../../examples/content-write/policies.cedar")
        .replace("api.github.com", "127.0.0.1")
        .replace("/repos/octo/", "/gh/repos/octo/");
    fs::write(directory.join("policies.cedar"), policy).expect("policy");
    let credential = json!({"apiVersion":"dekopon.dev/broker-credentials/v1alpha1","credentials":[{"name":"github-app","kind":"bearerToken","scheme":"Bearer","destinations":[authority],"secret":"synthetic-gh-content-write"}]});
    fs::write(directory.join("credentials.json"), credential.to_string())
        .expect("synthetic credential");
    fs::set_permissions(
        directory.join("credentials.json"),
        fs::Permissions::from_mode(0o600),
    )
    .expect("credential mode");
    let mut capabilities = serde_json::Map::new();
    for (id, method) in [("gh.content.write", "PUT"), ("gh.content.read", "GET")] {
        capabilities.insert(id.into(),json!({"credential":"github-app","constraints":{"timeoutMs":10000,"http":{"allowedHosts":[authority],"allowedMethods":[method],"maxRequests":1,"maxRequestBytes":16384,"maxResponseBytes":16384,"allowPlaintextLoopback":true}}}));
    }
    let uid = Command::new("id").arg("-u").output().expect("UID");
    let uid: u32 = String::from_utf8(uid.stdout)
        .expect("UID")
        .trim()
        .parse()
        .expect("UID");
    let config = json!({"apiVersion":"dekopon.dev/brokerd/v1alpha1","socketPath":socket,"credentialsPath":"credentials.json","policiesPath":"policies.cedar","strict":true,"providers":[component],"identities":[{"uid":uid,"principal":"content-write-gateway","actor":{"type":"service","principal":"content-write-gateway"},"attestor":{"namespaces":["slack.t0123abc"]}}],"principals":{"content-writer":{"subjects":["slack.t0123abc.ucontentwriter"]}},"providerSettings":{"gh":{"baseUrl":server.base}},"capabilities":{"gh":{"capabilities":capabilities}}});
    let config_path = directory.join("broker.json");
    fs::write(&config_path, config.to_string()).expect("config");
    let log = fs::File::create(directory.join("broker.log")).expect("log");
    runtime.child = Some(
        Command::new(&brokerd)
            .args(["--config"])
            .arg(&config_path)
            .env_remove("GH_CASSETTE_TOKEN")
            .stdout(Stdio::from(log.try_clone().expect("log")))
            .stderr(Stdio::from(log))
            .spawn()
            .expect("brokerd"),
    );
    tokio::time::timeout(Duration::from_secs(300), async {
        loop {
            if socket.exists() {
                break;
            }
            if let Some(status) = runtime
                .child
                .as_mut()
                .expect("child")
                .try_wait()
                .expect("status")
            {
                panic!(
                    "broker exited {status}: {}",
                    fs::read_to_string(directory.join("broker.log")).expect("log")
                );
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("broker ready");
    let input = json!({"owner":"octo","repo":"config","path":"config/mutable/agent.yaml","branch":"runs/content-123","message":"smoke","mode":"create","body":""});
    for (field, value) in [
        ("branch", "runs/other"),
        ("repo", "other"),
        ("path", "config/mutable/other.yaml"),
    ] {
        let mut denied = input.clone();
        denied[field] = json!(value);
        let (result, stdout) = invoke(&socket, denied).await;
        assert_ne!(result.outcome, InvocationOutcome::Succeeded);
        assert!(stdout.is_empty());
        assert_eq!(server.egress(), 0);
    }
    let (created, stdout) = invoke(&socket, input.clone()).await;
    assert_eq!(created.outcome, InvocationOutcome::Succeeded, "{created:?}");
    assert_eq!(server.egress(), 1);
    let output: Value = serde_json::from_slice(&stdout).expect("output");
    assert_eq!(output.as_object().expect("output object").len(), 6);
    let mut update = input.clone();
    update["mode"] = json!("update");
    update["sha"] = output["blobSha"].clone();
    let (updated, stdout) = invoke(&socket, update).await;
    assert_eq!(updated.outcome, InvocationOutcome::Succeeded, "{updated:?}");
    assert_eq!(server.egress(), 2);
    assert_eq!(
        serde_json::from_slice::<Value>(&stdout).expect("output")["commitSha"],
        "c".repeat(40)
    );
    for (index, case) in failure_cases().enumerate() {
        let (result, stdout) = invoke(&socket, input.clone()).await;
        assert_ne!(result.outcome, InvocationOutcome::Succeeded);
        assert!(stdout.is_empty());
        assert_eq!(
            result.exit_status.map(|status| status.get()),
            Some(case.exit_status)
        );
        assert_eq!(
            result.detail.expect("provider message").message,
            case.stderr
        );
        assert_eq!(server.egress(), 3 + index);
    }
    let mut expected_egress = 2 + failure_cases().count();
    for mode in ["create", "update"] {
        for _reply in outcomes::unreadable_replies() {
            let mut value = input.clone();
            value["mode"] = json!(mode);
            if mode == "update" {
                value["sha"] = output["blobSha"].clone();
            }
            let (result, stdout) = invoke(&socket, value).await;
            assert_ne!(result.outcome, InvocationOutcome::Succeeded);
            assert_eq!(result.exit_status.map(|status| status.get()), Some(5));
            assert_eq!(
                result.detail.expect("provider message").message,
                outcomes::UNKNOWN_STDERR
            );
            assert!(stdout.is_empty());
            expected_egress += 1;
            assert_eq!(server.egress(), expected_egress);
        }
    }
    let egress = server.egress();
    server.finish().await;
    println!(
        "brokerd=0.64.0 allowed=create,update denied=branch,repository,path deniedEgress=0 conflictExit=4 gatewayTimeoutExit=5 unreadableConfirmedReplyExit=5 unknownExit=5 totalEgress={egress} result=PASS"
    );
}
