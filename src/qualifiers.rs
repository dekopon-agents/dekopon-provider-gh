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
    .map(|name| {
        Arg::new(name)
            .long(name)
            .num_args(0..=1)
            .default_missing_value("true")
            .default_value("false")
            .require_equals(true)
            .value_parser(clap::value_parser!(bool))
    })
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

#[derive(Clone, Copy)]
enum Kind {
    Text,
    Login,
    Count,
    Date,
    Sha,
    Ref,
    Team,
    Project,
    Choice(&'static [&'static str]),
    Bool,
    NullableBool,
    Locked,
    Merged,
    Missing,
    Reviewer,
    App,
    Search,
}

#[derive(Clone, Copy)]
pub(crate) struct Field {
    flag: &'static str,
    property: &'static str,
    term: &'static str,
    kind: Kind,
    repeat: bool,
}

pub(crate) fn fields(command: &str) -> &'static [Field] {
    match command {
        "search issues" => &[
            Field {
                flag: "order",
                property: "order",
                term: "",
                kind: Kind::Choice(&["asc", "desc"]),
                repeat: false,
            },
            Field {
                flag: "sort",
                property: "sort",
                term: "",
                kind: Kind::Choice(&[
                    "comments",
                    "created",
                    "interactions",
                    "reactions",
                    "reactions-+1",
                    "reactions--1",
                    "reactions-smile",
                    "reactions-thinking_face",
                    "reactions-heart",
                    "reactions-tada",
                    "updated",
                ]),
                repeat: false,
            },
            Field {
                flag: "search-type",
                property: "searchType",
                term: "",
                kind: Kind::Choice(&["lexical", "semantic", "hybrid"]),
                repeat: false,
            },
            Field {
                flag: "include-prs",
                property: "includePrs",
                term: "",
                kind: Kind::Bool,
                repeat: false,
            },
            Field {
                flag: "app",
                property: "app",
                term: "author",
                kind: Kind::App,
                repeat: false,
            },
            Field {
                flag: "archived",
                property: "archived",
                term: "archived",
                kind: Kind::NullableBool,
                repeat: false,
            },
            Field {
                flag: "assignee",
                property: "assignee",
                term: "assignee",
                kind: Kind::Login,
                repeat: false,
            },
            Field {
                flag: "closed",
                property: "closed",
                term: "closed",
                kind: Kind::Date,
                repeat: false,
            },
            Field {
                flag: "commenter",
                property: "commenter",
                term: "commenter",
                kind: Kind::Login,
                repeat: false,
            },
            Field {
                flag: "comments",
                property: "comments",
                term: "comments",
                kind: Kind::Count,
                repeat: false,
            },
            Field {
                flag: "created",
                property: "created",
                term: "created",
                kind: Kind::Date,
                repeat: false,
            },
            Field {
                flag: "match",
                property: "match",
                term: "in",
                kind: Kind::Choice(&["title", "body", "comments"]),
                repeat: true,
            },
            Field {
                flag: "interactions",
                property: "interactions",
                term: "interactions",
                kind: Kind::Count,
                repeat: false,
            },
            Field {
                flag: "involves",
                property: "involves",
                term: "involves",
                kind: Kind::Login,
                repeat: false,
            },
            Field {
                flag: "visibility",
                property: "visibility",
                term: "is",
                kind: Kind::Choice(&["public", "private", "internal"]),
                repeat: true,
            },
            Field {
                flag: "label",
                property: "label",
                term: "label",
                kind: Kind::Text,
                repeat: true,
            },
            Field {
                flag: "language",
                property: "language",
                term: "language",
                kind: Kind::Text,
                repeat: false,
            },
            Field {
                flag: "locked",
                property: "locked",
                term: "is",
                kind: Kind::Locked,
                repeat: false,
            },
            Field {
                flag: "mentions",
                property: "mentions",
                term: "mentions",
                kind: Kind::Login,
                repeat: false,
            },
            Field {
                flag: "milestone",
                property: "milestone",
                term: "milestone",
                kind: Kind::Text,
                repeat: false,
            },
            Field {
                flag: "no-assignee",
                property: "noAssignee",
                term: "no",
                kind: Kind::Missing,
                repeat: false,
            },
            Field {
                flag: "no-label",
                property: "noLabel",
                term: "no",
                kind: Kind::Missing,
                repeat: false,
            },
            Field {
                flag: "no-milestone",
                property: "noMilestone",
                term: "no",
                kind: Kind::Missing,
                repeat: false,
            },
            Field {
                flag: "no-project",
                property: "noProject",
                term: "no",
                kind: Kind::Missing,
                repeat: false,
            },
            Field {
                flag: "project",
                property: "project",
                term: "project",
                kind: Kind::Project,
                repeat: false,
            },
            Field {
                flag: "reactions",
                property: "reactions",
                term: "reactions",
                kind: Kind::Count,
                repeat: false,
            },
            Field {
                flag: "team-mentions",
                property: "teamMentions",
                term: "team",
                kind: Kind::Team,
                repeat: false,
            },
            Field {
                flag: "updated",
                property: "updated",
                term: "updated",
                kind: Kind::Date,
                repeat: false,
            },
        ],
        "search prs" => &[
            Field {
                flag: "order",
                property: "order",
                term: "",
                kind: Kind::Choice(&["asc", "desc"]),
                repeat: false,
            },
            Field {
                flag: "sort",
                property: "sort",
                term: "",
                kind: Kind::Choice(&[
                    "comments",
                    "created",
                    "interactions",
                    "reactions",
                    "reactions-+1",
                    "reactions--1",
                    "reactions-smile",
                    "reactions-thinking_face",
                    "reactions-heart",
                    "reactions-tada",
                    "updated",
                ]),
                repeat: false,
            },
            Field {
                flag: "app",
                property: "app",
                term: "author",
                kind: Kind::App,
                repeat: false,
            },
            Field {
                flag: "archived",
                property: "archived",
                term: "archived",
                kind: Kind::NullableBool,
                repeat: false,
            },
            Field {
                flag: "assignee",
                property: "assignee",
                term: "assignee",
                kind: Kind::Login,
                repeat: false,
            },
            Field {
                flag: "closed",
                property: "closed",
                term: "closed",
                kind: Kind::Date,
                repeat: false,
            },
            Field {
                flag: "commenter",
                property: "commenter",
                term: "commenter",
                kind: Kind::Login,
                repeat: false,
            },
            Field {
                flag: "comments",
                property: "comments",
                term: "comments",
                kind: Kind::Count,
                repeat: false,
            },
            Field {
                flag: "created",
                property: "created",
                term: "created",
                kind: Kind::Date,
                repeat: false,
            },
            Field {
                flag: "match",
                property: "match",
                term: "in",
                kind: Kind::Choice(&["title", "body", "comments"]),
                repeat: true,
            },
            Field {
                flag: "interactions",
                property: "interactions",
                term: "interactions",
                kind: Kind::Count,
                repeat: false,
            },
            Field {
                flag: "involves",
                property: "involves",
                term: "involves",
                kind: Kind::Login,
                repeat: false,
            },
            Field {
                flag: "visibility",
                property: "visibility",
                term: "is",
                kind: Kind::Choice(&["public", "private", "internal"]),
                repeat: true,
            },
            Field {
                flag: "label",
                property: "label",
                term: "label",
                kind: Kind::Text,
                repeat: true,
            },
            Field {
                flag: "language",
                property: "language",
                term: "language",
                kind: Kind::Text,
                repeat: false,
            },
            Field {
                flag: "locked",
                property: "locked",
                term: "is",
                kind: Kind::Locked,
                repeat: false,
            },
            Field {
                flag: "mentions",
                property: "mentions",
                term: "mentions",
                kind: Kind::Login,
                repeat: false,
            },
            Field {
                flag: "milestone",
                property: "milestone",
                term: "milestone",
                kind: Kind::Text,
                repeat: false,
            },
            Field {
                flag: "no-assignee",
                property: "noAssignee",
                term: "no",
                kind: Kind::Missing,
                repeat: false,
            },
            Field {
                flag: "no-label",
                property: "noLabel",
                term: "no",
                kind: Kind::Missing,
                repeat: false,
            },
            Field {
                flag: "no-milestone",
                property: "noMilestone",
                term: "no",
                kind: Kind::Missing,
                repeat: false,
            },
            Field {
                flag: "no-project",
                property: "noProject",
                term: "no",
                kind: Kind::Missing,
                repeat: false,
            },
            Field {
                flag: "project",
                property: "project",
                term: "project",
                kind: Kind::Project,
                repeat: false,
            },
            Field {
                flag: "reactions",
                property: "reactions",
                term: "reactions",
                kind: Kind::Count,
                repeat: false,
            },
            Field {
                flag: "team-mentions",
                property: "teamMentions",
                term: "team",
                kind: Kind::Team,
                repeat: false,
            },
            Field {
                flag: "updated",
                property: "updated",
                term: "updated",
                kind: Kind::Date,
                repeat: false,
            },
            Field {
                flag: "base",
                property: "base",
                term: "base",
                kind: Kind::Ref,
                repeat: false,
            },
            Field {
                flag: "draft",
                property: "draft",
                term: "draft",
                kind: Kind::NullableBool,
                repeat: false,
            },
            Field {
                flag: "head",
                property: "head",
                term: "head",
                kind: Kind::Ref,
                repeat: false,
            },
            Field {
                flag: "merged-at",
                property: "mergedAt",
                term: "merged",
                kind: Kind::Date,
                repeat: false,
            },
            Field {
                flag: "merged",
                property: "merged",
                term: "is",
                kind: Kind::Merged,
                repeat: false,
            },
            Field {
                flag: "review",
                property: "review",
                term: "review",
                kind: Kind::Choice(&["none", "required", "approved", "changes_requested"]),
                repeat: false,
            },
            Field {
                flag: "review-requested",
                property: "reviewRequested",
                term: "review-requested",
                kind: Kind::Reviewer,
                repeat: false,
            },
            Field {
                flag: "reviewed-by",
                property: "reviewedBy",
                term: "reviewed-by",
                kind: Kind::Login,
                repeat: false,
            },
            Field {
                flag: "checks",
                property: "checks",
                term: "status",
                kind: Kind::Choice(&["pending", "success", "failure"]),
                repeat: false,
            },
        ],
        "search repos" => &[
            Field {
                flag: "order",
                property: "order",
                term: "",
                kind: Kind::Choice(&["asc", "desc"]),
                repeat: false,
            },
            Field {
                flag: "sort",
                property: "sort",
                term: "",
                kind: Kind::Choice(&["forks", "help-wanted-issues", "stars", "updated"]),
                repeat: false,
            },
            Field {
                flag: "archived",
                property: "archived",
                term: "archived",
                kind: Kind::NullableBool,
                repeat: false,
            },
            Field {
                flag: "created",
                property: "created",
                term: "created",
                kind: Kind::Date,
                repeat: false,
            },
            Field {
                flag: "followers",
                property: "followers",
                term: "followers",
                kind: Kind::Count,
                repeat: false,
            },
            Field {
                flag: "include-forks",
                property: "includeForks",
                term: "fork",
                kind: Kind::Choice(&["false", "true", "only"]),
                repeat: false,
            },
            Field {
                flag: "forks",
                property: "forks",
                term: "forks",
                kind: Kind::Count,
                repeat: false,
            },
            Field {
                flag: "good-first-issues",
                property: "goodFirstIssues",
                term: "good-first-issues",
                kind: Kind::Count,
                repeat: false,
            },
            Field {
                flag: "help-wanted-issues",
                property: "helpWantedIssues",
                term: "help-wanted-issues",
                kind: Kind::Count,
                repeat: false,
            },
            Field {
                flag: "match",
                property: "match",
                term: "in",
                kind: Kind::Choice(&["name", "description", "readme"]),
                repeat: true,
            },
            Field {
                flag: "visibility",
                property: "visibility",
                term: "is",
                kind: Kind::Choice(&["public", "private", "internal"]),
                repeat: true,
            },
            Field {
                flag: "language",
                property: "language",
                term: "language",
                kind: Kind::Text,
                repeat: false,
            },
            Field {
                flag: "license",
                property: "license",
                term: "license",
                kind: Kind::Text,
                repeat: true,
            },
            Field {
                flag: "updated",
                property: "updated",
                term: "pushed",
                kind: Kind::Date,
                repeat: false,
            },
            Field {
                flag: "size",
                property: "size",
                term: "size",
                kind: Kind::Count,
                repeat: false,
            },
            Field {
                flag: "stars",
                property: "stars",
                term: "stars",
                kind: Kind::Count,
                repeat: false,
            },
            Field {
                flag: "topic",
                property: "topic",
                term: "topic",
                kind: Kind::Text,
                repeat: true,
            },
            Field {
                flag: "number-topics",
                property: "numberTopics",
                term: "topics",
                kind: Kind::Count,
                repeat: false,
            },
        ],
        "search code" => &[
            Field {
                flag: "extension",
                property: "extension",
                term: "extension",
                kind: Kind::Text,
                repeat: false,
            },
            Field {
                flag: "filename",
                property: "filename",
                term: "filename",
                kind: Kind::Text,
                repeat: false,
            },
            Field {
                flag: "match",
                property: "match",
                term: "in",
                kind: Kind::Choice(&["file", "path"]),
                repeat: true,
            },
            Field {
                flag: "language",
                property: "language",
                term: "language",
                kind: Kind::Text,
                repeat: false,
            },
            Field {
                flag: "size",
                property: "size",
                term: "size",
                kind: Kind::Count,
                repeat: false,
            },
        ],
        "search commits" => &[
            Field {
                flag: "order",
                property: "order",
                term: "",
                kind: Kind::Choice(&["asc", "desc"]),
                repeat: false,
            },
            Field {
                flag: "sort",
                property: "sort",
                term: "",
                kind: Kind::Choice(&["best-match", "author-date", "committer-date"]),
                repeat: false,
            },
            Field {
                flag: "author-date",
                property: "authorDate",
                term: "author-date",
                kind: Kind::Date,
                repeat: false,
            },
            Field {
                flag: "author-email",
                property: "authorEmail",
                term: "author-email",
                kind: Kind::Text,
                repeat: false,
            },
            Field {
                flag: "author-name",
                property: "authorName",
                term: "author-name",
                kind: Kind::Text,
                repeat: false,
            },
            Field {
                flag: "committer",
                property: "committer",
                term: "committer",
                kind: Kind::Login,
                repeat: false,
            },
            Field {
                flag: "committer-date",
                property: "committerDate",
                term: "committer-date",
                kind: Kind::Date,
                repeat: false,
            },
            Field {
                flag: "committer-email",
                property: "committerEmail",
                term: "committer-email",
                kind: Kind::Text,
                repeat: false,
            },
            Field {
                flag: "committer-name",
                property: "committerName",
                term: "committer-name",
                kind: Kind::Text,
                repeat: false,
            },
            Field {
                flag: "hash",
                property: "hash",
                term: "hash",
                kind: Kind::Sha,
                repeat: false,
            },
            Field {
                flag: "merge",
                property: "merge",
                term: "merge",
                kind: Kind::NullableBool,
                repeat: false,
            },
            Field {
                flag: "parent",
                property: "parent",
                term: "parent",
                kind: Kind::Sha,
                repeat: false,
            },
            Field {
                flag: "tree",
                property: "tree",
                term: "tree",
                kind: Kind::Sha,
                repeat: false,
            },
            Field {
                flag: "visibility",
                property: "visibility",
                term: "is",
                kind: Kind::Choice(&["public", "private", "internal"]),
                repeat: true,
            },
        ],
        "pr list" => &[
            Field {
                flag: "base",
                property: "base",
                term: "base",
                kind: Kind::Ref,
                repeat: false,
            },
            Field {
                flag: "head",
                property: "head",
                term: "head",
                kind: Kind::Ref,
                repeat: false,
            },
            Field {
                flag: "label",
                property: "label",
                term: "label",
                kind: Kind::Text,
                repeat: true,
            },
            Field {
                flag: "assignee",
                property: "assignee",
                term: "assignee",
                kind: Kind::Login,
                repeat: false,
            },
            Field {
                flag: "search",
                property: "search",
                term: "",
                kind: Kind::Search,
                repeat: false,
            },
            Field {
                flag: "draft",
                property: "draft",
                term: "draft",
                kind: Kind::NullableBool,
                repeat: false,
            },
            Field {
                flag: "app",
                property: "app",
                term: "author",
                kind: Kind::App,
                repeat: false,
            },
        ],
        "issue list" => &[
            Field {
                flag: "assignee",
                property: "assignee",
                term: "assignee",
                kind: Kind::Login,
                repeat: false,
            },
            Field {
                flag: "label",
                property: "label",
                term: "label",
                kind: Kind::Text,
                repeat: true,
            },
            Field {
                flag: "author",
                property: "author",
                term: "author",
                kind: Kind::Login,
                repeat: false,
            },
            Field {
                flag: "mention",
                property: "mention",
                term: "mentions",
                kind: Kind::Login,
                repeat: false,
            },
            Field {
                flag: "milestone",
                property: "milestone",
                term: "milestone",
                kind: Kind::Text,
                repeat: false,
            },
            Field {
                flag: "app",
                property: "app",
                term: "author",
                kind: Kind::App,
                repeat: false,
            },
            Field {
                flag: "search",
                property: "search",
                term: "",
                kind: Kind::Search,
                repeat: false,
            },
            Field {
                flag: "type",
                property: "type",
                term: "type",
                kind: Kind::Text,
                repeat: false,
            },
        ],
        _ => &[],
    }
}

pub(crate) fn args(command: &str) -> Vec<Arg> {
    fields(command)
        .iter()
        .map(|field| {
            let mut arg = Arg::new(field.flag).long(field.flag).value_name("VALUE");
            arg = match field.kind {
                Kind::Bool | Kind::Locked | Kind::Merged | Kind::Missing => arg
                    .num_args(0..=1)
                    .default_missing_value("true")
                    .default_value("false")
                    .require_equals(true)
                    .value_parser(clap::value_parser!(bool)),
                Kind::NullableBool => arg
                    .num_args(0..=1)
                    .default_missing_value("true")
                    .default_value("false")
                    .require_equals(true)
                    .value_parser(clap::value_parser!(bool)),
                Kind::Choice(values) => arg.value_parser(values.to_vec()),
                Kind::Text
                | Kind::Login
                | Kind::Count
                | Kind::Date
                | Kind::Sha
                | Kind::Ref
                | Kind::Team
                | Kind::Project
                | Kind::Reviewer
                | Kind::App
                | Kind::Search => arg,
            };
            if field.repeat {
                arg = arg.action(ArgAction::Append).value_delimiter(',');
            }
            if command == "pr list" || command == "issue list" {
                arg = match field.flag {
                    "base" => arg.short('B'),
                    "head" => arg.short('H'),
                    "label" => arg.short('l'),
                    "assignee" => arg.short('a'),
                    "search" => arg.short('S'),
                    "draft" => arg.short('d'),
                    "author" => arg.short('A'),
                    "milestone" => arg.short('m'),
                    _ => arg,
                };
            } else if command == "search prs" {
                arg = match field.flag {
                    "base" => arg.short('B'),
                    "head" => arg.short('H'),
                    _ => arg,
                };
            }
            arg
        })
        .collect()
}

pub(crate) enum ValidationError {
    Value(&'static str),
    Scope,
    Conflict(&'static str, &'static str),
}

pub(crate) fn dispatch(
    command: &str,
    matches: &ArgMatches,
    input: &mut Map<String, Value>,
) -> Result<(), ProviderError> {
    for field in fields(command) {
        if !matches
            .value_source(field.flag)
            .is_some_and(|source| source == clap::parser::ValueSource::CommandLine)
        {
            continue;
        }
        let value = match field.kind {
            Kind::Bool | Kind::Locked | Kind::Merged | Kind::Missing | Kind::NullableBool => json!(
                matches
                    .get_one::<bool>(field.flag)
                    .copied()
                    .unwrap_or(false)
            ),
            Kind::Text
            | Kind::Login
            | Kind::Count
            | Kind::Date
            | Kind::Sha
            | Kind::Ref
            | Kind::Team
            | Kind::Project
            | Kind::Choice(_)
            | Kind::Reviewer
            | Kind::App
            | Kind::Search => {
                if field.repeat {
                    json!(
                        matches
                            .get_many::<String>(field.flag)
                            .expect("supplied values")
                            .cloned()
                            .collect::<Vec<_>>()
                    )
                } else {
                    json!(
                        matches
                            .get_one::<String>(field.flag)
                            .expect("supplied value")
                    )
                }
            }
        };
        input.insert(field.property.into(), value);
    }
    let value = Value::Object(input.clone());
    validate(command, &value).map_err(|error| match error {
        ValidationError::Scope => ProviderError::new(
            "usage",
            format!("gh {command} --search: query cannot change the structural scope"),
        ),
        ValidationError::Conflict(a, b) => ProviderError::new(
            "usage",
            format!("gh {command}: --{a} and --{b} cannot be used together"),
        ),
        ValidationError::Value(flag) => {
            ProviderError::new("usage", format!("gh {command} --{flag}: invalid value"))
        }
    })?;
    Ok(())
}

pub(crate) fn scope_word(text: &str) -> bool {
    text.split_whitespace().any(|word| {
        let word = word.to_ascii_lowercase();
        word == "or"
            || ["repo:", "org:", "user:", "owner:"]
                .iter()
                .any(|scope| word.contains(scope))
    })
}

fn atom(value: &str, date: bool) -> bool {
    if !date {
        return !value.is_empty()
            && value.bytes().all(|b| b.is_ascii_digit())
            && value.parse::<u64>().is_ok();
    }
    if value.len() == 10 {
        let Ok(format) = time::format_description::parse_borrowed::<2>("[year]-[month]-[day]")
        else {
            return false;
        };
        return time::Date::parse(value, &format).is_ok();
    }
    time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).is_ok()
}

fn comparison(value: &str, date: bool) -> bool {
    if let Some((a, b)) = value.split_once("..") {
        return (a != "*" || b != "*")
            && (a == "*" || atom(a, date))
            && (b == "*" || atom(b, date));
    }
    let value = [">=", "<=", ">", "<"]
        .iter()
        .find_map(|prefix| value.strip_prefix(prefix))
        .unwrap_or(value);
    atom(value, date)
}

fn token(value: &str, kind: Kind) -> bool {
    match kind {
        Kind::Text | Kind::App => free_text(value).is_ok(),
        Kind::Login => crate::validate_login(value).is_ok(),
        Kind::Count => comparison(value, false),
        Kind::Date => comparison(value, true),
        Kind::Sha => {
            (4..=40).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_hexdigit())
        }
        Kind::Ref => crate::validate_ref(value).is_ok() && free_text(value).is_ok(),
        Kind::Team => value.split_once('/').is_some_and(|(owner, team)| {
            crate::validate_login(owner).is_ok() && crate::validate_login(team).is_ok()
        }),
        Kind::Reviewer => {
            if value.contains('/') {
                token(value, Kind::Team)
            } else {
                crate::validate_login(value).is_ok()
            }
        }
        Kind::Project => value.split_once('/').is_some_and(|(owner, n)| {
            crate::validate_login(owner).is_ok()
                && atom(n, false)
                && n.parse::<u64>().is_ok_and(|n| n > 0)
        }),
        Kind::Choice(values) => values.contains(&value),
        Kind::Search => value.len() <= 512 && !value.chars().any(char::is_control),
        Kind::Bool | Kind::NullableBool | Kind::Locked | Kind::Merged | Kind::Missing => false,
    }
}

pub(crate) fn validate(command: &str, input: &Value) -> Result<(), ValidationError> {
    if input.get("author").is_some_and(|v| !v.is_null())
        && input.get("app").is_some_and(|v| !v.is_null())
    {
        return Err(ValidationError::Conflict("author", "app"));
    }
    if command == "search issues"
        && input["searchType"]
            .as_str()
            .is_some_and(|kind| kind == "semantic" || kind == "hybrid")
    {
        for (flag, property) in [
            ("include-prs", "includePrs"),
            ("sort", "sort"),
            ("order", "order"),
        ] {
            let present = if flag == "include-prs" {
                input[property] == true
            } else {
                input.get(property).is_some_and(|v| !v.is_null())
            };
            if present {
                return Err(ValidationError::Conflict("search-type", flag));
            }
        }
    }
    let mut bytes = input["query"].as_str().unwrap_or("").len();
    for field in fields(command) {
        let Some(value) = input.get(field.property).filter(|v| !v.is_null()) else {
            continue;
        };
        match field.kind {
            Kind::Bool | Kind::NullableBool | Kind::Locked | Kind::Merged | Kind::Missing => {
                if !value.is_boolean() {
                    return Err(ValidationError::Value(field.flag));
                }
            }
            Kind::Text
            | Kind::Login
            | Kind::Count
            | Kind::Date
            | Kind::Sha
            | Kind::Ref
            | Kind::Team
            | Kind::Project
            | Kind::Choice(_)
            | Kind::Reviewer
            | Kind::App
            | Kind::Search => {
                let values = if field.repeat {
                    value
                        .as_array()
                        .ok_or(ValidationError::Value(field.flag))?
                        .iter()
                        .collect::<Vec<_>>()
                } else {
                    vec![value]
                };
                if values.is_empty() {
                    return Err(ValidationError::Value(field.flag));
                }
                for value in values {
                    let text = value.as_str().ok_or(ValidationError::Value(field.flag))?;
                    if !token(text, field.kind) {
                        return Err(ValidationError::Value(field.flag));
                    }
                }
            }
        }
        if field.flag == "search" {
            bytes += value.as_str().unwrap_or("").len();
        }
    }
    if input["search"].as_str().is_some_and(scope_word) {
        return Err(ValidationError::Scope);
    }
    let terms = terms(command, input);
    let list = command == "pr list" || command == "issue list";
    let list_search = list
        && (input.get("search").is_some_and(|v| !v.is_null())
            || input.get("app").is_some_and(|v| !v.is_null())
            || input["milestone"]
                .as_str()
                .is_some_and(|value| value.parse::<u64>().is_err()));
    if list && !list_search {
        return Ok(());
    }
    if list_search || !terms.is_empty() {
        if let Some(state) = input["state"].as_str().filter(|state| *state != "all") {
            let prefix = if list { "is" } else { "state" };
            bytes += prefix.len() + 1 + state.len() + usize::from(bytes != 0);
        }
        if command != "issue list"
            && let Some(author) = input["author"].as_str()
        {
            bytes += "author:".len() + author.len() + usize::from(bytes != 0);
        }
    }
    for term in terms {
        bytes += term.len() + usize::from(bytes != 0);
    }
    if bytes > 512 {
        let flag = if input.get("search").is_some_and(|v| !v.is_null()) {
            "search"
        } else {
            fields(command)
                .iter()
                .filter(|field| input.get(field.property).is_some_and(|v| !v.is_null()))
                .map(|field| field.flag)
                .next_back()
                .unwrap_or("search")
        };
        return Err(ValidationError::Value(flag));
    }

    Ok(())
}

pub(crate) fn terms(command: &str, input: &Value) -> Vec<String> {
    let mut terms = Vec::new();
    for field in fields(command) {
        if field.term.is_empty() {
            continue;
        }
        let Some(value) = input.get(field.property).filter(|v| !v.is_null()) else {
            continue;
        };
        let values = value
            .as_array()
            .map(|values| values.iter().collect::<Vec<_>>())
            .unwrap_or_else(|| vec![value]);
        for value in values {
            let text = value.as_str().unwrap_or("");
            let term = match field.kind {
                Kind::Text | Kind::Ref => format!("{}:\"{text}\"", field.term),
                Kind::App => format!("author:\"app/{text}\""),
                Kind::NullableBool => {
                    format!("{}:{}", field.term, value.as_bool().unwrap_or(false))
                }
                Kind::Locked => {
                    if value == true {
                        "is:locked".into()
                    } else if command == "search issues" {
                        "is:unlocked".into()
                    } else {
                        continue;
                    }
                }
                Kind::Merged => {
                    if value == true {
                        "is:merged".into()
                    } else {
                        "is:unmerged".into()
                    }
                }
                Kind::Missing if value == true => format!(
                    "no:{}",
                    field.flag.strip_prefix("no-").expect("missing qualifier")
                ),
                Kind::Reviewer if text.contains('/') => format!("team-review-requested:{text}"),
                Kind::Login
                | Kind::Count
                | Kind::Date
                | Kind::Sha
                | Kind::Team
                | Kind::Project
                | Kind::Choice(_)
                | Kind::Reviewer => format!("{}:{text}", field.term),
                Kind::Bool | Kind::Missing | Kind::Search => continue,
            };
            terms.push(term);
        }
    }
    terms
}

pub(crate) fn add_schema(command: &str, properties: &mut Value) {
    for field in fields(command) {
        let schema = match field.kind {
            Kind::Bool | Kind::NullableBool | Kind::Locked | Kind::Merged | Kind::Missing => {
                json!({"type":"boolean"})
            }
            Kind::Choice(values) => json!({"type":"string","enum":values}),
            Kind::Text
            | Kind::Login
            | Kind::Count
            | Kind::Date
            | Kind::Sha
            | Kind::Ref
            | Kind::Team
            | Kind::Project
            | Kind::Reviewer
            | Kind::App
            | Kind::Search => json!({"type":"string"}),
        };
        properties[field.property] = if field.repeat {
            json!({"type":"array","minItems":1,"items":schema})
        } else {
            schema
        };
    }
}

pub(crate) fn unsupported(command: &str, field: &Field, input: &Value) -> bool {
    input.get(field.property).is_some()
        && !fields(command)
            .iter()
            .any(|allowed| allowed.property == field.property)
}

pub(crate) fn has_flags(command: &str, matches: &ArgMatches) -> bool {
    fields(command).iter().any(|field| {
        matches
            .value_source(field.flag)
            .is_some_and(|source| source == clap::parser::ValueSource::CommandLine)
    })
}

pub(crate) fn has_values(command: &str, input: &Value) -> bool {
    fields(command)
        .iter()
        .any(|field| input.get(field.property).is_some_and(|v| !v.is_null()))
}

pub(crate) fn remove_nulls(command: &str, input: &mut Value) {
    if let Some(input) = input.as_object_mut() {
        for field in fields(command) {
            if input.get(field.property) == Some(&Value::Null) {
                input.remove(field.property);
            }
        }
    }
}

pub(crate) fn flag_names(command: &str) -> Vec<&'static str> {
    fields(command).iter().map(|field| field.flag).collect()
}

pub(crate) fn query_schema(command: &str, schema: &mut Value) {
    if command.starts_with("search ") {
        schema["properties"]["query"]["minLength"] = json!(0);
        let mut alternatives = vec![json!({"properties":{"query":{"minLength":1}}})];
        alternatives.extend(
            fields(command)
                .iter()
                .map(|field| json!({"required":[field.property]})),
        );
        schema["anyOf"] = json!(alternatives);
    }
}
