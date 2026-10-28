use crate::{error::ProviderError, ids, invalid_input};
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::{Map, Value, json};
fn repo() -> Arg {
    Arg::new("repo").long("repo").short('R').required(true)
}
fn flag(name: &'static str, short: Option<char>) -> Arg {
    let a = Arg::new(name).long(name).action(ArgAction::SetTrue);
    if let Some(short) = short {
        a.short(short)
    } else {
        a
    }
}
fn value(name: &'static str, short: Option<char>) -> Arg {
    let a = Arg::new(name).long(name);
    if let Some(short) = short {
        a.short(short)
    } else {
        a
    }
}
fn json_arg() -> Arg {
    value("json", None)
        .num_args(0..=1)
        .default_missing_value("")
}
fn limit() -> Arg {
    value("limit", Some('L')).value_parser(clap::value_parser!(u64).range(1..=100))
}
fn workflow() -> Arg {
    Arg::new("workflow").required(true)
}
pub(crate) fn run_commands(command: Command) -> Command {
    let write = |verb| {
        Command::new(verb)
            .arg(repo())
            .arg(
                Arg::new("runId")
                    .required(true)
                    .value_parser(clap::value_parser!(u64).range(1..)),
            )
            .arg(value("match-head-commit", None))
    };
    command
        .subcommand(write("cancel").arg(flag("force", None)))
        .subcommand(write("delete"))
        .subcommand(
            write("rerun")
                .arg(flag("failed", None).conflicts_with("job"))
                .arg(value("job", Some('j')).value_parser(clap::value_parser!(u64).range(1..)))
                .arg(flag("debug", Some('d'))),
        )
}
pub(crate) fn workflow_commands() -> Command {
    Command::new("workflow")
        .about("List, inspect, dispatch and change workflow state")
        .subcommand_required(true)
        .subcommand(
            Command::new("list")
                .arg(repo())
                .arg(limit())
                .arg(flag("all", Some('a')))
                .arg(json_arg()),
        )
        .subcommand(
            Command::new("view")
                .arg(repo())
                .arg(workflow())
                .arg(value("ref", Some('r')))
                .arg(flag("yaml", Some('y'))),
        )
        .subcommand(Command::new("enable").arg(repo()).arg(workflow()))
        .subcommand(Command::new("disable").arg(repo()).arg(workflow()))
        .subcommand(
            Command::new("run")
                .arg(repo())
                .arg(workflow())
                .arg(value("ref", Some('r')))
                .arg(
                    value("field", Some('F'))
                        .action(ArgAction::Append)
                        .conflicts_with("json"),
                )
                .arg(
                    value("raw-field", Some('f'))
                        .action(ArgAction::Append)
                        .conflicts_with("json"),
                )
                .arg(flag("json", None)),
        )
}
pub(crate) fn cache_commands() -> Command {
    Command::new("cache")
        .about("Read and delete bounded Actions caches")
        .subcommand_required(true)
        .subcommand(
            Command::new("list")
                .arg(repo())
                .arg(limit())
                .arg(json_arg())
                .arg(value("order", Some('O')).value_parser(["asc", "desc"]))
                .arg(value("sort", Some('S')).value_parser([
                    "last_accessed_at",
                    "created_at",
                    "size_in_bytes",
                ]))
                .arg(value("key", Some('k')))
                .arg(value("ref", Some('r'))),
        )
        .subcommand(
            Command::new("delete")
                .arg(repo())
                .arg(
                    Arg::new("target")
                        .required_unless_present("all")
                        .conflicts_with("all"),
                )
                .arg(flag("all", Some('a')))
                .arg(value("ref", Some('r')))
                .arg(flag("succeed-on-no-caches", None)),
        )
}
pub(crate) fn dispatch(
    area: &str,
    verb: &str,
    m: &ArgMatches,
    input: &mut Map<String, Value>,
    stdin_piped: bool,
) -> Result<&'static str, ProviderError> {
    let id = match (area, verb) {
        ("run", "cancel") => {
            if m.get_flag("force") {
                ids::RUN_FORCE_CANCEL
            } else {
                ids::RUN_CANCEL
            }
        }
        ("run", "delete") => ids::RUN_DELETE,
        ("run", "rerun") => ids::RUN_RERUN,
        ("workflow", "list") => ids::WORKFLOW_LIST,
        ("workflow", "view") => ids::WORKFLOW_READ,
        ("workflow", "run") => ids::WORKFLOW_RUN,
        ("workflow", "enable") => ids::WORKFLOW_ENABLE,
        ("workflow", "disable") => ids::WORKFLOW_DISABLE,
        ("cache", "list") => ids::CACHE_LIST,
        ("cache", "delete") => ids::CACHE_DELETE,
        _ => return Err(invalid_input()),
    };
    if area == "run" {
        input.insert(
            "runId".into(),
            json!(m.get_one::<u64>("runId").ok_or_else(invalid_input)?),
        );
        if let Some(sha) = m.get_one::<String>("match-head-commit") {
            input.insert("expectedHeadSha".into(), json!(sha));
        }
        if verb == "rerun" {
            for name in ["failed", "debug"] {
                if m.get_flag(name) {
                    input.insert(name.into(), json!(true));
                }
            }
            if let Some(job) = m.get_one::<u64>("job") {
                input.insert("job".into(), json!(job));
            }
        }
        crate::action_writes::validate(id, json!(input))?;
    } else {
        if verb == "list" {
            input.insert(
                "limit".into(),
                json!(
                    m.get_one::<u64>("limit")
                        .copied()
                        .unwrap_or(if area == "workflow" { 50 } else { 30 })
                ),
            );
            if area == "workflow" {
                if m.get_flag("all") {
                    input.insert("all".into(), json!(true));
                }
            } else {
                for name in ["order", "sort", "key"] {
                    if let Some(v) = m.get_one::<String>(name) {
                        input.insert(name.into(), json!(v));
                    }
                }
            }
        } else if area == "workflow" {
            let workflow = m.get_one::<String>("workflow").ok_or_else(invalid_input)?;
            input.insert(
                "workflow".into(),
                workflow
                    .parse::<u64>()
                    .map_or_else(|_| json!(workflow), |n| json!(n)),
            );
            if verb == "view" && m.get_flag("yaml") {
                input.insert("yaml".into(), json!(true));
            }
            if verb == "run" {
                if m.get_flag("json") {
                    if !stdin_piped {
                        return Err(ProviderError::new(
                            "usage",
                            "gh workflow run --json requires piped inputs",
                        ));
                    }
                    input.insert("_stdinInputs".into(), json!(true));
                } else {
                    let mut inputs = Map::new();
                    for name in ["field", "raw-field"] {
                        if let Some(fields) = m.get_many::<String>(name) {
                            for field in fields {
                                let (key, value) =
                                    field.split_once('=').ok_or_else(invalid_input)?;
                                if (name == "field" && value.starts_with('@'))
                                    || inputs.insert(key.into(), json!(value)).is_some()
                                {
                                    return Err(invalid_input());
                                }
                            }
                        }
                    }
                    if !inputs.is_empty() {
                        input.insert(
                            "inputs".into(),
                            json!(
                                inputs
                                    .into_iter()
                                    .map(|(name, value)| json!({"name":name,"value":value}))
                                    .collect::<Vec<_>>()
                            ),
                        );
                    }
                }
            }
        } else {
            if m.get_flag("all") {
                input.insert("all".into(), json!(true));
            }
            if m.get_flag("succeed-on-no-caches") {
                input.insert("succeedOnNoCaches".into(), json!(true));
            }
            if let Some(target) = m.get_one::<String>("target") {
                if target.bytes().all(|b| b.is_ascii_digit()) {
                    input.insert(
                        "cacheId".into(),
                        json!(target.parse::<u64>().map_err(|_| invalid_input())?),
                    );
                } else {
                    input.insert("key".into(), json!(target));
                }
            }
        }
        if m.try_get_one::<String>("ref").ok().flatten().is_some() {
            input.insert(
                "ref".into(),
                json!(m.get_one::<String>("ref").ok_or_else(invalid_input)?),
            );
        }
        if area == "workflow" {
            crate::workflows::validate(id, json!(input))?;
        } else {
            crate::caches::validate(id, json!(input))?;
        }
    }
    Ok(id)
}
