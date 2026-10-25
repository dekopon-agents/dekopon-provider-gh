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

fn class_char(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Result<char, ProviderError> {
    match chars.next() {
        Some('\\') => chars.next().ok_or_else(invalid_input),
        Some('-' | ']') | None => Err(invalid_input()),
        Some(value) => Ok(value),
    }
}

fn pattern(value: &str) -> Result<regex::Regex, ProviderError> {
    if value.is_empty() || value.len() > 1024 || value.chars().any(char::is_control) {
        return Err(invalid_input());
    }
    let mut expression = String::from("\\A(?:");
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '*' => expression.push_str("[^/]*"),
            '?' => expression.push_str("[^/]"),
            '\\' => expression.push_str(&regex::escape(
                &chars.next().ok_or_else(invalid_input)?.to_string(),
            )),
            '[' => {
                expression.push('[');
                if chars.peek() == Some(&'^') {
                    chars.next();
                    expression.push('^');
                }
                let mut count = 0;
                while chars.peek() != Some(&']') {
                    let lo = class_char(&mut chars)?;
                    expression.push_str(&regex::escape(&lo.to_string()));
                    if chars.peek() == Some(&'-') {
                        chars.next();
                        let hi = class_char(&mut chars)?;
                        if lo > hi {
                            return Err(invalid_input());
                        }
                        expression.push('-');
                        expression.push_str(&regex::escape(&hi.to_string()));
                    }
                    count += 1;
                }
                if count == 0 {
                    return Err(invalid_input());
                }
                chars.next();
                expression.push(']');
            }
            other => expression.push_str(&regex::escape(&other.to_string())),
        }
    }
    expression.push_str(")\\z");
    regex::Regex::new(&expression).map_err(|_| invalid_input())
}

pub(crate) fn patterns(values: &[String]) -> Result<Vec<regex::Regex>, ProviderError> {
    values.iter().map(|value| pattern(value)).collect()
}

pub(crate) fn transform(
    text: &str,
    name_only: bool,
    patterns: &[regex::Regex],
) -> Result<String, ProviderError> {
    let header = regex::Regex::new(r#"(?:^|\n)diff\s--git.*\s("?)b/(.*)"#)
        .map_err(|_| invalid_response())?;
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
            if patterns
                .iter()
                .any(|pattern| pattern.is_match(name) || pattern.is_match(basename))
            {
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
