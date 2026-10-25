use clap::{Arg, ArgMatches, Command};
use dekopon_provider_sdk::provider::Usage;

#[derive(Clone, Copy)]
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

type Groups = std::collections::BTreeMap<&'static str, Vec<Entry>>;

fn groups() -> Groups {
    let mut groups = Groups::new();
    for entry in entries() {
        groups.entry(entry.command).or_default().push(entry);
    }
    groups
}

fn find_leaf<'a>(
    root: &'a mut Command,
    words: std::str::SplitWhitespace<'static>,
) -> Option<&'a mut Command> {
    let mut command = root;
    for word in words {
        command = command.find_subcommand_mut(word)?;
    }
    Some(command)
}

fn add_leaf<'a>(
    root: &'a mut Command,
    mut words: std::str::SplitWhitespace<'static>,
) -> &'a mut Command {
    let Some(word) = words.next() else {
        return root;
    };
    if root.find_subcommand(word).is_none() {
        *root = std::mem::take(root).subcommand(Command::new(word));
    }
    add_leaf(
        root.find_subcommand_mut(word).expect("registered command"),
        words,
    )
}

fn flags(command: &mut Command, entries: &[Entry]) {
    for entry in entries {
        match entry.kind {
            Kind::Command | Kind::Flag => {}
            Kind::UnscopedSearch => continue,
        }
        for flag in entry.flags.split_whitespace() {
            let (name, short) = split_flag(flag);
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
}

fn split_flag(flag: &'static str) -> (&'static str, Option<char>) {
    flag.split_once('/')
        .map_or((flag, None), |(name, short)| (name, short.chars().next()))
}

fn external_commands(mut command: Command) -> Command {
    let children: Vec<_> = command
        .get_subcommands()
        .map(|child| child.get_name().to_owned())
        .collect();
    if !children.is_empty() {
        command = command.allow_external_subcommands(true);
        for child in children {
            command = command.mut_subcommand(child, external_commands);
        }
    }
    command
}

pub(crate) fn register(mut root: Command) -> Command {
    let mut help = String::from("Not available in this environment:\n");
    for (path, entries) in groups() {
        for entry in &entries {
            match entry.kind {
                Kind::Command => help.push_str(&format!("  gh {path} ({})\n", entry.reason)),
                Kind::Flag => {
                    for flag in entry.flags.split_whitespace() {
                        let (name, short) = split_flag(flag);
                        let short = short.map_or(String::new(), |short| format!("/-{short}"));
                        help.push_str(&format!("  gh {path} --{name}{short} ({})\n", entry.reason));
                    }
                }
                Kind::UnscopedSearch => help.push_str(&format!(
                    "  gh {path} without --owner or -R ({})\n",
                    entry.reason
                )),
            }
        }
        if let Some(command) = find_leaf(&mut root, path.split_whitespace()) {
            flags(command, &entries);
        }
    }
    external_commands(root.after_help(help))
}

fn message(entry: &Entry, subject: &str) -> String {
    format!(
        "gh {}{subject}: not available in this environment ({})",
        entry.command, entry.reason
    )
}

pub(crate) fn api_refusal() -> String {
    let entry = entries()
        .find(|entry| entry.command == "api")
        .expect("API policy entry");
    message(&entry, "")
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
        match entry.kind {
            Kind::Command => return Some(Usage::new(message(&entry, ""))),
            Kind::Flag => {
                for flag in entry.flags.split_whitespace() {
                    let (name, _) = split_flag(flag);
                    if current.try_get_one::<String>(name).ok().flatten().is_some() {
                        return Some(Usage::new(message(&entry, &format!(" --{name}"))));
                    }
                }
            }
            Kind::UnscopedSearch => {
                if current
                    .try_get_one::<String>("owner")
                    .ok()
                    .flatten()
                    .is_none()
                    && current
                        .try_get_one::<String>("repo")
                        .ok()
                        .flatten()
                        .is_none()
                {
                    return Some(Usage::new(message(&entry, " without --owner or -R")));
                }
            }
        }
    }
    None
}

pub(crate) fn external_refusal(
    matches: &ArgMatches,
    base: impl FnOnce() -> Command,
) -> Result<Option<Usage>, clap::Error> {
    let mut current = matches;
    let mut argv = vec![std::ffi::OsString::from("gh")];
    while let Some((name, child)) = current.subcommand() {
        argv.push(name.into());
        current = child;
    }
    let Some(raw) = current
        .try_get_many::<std::ffi::OsString>("")
        .ok()
        .flatten()
    else {
        return Ok(None);
    };
    argv.extend(raw.cloned());
    let groups = groups();
    let candidate = groups
        .iter()
        .filter(|(path, entries)| {
            let words: Vec<_> = path.split_whitespace().collect();
            argv[1..].starts_with(
                &words
                    .iter()
                    .map(std::ffi::OsString::from)
                    .collect::<Vec<_>>(),
            ) && entries.iter().any(|entry| match entry.kind {
                Kind::Command => true,
                Kind::Flag => entry.flags.split_whitespace().any(|flag| {
                    let (name, short) = split_flag(flag);
                    argv.iter()
                        .take_while(|arg| arg.as_os_str() != std::ffi::OsStr::new("--"))
                        .any(|arg| {
                            let arg = arg.to_string_lossy();
                            arg == format!("--{name}")
                                || arg.starts_with(&format!("--{name}="))
                                || short.is_some_and(|short| {
                                    arg.starts_with(&format!("-{short}")) && !arg.starts_with("--")
                                })
                        })
                }),
                Kind::UnscopedSearch => false,
            })
        })
        .max_by_key(|(path, _)| path.len());
    let Some((path, entries)) = candidate else {
        return Err(base()
            .try_get_matches_from_mut(argv)
            .expect_err("external command is absent from the supported tree"));
    };
    let mut parser = Command::new("gh");
    let command = add_leaf(&mut parser, path.split_whitespace());
    *command = std::mem::take(command).arg(
        Arg::new("arguments")
            .value_name("ARG")
            .hide(true)
            .num_args(0..),
    );
    flags(command, entries);
    if (matches!(
        path.split_whitespace().next(),
        Some(
            "pr" | "issue"
                | "run"
                | "release"
                | "workflow"
                | "label"
                | "discussion"
                | "cache"
                | "secret"
                | "variable"
                | "ruleset"
        )
    ) || path.starts_with("repo deploy-key ")
        || path.starts_with("repo autolink "))
        && !command.get_arguments().any(|arg| arg.get_id() == "repo")
    {
        *command = std::mem::take(command).arg(Arg::new("repo").long("repo").short('R'));
    }
    let parsed = parser.try_get_matches_from(argv.clone())?;
    match refusal(&parsed) {
        Some(refusal) => Ok(Some(refusal)),
        None => Err(base()
            .try_get_matches_from_mut(argv)
            .expect_err("external command is absent from the supported tree")),
    }
}
