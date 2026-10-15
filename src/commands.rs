//! GitHub-CLI-shaped command words over this provider's own `gh.*` capabilities.
//!
//! This is a command-line program, not a client. It owns a fixed subcommand vocabulary with `gh`-CLI
//! spellings and maps each one onto exactly one `gh.*` capability, returning a
//! `CommandInvocation` the broker then authorizes on the identical path a direct
//! `gh.pull-request.read --number 7` takes — same constraint set, same Cedar decision, same
//! credential injection. Rewriting proposes; it never grants.
//!
//! The command tree is declared once as a [`Command`] and parsed by the SDK's `clap` layer, so
//! `gh --help` and `gh pr view --help` render on stdout at status 0, `gh bogus` and `gh pr view`
//! (missing its number) render clap's own usage error on stderr at status 2, and only a
//! well-formed argv reaches `dispatch`. Every subcommand names its capability through the
//! `ids::*` constants `lib.rs` builds the manifest from, so a capability renamed there and
//! forgotten here is a build error rather than an exit code a model discovers mid-session.
//!
//! Unsupported flags refuse rather than silently changing the meaning of a command.
//! Rendering and closed JSON field selection are introduced alongside the output handlers.
//!
//! Every `gh.*` capability remains directly invocable as a command word
//! (`gh.pull-request.read --owner o --repo r --number 7`) with none of this involved.

use crate::error::ProviderError;
use clap::{
    self, Arg, ArgAction, ArgGroup, ArgMatches, Command, CommandFactory, FromArgMatches, Parser,
};
use dekopon_provider_sdk::provider::{Proposal, Usage};
use serde_json::{Map, Value};

use crate::ids;

const API_HELP: &str = "gh api accepts only users/LOGIN, repos/OWNER/REPO/branches/BRANCH, and repos/OWNER/REPO/commits/REF; no raw API passthrough";

/// Runs one `gh …` argv the way the upstream tool would.
///
/// `argv` holds only the arguments after the command word: the broker selects the declaring
/// provider by the word before the guest runs and carries it in its own protocol field, so
/// `gh pr view 7` arrives as `["pr", "view", "7"]`.
///
/// `stdin` is the value piped into the word, `None` when nothing was piped. It is read by
/// `--body-file -`, the one place text of unbounded length is an argument; the rest of the
/// surface is flags and identifiers.
pub struct GhArgs(ArgMatches);
impl CommandFactory for GhArgs {
    fn command() -> Command {
        tree()
    }
    fn command_for_update() -> Command {
        tree()
    }
}
impl FromArgMatches for GhArgs {
    fn from_arg_matches(matches: &ArgMatches) -> Result<Self, clap::Error> {
        Ok(Self(matches.clone()))
    }
    fn from_arg_matches_mut(matches: &mut ArgMatches) -> Result<Self, clap::Error> {
        Self::from_arg_matches(matches)
    }
    fn update_from_arg_matches(&mut self, matches: &ArgMatches) -> Result<(), clap::Error> {
        *self = Self::from_arg_matches(matches)?;
        Ok(())
    }
    fn update_from_arg_matches_mut(&mut self, matches: &mut ArgMatches) -> Result<(), clap::Error> {
        self.update_from_arg_matches(matches)
    }
}
impl Parser for GhArgs {}

pub(crate) fn propose(args: GhArgs, stdin_piped: bool) -> Result<Proposal<crate::Gh>, Usage> {
    for (flag, reason) in REJECTED_FLAGS {
        if args.0.contains_id(flag.trim_start_matches('-'))
            && args
                .0
                .get_one::<String>(flag.trim_start_matches('-'))
                .is_some()
        {
            return Err(Usage::new(format!("gh: {flag} is not supported: {reason}")));
        }
    }
    let (capability, input) =
        dispatch(args.0, stdin_piped).map_err(|error| Usage::new(error.message().to_owned()))?;
    Ok(crate::typed::proposal(capability, input))
}

/// A usage failure the shell reports to the model verbatim.
fn usage(message: impl Into<String>) -> ProviderError {
    ProviderError::new("usage", message)
}

/// Flags real `gh` accepts that this command refuses by name, with the reason.
const REJECTED_FLAGS: &[(&str, &str)] = &[
    ("--web", "there is no browser to open"),
    ("--jq", "pipe the output to the jq builtin instead"),
    ("--template", "format the JSON output with jq instead"),
    ("--checkout", "there is no working tree to check out into"),
    ("--editor", "there is no editor; pass text with --body"),
    ("--fill", "there is no commit context to fill from"),
    ("--required", "required-check data is GraphQL-only"),
];

// ---------------------------------------------------------------------------
// The command tree
// ---------------------------------------------------------------------------

/// The whole `gh` surface, rebuilt on every call.
///
/// A command word runs in a fresh store under a fuel bound, so there is no process-lifetime
/// static to construct this into; the bound is what caps the work.
fn tree() -> Command {
    Command::new("gh")
        .about("Narrow GitHub operations, each mapping to exactly one gh.* capability")
        .after_help(API_HELP)
        .version(env!("CARGO_PKG_VERSION"))
        .subcommand_required(true)
        .args(REJECTED_FLAGS.iter().map(|(flag, _)| {
            let name = flag.trim_start_matches('-');
            Arg::new(name)
                .long(name)
                .global(true)
                .hide(true)
                .num_args(0..=1)
                .default_missing_value("")
        }))
        .subcommand(
            Command::new("api")
                .about("Read one of three finite GitHub REST routes")
                .arg(Arg::new("route").required(true).value_name("ROUTE")),
        )
        .subcommand(
            Command::new("auth")
                .about("Inspect the broker-bound GitHub credential")
                .subcommand_required(true)
                .subcommand(
                    Command::new("status").about("Read quota, token expiration, and scopes"),
                ),
        )
        .subcommand(pull_requests())
        .subcommand(
            Command::new("repo")
                .about("Work with repositories")
                .subcommand_required(true)
                .subcommand(
                    Command::new("read-file")
                        .arg(Arg::new("path").required(true).value_name("PATH"))
                        .arg(repo_arg(true))
                        .arg(Arg::new("ref").long("ref").value_name("REF"))
                        .arg(json_arg()),
                )
                .subcommand(
                    Command::new("read-dir")
                        .arg(Arg::new("path").value_name("PATH"))
                        .arg(repo_arg(true))
                        .arg(Arg::new("ref").long("ref").value_name("REF"))
                        .arg(json_arg()),
                )
                .subcommand(
                    Command::new("view")
                        .about("Read repository metadata: default branch, visibility, flags")
                        .arg(Arg::new("slug").value_name("OWNER/REPO").required(true))
                        .arg(json_arg()),
                ),
        )
        .subcommand(issues())
}

fn pull_requests() -> Command {
    Command::new("pr")
        .about("Work with pull requests")
        .subcommand_required(true)
        .subcommand(
            Command::new("list")
                .about("List pull requests")
                .arg(repo_arg(true))
                .arg(
                    Arg::new("state")
                        .long("state")
                        .value_name("STATE")
                        .value_parser(["open", "closed"])
                        .help("open or closed"),
                )
                .arg(
                    Arg::new("author")
                        .long("author")
                        .value_name("LOGIN")
                        .help("Login filter applied to the fetched page, after pagination"),
                )
                .arg(json_arg()),
        )
        .subcommand(
            Command::new("view")
                .about("Read one pull request's metadata, state, and head/base")
                .arg(number_arg("Pull-request number"))
                .arg(repo_arg(true))
                .arg(json_arg()),
        )
        .subcommand(
            Command::new("diff")
                .about("Read one pull request's unified diff, truncated with a marker")
                .arg(number_arg("Pull-request number"))
                .arg(repo_arg(true)),
        )
        .subcommand(
            Command::new("checks")
                .about("Read the head's Actions workflow runs and legacy commit statuses")
                .arg(number_arg("Pull-request number"))
                .arg(repo_arg(true)),
        )
        .subcommand(
            Command::new("review")
                .about("Submit a review pinned to the verified head SHA")
                .arg(number_arg("Pull-request number"))
                .arg(repo_arg(true))
                .arg(event_flag("approve", "Submit an APPROVE review"))
                .arg(event_flag("comment", "Submit a COMMENT review"))
                .arg(event_flag(
                    "request-changes",
                    "Submit a REQUEST_CHANGES review",
                ))
                .group(
                    ArgGroup::new("event")
                        .args(["approve", "comment", "request-changes"])
                        .required(true),
                )
                .args(body()),
        )
        .subcommand(
            Command::new("merge")
                .about("Merge one pull request, pinned to the verified head SHA")
                .arg(number_arg("Pull-request number"))
                .arg(repo_arg(true))
                .arg(event_flag("merge", "Merge with a merge commit"))
                .arg(event_flag("squash", "Squash and merge"))
                .arg(event_flag("rebase", "Rebase and merge"))
                .group(ArgGroup::new("method").args(["merge", "squash", "rebase"]))
                .arg(match_head_commit()),
        )
}

fn issues() -> Command {
    Command::new("issue")
        .about("Work with issues")
        .subcommand_required(true)
        .subcommand(
            Command::new("view")
                .about("Read one issue with a bounded body")
                .arg(number_arg("Issue number"))
                .arg(repo_arg(true))
                .arg(json_arg()),
        )
        .subcommand(
            Command::new("list")
                .about("List issues (GitHub includes pull requests; each item is flagged)")
                .arg(repo_arg(true))
                .arg(
                    Arg::new("state")
                        .long("state")
                        .value_name("STATE")
                        .value_parser(["open", "closed"])
                        .help("open or closed"),
                )
                .arg(json_arg()),
        )
        .subcommand(
            Command::new("comment")
                .about("Post one comment on an issue or pull request")
                .arg(number_arg("Issue or pull-request number"))
                .arg(repo_arg(true))
                .args(body()),
        )
}

/// `-R owner/repo`, required wherever there is a repository to name.
///
/// There is no git working tree here, so nothing can be inferred: a missing `-R` is clap's own
/// required-argument error naming the exact form, never a guess.
fn json_arg() -> Arg {
    Arg::new("json")
        .long("json")
        .value_name("FIELDS")
        .num_args(0..=1)
        .default_missing_value("")
}

fn repo_arg(required: bool) -> Arg {
    Arg::new("repo")
        .short('R')
        .long("repo")
        .value_name("OWNER/REPO")
        .required(required)
        .help("Repository to act on; there is no working tree to infer one from")
}

/// The single positional number, refused below 1 by the parser rather than by the capability.
fn number_arg(help: &'static str) -> Arg {
    Arg::new("number")
        .value_name("NUMBER")
        .required(true)
        .value_parser(clap::value_parser!(u64).range(1..=crate::MAX_NUMBER as u64))
        .help(help)
}

/// `--body TEXT` or `--body-file -`, the one argument whose value may be piped in.
fn body() -> [Arg; 2] {
    [
        Arg::new("body")
            .short('b')
            .long("body")
            .value_name("TEXT")
            .help("Body text"),
        Arg::new("body-file")
            .long("body-file")
            .value_name("-")
            .value_parser(["-"])
            .conflicts_with("body")
            .help("Read the body from the value piped into the word"),
    ]
}

fn match_head_commit() -> Arg {
    Arg::new("match-head-commit")
        .long("match-head-commit")
        .value_name("SHA")
        .help("Refuse unless the pull request's head still matches this SHA")
}

/// One member of a mutually exclusive group, which is how `gh` spells a choice.
fn event_flag(name: &'static str, help: &'static str) -> Arg {
    Arg::new(name)
        .long(name)
        .action(ArgAction::SetTrue)
        .help(help)
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Turns clap's matches into the proposal the selected subcommand names.
///
/// Runs only after clap accepted the argv, so the subcommand and its required arguments are
/// present; what remains is what clap cannot know — whether `owner/repo` is well formed, whether
/// anything was piped, and which capability an event flag selects.
fn dispatch(
    matches: ArgMatches,
    stdin_piped: bool,
) -> Result<(&'static str, Value), ProviderError> {
    let (area, area_matches) = matches
        .subcommand()
        .expect("the tree requires a subcommand");
    if area == "api" {
        return api_route(area_matches);
    }
    let (verb, matches) = area_matches
        .subcommand()
        .expect("the tree requires a subcommand");
    let command = format!("{area} {verb}");
    let mut input = Map::new();

    let capability = match (area, verb) {
        ("pr", "list") => {
            insert_repo(&mut input, matches)?;
            insert_text(&mut input, "state", matches.get_one::<String>("state"));
            if let Some(author) = matches.get_one::<String>("author") {
                crate::validate_login(author).map_err(|_| usage("gh: invalid author login"))?;
                insert_text(&mut input, "author", Some(author));
            }
            ids::PR_LIST
        }
        ("pr", "view") => {
            insert_repo_and_number(&mut input, matches)?;
            ids::PR_READ
        }
        ("pr", "diff") => {
            insert_repo_and_number(&mut input, matches)?;
            ids::PR_DIFF
        }
        ("pr", "checks") => {
            insert_repo_and_number(&mut input, matches)?;
            ids::PR_STATUS
        }
        ("pr", "review") => {
            insert_repo_and_number(&mut input, matches)?;
            // The group is `required`, so exactly one of the three is set.
            if matches.get_flag("approve") {
                insert_text(
                    &mut input,
                    "body",
                    body_text(matches, stdin_piped)?.as_ref(),
                );
                mark_piped_body(&mut input, matches);
                ids::PR_APPROVE
            } else {
                let event = if matches.get_flag("comment") {
                    "--comment"
                } else {
                    "--request-changes"
                };
                let text = body_text(matches, stdin_piped)?
                    .ok_or_else(|| usage(format!("gh: {event} requires --body text")))?;
                input.insert("body".to_owned(), Value::String(text));
                mark_piped_body(&mut input, matches);
                if matches.get_flag("comment") {
                    ids::PR_COMMENT
                } else {
                    ids::PR_REQUEST_CHANGES
                }
            }
        }
        ("pr", "merge") => {
            insert_repo_and_number(&mut input, matches)?;
            insert_text(
                &mut input,
                "expectedHeadSha",
                matches.get_one::<String>("match-head-commit"),
            );
            for method in ["merge", "squash", "rebase"] {
                if matches.get_flag(method) {
                    input.insert("mergeMethod".to_owned(), Value::String(method.to_owned()));
                }
            }
            ids::PR_MERGE
        }
        ("repo", "view") => {
            let slug = matches.get_one::<String>("slug").expect("required slug");
            let (owner, repo) = parse_repo(slug)?;
            input.insert("owner".to_owned(), Value::String(owner));
            input.insert("repo".to_owned(), Value::String(repo));
            ids::REPO_READ
        }
        ("repo", "read-file" | "read-dir") => {
            insert_repo(&mut input, matches)?;
            if verb == "read-file" {
                insert_required_text(&mut input, "path", matches);
            } else {
                input.insert(
                    "path".to_owned(),
                    Value::String(
                        matches
                            .get_one::<String>("path")
                            .cloned()
                            .unwrap_or_default(),
                    ),
                );
            }
            crate::validate_path(input["path"].as_str().expect("path is a string"))
                .map_err(|_| usage("gh: invalid content path"))?;
            if let Some(reference) = matches.get_one::<String>("ref") {
                crate::validate_ref(reference).map_err(|_| usage("gh: invalid ref"))?;
                insert_text(&mut input, "ref", Some(reference));
            }
            ids::CONTENT_READ
        }
        ("issue", "view") => {
            insert_repo_and_number(&mut input, matches)?;
            ids::ISSUE_READ
        }
        ("issue", "list") => {
            insert_repo(&mut input, matches)?;
            insert_text(&mut input, "state", matches.get_one::<String>("state"));
            ids::ISSUE_LIST
        }
        ("issue", "comment") => {
            insert_repo_and_number(&mut input, matches)?;
            let text = body_text(matches, stdin_piped)?
                .ok_or_else(|| usage(format!("gh: {command} requires --body text")))?;
            input.insert("body".to_owned(), Value::String(text));
            mark_piped_body(&mut input, matches);
            ids::ISSUE_COMMENT
        }
        ("auth", "status") => ids::AUTH_STATUS,
        _ => unreachable!("the tree has no other subcommand than {command}"),
    };

    if let Some(fields) = crate::output::fields(area, verb)
        && matches.try_get_one::<String>("json").is_ok()
    {
        let selection = match matches.get_one::<String>("json") {
            Some(value) if value.is_empty() => {
                return Err(usage(format!(
                    "gh {command}: --json requires fields: {fields}"
                )));
            }
            Some(value) => {
                let mut seen = std::collections::HashSet::new();
                for field in value.split(',') {
                    if !fields.split(',').any(|allowed| allowed == field) || !seen.insert(field) {
                        return Err(usage(format!(
                            "gh {command}: invalid --json field selection"
                        )));
                    }
                }
                value.as_str()
            }
            None => "",
        };
        input.insert("_cliOutput".to_owned(), Value::String(selection.to_owned()));
        if area == "repo" && matches!(verb, "read-file" | "read-dir") {
            input.insert("_cliKind".to_owned(), Value::String(verb.to_owned()));
        }
    }
    Ok((capability, Value::Object(input)))
}

fn api_route(matches: &ArgMatches) -> Result<(&'static str, Value), ProviderError> {
    let route = matches.get_one::<String>("route").expect("required route");
    let parts: Vec<_> = route.splitn(5, '/').collect();
    let (id, input) = match parts.as_slice() {
        ["users", login] => {
            crate::validate_login(login).map_err(|_| usage("gh api: invalid login"))?;
            (ids::USER_READ, serde_json::json!({"login": login}))
        }
        ["repos", owner, repo, "branches", branch] => {
            crate::validate_login(owner).map_err(|_| usage("gh api: invalid owner"))?;
            crate::validate_repo(repo).map_err(|_| usage("gh api: invalid repository"))?;
            crate::validate_ref(branch).map_err(|_| usage("gh api: invalid branch"))?;
            (
                ids::BRANCH_READ,
                serde_json::json!({"owner": owner, "repo": repo, "branch": branch}),
            )
        }
        ["repos", owner, repo, "commits", reference] => {
            crate::validate_login(owner).map_err(|_| usage("gh api: invalid owner"))?;
            crate::validate_repo(repo).map_err(|_| usage("gh api: invalid repository"))?;
            crate::validate_ref(reference).map_err(|_| usage("gh api: invalid ref"))?;
            (
                ids::COMMIT_READ,
                serde_json::json!({"owner": owner, "repo": repo, "ref": reference}),
            )
        }
        _ => return Err(usage(API_HELP)),
    };
    Ok((id, input))
}

/// Every `(area, verb)` `dispatch` answers, paired with the capability it proposes.
///
/// The table exists so a test can walk it against both the command tree and the manifest; nothing
/// at runtime reads it.
#[cfg(test)]
pub(crate) const DISPATCH_TABLE: &[(&str, &str, &str)] = &[
    ("pr", "list", ids::PR_LIST),
    ("pr", "view", ids::PR_READ),
    ("pr", "diff", ids::PR_DIFF),
    ("pr", "checks", ids::PR_STATUS),
    ("pr", "review", ids::PR_APPROVE),
    ("pr", "review", ids::PR_COMMENT),
    ("pr", "review", ids::PR_REQUEST_CHANGES),
    ("pr", "merge", ids::PR_MERGE),
    ("repo", "view", ids::REPO_READ),
    ("repo", "read-file", ids::CONTENT_READ),
    ("repo", "read-dir", ids::CONTENT_READ),
    ("issue", "view", ids::ISSUE_READ),
    ("issue", "list", ids::ISSUE_LIST),
    ("issue", "comment", ids::ISSUE_COMMENT),
    ("auth", "status", ids::AUTH_STATUS),
];

fn insert_repo(input: &mut Map<String, Value>, matches: &ArgMatches) -> Result<(), ProviderError> {
    let slug = matches
        .get_one::<String>("repo")
        .expect("the tree requires -R");
    let (owner, repo) = parse_repo(slug)?;
    input.insert("owner".to_owned(), Value::String(owner));
    input.insert("repo".to_owned(), Value::String(repo));
    Ok(())
}

fn insert_repo_and_number(
    input: &mut Map<String, Value>,
    matches: &ArgMatches,
) -> Result<(), ProviderError> {
    insert_repo(input, matches)?;
    let number = matches
        .get_one::<u64>("number")
        .expect("the tree requires a number");
    input.insert("number".to_owned(), Value::from(*number));
    Ok(())
}

fn insert_text(input: &mut Map<String, Value>, key: &str, value: Option<&String>) {
    if let Some(value) = value {
        input.insert(key.to_owned(), Value::String(value.clone()));
    }
}

fn insert_required_text(input: &mut Map<String, Value>, key: &str, matches: &ArgMatches) {
    let value = matches
        .get_one::<String>(key)
        .expect("the tree requires this argument");
    input.insert(key.to_owned(), Value::String(value.clone()));
}

fn mark_piped_body(input: &mut Map<String, Value>, matches: &ArgMatches) {
    if matches.get_one::<String>("body-file").is_some() {
        input.insert(crate::typed::STDIN_BODY_FIELD.to_owned(), Value::Bool(true));
    }
}

/// Resolves `--body TEXT` or `--body-file -` into the text to post.
///
/// Whether anything was piped is exactly what clap cannot know, so `--body-file -` with nothing
/// on stdin is a decline naming its cause rather than an empty comment.
fn body_text(matches: &ArgMatches, stdin_piped: bool) -> Result<Option<String>, ProviderError> {
    if let Some(text) = matches.get_one::<String>("body") {
        return Ok(Some(text.clone()));
    }
    if matches.get_one::<String>("body-file").is_some() {
        if !stdin_piped {
            return Err(usage("gh: --body-file -: nothing was piped into the word"));
        }
        return Ok(Some(crate::typed::STDIN_BODY.to_owned()));
    }
    Ok(None)
}

/// Splits `owner/repo`, structurally only; deeper grammar checks belong to the provider.
fn parse_repo(value: &str) -> Result<(String, String), ProviderError> {
    let mut parts = value.splitn(2, '/');
    let owner = parts.next().unwrap_or_default();
    let repo = parts.next().unwrap_or_default();
    if crate::validate_login(owner).is_err()
        || crate::validate_repo(repo).is_err()
        || repo.contains('/')
    {
        return Err(usage(format!(
            "gh: repository {value:?} must be formatted as owner/repo"
        )));
    }
    Ok((owner.to_owned(), repo.to_owned()))
}

#[cfg(test)]
mod tests {
    use dekopon_provider_sdk::CommandRunOutcome;
    use dekopon_provider_sdk::provider;
    use serde_json::{Value, json};

    use super::{DISPATCH_TABLE, tree};
    use crate::{Gh, error::ProviderError};

    #[derive(Debug, PartialEq)]
    enum CommandRun {
        Proposal {
            capability: String,
            input: Value,
        },
        Rendered {
            stdout: String,
            stderr: String,
            status: u8,
        },
    }
    impl CommandRun {
        fn proposal(capability: dekopon_provider_sdk::CapabilityId, input: Value) -> Self {
            Self::Proposal {
                capability: capability.to_string(),
                input,
            }
        }
    }
    fn run(argv: &[String], stdin: Option<&str>) -> Result<CommandRun, ProviderError> {
        match provider::command::<Gh>(argv, stdin.is_some()) {
            CommandRunOutcome::Proposed {
                capability, input, ..
            } => Ok(CommandRun::Proposal {
                capability: capability.to_string(),
                input,
            }),
            CommandRunOutcome::Rendered {
                stdout,
                stderr,
                status,
            } => Ok(CommandRun::Rendered {
                stdout,
                stderr,
                status,
            }),
            CommandRunOutcome::Failed { error } => Err(ProviderError::new("usage", error.message)),
        }
    }

    /// The argv one `gh …` command line reaches the guest as: the word is not part of it.
    fn argv(arguments: &[&str]) -> Vec<String> {
        arguments
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect()
    }

    /// Runs one `gh …` argv, asserting it proposed.
    fn dispatch(arguments: &[&str]) -> (String, Value) {
        match run(&argv(arguments), None).expect("gh proposes") {
            CommandRun::Proposal { capability, input } => (capability, input),
            other => panic!("expected a proposal for {arguments:?}, got {other:?}"),
        }
    }

    /// Runs one `gh …` argv, asserting the guest declined it outright.
    fn refuse(arguments: &[&str]) -> ProviderError {
        run(&argv(arguments), None).expect_err("gh must decline")
    }

    /// Runs one `gh …` argv, asserting clap answered it as a command-line program would.
    fn rendered(arguments: &[&str]) -> (String, String, u8) {
        match run(&argv(arguments), None).expect("rendered, not declined") {
            CommandRun::Rendered {
                stdout,
                stderr,
                status,
            } => (stdout, stderr, status),
            other => panic!("expected rendered text for {arguments:?}, got {other:?}"),
        }
    }

    /// The word is selected before the guest runs, so it is never in `argv`.
    ///
    /// `dekopon-provider-sdk`'s `Provider::run_command` says so outright — "the command word is
    /// selected before this call; `argv` contains only the arguments after it" — and the broker
    /// protocol's `RunCommand` frame carries it in its own field. A parser that dropped the first
    /// element read `view` as the area and failed every `gh <area> <verb>` with a usage error, so
    /// this asserts the wire shape directly rather than only through the helper above.
    #[test]
    fn the_first_argument_is_the_area_not_the_command_word() {
        let (capability, input) = dispatch(&["pr", "view", "7", "-R", "o/r"]);
        assert_eq!(capability, "gh.pull-request.read");
        assert_eq!(
            input,
            json!({"owner": "o", "repo": "r", "number": 7, "_cliOutput": ""})
        );
    }

    /// The manifest is what the broker authorizes against, so a command word that proposes a
    /// capability nobody declared would be an exit code a model discovers mid-session.
    ///
    /// The `ids::*` constants make a renamed capability a build error; this closes the other half,
    /// that `capabilities()` still declares every one of them.
    #[test]
    fn every_dispatch_target_is_declared_in_the_manifest() {
        let declared = provider::manifest::<Gh>()
            .expect("manifest")
            .capabilities
            .iter()
            .map(|capability| capability.id.as_str().to_owned())
            .collect::<Vec<_>>();
        for (area, verb, capability) in DISPATCH_TABLE {
            assert!(
                declared.iter().any(|id| id == capability),
                "gh {area} {verb} proposes {capability}, which the manifest does not declare"
            );
        }
    }

    /// The command tree is what clap parses and what the help pages are rendered from, so it is
    /// asserted directly: every dispatch target must be reachable as a subcommand.
    #[test]
    fn every_dispatch_target_is_a_subcommand_of_the_tree() {
        let tree = tree();
        for (area, verb, capability) in DISPATCH_TABLE {
            let area_command = tree
                .get_subcommands()
                .find(|command| command.get_name() == *area)
                .unwrap_or_else(|| panic!("{area} is dispatched to but not in the tree"));
            assert!(
                area_command
                    .get_subcommands()
                    .any(|command| command.get_name() == *verb),
                "{area} {verb} ({capability}) is dispatched to but not in the tree"
            );
        }
    }

    #[test]
    fn every_subcommand_maps_to_its_capability_and_input() {
        let cases: &[(&[&str], &str, Value)] = &[
            (
                &[
                    "pr",
                    "list",
                    "-R",
                    "o/r",
                    "--state",
                    "open",
                    "--author",
                    "cpetersen",
                ],
                "gh.pull-request.list",
                json!({"owner": "o", "repo": "r", "state": "open", "author": "cpetersen"}),
            ),
            (
                &["pr", "view", "7", "-R", "o/r"],
                "gh.pull-request.read",
                json!({"owner": "o", "repo": "r", "number": 7}),
            ),
            (
                &["pr", "diff", "7", "-R", "o/r"],
                "gh.pull-request.diff",
                json!({"owner": "o", "repo": "r", "number": 7}),
            ),
            (
                &["pr", "checks", "7", "-R", "o/r"],
                "gh.pull-request.status",
                json!({"owner": "o", "repo": "r", "number": 7}),
            ),
            (
                &["pr", "review", "7", "-R", "o/r", "--approve"],
                "gh.pull-request.approve",
                json!({"owner": "o", "repo": "r", "number": 7}),
            ),
            (
                &["pr", "review", "7", "-R", "o/r", "--comment", "-b", "hm"],
                "gh.pull-request.comment",
                json!({"owner": "o", "repo": "r", "number": 7, "body": "hm"}),
            ),
            (
                &[
                    "pr",
                    "review",
                    "7",
                    "-R",
                    "o/r",
                    "--request-changes",
                    "-b",
                    "no",
                ],
                "gh.pull-request.request-changes",
                json!({"owner": "o", "repo": "r", "number": 7, "body": "no"}),
            ),
            (
                &["pr", "merge", "7", "-R", "o/r", "--squash"],
                "gh.pull-request.merge",
                json!({"owner": "o", "repo": "r", "number": 7, "mergeMethod": "squash"}),
            ),
            (
                &["repo", "view", "o/r"],
                "gh.repo.read",
                json!({"owner": "o", "repo": "r"}),
            ),
            (
                &[
                    "repo",
                    "read-file",
                    "src/lib.rs",
                    "-R",
                    "o/r",
                    "--ref",
                    "main",
                ],
                "gh.content.read",
                json!({"owner": "o", "repo": "r", "path": "src/lib.rs", "ref": "main"}),
            ),
            (
                &["repo", "read-dir", "-R", "o/r"],
                "gh.content.read",
                json!({"owner": "o", "repo": "r", "path": ""}),
            ),
            (
                &["issue", "view", "9", "-R", "o/r"],
                "gh.issue.read",
                json!({"owner": "o", "repo": "r", "number": 9}),
            ),
            (
                &["issue", "list", "-R", "o/r"],
                "gh.issue.list",
                json!({"owner": "o", "repo": "r"}),
            ),
            (
                &["issue", "comment", "9", "-R", "o/r", "-b", "done"],
                "gh.issue.comment",
                json!({"owner": "o", "repo": "r", "number": 9, "body": "done"}),
            ),
            (
                &["api", "repos/o/r/branches/main"],
                "gh.branch.read",
                json!({"owner": "o", "repo": "r", "branch": "main"}),
            ),
            (
                &["api", "repos/o/r/branches/release/1.2"],
                "gh.branch.read",
                json!({"owner": "o", "repo": "r", "branch": "release/1.2"}),
            ),
            (
                &["api", "repos/o/r/commits/abc123"],
                "gh.commit.read",
                json!({"owner": "o", "repo": "r", "ref": "abc123"}),
            ),
            (
                &["api", "users/cpetersen"],
                "gh.user.read",
                json!({"login": "cpetersen"}),
            ),
        ];

        for (arguments, capability, input) in cases {
            let (called, sent) = dispatch(arguments);
            assert_eq!(called, *capability, "{arguments:?}");
            let mut expected = input.clone();
            if let Some(fields) = crate::output::fields(arguments[0], arguments[1])
                && !matches!((arguments[0], arguments[1]), ("pr", "checks"))
            {
                assert!(!fields.is_empty());
                expected["_cliOutput"] = json!("");
                if arguments[0] == "repo" && matches!(arguments[1], "read-file" | "read-dir") {
                    expected["_cliKind"] = json!(arguments[1]);
                }
            }
            assert_eq!(sent, expected, "{arguments:?}");
        }
    }

    #[test]
    fn help_renders_on_stdout_at_status_zero() {
        for words in [
            &["--help"][..],
            &["pr", "--help"][..],
            &["pr", "view", "-h"][..],
        ] {
            let (stdout, stderr, status) = rendered(words);
            assert_eq!(status, 0, "{words:?}");
            assert!(stderr.is_empty(), "{words:?}: {stderr:?}");
            assert!(
                !stdout.contains('\u{1b}'),
                "{words:?}: plain, never coloured"
            );
        }
        let (stdout, _, _) = rendered(&["--help"]);
        assert!(stdout.starts_with("Narrow GitHub operations"), "{stdout:?}");
        assert!(stdout.contains("<COMMAND>\n"), "{stdout:?}");
        assert!(stdout.contains("no raw API passthrough"), "{stdout:?}");
        let (stdout, _, _) = rendered(&["pr", "view", "-h"]);
        assert!(
            stdout.contains("\nUsage: gh pr view [OPTIONS] --repo <OWNER/REPO> <NUMBER>\n"),
            "{stdout:?}"
        );
    }

    #[test]
    fn a_version_renders_on_stdout_at_status_zero() {
        let (stdout, _, status) = rendered(&["--version"]);
        assert_eq!(status, 0);
        assert_eq!(stdout, format!("gh {}\n", env!("CARGO_PKG_VERSION")));
    }

    /// A bare `gh`, an unknown subcommand, and a missing argument are all usage errors on stderr
    /// at status 2 — the shape a command-line program has, so `$(gh bogus)` captures nothing.
    #[test]
    fn malformed_argv_is_a_usage_error_on_stderr_at_status_two() {
        for words in [
            &[][..],
            &["bogus"][..],
            &["pr"][..],
            &["pr", "create", "-R", "o/r"][..],
            &["pr", "view"][..],
            &["pr", "view", "7"][..],
            &["pr", "view", "seven", "-R", "o/r"][..],
            &["pr", "view", "0", "-R", "o/r"][..],
            &["repo", "view"][..],
            &["pr", "review", "7", "-R", "o/r"][..],
            &["pr", "diff", "7", "-R", "o/r", "--approve"][..],
        ] {
            let (stdout, stderr, status) = rendered(words);
            assert_eq!(status, 2, "{words:?}");
            assert!(stdout.is_empty(), "{words:?}: {stdout:?}");
            assert!(!stderr.is_empty(), "{words:?}");
            assert!(
                !stderr.contains('\u{1b}'),
                "{words:?}: plain, never coloured"
            );
        }
        let (_, stderr, _) = rendered(&["pr", "create", "-R", "o/r"]);
        assert!(
            stderr.starts_with("error: unrecognized subcommand 'create'"),
            "{stderr:?}"
        );
        // A subcommand's usage line carries the word the model typed, not the bare subcommand.
        let (_, stderr, _) = rendered(&["pr", "view", "7"]);
        assert!(stderr.contains("--repo <OWNER/REPO>"), "{stderr:?}");
        let (_, stderr, _) = rendered(&["pr", "diff", "7", "-R", "o/r", "--approve"]);
        assert!(
            stderr.starts_with("error: unexpected argument '--approve'"),
            "{stderr:?}"
        );
    }

    #[test]
    fn review_requires_exactly_one_event() {
        let (_, stderr, status) = rendered(&["pr", "review", "7", "-R", "o/r"]);
        assert_eq!(status, 2);
        assert!(stderr.contains("--approve"), "{stderr:?}");
        let (_, stderr, status) =
            rendered(&["pr", "review", "7", "-R", "o/r", "--approve", "--comment"]);
        assert_eq!(status, 2);
        assert!(stderr.contains("cannot be used with"), "{stderr:?}");
    }

    #[test]
    fn comment_and_request_changes_require_a_body() {
        for event in ["--comment", "--request-changes"] {
            let message = refuse(&["pr", "review", "7", "-R", "o/r", event])
                .message()
                .to_owned();
            assert!(message.contains("--body"), "{message}");
        }
        let message = refuse(&["issue", "comment", "9", "-R", "o/r"])
            .message()
            .to_owned();
        assert!(message.contains("--body"), "{message}");
    }

    /// `--body-file -` is the one argument whose value is piped in, and nothing piped is a decline
    /// naming its cause rather than an empty comment.
    #[test]
    fn a_body_file_dash_reads_the_piped_value() {
        let comment = run(
            &argv(&["issue", "comment", "9", "-R", "o/r", "--body-file", "-"]),
            Some("piped body"),
        )
        .expect("a piped body proposes");
        assert_eq!(
            comment,
            CommandRun::proposal(
                "gh.issue.comment".parse().expect("static capability"),
                json!({"owner": "o", "repo": "r", "number": 9, "body": crate::typed::STDIN_BODY, "_stdinBody": true})
            )
        );

        let review = run(
            &argv(&[
                "pr",
                "review",
                "7",
                "-R",
                "o/r",
                "--request-changes",
                "--body-file",
                "-",
            ]),
            Some("needs work"),
        )
        .expect("a piped body proposes");
        assert_eq!(
            review,
            CommandRun::proposal(
                "gh.pull-request.request-changes"
                    .parse()
                    .expect("static capability"),
                json!({"owner": "o", "repo": "r", "number": 7, "body": crate::typed::STDIN_BODY, "_stdinBody": true})
            )
        );

        let error = refuse(&["issue", "comment", "9", "-R", "o/r", "--body-file", "-"]);
        assert_eq!(error.code(), "usage");
        assert!(error.message().contains("nothing was piped"), "{error:?}");
    }

    /// A path argument is the one place `--body-file` could have pretended to read a file.
    #[test]
    fn a_body_file_path_is_refused_because_there_is_no_filesystem() {
        let (_, stderr, status) = rendered(&[
            "issue",
            "comment",
            "9",
            "-R",
            "o/r",
            "--body-file",
            "body.md",
        ]);
        assert_eq!(status, 2);
        assert!(stderr.contains("body.md"), "{stderr:?}");
    }

    #[test]
    fn lying_flags_are_rejected_with_guidance() {
        for flag in ["--web", "--jq", "--template", "--checkout"] {
            let failure = refuse(&["pr", "view", "7", "-R", "o/r", flag]);
            assert_eq!(failure.code(), "usage");
            let message = failure.message().to_owned();
            assert!(message.contains(flag), "{message}");
        }
        let (id, input) = dispatch(&["pr", "view", "7", "-R", "o/r", "--json=number,title"]);
        assert_eq!(id, "gh.pull-request.read");
        assert_eq!(input["_cliOutput"], "number,title");
    }

    #[test]
    fn native_json_field_selection_is_closed_before_proposal() {
        for (area, verb, args, listed) in [
            ("pr", "view", vec!["7", "-R", "o/r"], "headRefOid"),
            ("issue", "list", vec!["-R", "o/r"], "assignees"),
            ("repo", "view", vec!["o/r"], "defaultBranchRef"),
            (
                "repo",
                "read-file",
                vec!["README.md", "-R", "o/r"],
                "gitSHA",
            ),
        ] {
            let mut words = vec![area, verb];
            words.extend(&args);
            words.push("--json");
            let error = refuse(&words);
            assert_eq!(error.code(), "usage");
            assert!(error.message().contains(listed), "{error:?}");
            for selection in ["", "title,title", "unknown", "title,"] {
                let mut words = vec![area, verb];
                words.extend(&args);
                words.push("--json");
                words.push(selection);
                let outcome =
                    dekopon_provider_sdk::provider::command::<crate::Gh>(&argv(&words), false);
                assert!(
                    matches!(outcome, dekopon_provider_sdk::CommandRunOutcome::Failed { error } if error.code == "usage"),
                    "{words:?}"
                );
                assert_eq!(
                    dekopon_provider_sdk::provider::Code::USAGE.status().get(),
                    2
                );
            }
        }
        let (id, input) = dispatch(&["repo", "view", "o/r", "--json", "owner,url"]);
        assert_eq!(id, "gh.repo.read");
        assert_eq!(input["_cliOutput"], "owner,url");
    }

    #[test]
    fn native_view_only_fields_are_closed_before_proposal() {
        for words in [
            &["pr", "list", "-R", "o/r", "--json", "files"][..],
            &["pr", "view", "7", "-R", "o/r", "--json", "files,unknown"],
            &[
                "issue",
                "view",
                "9",
                "-R",
                "o/r",
                "--json",
                "comments,comments",
            ],
        ] {
            let outcome = dekopon_provider_sdk::provider::command::<crate::Gh>(&argv(words), false);
            assert!(
                matches!(outcome, dekopon_provider_sdk::CommandRunOutcome::Failed { error } if error.code == "usage"),
                "{words:?}"
            );
        }
        let (id, input) = dispatch(&[
            "pr",
            "view",
            "7",
            "-R",
            "o/r",
            "--json",
            "files,latestReviews",
        ]);
        assert_eq!(id, "gh.pull-request.read");
        assert_eq!(input["_cliOutput"], "files,latestReviews");
    }

    #[test]
    fn api_accepts_only_finite_routes_without_flags_or_queries() {
        for route in [
            "repos/o/r/pulls",
            "graphql",
            "user",
            "users/a?x=1",
            "users/a/b",
        ] {
            assert_eq!(refuse(&["api", route]).code(), "usage");
        }
        for words in [
            &["api", "users/a", "-X", "GET"][..],
            &["api", "users/a", "-f", "x=y"],
            &["api", "users/a", "--method", "POST"],
        ] {
            let (_, _, status) = rendered(words);
            assert_eq!(status, 2, "{words:?}");
        }
    }

    #[test]
    fn removed_words_and_unproved_flags_refuse_before_proposing() {
        let required = refuse(&["pr", "checks", "7", "-R", "o/r", "--required"]);
        assert!(required.message().contains("GraphQL-only"));
        assert_eq!(refuse(&["pr", "view", "7", "-R", "o?/r"]).code(), "usage");
        for words in [
            &["pr", "status", "7", "-R", "o/r"][..],
            &["pr", "files", "7", "-R", "o/r"],
            &["pr", "reviews", "7", "-R", "o/r"],
            &["issue", "comments", "7", "-R", "o/r"],
            &["content", "view", "x", "-R", "o/r"],
            &["user", "view", "alice"],
            &["branch", "view", "main", "-R", "o/r"],
            &["commit", "view", "main", "-R", "o/r"],
            &["pr", "checks", "7", "-R", "o/r", "--watch"],
            &["pr", "list", "-R", "o/r", "--page", "2"],
            &["issue", "list", "-R", "o/r", "--per-page", "5"],
            &["pr", "files", "7", "-R", "o/r", "--no-patch"],
            &[
                "pr",
                "review",
                "7",
                "-R",
                "o/r",
                "--approve",
                "--expected-head-sha",
                "abc",
            ],
            &["repo", "view", "-R", "o/r"],
        ] {
            let (stdout, _, status) = rendered(words);
            assert_eq!(status, 2, "{words:?}");
            assert!(stdout.is_empty(), "{words:?}");
        }
    }

    #[test]
    fn malformed_repositories_are_declined_naming_the_form() {
        let failure = refuse(&["pr", "view", "7", "-R", "just-a-name"]);
        assert_eq!(failure.code(), "usage");
        assert!(failure.message().contains("owner/repo"), "{failure:?}");
    }

    /// Whether a capability is *granted* is not this component's question.
    ///
    /// The rewrite names a capability; the broker decides whether the caller may reach it. That
    /// split is what keeps this a pure function — it holds no session state and could not consult
    /// a grant if it wanted to. `dekopon-shell` reports the ungranted case by name, and
    /// `dekopon-broker` denies the invocation; both are tested there.
    #[test]
    fn rewriting_names_a_capability_without_asserting_any_authority() {
        let (capability, _) = dispatch(&["pr", "review", "7", "-R", "o/r", "--approve"]);
        assert_eq!(capability, "gh.pull-request.approve");
    }
}
