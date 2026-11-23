use super::*;
use crate::testutil::{Step, json_response, scripted, step};

const BASE: &str = "https://api.github.com/gh";
const REPO: &str = "/repos/xrl/rpi-homelab";
const APP: &str = "apps/dekopon.yaml";
const PROVIDERS: &str = "manifests/dekopon/config/providers.yaml";
const OLD_PROVIDER: &str =
    "sha256:c7df09ab39a9d8b31ca3ef95f9f94fee8b3fc99916b9117d153de429401a57a6";
const TITLE: &str = "Update pinned versions";

fn sha(digit: &str) -> String {
    digit.repeat(40)
}

fn digest(digit: &str) -> String {
    format!("sha256:{}", digit.repeat(64))
}

fn app_site(pattern: &str, after: &str) -> Value {
    json!({"path": APP, "pattern": pattern, "after": after})
}

fn app_settings() -> Value {
    json!({
        "baseUrl": BASE,
        "pinBotLogin": "pin-app[bot]",
        "pins": {
            "dekopon-chart": {"repo": "xrl/rpi-homelab", "kind":"semver",
                "sites":[app_site(r"      targetRevision: ([0-9.]+)", "      chart: dekopon")]},
            "dekopon-core": {"repo": "xrl/rpi-homelab", "kind":"digest",
                "sites":[app_site(r"            digest: (sha256:[0-9a-f]{64})", "          image:")]}
        }
    })
}

fn provider_pin(pattern: &str) -> Value {
    json!({"repo": "xrl/rpi-homelab", "kind":"digest",
        "sites":[{"path": PROVIDERS, "pattern": pattern}]})
}

fn provider_settings(pattern: &str) -> Value {
    let mut settings = app_settings();
    settings["pins"]["provider-curl"] = provider_pin(pattern);
    settings
}

fn app_file(version: &str, image_digest: &str) -> String {
    let mut text = format!(
        "apiVersion: argoproj.io/v1alpha1\nkind: Application\nmetadata:\n  name: dekopon\nspec:\n  sources:\n    - repoURL: ghcr.io/dekopon-agents/charts\n      chart: dekopon\n      targetRevision: {version}\n      helm:\n        releaseName: dekopon\n        values: |\n          image:\n            digest: {image_digest}\n          helper:\n            digest: {image_digest}\n"
    );
    text.push_str(&"          # configuration stays byte-for-byte unchanged\n".repeat(220));
    assert!((12 * 1024..13 * 1024).contains(&text.len()));
    text
}

fn wire(method: &'static str, suffix: impl AsRef<str>, status: u16, body: Value) -> Step {
    inspect(method, suffix, status, body, |_| {})
}

fn inspect(
    method: &'static str,
    suffix: impl AsRef<str>,
    status: u16,
    response: Value,
    check: impl Fn(&Request) + 'static,
) -> Step {
    let uri = format!("{BASE}{REPO}{}", suffix.as_ref());
    step(
        move |request| {
            assert_eq!(request.method, method);
            assert_eq!(request.uri, uri);
            check(request);
        },
        json_response(status, &response),
    )
}

fn call(
    handler: fn(Value, &GhSettings, &mut Send<'_>) -> Result<Value, ProviderError>,
    settings: Value,
    input: Value,
    steps: Vec<Step>,
) -> Result<Value, ProviderError> {
    let settings: GhSettings = serde_json::from_value(settings).expect("valid settings");
    let expected = steps.len();
    let mut scripts = scripted(steps);
    let mut sent = 0;
    let result = handler(input, &settings, &mut |request| {
        sent += 1;
        scripts(request)
    });
    assert_eq!(sent, expected);
    result
}

fn ref_step() -> Step {
    wire(
        "GET",
        "/git/ref/heads/main",
        200,
        json!({"ref":"refs/heads/main","object":{"sha":sha("a")}}),
    )
}

fn file_step(path: &str, reference: &str, blob: &str, text: &str) -> Step {
    wire(
        "GET",
        format!("/contents/{path}?ref={}", sha(reference)),
        200,
        json!({
            "type":"file","path":path,"sha":sha(blob),"size":text.len(),
            "encoding":"base64","content":STANDARD.encode(text),
        }),
    )
}

fn update_input(changes: &[(&str, &str)]) -> Value {
    json!({"pins":changes.iter().map(|(name, value)| json!({"name":name,"value":value})).collect::<Vec<_>>(),"title":TITLE})
}

fn request_body(request: &Request) -> Value {
    serde_json::from_slice(&request.body).expect("request JSON")
}

fn writes(input: &Value, files: Vec<(&str, String)>) -> (String, Vec<Step>) {
    let first = input["pins"][0]["name"].as_str().expect("pin name");
    let values: Values = input["pins"]
        .as_array()
        .expect("pins")
        .iter()
        .map(|change| {
            (
                change["name"].as_str().unwrap().to_owned(),
                change["value"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let hash = format!("{:x}", Sha256::digest(serde_json::to_vec(&values).unwrap()));
    let branch = format!("pin/{first}-{}", &hash[..8]);
    let git_ref = format!("refs/heads/{branch}");
    let body = json!({"ref":git_ref,"sha":sha("a")});
    let mut steps = vec![inspect(
        "POST",
        "/git/refs",
        201,
        json!({"ref":git_ref,"object":{"sha":sha("a")}}),
        move |request| assert_eq!(request_body(request), body),
    )];
    let mut parent = sha("a");
    for (index, (path, text)) in files.into_iter().enumerate() {
        let commit = sha(if index == 0 { "d" } else { "e" });
        let expected =
            json!({"branch":branch,"sha":sha("b"),"message":TITLE,"content":STANDARD.encode(text)});
        steps.push(inspect(
            "PUT",
            format!("/contents/{path}"),
            200,
            json!({"content":{"type":"file","path":path,"sha":sha("c")},
                "commit":{"sha":commit,"parents":[{"sha":parent}]}}),
            move |request| assert_eq!(request_body(request), expected),
        ));
        parent = commit;
    }
    let output =
        json!({"number":7,"head":{"ref":branch,"sha":parent},"base":{"ref":"main","sha":sha("a")}});
    let body = json!({"title":TITLE,"head":branch,"base":"main","maintainer_can_modify":false});
    steps.push(inspect("POST", "/pulls", 201, output, move |request| {
        assert_eq!(request_body(request), body)
    }));
    (branch, steps)
}

#[test]
fn native_update_batches_chart_and_core_and_writes_each_changed_file_once() {
    let path = "values.yaml";
    let mut settings = app_settings();
    settings["pins"]["dekopon-chart"]["sites"]
        .as_array_mut()
        .expect("sites")
        .push(json!({"path":path,"pattern":r"chartVersion: ([0-9.]+)"}));
    let old = digest("a");
    let new = digest("b");
    let before = app_file("0.81.0", &old);
    let expected = before
        .replace("targetRevision: 0.81.0", "targetRevision: 0.82.0")
        .replacen(&old, &new, 1);
    let input = update_input(&[("dekopon-chart", "0.82.0"), ("dekopon-core", &new)]);
    let mut steps = vec![
        ref_step(),
        file_step(APP, "a", "b", &before),
        file_step(path, "a", "b", "chartVersion: 0.81.0"),
    ];
    let (branch, writable) = writes(
        &input,
        vec![(APP, expected), (path, "chartVersion: 0.82.0".to_string())],
    );
    steps.extend(writable);
    let output = call(update, settings, input, steps).expect("update");
    assert_eq!(
        output,
        json!({"branch":branch,"number":7,"headSha":sha("e"),"files":[APP,path]})
    );
}

#[test]
fn native_update_matches_the_provider_digest_only_with_its_literal_list_prefix() {
    let text = include_str!("../../tests/fixtures/pins/providers.yaml");
    let input = update_input(&[("provider-curl", &digest("b"))]);
    let good = r"  - source: ghcr\.io/dekopon-agents/provider-curl@(sha256:[0-9a-f]{64})";
    let (branch, writable) = writes(
        &input,
        vec![(PROVIDERS, text.replace(OLD_PROVIDER, &digest("b")))],
    );
    let mut steps = vec![ref_step(), file_step(PROVIDERS, "a", "b", text)];
    steps.extend(writable);
    let output = call(update, provider_settings(good), input.clone(), steps).expect("provider pin");
    assert_eq!(
        output,
        json!({"branch":branch,"number":7,"headSha":sha("d"),"files":[PROVIDERS]})
    );

    let steps = vec![ref_step(), file_step(PROVIDERS, "a", "b", text)];
    let error = call(
        update,
        provider_settings(good.trim_start_matches("  - ")),
        input,
        steps,
    )
    .unwrap_err();
    assert_eq!(error.code(), "no-match");
    assert_eq!(
        error.message(),
        format!("pin provider-curl matched no line at {PROVIDERS}")
    );
}

#[test]
fn native_update_rejects_disagreeing_sites_and_rejects_a_tag_on_a_digest_pin() {
    let mut settings = app_settings();
    settings["pins"]["dekopon-chart"]["sites"]
        .as_array_mut()
        .expect("sites")
        .push(json!({"path":"values.yaml","pattern":r"chartVersion: ([0-9.]+)"}));
    let error = call(
        update,
        settings.clone(),
        update_input(&[("dekopon-chart", "0.82.0")]),
        vec![
            ref_step(),
            file_step(APP, "a", "b", &app_file("0.81.0", &digest("a"))),
            file_step("values.yaml", "a", "c", "chartVersion: 0.80.0\n"),
        ],
    )
    .unwrap_err();
    assert_eq!(error.code(), "pin-disagreement");
    let error = call(
        update,
        settings,
        update_input(&[("dekopon-core", &format!("tag@{}", digest("a")))]),
        vec![],
    )
    .unwrap_err();
    assert_eq!(error.code(), "invalid-pin-value");
}

fn pull(head: &str, user: &str, kind: &str) -> Value {
    json!({"number":7,"title":TITLE,"state":"open",
        "user":{"login":user,"type":kind},
        "head":{"ref":head,"sha":sha("d"),"repo":{"full_name":"xrl/rpi-homelab"}},
        "base":{"ref":"main","sha":sha("a")},
        "created_at":"2026-11-23T00:00:00Z","updated_at":"2026-11-23T00:00:00Z"})
}

fn merge_input() -> Value {
    json!({"repo":"xrl/rpi-homelab","number":7,"expectedHeadSha":sha("d")})
}

#[test]
fn native_merge_refuses_a_non_pin_head_or_an_author_other_than_the_app_bot() {
    for pull in [
        pull("feature/change", "pin-app[bot]", "Bot"),
        pull("pin/test", "operator", "User"),
        pull("pin/test", "other[bot]", "Bot"),
        pull("pin/test", "pin-app[bot]", "User"),
    ] {
        let error = call(
            merge,
            app_settings(),
            merge_input(),
            vec![wire("GET", "/pulls/7", 200, pull)],
        )
        .unwrap_err();
        assert_eq!(error.code(), "unsafe-pin-diff");
    }
}

fn tree_step(tree: &str, path: &str, mode: &str, blob: &str) -> Step {
    wire(
        "GET",
        format!("/git/trees/{}?recursive=1", sha(tree)),
        200,
        json!({
            "truncated":false,"sha":sha(tree),"tree":[{"type":"blob","mode":mode,"path":path,"sha":sha(blob)}],
        }),
    )
}

fn proof(path: &str, new_mode: &str) -> Vec<Step> {
    vec![
        wire(
            "GET",
            "/pulls/7",
            200,
            pull("pin/test", "pin-app[bot]", "Bot"),
        ),
        wire(
            "GET",
            format!("/compare/{}...{}?per_page=1", sha("a"), sha("d")),
            200,
            json!({"merge_base_commit":{"sha":sha("a"),"commit":{"tree":{"sha":sha("e")}}}}),
        ),
        wire(
            "GET",
            format!("/git/commits/{}", sha("d")),
            200,
            json!({"tree":{"sha":sha("f")}}),
        ),
        tree_step("e", path, "100644", "b"),
        tree_step("f", path, new_mode, "c"),
    ]
}

#[test]
fn native_merge_refuses_files_outside_the_pin_set_and_mode_changes() {
    for (path, mode) in [("README.md", "100644"), (APP, "100755")] {
        let error = call(merge, app_settings(), merge_input(), proof(path, mode)).unwrap_err();
        assert_eq!(error.code(), "unsafe-pin-diff");
    }
}

#[test]
fn native_merge_refuses_any_changed_non_pattern_line_and_changed_anchors() {
    let old = app_file("0.81.0", &digest("a"));
    for new in [
        old.replace("targetRevision: 0.81.0", "targetRevision: 0.82.0")
            .replace("releaseName: dekopon", "releaseName: another"),
        old.replace("targetRevision: 0.81.0", "targetRevision: 0.82.0")
            .replace("chart: dekopon", "chart: another"),
    ] {
        let mut steps = proof(APP, "100644");
        steps.extend([
            file_step(APP, "a", "b", &old),
            file_step(APP, "d", "c", &new),
        ]);
        let error = call(merge, app_settings(), merge_input(), steps).unwrap_err();
        assert_eq!(error.code(), "unsafe-pin-diff");
    }
}

#[test]
fn native_merge_checks_pinned_files_then_surfaces_the_exact_github_denial_without_protection_reads()
{
    let old = app_file("0.81.0", &digest("a"));
    let new = app_file("0.82.0", &digest("a"));
    for status in [405, 409] {
        let mut steps = proof(APP, "100644");
        steps.extend([
            file_step(APP, "a", "b", &old),
            file_step(APP, "d", "c", &new),
            wire(
                "GET",
                "/pulls/7",
                200,
                pull("pin/test", "pin-app[bot]", "Bot"),
            ),
            inspect(
                "PUT",
                "/pulls/7/merge",
                status,
                json!({"message":"GitHub's policy refused this merge"}),
                |request| assert_eq!(request_body(request), json!({"sha":sha("d")})),
            ),
        ]);
        let error = call(merge, app_settings(), merge_input(), steps).unwrap_err();
        assert_eq!(error.code(), "github-merge-refused");
        assert_eq!(
            error.message(),
            format!("GitHub HTTP {status}: GitHub's policy refused this merge")
        );
    }
}

#[test]
fn native_settings_reject_invalid_regex_or_unknown_pin_fields_before_any_request() {
    let mut settings = app_settings();
    settings["pins"]["dekopon-chart"]["sites"][0]["pattern"] = json!(r"(win)(lose)");
    assert!(serde_json::from_value::<GhSettings>(settings).is_err());
    let mut settings = app_settings();
    settings["pins"]["dekopon-chart"]["sites"][0]["contents"] = json!("unknown");
    assert!(serde_json::from_value::<GhSettings>(settings).is_err());
}
