use crate::action_writes::{self as writes, Outcome};
use crate::actions::{self, Reads, Workflow};
use crate::{
    Base, HttpError, Request, Response, error::ProviderError, invalid_input, invalid_response,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(crate) const FIELDS: &str = "id,name,path,state,dynamic";
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListInput {
    owner: String,
    repo: String,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    all: bool,
    #[serde(rename = "_cliOutput")]
    output: Option<String>,
}
fn default_limit() -> usize {
    50
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Input {
    owner: String,
    repo: String,
    workflow: Workflow,
    #[serde(rename = "ref")]
    reference: Option<String>,
    #[serde(default)]
    yaml: bool,
    #[serde(default)]
    inputs: Vec<InputField>,
    #[serde(default, rename = "_stdinInputs")]
    stdin_inputs: bool,
}
#[derive(Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct InputField {
    name: String,
    value: String,
}
struct StdinInputs(Vec<InputField>);
impl<'de> Deserialize<'de> for StdinInputs {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct InputsVisitor;
        impl<'de> serde::de::Visitor<'de> for InputsVisitor {
            type Value = StdinInputs;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an object with unique string input names")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut inputs: Vec<InputField> = Vec::new();
                while let Some((name, value)) = map.next_entry::<String, String>()? {
                    if inputs.len() >= 25 || inputs.iter().any(|field| field.name == name) {
                        return Err(serde::de::Error::custom(
                            "duplicate or excessive workflow inputs",
                        ));
                    }
                    inputs.push(InputField { name, value });
                }
                Ok(StdinInputs(inputs))
            }
        }
        deserializer.deserialize_map(InputsVisitor)
    }
}
fn input_map(inputs: &[InputField]) -> Result<BTreeMap<&str, &str>, ProviderError> {
    if inputs.len() > 25 {
        return Err(invalid_input());
    }
    let mut result = BTreeMap::new();
    for field in inputs {
        if field.name.is_empty()
            || field.name.len() > 128
            || !field
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            || field.value.len() > 4096
            || field
                .value
                .chars()
                .any(|c| c == '\0' || (c.is_control() && !"\n\r\t".contains(c)))
            || result
                .insert(field.name.as_str(), field.value.as_str())
                .is_some()
        {
            return Err(invalid_input());
        }
    }
    if serde_json::to_vec(&result)
        .map_err(|_| invalid_input())?
        .len()
        > 4096
    {
        return Err(invalid_input());
    }
    Ok(result)
}
impl Input {
    fn validate(&self, id: &str) -> Result<(), ProviderError> {
        crate::validate_login(&self.owner)?;
        crate::validate_repo(&self.repo)?;
        self.workflow.validate()?;
        if let Some(reference) = &self.reference {
            crate::validate_ref(reference)?;
        }
        input_map(&self.inputs)?;
        if (id != crate::ids::WORKFLOW_RUN && (!self.inputs.is_empty() || self.stdin_inputs))
            || (id != crate::ids::WORKFLOW_READ && self.yaml)
            || (id == crate::ids::WORKFLOW_READ && self.reference.is_some() && !self.yaml)
            || ((id == crate::ids::WORKFLOW_ENABLE || id == crate::ids::WORKFLOW_DISABLE)
                && self.reference.is_some())
            || (self.stdin_inputs && !self.inputs.is_empty())
        {
            return Err(invalid_input());
        }
        Ok(())
    }
}
pub(crate) fn validate(id: &str, input: Value) -> Result<(), ProviderError> {
    if id == crate::ids::WORKFLOW_LIST {
        let i: ListInput = serde_json::from_value(input).map_err(|_| invalid_input())?;
        crate::validate_login(&i.owner)?;
        crate::validate_repo(&i.repo)?;
        if !(1..=100).contains(&i.limit) {
            return Err(invalid_input());
        }
        actions::selection(i.output.as_deref(), FIELDS)
    } else {
        let i: Input = serde_json::from_value(input).map_err(|_| invalid_input())?;
        i.validate(id)
    }
}
fn row(value: &Value) -> Result<Value, ProviderError> {
    let state = value["state"].as_str().ok_or_else(invalid_response)?;
    if ![
        "active",
        "deleted",
        "disabled_fork",
        "disabled_inactivity",
        "disabled_manually",
    ]
    .contains(&state)
    {
        return Err(invalid_response());
    }
    let path = value["path"].as_str().ok_or_else(invalid_response)?;
    Ok(
        json!({"id":actions::positive(value,"id")?,"name":actions::text(value,"name",256)?,"path":actions::clean(path,1024),"state":state,"dynamic":path.starts_with("dynamic/")}),
    )
}
pub(crate) fn execute(
    id: &str,
    mut input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
    out: &mut dyn std::io::Write,
) -> Result<(), ProviderError> {
    validate(id, input.clone())?;
    if input["_stdinInputs"] == true {
        let mut reader = dekopon_provider_sdk::provider::stdin().ok_or_else(|| {
            ProviderError::new("usage", "gh workflow run --json: nothing was piped")
        })?;
        let mut bytes = Vec::new();
        use std::io::Read;
        reader
            .by_ref()
            .take(4097)
            .read_to_end(&mut bytes)
            .map_err(|_| invalid_input())?;
        if bytes.len() > 4096 {
            return Err(invalid_input());
        }
        let inputs: StdinInputs = serde_json::from_slice(&bytes).map_err(|_| invalid_input())?;
        let inputs = inputs.0;
        input_map(&inputs)?;
        input["inputs"] = json!(inputs);
        input
            .as_object_mut()
            .ok_or_else(invalid_input)?
            .remove("_stdinInputs");
    }
    let selection = input
        .get("_cliOutput")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let yaml = input["yaml"] == true;
    let result = run(id, input, base, send)?;
    let text = if yaml {
        result["yaml"]
            .as_str()
            .ok_or_else(invalid_response)?
            .to_owned()
            + "\n"
    } else if let Some(fields) = selection {
        if fields.is_empty() {
            result["workflows"]
                .as_array()
                .ok_or_else(invalid_response)?
                .iter()
                .map(|r| {
                    format!(
                        "{}\t{}\t{}\t{}\n",
                        r["id"],
                        r["name"].as_str().unwrap_or(""),
                        r["state"].as_str().unwrap_or(""),
                        r["path"].as_str().unwrap_or("")
                    )
                })
                .collect()
        } else {
            serde_json::to_string(&result).map_err(|_| invalid_response())? + "\n"
        }
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
pub(crate) fn run(
    id: &str,
    input: Value,
    base: &Base,
    send: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    validate(id, input.clone())?;
    if id == crate::ids::WORKFLOW_LIST {
        let input: ListInput = serde_json::from_value(input).map_err(|_| invalid_input())?;
        let mut reads = Reads {
            base,
            send,
            used: 0,
            max: 11,
            target_errors: true,
        };
        let root = actions::root(&input.owner, &input.repo);
        writes::repository(&mut reads, &root)?;
        let (rows, coverage) = actions::pages_with(
            &mut reads,
            &format!("{root}/actions/workflows?"),
            "workflows",
            10,
            input.limit,
            true,
            |v| {
                let row = row(v)?;
                Ok((input.all || row["state"] == "active").then_some(row))
            },
        )?;
        let rows = if let Some(fields) = input.output.filter(|s| !s.is_empty()) {
            rows.iter()
                .map(|r| crate::output::select(r, &fields, false))
                .collect()
        } else {
            rows
        };
        return Ok(json!({"workflows":rows,"coverage":coverage.json(),"requestsUsed":reads.used}));
    }
    let input: Input = serde_json::from_value(input).map_err(|_| invalid_input())?;
    if input.stdin_inputs {
        return Err(invalid_input());
    }
    let mut reads = Reads {
        base,
        send,
        used: 0,
        max: 3,
        target_errors: true,
    };
    let root = actions::root(&input.owner, &input.repo);
    let repository = writes::repository(&mut reads, &root)?;
    let raw = writes::get(
        &mut reads,
        &format!(
            "{root}/actions/workflows/{}",
            writes::workflow_path(&input.workflow)
        ),
        false,
    )?;
    let workflow = row(&raw)?;
    match &input.workflow {
        Workflow::Id(id) => {
            if workflow["id"] != *id {
                return Err(invalid_response());
            }
        }
        Workflow::File(file) => {
            if raw["path"] != format!(".github/workflows/{file}") {
                return Err(invalid_response());
            }
        }
    }
    let path = format!("{root}/actions/workflows/{}", workflow["id"]);
    if id == crate::ids::WORKFLOW_READ {
        let mut result = workflow;
        if input.yaml {
            let raw_path = raw["path"].as_str().ok_or_else(invalid_response)?;
            let file = raw_path
                .strip_prefix(".github/workflows/")
                .ok_or(Outcome::Conflict)?;
            Workflow::File(file.to_owned())
                .validate()
                .map_err(|_| ProviderError::from(Outcome::Conflict))?;
            let reference = input.reference.as_deref();
            let mut target = format!("{root}/contents/{}", crate::encode_path(raw_path));
            if let Some(reference) = reference {
                target.push_str(&format!("?ref={}", crate::percent_encode(reference)));
            }
            let content = writes::get(&mut reads, &target, false)?;
            use base64::Engine;
            if content["encoding"] != "base64"
                || content["type"] != "file"
                || content["path"] != raw["path"]
            {
                return Err(invalid_response());
            }
            let encoded = content["content"].as_str().ok_or_else(invalid_response)?;
            if encoded.len() > 3 * 1024 * 1024 {
                return Err(invalid_response());
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded.replace(['\n', '\r'], ""))
                .map_err(|_| invalid_response())?;
            let text = std::str::from_utf8(&bytes).map_err(|_| invalid_response())?;
            result["yaml"] = json!(crate::marked_text(
                &text
                    .chars()
                    .filter(|c| !c.is_control() || "\n\t".contains(*c))
                    .collect::<String>(),
                64 * 1024
            ));
            result["untrusted"] = json!(true);
            result["truncated"] = json!(text.len() > 64 * 1024);
        } else {
            let target = format!("{path}/runs?per_page=3&page=1");
            let raw = writes::get(&mut reads, &target, false)?;
            let runs = raw["workflow_runs"]
                .as_array()
                .ok_or_else(invalid_response)?;
            let total = raw["total_count"].as_u64().ok_or_else(invalid_response)?;
            let incomplete = runs.len() as u64 != total.min(3);
            let returned = runs.len().min(3);
            result["runs"] = json!(
                runs.iter()
                    .take(3)
                    .map(|v| actions::run_row(v, None, None))
                    .collect::<Result<Vec<_>, _>>()?
            );
            result["coverage"] = json!({"scope":"recent-runs","returnedCount":returned,"total_count":total,"incomplete":incomplete,"reasons":if incomplete {vec!["count-mismatch"]} else {vec![]},"notes":["recent-three-only"]});
        }
        result["requestsUsed"] = json!(reads.used);
        return Ok(result);
    }
    let active = workflow["state"] == "active";
    if (id == crate::ids::WORKFLOW_ENABLE && (active || workflow["state"] == "deleted"))
        || (id == crate::ids::WORKFLOW_DISABLE && !active)
        || (id == crate::ids::WORKFLOW_RUN && !active)
    {
        return Err(Outcome::Conflict.into());
    }
    let (method, suffix, body) = match id {
        crate::ids::WORKFLOW_ENABLE => ("PUT", "enable", None),
        crate::ids::WORKFLOW_DISABLE => ("PUT", "disable", None),
        crate::ids::WORKFLOW_RUN => {
            let reference = input
                .reference
                .or_else(|| repository["default_branch"].as_str().map(str::to_owned))
                .ok_or_else(invalid_response)?;
            crate::validate_ref(&reference).map_err(|_| invalid_response())?;
            (
                "POST",
                "dispatches",
                Some(json!({"ref":reference,"inputs":input_map(&input.inputs)?})),
            )
        }
        _ => return Err(invalid_input()),
    };
    writes::write(&mut reads, method, &format!("{path}/{suffix}"), body, 204)?;
    Ok(json!({"workflow":workflow,"outcome":"accepted","requestsUsed":reads.used}))
}
pub(crate) fn schema(id: &str) -> Value {
    let mut p = json!({});
    if id == crate::ids::WORKFLOW_LIST {
        p = json!({"limit":{"type":"integer","minimum":1,"maximum":100},"all":{"type":"boolean"},"_cliOutput":{"type":"string","maxLength":512}});
        return crate::repo_schema(p, &[]);
    }
    p["workflow"] =
        json!({"anyOf":[{"type":"integer","minimum":1},{"type":"string","maxLength":255}]});
    if id == crate::ids::WORKFLOW_READ || id == crate::ids::WORKFLOW_RUN {
        p["ref"] = crate::ref_property();
    }
    if id == crate::ids::WORKFLOW_READ {
        p["yaml"] = json!({"type":"boolean"});
    }
    if id == crate::ids::WORKFLOW_RUN {
        p["inputs"] = json!({"type":"array","maxItems":25,"items":{"type":"object","additionalProperties":false,"properties":{"name":{"type":"string","minLength":1,"maxLength":128},"value":{"type":"string","maxLength":4096}},"required":["name","value"]}});
        p["_stdinInputs"] = json!({"type":"boolean","const":true});
    }
    crate::repo_schema(p, &["workflow"])
}

pub(crate) fn unused(
    _: Value,
    _: &Base,
    _: &mut dyn FnMut(Request) -> Result<Response, HttpError>,
) -> Result<Value, ProviderError> {
    Err(invalid_input())
}
