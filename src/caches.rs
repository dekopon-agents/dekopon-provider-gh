use crate::action_writes::{self as writes, Outcome};
use crate::actions::{self, Reads};
use crate::{
    Base, HttpError, Request, Response, error::ProviderError, invalid_input, invalid_response,
};
use serde::Deserialize;
use serde_json::{Value, json};

pub(crate) const FIELDS: &str = "id,key,ref,sizeInBytes,createdAt,lastAccessedAt";
#[derive(Clone, Copy, Deserialize)]
#[serde(transparent)]
struct CacheId(u64);
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListInput {
    owner: String,
    repo: String,
    #[serde(default = "default_limit")]
    limit: usize,
    key: Option<String>,
    #[serde(rename = "ref")]
    reference: Option<String>,
    #[serde(default)]
    sort: Sort,
    #[serde(default)]
    order: Order,
    #[serde(rename = "_cliOutput")]
    output: Option<String>,
}
#[derive(Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Sort {
    #[default]
    LastAccessedAt,
    CreatedAt,
    SizeInBytes,
}
impl Sort {
    fn wire(&self) -> &'static str {
        match self {
            Self::LastAccessedAt => "last_accessed_at",
            Self::CreatedAt => "created_at",
            Self::SizeInBytes => "size_in_bytes",
        }
    }
}
#[derive(Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Order {
    Asc,
    #[default]
    Desc,
}
impl Order {
    fn wire(&self) -> &'static str {
        match self {
            Self::Asc => "asc",
            Self::Desc => "desc",
        }
    }
}
fn default_limit() -> usize {
    30
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeleteInput {
    owner: String,
    repo: String,
    cache_id: Option<CacheId>,
    key: Option<String>,
    #[serde(rename = "ref")]
    reference: Option<String>,
    #[serde(default)]
    all: bool,
    #[serde(default)]
    succeed_on_no_caches: bool,
}
fn common(
    owner: &str,
    repo: &str,
    key: Option<&str>,
    reference: Option<&str>,
) -> Result<(), ProviderError> {
    crate::validate_login(owner)?;
    crate::validate_repo(repo)?;
    if let Some(key) = key
        && (key.is_empty() || key.len() > 512 || key.chars().any(char::is_control))
    {
        return Err(invalid_input());
    }
    if let Some(reference) = reference {
        crate::validate_ref(reference)?;
    }
    Ok(())
}
pub(crate) fn validate(id: &str, input: Value) -> Result<(), ProviderError> {
    if id == crate::ids::CACHE_LIST {
        let i: ListInput = serde_json::from_value(input).map_err(|_| invalid_input())?;
        common(&i.owner, &i.repo, i.key.as_deref(), i.reference.as_deref())?;
        if !(1..=100).contains(&i.limit) {
            return Err(invalid_input());
        }
        actions::selection(i.output.as_deref(), FIELDS)
    } else {
        let i: DeleteInput = serde_json::from_value(input).map_err(|_| invalid_input())?;
        common(&i.owner, &i.repo, i.key.as_deref(), i.reference.as_deref())?;
        if usize::from(i.all) + usize::from(i.cache_id.is_some()) + usize::from(i.key.is_some())
            != 1
            || i.cache_id.is_some_and(|v| v.0 == 0)
            || (i.cache_id.is_some() && i.reference.is_some())
        {
            return Err(invalid_input());
        }
        Ok(())
    }
}
fn row(v: &Value) -> Result<Value, ProviderError> {
    Ok(
        json!({"id":actions::positive(v,"id")?,"key":actions::text(v,"key",512)?,"ref":actions::text(v,"ref",1024)?,"sizeInBytes":v["size_in_bytes"].as_u64().ok_or_else(invalid_response)?,"createdAt":actions::stamp(v,"created_at")?,"lastAccessedAt":actions::stamp(v,"last_accessed_at")?}),
    )
}
fn path(root: &str, key: Option<&str>, reference: Option<&str>) -> String {
    let mut p = format!("{root}/actions/caches?");
    if let Some(key) = key {
        p.push_str(&format!("key={}&", crate::percent_encode(key)));
    }
    if let Some(reference) = reference {
        p.push_str(&format!("ref={}&", crate::percent_encode(reference)));
    }
    p.trim_end_matches('&').to_owned()
}
pub(crate) fn run(
    id: &str,
    input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    validate(id, input.clone())?;
    if id == crate::ids::CACHE_LIST {
        let i: ListInput = serde_json::from_value(input).map_err(|_| invalid_input())?;
        let mut reads = Reads {
            base,
            send,
            used: 0,
            max: 11,
            target_errors: true,
        };
        let root = actions::root(&i.owner, &i.repo);
        writes::repository(&mut reads, &root)?;
        let p = format!(
            "{}{}sort={}&direction={}",
            path(&root, i.key.as_deref(), i.reference.as_deref()),
            if i.key.is_some() || i.reference.is_some() {
                "&"
            } else {
                ""
            },
            i.sort.wire(),
            i.order.wire()
        );
        let (rows, coverage) =
            actions::pages_with(&mut reads, &p, "actions_caches", 10, i.limit, true, |v| {
                Ok(Some(row(v)?))
            })?;
        let rows = if let Some(fields) = i.output.filter(|s| !s.is_empty()) {
            rows.iter()
                .map(|r| crate::output::select(r, &fields, false))
                .collect()
        } else {
            rows
        };
        return Ok(json!({"caches":rows,"coverage":coverage.json(),"requestsUsed":reads.used}));
    }
    let i: DeleteInput = serde_json::from_value(input).map_err(|_| invalid_input())?;
    let mut reads = Reads {
        base,
        send,
        used: 0,
        max: 31,
        target_errors: true,
    };
    let root = actions::root(&i.owner, &i.repo);
    writes::repository(&mut reads, &root)?;
    let p = path(&root, i.key.as_deref(), i.reference.as_deref());
    let (rows, coverage) =
        actions::pages_with(&mut reads, &p, "actions_caches", 10, 1000, false, |v| {
            row(v)?;
            let selected = if let Some(id) = i.cache_id {
                v["id"] == id.0
            } else if let Some(key) = &i.key {
                v["key"] == *key
            } else {
                true
            };
            if selected
                && i.reference
                    .as_ref()
                    .is_some_and(|reference| v["ref"] != *reference)
            {
                return Err(invalid_response());
            }
            Ok(selected.then(|| v.clone()))
        })?;
    if coverage.json()["incomplete"] == true {
        return Err(Outcome::Incomplete.into());
    }
    if rows.is_empty() {
        if i.succeed_on_no_caches {
            return Ok(json!({"deletedIds":[],"outcome":"no-caches","requestsUsed":reads.used}));
        }
        return Err(Outcome::NotFound.into());
    }
    let mut deleted = Vec::new();
    if i.all {
        all_bound(rows.len())?;
        for cache in rows {
            let id = actions::positive(&cache, "id")?;
            writes::write(
                &mut reads,
                "DELETE",
                &format!("{root}/actions/caches/{id}"),
                None,
                204,
            )?;
            deleted.push(id);
        }
    } else if let Some(id) = i.cache_id {
        writes::write(
            &mut reads,
            "DELETE",
            &format!("{root}/actions/caches/{}", id.0),
            None,
            204,
        )?;
        deleted.push(id.0);
    } else {
        let mut target = format!(
            "{root}/actions/caches?key={}",
            crate::percent_encode(i.key.as_deref().ok_or_else(invalid_input)?)
        );
        if let Some(reference) = &i.reference {
            target.push_str(&format!("&ref={}", crate::percent_encode(reference)));
        }
        let response = writes::send(&mut reads, "DELETE", &target, None)?;
        if response.status != 200 {
            return Err(writes::status(&response, false)
                .map(ProviderError::from)
                .unwrap_or_else(|| crate::status_error(&response)));
        }
        let response: Value = crate::decode(&response.body)?;
        let caches = response["actions_caches"]
            .as_array()
            .ok_or_else(invalid_response)?;
        if caches.len() > 1000 {
            return Err(invalid_response());
        }
        for cache in caches {
            deleted.push(actions::positive(cache, "id")?);
        }
    }
    Ok(json!({"deletedIds":deleted,"outcome":"deleted","requestsUsed":reads.used}))
}
pub(crate) fn execute(
    id: &str,
    input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
    out: &mut dyn std::io::Write,
) -> Result<(), ProviderError> {
    let selection = input
        .get("_cliOutput")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let result = run(id, input, base, send)?;
    let text = if selection.as_deref() == Some("") {
        result["caches"]
            .as_array()
            .ok_or_else(invalid_response)?
            .iter()
            .map(|r| {
                format!(
                    "{}\t{}\t{}\t{}\n",
                    r["id"],
                    r["key"].as_str().unwrap_or(""),
                    r["ref"].as_str().unwrap_or(""),
                    r["sizeInBytes"]
                )
            })
            .collect()
    } else {
        serde_json::to_string(&result).map_err(|_| invalid_response())? + "\n"
    };
    out.write_all(text.as_bytes())
        .map_err(|_| invalid_response())?;
    if result["coverage"]["incomplete"] == true {
        return Err(Outcome::Incomplete.into());
    }
    Ok(())
}
pub(crate) fn schema(id: &str) -> Value {
    let mut p =
        json!({"key":{"type":"string","minLength":1,"maxLength":512},"ref":crate::ref_property()});
    if id == crate::ids::CACHE_LIST {
        p["limit"] = json!({"type":"integer","minimum":1,"maximum":100});
        p["order"] = json!({"type":"string","enum":["asc","desc"]});
        p["sort"] =
            json!({"type":"string","enum":["last_accessed_at","created_at","size_in_bytes"]});
        p["_cliOutput"] = json!({"type":"string","maxLength":512});
    } else {
        p["cacheId"] = json!({"type":"integer","minimum":1});
        p["all"] = json!({"type":"boolean"});
        p["succeedOnNoCaches"] = json!({"type":"boolean"});
    }
    crate::repo_schema(p, &[])
}

pub(crate) fn unused(
    _: Value,
    _: &Base,
    _: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    Err(invalid_input())
}

fn all_bound(count: usize) -> Result<(), Outcome> {
    if count > 20 {
        Err(Outcome::Incomplete)
    } else {
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn native_bulk_delete_rejects_oversized_discovery_with_the_incomplete_variant() {
        assert!(matches!(
            super::all_bound(21),
            Err(super::Outcome::Incomplete)
        ));
    }
}
