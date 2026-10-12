use dekopon_provider_sdk::provider::endpoint::Base;
use dekopon_provider_sdk::provider::{HttpError, Request, Response};
use serde_json::{Map, Value, json};

use crate::error::ProviderError;
use crate::{
    ACCEPT_JSON, decode, header_values, invalid_input, invalid_response, permission_header,
    send_get, timestamp, url,
};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusInput {}

#[derive(serde::Deserialize)]
struct RawRateLimit {
    resources: RawResources,
}

#[derive(serde::Deserialize)]
struct RawResources {
    core: RawQuota,
}

#[derive(serde::Deserialize)]
struct RawQuota {
    limit: u64,
    remaining: u64,
    reset: u64,
}

pub(crate) fn status(
    input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    serde_json::from_value::<StatusInput>(input).map_err(|_| invalid_input())?;

    let response = send_get(send, url(base, "/rate_limit")?, ACCEPT_JSON)?;
    let core = decode::<RawRateLimit>(&response.body)?.resources.core;

    let mut output = Map::new();
    output.insert(
        "core".to_owned(),
        json!({"limit": core.limit, "remaining": core.remaining, "reset": core.reset}),
    );
    if let Some(expiration) =
        header_values(&response, "github-authentication-token-expiration").next()
    {
        let expiration = core::str::from_utf8(expiration).map_err(|_| invalid_response())?;
        output.insert(
            "tokenExpiration".to_owned(),
            Value::String(timestamp(expiration.trim())?.to_owned()),
        );
    }
    if let Some(raw) = header_values(&response, "x-oauth-scopes").next() {
        let held = if raw.trim_ascii().is_empty() {
            ""
        } else {
            permission_header(&response, "x-oauth-scopes").ok_or_else(invalid_response)?
        };
        let scopes = held
            .split(',')
            .map(str::trim)
            .filter(|scope| !scope.is_empty())
            .map(|scope| Value::String(scope.to_owned()))
            .collect();
        output.insert("scopes".to_owned(), Value::Array(scopes));
    }
    Ok(Value::Object(output))
}
