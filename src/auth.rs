use crate::error::ProviderError;
use crate::{
    ACCEPT_JSON, MAX_MESSAGE_OUT_BYTES, MAX_TIMESTAMP_BYTES, decode, header_text, invalid_input,
    send_get, timestamp, truncate_text, url,
};
use dekopon_provider_sdk::provider::endpoint::Base;
use dekopon_provider_sdk::provider::{HttpError, Request, Response};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusInput {}

#[derive(Deserialize)]
struct RateLimit {
    resources: Resources,
}

#[derive(Deserialize)]
struct Resources {
    core: Quota,
}

#[derive(Deserialize)]
struct Quota {
    limit: u64,
    remaining: u64,
    reset: u64,
}

pub(crate) fn status(
    input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    let _input = serde_json::from_value::<StatusInput>(input).map_err(|_| invalid_input())?;
    let response = send_get(send, url(base, "/rate_limit")?, ACCEPT_JSON)?;
    let quota = decode::<RateLimit>(&response.body)?.resources.core;
    let mut output = json!({
        "limit": quota.limit,
        "remaining": quota.remaining,
        "reset": quota.reset,
    });
    for (header, field) in [
        ("github-authentication-token-expiration", "tokenExpiration"),
        ("x-oauth-scopes", "scopes"),
    ] {
        if let Some(value) = header_text(&response, header) {
            let max = if field == "tokenExpiration" {
                MAX_TIMESTAMP_BYTES
            } else {
                MAX_MESSAGE_OUT_BYTES
            };
            let bounded = truncate_text(value, max).0;
            output[field] = json!(if field == "tokenExpiration" {
                timestamp(&bounded)?
            } else {
                &bounded
            });
        }
    }
    Ok(output)
}
