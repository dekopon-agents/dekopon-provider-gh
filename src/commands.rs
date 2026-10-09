//! GitHub-CLI-shaped command words over this provider's own `gh.*` capabilities.
//!
//! This is a command-line program, not a client. It owns a fixed subcommand vocabulary with `gh`-CLI
//! spellings and maps each one onto exactly one `gh.*` capability, returning a
//! [`CommandInvocation`] the broker then authorizes on the identical path a direct
//! `gh.pull-request.read --number 7` takes — same constraint set, same Cedar decision, same
//! credential injection. Rewriting proposes; it never grants.
//!
//! The command tree is declared once as a [`Command`] and parsed by the SDK's `clap` layer, so
//! `gh --help` and `gh pr view --help` render on stdout at status 0, `gh bogus` and `gh pr view`
//! (missing its number) render clap's own usage error on stderr at status 2, and only a
//! well-formed argv reaches [`dispatch`]. Every subcommand names its capability through the
//! `ids::*` constants `lib.rs` builds the manifest from, so a capability renamed there and
//! forgotten here is a build error rather than an exit code a model discovers mid-session.
//!
//! Flags that would change what a command *means* — `--json`, `--jq`, `--web`, `--checkout` — are
//! rejected by name instead of accepted as no-ops. Output is always the capability's structured
//! JSON value; filter it with the shell's `jq` builtin.
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

/// Why `gh api` does not exist here, said once and reachable from `gh --help`.
const API_REFUSAL: &str = "gh: `gh api` is not available: raw API passthrough would bypass \
                           per-capability authorization; use the gh.* capabilities directly (see \
                           `cap --list`)";

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
    if args.0.subcommand_name() == Some("api") {
        return Err(Usage::new(API_REFUSAL));
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
    (
        "--json",
        "output is always a structured JSON value already; filter it with the jq builtin",
    ),
    ("--jq", "pipe the output to the jq builtin instead"),
    ("--template", "format the JSON output with jq instead"),
    ("--checkout", "there is no working tree to check out into"),
    ("--editor", "there is no editor; pass text with --body"),
    ("--fill", "there is no commit context to fill from"),
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
        .after_help(API_REFUSAL)
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
            Command::new("api").hide(true).arg(
                Arg::new("rest")
                    .trailing_var_arg(true)
                    .allow_hyphen_values(true)
                    .num_args(0..),
            ),
        )
        .subcommand(
            Command::new("auth")
                .about("Inspect credential status")
                .subcommand_required(true)
                .subcommand(
                    Command::new("status").about("Read core quota and authentication metadata"),
                ),
        )
        .subcommand(pull_requests())
        .subcommand(
            Command::new("repo")
                .about("Work with repositories")
                .subcommand_required(true)
                .subcommand(
                    Command::new("view")
                        .about("Read repository metadata: default branch, visibility, flags")
                        .arg(
                            Arg::new("slug")
                                .value_name("OWNER/REPO")
                                .help("Repository to read; -R works too"),
                        )
                        .arg(repo_arg(false))
                        .group(
                            ArgGroup::new("target")
                                .args(["slug", "repo"])
                                .required(true),
                        ),
                ),
        )
        .subcommand(
            Command::new("content")
                .about("Work with repository contents")
                .subcommand_required(true)
                .subcommand(
                    Command::new("view")
                        .about("Read one file or directory listing")
                        .arg(
                            Arg::new("path")
                                .value_name("PATH")
                                .required(true)
                                .help("Repository-relative path; the empty string lists the root"),
                        )
                        .arg(repo_arg(true))
                        .arg(
                            Arg::new("ref")
                                .long("ref")
                                .value_name("REF")
                                .help("Branch, tag, or commit SHA; defaults to the default branch"),
                        ),
                ),
        )
        .subcommand(issues())
        .subcommand(
            Command::new("branch")
                .about("Work with branches")
                .subcommand_required(true)
                .subcommand(
                    Command::new("view")
                        .about("Read one branch's head SHA and protection flag")
                        .arg(
                            Arg::new("branch")
                                .value_name("BRANCH")
                                .required(true)
                                .help("Branch name"),
                        )
                        .arg(repo_arg(true)),
                ),
        )
        .subcommand(
            Command::new("commit")
                .about("Work with commits")
                .subcommand_required(true)
                .subcommand(
                    Command::new("view")
                        .about("Read one commit's message, author, stats, and file list")
                        .arg(
                            Arg::new("ref")
                                .value_name("REF")
                                .required(true)
                                .help("Commit SHA, branch, or tag"),
                        )
                        .arg(repo_arg(true)),
                ),
        )
        .subcommand(
            Command::new("user")
                .about("Work with users")
                .subcommand_required(true)
                .subcommand(
                    Command::new("view")
                        .about("Read one user's public profile")
                        .arg(
                            Arg::new("login")
                                .value_name("LOGIN")
                                .required(true)
                                .help("GitHub login"),
                        ),
                ),
        )
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
                        .help("open, closed, or all"),
                )
                .arg(
                    Arg::new("author")
                        .long("author")
                        .value_name("LOGIN")
                        .help("Login filter applied to the fetched page, after pagination"),
                )
                .args(paging()),
        )
        .subcommand(
            Command::new("view")
                .about("Read one pull request's metadata, state, and head/base")
                .arg(number_arg("Pull-request number"))
                .arg(repo_arg(true)),
        )
        .subcommand(
            Command::new("files")
                .about("List one pull request's changed files with bounded patches")
                .arg(number_arg("Pull-request number"))
                .arg(repo_arg(true))
                .args(paging())
                .arg(
                    Arg::new("no-patch")
                        .long("no-patch")
                        .action(ArgAction::SetTrue)
                        .help("Omit the per-file patch"),
                ),
        )
        .subcommand(
            Command::new("diff")
                .about("Read one pull request's unified diff, truncated with a marker")
                .arg(number_arg("Pull-request number"))
                .arg(repo_arg(true)),
        )
        // `status` is the primary spelling; `checks` is visible because real `gh` uses it for the
        // same CI-status question. Both dispatch to the identical bounded capability.
        .subcommand(
            Command::new("status")
                .visible_alias("checks")
                .about("Read the head's Actions workflow runs and legacy commit statuses")
                .arg(number_arg("Pull-request number"))
                .arg(repo_arg(true)),
        )
        .subcommand(
            Command::new("reviews")
                .about("List existing reviews on one pull request")
                .arg(number_arg("Pull-request number"))
                .arg(repo_arg(true))
                .args(paging()),
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
                .args(body())
                .arg(expected_head_sha()),
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
                .arg(expected_head_sha()),
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
                .arg(repo_arg(true)),
        )
        .subcommand(
            Command::new("list")
                .about("List issues (GitHub includes pull requests; each item is flagged)")
                .arg(repo_arg(true))
                .arg(
                    Arg::new("state")
                        .long("state")
                        .value_name("STATE")
                        .help("open, closed, or all"),
                )
                .args(paging()),
        )
        .subcommand(
            Command::new("comments")
                .about("List comments on one issue or pull request")
                .arg(number_arg("Issue or pull-request number"))
                .arg(repo_arg(true))
                .args(paging()),
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
        .value_parser(clap::value_parser!(u64).range(1..))
        .help(help)
}

fn paging() -> [Arg; 2] {
    [
        Arg::new("page")
            .long("page")
            .value_name("N")
            .value_parser(clap::value_parser!(u64))
            .help("Page number"),
        Arg::new("per-page")
            .long("per-page")
            .value_name("N")
            .value_parser(clap::value_parser!(u64))
            .help("Items per page"),
    ]
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

fn expected_head_sha() -> Arg {
    Arg::new("expected-head-sha")
        .long("expected-head-sha")
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
    let (verb, matches) = area_matches
        .subcommand()
        .expect("the tree requires a subcommand");
    let command = format!("{area} {verb}");
    let mut input = Map::new();

    let capability = match (area, verb) {
        ("pr", "list") => {
            insert_repo(&mut input, matches)?;
            insert_text(&mut input, "state", matches.get_one::<String>("state"));
            insert_text(&mut input, "author", matches.get_one::<String>("author"));
            insert_paging(&mut input, matches);
            ids::PR_LIST
        }
        ("pr", "view") => {
            insert_repo_and_number(&mut input, matches)?;
            ids::PR_READ
        }
        ("pr", "files") => {
            insert_repo_and_number(&mut input, matches)?;
            insert_paging(&mut input, matches);
            if matches.get_flag("no-patch") {
                input.insert("includePatch".to_owned(), Value::Bool(false));
            }
            ids::PR_FILES
        }
        ("pr", "diff") => {
            insert_repo_and_number(&mut input, matches)?;
            ids::PR_DIFF
        }
        ("pr", "status") => {
            insert_repo_and_number(&mut input, matches)?;
            ids::PR_STATUS
        }
        ("pr", "reviews") => {
            insert_repo_and_number(&mut input, matches)?;
            insert_paging(&mut input, matches);
            ids::PR_REVIEWS
        }
        ("pr", "review") => {
            insert_repo_and_number(&mut input, matches)?;
            insert_text(
                &mut input,
                "expectedHeadSha",
                matches.get_one::<String>("expected-head-sha"),
            );
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
                matches.get_one::<String>("expected-head-sha"),
            );
            for method in ["merge", "squash", "rebase"] {
                if matches.get_flag(method) {
                    input.insert("mergeMethod".to_owned(), Value::String(method.to_owned()));
                }
            }
            ids::PR_MERGE
        }
        ("repo", "view") => {
            // `gh repo view owner/repo` is the natural spelling; `-R` works too. The group makes
            // exactly one of them present.
            let slug = matches
                .get_one::<String>("slug")
                .or_else(|| matches.get_one::<String>("repo"))
                .expect("the tree requires one of them");
            let (owner, repo) = parse_repo(slug)?;
            input.insert("owner".to_owned(), Value::String(owner));
            input.insert("repo".to_owned(), Value::String(repo));
            ids::REPO_READ
        }
        ("content", "view") => {
            insert_repo(&mut input, matches)?;
            insert_required_text(&mut input, "path", matches);
            insert_text(&mut input, "ref", matches.get_one::<String>("ref"));
            ids::CONTENT_READ
        }
        ("issue", "view") => {
            insert_repo_and_number(&mut input, matches)?;
            ids::ISSUE_READ
        }
        ("issue", "list") => {
            insert_repo(&mut input, matches)?;
            insert_text(&mut input, "state", matches.get_one::<String>("state"));
            insert_paging(&mut input, matches);
            ids::ISSUE_LIST
        }
        ("issue", "comments") => {
            insert_repo_and_number(&mut input, matches)?;
            insert_paging(&mut input, matches);
            ids::ISSUE_COMMENTS_READ
        }
        ("issue", "comment") => {
            insert_repo_and_number(&mut input, matches)?;
            let text = body_text(matches, stdin_piped)?
                .ok_or_else(|| usage(format!("gh: {command} requires --body text")))?;
            input.insert("body".to_owned(), Value::String(text));
            mark_piped_body(&mut input, matches);
            ids::ISSUE_COMMENT
        }
        ("branch", "view") => {
            insert_repo(&mut input, matches)?;
            insert_required_text(&mut input, "branch", matches);
            ids::BRANCH_READ
        }
        ("commit", "view") => {
            insert_repo(&mut input, matches)?;
            insert_required_text(&mut input, "ref", matches);
            ids::COMMIT_READ
        }
        ("auth", "status") => ids::AUTH_STATUS,
        ("user", "view") => {
            insert_required_text(&mut input, "login", matches);
            ids::USER_READ
        }
        _ => unreachable!("the tree has no other subcommand than {command}"),
    };

    Ok((capability, Value::Object(input)))
}

/// Every `(area, verb)` `dispatch` answers, paired with the capability it proposes.
///
/// The table exists so a test can walk it against both the command tree and the manifest; nothing
/// at runtime reads it.
#[cfg(test)]
pub(crate) const DISPATCH_TABLE: &[(&str, &str, &str)] = &[
    ("pr", "list", ids::PR_LIST),
    ("pr", "view", ids::PR_READ),
    ("pr", "files", ids::PR_FILES),
    ("pr", "diff", ids::PR_DIFF),
    ("pr", "status", ids::PR_STATUS),
    ("pr", "reviews", ids::PR_REVIEWS),
    ("pr", "review", ids::PR_APPROVE),
    ("pr", "review", ids::PR_COMMENT),
    ("pr", "review", ids::PR_REQUEST_CHANGES),
    ("pr", "merge", ids::PR_MERGE),
    ("repo", "view", ids::REPO_READ),
    ("content", "view", ids::CONTENT_READ),
    ("issue", "view", ids::ISSUE_READ),
    ("issue", "list", ids::ISSUE_LIST),
    ("issue", "comments", ids::ISSUE_COMMENTS_READ),
    ("issue", "comment", ids::ISSUE_COMMENT),
    ("branch", "view", ids::BRANCH_READ),
    ("commit", "view", ids::COMMIT_READ),
    ("user", "view", ids::USER_READ),
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

fn insert_paging(input: &mut Map<String, Value>, matches: &ArgMatches) {
    if let Some(page) = matches.get_one::<u64>("page") {
        input.insert("page".to_owned(), Value::from(*page));
    }
    if let Some(per_page) = matches.get_one::<u64>("per-page") {
        input.insert("perPage".to_owned(), Value::from(*per_page));
    }
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
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
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
        assert_eq!(input, json!({"owner": "o", "repo": "r", "number": 7}));
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
            (&["auth", "status"], ids::AUTH_STATUS, serde_json::json!({})),
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
                &["pr", "files", "7", "-R", "o/r", "--page", "2", "--no-patch"],
                "gh.pull-request.files",
                json!({"owner": "o", "repo": "r", "number": 7, "page": 2, "includePatch": false}),
            ),
            (
                &["pr", "diff", "7", "-R", "o/r"],
                "gh.pull-request.diff",
                json!({"owner": "o", "repo": "r", "number": 7}),
            ),
            (
                &["pr", "status", "7", "-R", "o/r"],
                "gh.pull-request.status",
                json!({"owner": "o", "repo": "r", "number": 7}),
            ),
            (
                &["pr", "checks", "7", "-R", "o/r"],
                "gh.pull-request.status",
                json!({"owner": "o", "repo": "r", "number": 7}),
            ),
            (
                &["pr", "reviews", "7", "-R", "o/r"],
                "gh.pull-request.reviews",
                json!({"owner": "o", "repo": "r", "number": 7}),
            ),
            (
                &[
                    "pr",
                    "review",
                    "7",
                    "-R",
                    "o/r",
                    "--approve",
                    "--expected-head-sha",
                    "abc123",
                ],
                "gh.pull-request.approve",
                json!({"owner": "o", "repo": "r", "number": 7, "expectedHeadSha": "abc123"}),
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
                &["repo", "view", "-R", "o/r"],
                "gh.repo.read",
                json!({"owner": "o", "repo": "r"}),
            ),
            (
                &[
                    "content",
                    "view",
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
                &["issue", "view", "9", "-R", "o/r"],
                "gh.issue.read",
                json!({"owner": "o", "repo": "r", "number": 9}),
            ),
            (
                &["issue", "list", "-R", "o/r", "--per-page", "5"],
                "gh.issue.list",
                json!({"owner": "o", "repo": "r", "perPage": 5}),
            ),
            (
                &["issue", "comments", "9", "-R", "o/r"],
                "gh.issue-comments.read",
                json!({"owner": "o", "repo": "r", "number": 9}),
            ),
            (
                &["issue", "comment", "9", "-R", "o/r", "-b", "done"],
                "gh.issue.comment",
                json!({"owner": "o", "repo": "r", "number": 9, "body": "done"}),
            ),
            (
                &["branch", "view", "main", "-R", "o/r"],
                "gh.branch.read",
                json!({"owner": "o", "repo": "r", "branch": "main"}),
            ),
            (
                &["commit", "view", "abc123", "-R", "o/r"],
                "gh.commit.read",
                json!({"owner": "o", "repo": "r", "ref": "abc123"}),
            ),
            (
                &["user", "view", "cpetersen"],
                "gh.user.read",
                json!({"login": "cpetersen"}),
            ),
        ];

        for (arguments, capability, input) in cases {
            let (called, sent) = dispatch(arguments);
            assert_eq!(called, *capability, "{arguments:?}");
            assert_eq!(sent, *input, "{arguments:?}");
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
        // The refusal `gh api` earns is reachable from the help page, not only by typing it.
        assert!(
            stdout.contains("per-capability authorization"),
            "{stdout:?}"
        );
        let (stdout, _, _) = rendered(&["pr", "view", "-h"]);
        assert!(
            stdout.contains("\nUsage: gh pr view --repo <OWNER/REPO> <NUMBER>\n"),
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
        for flag in ["--web", "--json", "--jq", "--template", "--checkout"] {
            let failure = refuse(&["pr", "view", "7", "-R", "o/r", flag]);
            assert_eq!(failure.code(), "usage");
            let message = failure.message().to_owned();
            assert!(message.contains(flag), "{message}");
        }
        // The `--json=fields` spelling is the same lie with a value attached.
        let failure = refuse(&["pr", "view", "7", "-R", "o/r", "--json=number,title"]);
        assert!(failure.message().contains("--json"), "{failure:?}");
    }

    #[test]
    fn gh_api_is_refused_as_a_policy_bypass() {
        let message = refuse(&["api", "/repos/o/r/pulls"]).message().to_owned();
        assert!(message.contains("per-capability"), "{message}");
        // Whatever is piped into it reaches the guest and is refused with everything else.
        let error = run(&argv(&["api", "--input", "-"]), Some("{\"a\":1}"))
            .expect_err("a piped value does not buy a passthrough");
        assert!(error.message().contains("per-capability"), "{error:?}");
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
