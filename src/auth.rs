use crate::error::ProviderError;
use crate::{ACCEPT_JSON, decode, header_text, invalid_input, send_get, url};
use dekopon_provider_sdk::provider::endpoint::Base;
use dekopon_provider_sdk::provider::{HttpError, Request, Response};
use serde::Deserialize;
use serde_json::{Value, json};

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
    if input.as_object().is_none_or(|object| !object.is_empty()) {
        return Err(invalid_input());
    }
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
            output[field] = json!(value);
        }
    }
    Ok(output)
}
