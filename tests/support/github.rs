use base64::Engine as _;
use serde_json::Value;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

pub struct GitHub {
    pub base: String,
    seen: Arc<AtomicUsize>,
    expected: usize,
    server: tokio::task::JoinHandle<()>,
}

impl GitHub {
    pub async fn start(steps: Vec<Value>) -> Self {
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
                let authorization = head.lines().find_map(|line| {
                    line.split_once(':')
                        .filter(|(key, _)| key.eq_ignore_ascii_case("authorization"))
                        .map(|(_, value)| value.trim())
                });
                assert_eq!(authorization, request["brokerAuthorization"].as_str());
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
                if response["status"] == 0 {
                    continue;
                }
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

        Self {
            base,
            seen,
            expected,
            server,
        }
    }
    pub fn egress(&self) -> usize {
        self.seen.load(Ordering::SeqCst)
    }
    pub async fn finish(self) {
        self.server.abort();
        let observed = self.egress();
        let error = self
            .server
            .await
            .expect_err("script waits for extra requests");
        assert!(error.is_cancelled(), "HTTP script failed: {error}");
        assert_eq!(observed, self.expected);
    }
}
