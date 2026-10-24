use clap::{Arg, ArgMatches, Command};
use dekopon_provider_sdk::provider::Usage;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Command,
    Flag,
    UnscopedSearch,
}

pub(crate) struct Entry {
    pub command: &'static str,
    pub kind: Kind,
    pub reason: &'static str,
    pub flags: &'static str,
}

pub(crate) fn entries() -> impl Iterator<Item = Entry> {
    include_str!("unavailable.tsv").lines().map(|line| {
        let mut fields = line.split('\t');
        let kind = match fields.next().expect("entry kind") {
            "command" => Kind::Command,
            "flag" => Kind::Flag,
            "scope" => Kind::UnscopedSearch,
            _ => unreachable!("closed entry kinds"),
        };
        let command = fields.next().expect("command");
        let reason = match fields.next().expect("reason") {
            "W1" => "needs a local git checkout",
            "W2" => "needs an interactive terminal",
            "W3" => "needs a browser or device OAuth flow",
            "W4" => "needs a local credential store",
            "W5" => "needs a browser or local application",
            "W6" => "needs a long-lived invocation",
            "W7" => "needs a dev tunnel or websocket",
            "W8" => "needs a local filesystem",
            "W10" => "needs redirects to rotating storage hosts",
            "W11" => "needs an OCI registry token exchange",
            "W12" => "needs local executable support",
            "W13" => "needs a local configuration store",
            "W14" => "needs a host-shell completion hook",
            "X1" | "X2" | "X3" | "X4" | "X5" | "X6" | "X7" => "by policy",
            _ => unreachable!("closed inventory reasons"),
        };
        Entry {
            command,
            kind,
            reason,
            flags: fields.next().unwrap_or_default(),
        }
    })
}

fn leaf<'a>(
    root: &'a mut Command,
    mut words: std::str::SplitWhitespace<'static>,
) -> &'a mut Command {
    let Some(word) = words.next() else {
        return root;
    };
    if root.find_subcommand(word).is_none() {
        *root = std::mem::take(root).subcommand(Command::new(word).hide(true));
    }
    leaf(
        root.find_subcommand_mut(word)
            .expect("registered subcommand"),
        words,
    )
}

pub(crate) fn register(mut root: Command) -> Command {
    let mut help = String::from("Not available in this environment:\n");
    for entry in entries() {
        if entry.kind == Kind::UnscopedSearch {
            help.push_str(&format!(
                "  gh {} without --owner or -R ({})\n",
                entry.command, entry.reason
            ));
            continue;
        }
        let command = leaf(&mut root, entry.command.split_whitespace());
        if entry.kind == Kind::Flag
            && command.is_hide_set()
            && !command
                .get_arguments()
                .any(|arg| arg.get_id() == "_unavailable_args")
        {
            *command = std::mem::take(command).arg(Arg::new("_unavailable_args").num_args(0..));
        }
        if (matches!(
            entry.command.split_whitespace().next(),
            Some(
                "pr" | "issue"
                    | "release"
                    | "run"
                    | "workflow"
                    | "cache"
                    | "secret"
                    | "variable"
                    | "ruleset"
                    | "label"
                    | "discussion"
            )
        ) || matches!(
            entry.command,
            "repo autolink list"
                | "repo autolink view"
                | "repo deploy-key add"
                | "repo deploy-key list"
        )) && !command.get_arguments().any(|arg| arg.get_id() == "repo")
        {
            *command = std::mem::take(command).arg(Arg::new("repo").short('R').long("repo"));
        }
        if entry.kind == Kind::Command {
            *command = std::mem::take(command)
                .hide(true)
                .arg(Arg::new("_unavailable_args").num_args(0..));
            help.push_str(&format!("  gh {} ({})\n", entry.command, entry.reason));
        } else {
            help.push_str(&format!(
                "  gh {}: {} ({})\n",
                entry.command, entry.flags, entry.reason
            ));
        }
        let rejected: Vec<_> = entries()
            .filter(|other| other.command == entry.command && other.kind == Kind::Flag)
            .flat_map(|other| {
                other
                    .flags
                    .split_whitespace()
                    .map(|flag| flag.split('/').next().expect("flag"))
            })
            .collect();
        let required: Vec<_> = command
            .get_arguments()
            .filter(|arg| arg.is_required_set())
            .map(|arg| arg.get_id().clone())
            .collect();
        for id in required {
            *command = std::mem::take(command).mut_arg(id, |arg| {
                arg.required(false).required_unless_present_any(&rejected)
            });
        }
        for flag in entry.flags.split_whitespace() {
            let (name, short) = flag
                .split_once('/')
                .map_or((flag, None), |(name, short)| (name, short.chars().next()));
            let mut arg = Arg::new(name)
                .long(name)
                .hide(true)
                .num_args(0..=1)
                .default_missing_value("");
            if let Some(short) = short {
                if short == 'h' {
                    *command = std::mem::take(command)
                        .disable_help_flag(true)
                        .arg(Arg::new("help").long("help").action(clap::ArgAction::Help));
                }
                arg = arg.short(short);
            }
            *command = std::mem::take(command).arg(arg);
        }
    }
    root.after_help(help)
}

fn message(entry: &Entry) -> String {
    format!(
        "gh {}: not available in this environment ({})",
        entry.command, entry.reason
    )
}

pub(crate) fn api_refusal() -> String {
    message(
        &entries()
            .find(|entry| entry.command == "api")
            .expect("API policy entry"),
    )
}

pub(crate) fn refusal(matches: &ArgMatches) -> Option<Usage> {
    let mut current = matches;
    let mut words = Vec::new();
    while let Some((name, child)) = current.subcommand() {
        words.push(name);
        current = child;
    }
    let command = words.join(" ");
    for entry in entries().filter(|entry| entry.command == command) {
        let unscoped = entry.kind == Kind::UnscopedSearch
            && current
                .try_get_one::<String>("owner")
                .ok()
                .flatten()
                .is_none()
            && current
                .try_get_one::<String>("repo")
                .ok()
                .flatten()
                .is_none();
        if entry.kind == Kind::Command
            || unscoped
            || entry.flags.split_whitespace().any(|flag| {
                let name = flag.split('/').next().expect("flag name");
                current.try_get_one::<String>(name).ok().flatten().is_some()
            })
        {
            return Some(Usage::new(message(&entry)));
        }
    }
    None
}
