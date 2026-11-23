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
            "drn:secret:github/org",
        ),
        (
            include_str!("../examples/pins/constraints.yaml"),
            include_str!("../examples/pins/secret-map.yaml.example"),
            include_str!("../examples/pins/policies.cedar"),
            "pin-writer",
            &["gh.pin.update", "gh.pin.merge"][..],
            "drn:secret:github/xrl",
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
