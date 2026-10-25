use crate::error::ProviderError;
use crate::{invalid_input, invalid_response};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Input {
    pub owner: String,
    pub repo: String,
    pub number: u32,
    #[serde(default)]
    pub patch: bool,
    #[serde(default)]
    pub name_only: bool,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default, rename = "allowEscapeSequences")]
    pub _allow_escape_sequences: bool,
    #[serde(default, rename = "_cli")]
    pub _cli: Option<String>,
}

pub(crate) fn patterns(values: &[String]) -> Result<Vec<glob::Pattern>, ProviderError> {
    values
        .iter()
        .map(|value| {
            if value.is_empty() || value.len() > 1024 || value.chars().any(char::is_control) {
                return Err(invalid_input());
            }
            glob::Pattern::new(value).map_err(|_| invalid_input())
        })
        .collect()
}

pub(crate) fn transform(
    text: &str,
    name_only: bool,
    patterns: &[glob::Pattern],
) -> Result<String, ProviderError> {
    let header = regex::Regex::new(r#"(?:^|\n)diff\s--git.*\s("?)b/(.*)"#)
        .map_err(|_| invalid_response())?;
    let options = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: true,
        require_literal_leading_dot: false,
    };
    let mut output = String::new();
    for (index, section) in text.split("\ndiff --git ").enumerate() {
        let section = if index == 0 {
            section.to_owned()
        } else {
            format!("diff --git {section}")
        };
        let name = header
            .captures(&section)
            .map(|capture| format!("{}{}", &capture[1], &capture[2]).trim().to_owned());
        if let Some(name) = &name {
            let basename = name.rsplit('/').next().unwrap_or(name);
            if patterns.iter().any(|pattern| {
                pattern.matches_with(name, options) || pattern.matches_with(basename, options)
            }) {
                continue;
            }
        }
        if name_only {
            if let Some(name) = name {
                output.push_str(&name);
                output.push('\n');
            }
        } else {
            if index != 0 && !output.is_empty() && !output.ends_with('\n') {
                output.push('\n');
            }
            output.push_str(&section);
        }
    }
    Ok(output)
}
