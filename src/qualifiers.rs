use crate::{error::ProviderError, invalid_input};
use clap::{Arg, ArgAction, ArgMatches};
use serde_json::{Map, Value, json};

pub(crate) fn free_text(value: &str) -> Result<(), ProviderError> {
    if value.is_empty()
        || value
            .chars()
            .any(|c| c == '"' || c == '\\' || c.is_control())
    {
        return Err(invalid_input());
    }
    Ok(())
}

pub(crate) fn repo_args() -> Vec<Arg> {
    let mut args = [
        "source",
        "fork",
        "archived",
        "no-archived",
        "private",
        "public",
    ]
    .into_iter()
    .map(|name| Arg::new(name).long(name).action(ArgAction::SetTrue))
    .collect::<Vec<_>>();
    args.push(Arg::new("language").long("language").short('l'));
    args.push(
        Arg::new("topic")
            .long("topic")
            .action(ArgAction::Append)
            .value_delimiter(','),
    );
    args.push(
        Arg::new("visibility")
            .long("visibility")
            .value_name("VALUE")
            .value_parser(["public", "private", "internal"]),
    );
    args
}

pub(crate) fn repo_dispatch(
    matches: &ArgMatches,
    input: &mut Map<String, Value>,
) -> Result<(), ProviderError> {
    for (a, b) in [
        ("source", "fork"),
        ("archived", "no-archived"),
        ("private", "public"),
    ] {
        if matches.get_flag(a) && matches.get_flag(b) {
            return Err(ProviderError::new(
                "usage",
                format!("gh repo list: --{a} and --{b} cannot be used together"),
            ));
        }
    }
    for flag in ["private", "public"] {
        if matches.contains_id("visibility") && matches.get_flag(flag) {
            return Err(ProviderError::new(
                "usage",
                format!("gh repo list: --visibility and --{flag} cannot be used together"),
            ));
        }
    }
    for (flag, field) in [
        ("source", "source"),
        ("fork", "fork"),
        ("archived", "archived"),
        ("no-archived", "noArchived"),
        ("private", "private"),
        ("public", "public"),
    ] {
        if matches.get_flag(flag) {
            input.insert(field.into(), json!(true));
        }
    }
    for flag in ["language", "visibility"] {
        if let Some(value) = matches.get_one::<String>(flag) {
            free_text(value).map_err(|_| {
                ProviderError::new("usage", format!("gh repo list --{flag}: invalid value"))
            })?;
            input.insert(flag.into(), json!(value));
        }
    }
    if let Some(values) = matches.get_many::<String>("topic") {
        let values = values.cloned().collect::<Vec<_>>();
        for value in &values {
            free_text(value)
                .map_err(|_| ProviderError::new("usage", "gh repo list --topic: invalid value"))?;
        }
        input.insert("topic".into(), json!(values));
    }
    Ok(())
}
