use dekopon_broker::{ConstraintSet, PolicyEngine, PolicyWorld};
use serde_json::{Value, json};

fn yaml(source: &str) -> Value {
    serde_yaml_ng::from_str(source).expect("valid example YAML")
}

#[test]
fn each_example_capability_has_one_github_app_binding_and_a_valid_cedar_policy() {
    for (constraints, map, policy, principal, capabilities, expected_source) in [
        (
            include_str!("../examples/pr-enqueue/constraints.yaml"),
            include_str!("../examples/pr-enqueue/secret-map.yaml.example"),
            include_str!("../examples/pr-enqueue/policies.cedar"),
            "queue-writer",
            &["gh.pull-request.enqueue"][..],
            "drn:com.xrl:secret:prod:github/org",
        ),
        (
            include_str!("../examples/pins/constraints.yaml"),
            include_str!("../examples/pins/secret-map.yaml.example"),
            include_str!("../examples/pins/policies.cedar"),
            "pin-writer",
            &["gh.pin.update", "gh.pin.merge"][..],
            "drn:com.xrl:secret:prod:github/xrl",
        ),
    ] {
        let constraints = yaml(constraints);
        let map = yaml(map);
        let sets = constraints["capabilities"]["gh"]["capabilities"]
            .as_object()
            .expect("capability map");
        assert_eq!(sets.len(), capabilities.len());
        if let Some(settings) = constraints.pointer("/providerSettings/gh") {
            serde_json::from_value::<dekopon_gh_provider::GhSettings>(settings.clone())
                .expect("valid owner settings");
        }
        let world = PolicyWorld::new(
            [principal.parse().expect("principal")],
            capabilities.iter().map(|id| {
                (
                    id.parse().expect("capability"),
                    "gh".parse().expect("provider"),
                )
            }),
        )
        .expect("world")
        .with_secrets([expected_source.parse().expect("secret")]);
        PolicyEngine::new(policy, &world).expect("valid Cedar");

        for id in capabilities {
            let mut set = sets[*id].clone();
            set["provider"] = json!("gh");
            set["effect"] = json!("external-write");
            set["risk"] = json!(dekopon_core::RiskLevel::High);
            let set: ConstraintSet = serde_json::from_value(set).expect("valid broker constraints");
            assert!(set.credential.is_none());
            let default = set.default_secret_use.expect("explicit source");
            assert_eq!(default.drn.as_str(), expected_source);
            let mut bindings = map["secrets"]
                .as_array()
                .expect("secret sources")
                .iter()
                .flat_map(|source| {
                    source["bindings"]
                        .as_array()
                        .expect("bindings")
                        .iter()
                        .map(move |binding| (source, binding))
                })
                .filter(|(_, binding)| {
                    binding["capabilities"]
                        .as_array()
                        .expect("capabilities")
                        .contains(&json!(id))
                });
            let (source, binding) = bindings.next().expect("one binding");
            assert!(
                bindings.next().is_none(),
                "{id} has more than one source binding"
            );
            assert_eq!(source["drn"], expected_source);
            assert_eq!(source["source"]["kind"], "githubApp");
            assert_eq!(source["destinations"], json!(["api.github.com"]));
            if *id != "gh.pull-request.enqueue" {
                assert_eq!(
                    binding["allowedPaths"],
                    json!([{"match":"segmentPrefix","path":"/repos/xrl/rpi-homelab"}])
                );
            }
            let http = set.constraints.http.expect("HTTP constraint");
            assert_eq!(json!(http.allowed_methods), binding["allowedMethods"]);
            assert_eq!(json!(http.max_requests), binding["maxInjections"]);
        }
    }
}

#[tokio::test]
async fn native_pin_update_grant_admits_contents_at_a_commit_sha() {
    assert_pin_query(
        "gh.pin.update",
        &format!(
            "https://api.github.com/repos/xrl/rpi-homelab/contents/apps/dekopon.yaml?ref={}",
            "a".repeat(40)
        ),
    )
    .await;
}

#[tokio::test]
async fn native_pin_merge_grant_admits_the_queries_needed_to_verify_the_diff() {
    let base = "a".repeat(40);
    let head = "b".repeat(40);
    for path in [
        format!("/compare/{base}...{head}?per_page=1"),
        format!("/git/trees/{base}?recursive=1"),
        format!("/contents/apps/dekopon.yaml?ref={head}"),
    ] {
        assert_pin_query(
            "gh.pin.merge",
            &format!("https://api.github.com/repos/xrl/rpi-homelab{path}"),
        )
        .await;
    }
}

async fn assert_pin_query(capability: &str, uri: &str) {
    use dekopon_capability::SecretUseGrant;
    use dekopon_core::SecretBytes;
    use dekopon_http_host::{
        BoundCredential, BufferedHttpClient, ErrorCode, HttpHostCeilings, Request, Response,
        test_transport::Transport,
    };
    use std::time::Duration;

    let constraints = yaml(include_str!("../examples/pins/constraints.yaml"));
    let http = serde_json::from_value(
        constraints["capabilities"]["gh"]["capabilities"][capability]["constraints"]["http"]
            .clone(),
    )
    .expect("valid HTTP constraints");

    let map = yaml(include_str!("../examples/pins/secret-map.yaml.example"));
    let source = &map["secrets"][0];
    let mut binding = source["bindings"]
        .as_array()
        .expect("pin App bindings")
        .iter()
        .find(|binding| binding["capabilities"] == json!([capability]))
        .and_then(Value::as_object)
        .expect("capability binding")
        .clone();
    binding.remove("capabilities").expect("capability list");
    let id = binding.remove("id").expect("binding ID");
    binding.insert("bindingId".into(), id);
    binding.insert("secret".into(), source["drn"].clone());
    let grant: SecretUseGrant =
        serde_json::from_value(Value::Object(binding)).expect("valid secret-use grant");

    let credential = BoundCredential::secret_bearer(
        SecretBytes::new(b"fixture-pin-app-credential".to_vec()),
        &grant,
    )
    .expect("fixture credential");
    let reply = Response {
        status: 200,
        headers: Vec::new(),
        body: Vec::new(),
    };
    let transport = Transport::new(
        ("api.github.com".into(), vec![reply.clone()]),
        ("unused.test".into(), reply),
        1,
    );
    let mut client = BufferedHttpClient::authorized_with_secret_credential(
        http,
        grant,
        credential,
        HttpHostCeilings::default(),
        Duration::from_secs(5),
    )
    .expect("valid HTTP authorization")
    .with_test_transport(transport.clone());

    let get = |uri: String| Request {
        method: "GET".into(),
        uri,
        headers: Vec::new(),
        body: Vec::new(),
    };
    let neighbor = uri.replacen("/rpi-homelab/", "/rpi-homelab-other/", 1);
    let error = client
        .send(get(neighbor))
        .await
        .expect_err("the neighboring repository is outside the allowed path");
    assert_eq!(error.code, ErrorCode::Denied);
    assert!(transport.requests().is_empty());

    let reply = client
        .send(get(uri.to_owned()))
        .await
        .unwrap_or_else(|error| panic!("{capability} must admit GET {uri}: {error}"));
    assert_eq!(reply.status, 200);
    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].guest.uri, uri);
    assert_eq!(requests[0].sent.uri, uri);
    assert_eq!(requests[0].sent.method, "GET");
    assert!(
        requests[0]
            .guest
            .headers
            .iter()
            .all(|header| !header.name.eq_ignore_ascii_case("authorization"))
    );
    assert!(
        client
            .into_evidence()
            .iter()
            .any(|call| call.credential_injected)
    );
}
