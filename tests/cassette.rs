use base64::Engine as _;
use dekopon_gh_provider::Gh;
use dekopon_provider_sdk::provider::{Header, Response};
use dekopon_provider_sdk_testkit::{HttpScript, Native};
use serde_json::{Value, json};

const BASE_URL: &str = "https://fixture.example.test/gh";

fn cassette(name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/cassettes/gh")
        .join(name);
    serde_json::from_slice(&std::fs::read(path).expect("cassette file")).expect("cassette JSON")
}

fn recorded_response(exchange: &Value) -> Response {
    let recorded = &exchange["response"];
    let mut headers = Vec::new();
    for (name, value) in recorded["headers"].as_object().expect("header map") {
        let values = match value {
            Value::Array(values) => values.iter().collect(),
            single => vec![single],
        };
        for value in values {
            headers.push(
                Header::text(name.as_str(), value.as_str().expect("text header"))
                    .expect("valid header"),
            );
        }
    }
    let body = match &recorded["body"] {
        Value::Null => Vec::new(),
        body if body.get("json").is_some() => serde_json::to_vec(&body["json"]).expect("JSON"),
        body if body.get("text").is_some() => {
            body["text"].as_str().expect("text").as_bytes().to_vec()
        }
        body => base64::engine::general_purpose::STANDARD
            .decode(body["base64"].as_str().expect("base64 body"))
            .expect("valid base64"),
    };
    Response {
        status: u16::try_from(recorded["status"].as_u64().expect("status")).expect("u16"),
        headers,
        body,
    }
}

#[test]
fn recorded_pull_request_replays_through_the_base_url_setting() {
    let exchange = cassette("0001-GET-repos-dekopon-agents-dekopon-provider-gh-pulls-14.json");
    let request = &exchange["request"];
    assert_eq!(request["headers"]["authorization"], "[redacted]");
    let native = Native::<Gh>::new()
        .settings(json!({"baseUrl": BASE_URL}))
        .http(HttpScript::new(
            "fixture.example.test",
            "GET",
            recorded_response(&exchange),
        ));
    let output = native.call(
        "gh.pull-request.read",
        &json!({"owner": "dekopon-agents", "repo": "dekopon-provider-gh", "number": 14})
            .to_string(),
    );
    assert_eq!(output.status, 0, "{}", output.stderr);

    let sent = native.requests();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].method, request["method"]);
    let query = request["query"]
        .as_str()
        .map_or_else(String::new, |query| format!("?{query}"));
    assert_eq!(
        sent[0].uri,
        format!(
            "{BASE_URL}{}{query}",
            request["path"].as_str().expect("path")
        )
    );
    let accept = sent[0]
        .headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case("accept"))
        .expect("accept header");
    assert_eq!(
        accept.value,
        request["headers"]["accept"]
            .as_str()
            .expect("accept")
            .as_bytes()
    );
    assert!(
        !sent[0]
            .headers
            .iter()
            .any(|header| header.name.eq_ignore_ascii_case("authorization"))
    );

    let pull: Value = serde_json::from_slice(&output.stdout).expect("JSON output");
    assert_eq!(pull["number"], 14);
    assert_eq!(pull["state"], "closed");
    assert_eq!(pull["merged"], true);
    assert_eq!(pull["headSha"], "9071c9b08be3105b89ec07c5a9eb2bcef41d0d4a");
    assert_eq!(
        pull["title"],
        "docs: update PR reviewer example for dekopon-gatewayd"
    );
}
