use crate::error::ProviderError;
use dekopon_provider_sdk::provider::endpoint::Base;
use dekopon_provider_sdk::provider::{HttpError, Request, Response, method};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryVariables {
    pub owner: String,
    pub name: String,
}

#[derive(Serialize, Deserialize)]
pub struct RepositoryNode {
    pub id: String,
}

#[derive(Deserialize)]
struct RepositoryData {
    repository: RepositoryNode,
}

#[derive(Deserialize)]
struct Envelope {
    data: Option<serde_json::Value>,
    #[serde(default)]
    errors: Vec<serde_json::Value>,
}

pub fn repository_node(
    variables: RepositoryVariables,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<RepositoryNode, ProviderError> {
    crate::validate_login(&variables.owner)?;
    crate::validate_repo(&variables.name)?;
    let body = json!({
        "query": "query RepositoryNode($owner: String!, $name: String!) { repository(owner: $owner, name: $name) { id } }",
        "variables": variables,
    });
    let request = crate::github_json_request(method::POST, crate::url(base, "/graphql")?, &body)?;
    let response = send(request).map_err(|_| crate::http_failed())?;
    if response.status != 200 {
        return Err(crate::status_error(&response));
    }
    let envelope: Envelope = crate::decode(&response.body)?;
    if !envelope.errors.is_empty() {
        return Err(ProviderError::new(
            "graphql-error",
            "GitHub GraphQL reported an error",
        ));
    }
    let data: RepositoryData =
        serde_json::from_value(envelope.data.ok_or_else(crate::invalid_response)?)
            .map_err(|_| crate::invalid_response())?;
    let node = data.repository;
    if node.id.is_empty() || node.id.len() > 256 || node.id.chars().any(char::is_control) {
        return Err(crate::invalid_response());
    }
    Ok(node)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dekopon_provider_sdk::provider::Failure;

    #[test]
    fn native_graphql_errors_have_a_typed_code_even_with_partial_data() {
        for data in [
            serde_json::Value::Null,
            json!({"repository":null}),
            json!({"repository":{"id":"R_fixed"}}),
        ] {
            let mut requests = Vec::new();
            let error = repository_node(
                RepositoryVariables {
                    owner: "octo".into(),
                    name: "hello".into(),
                },
                &crate::GITHUB_API,
                &mut |request| {
                    requests.push(request);
                    Ok(Response {
                        status: 200,
                        headers: vec![],
                        body: json!({"data":data,"errors":[{"message":"private error"}]})
                            .to_string()
                            .into_bytes(),
                    })
                },
            )
            .err()
            .expect("GraphQL error");
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].method, "POST");
            assert_eq!(requests[0].uri, "https://api.github.com/graphql");
            assert!(
                !requests[0]
                    .headers
                    .iter()
                    .any(|header| header.name.eq_ignore_ascii_case("authorization"))
            );
            assert_eq!(Failure::code(&error).as_str(), "graphql-error");
            assert_eq!(error.message(), "GitHub GraphQL reported an error");
        }
    }
}
