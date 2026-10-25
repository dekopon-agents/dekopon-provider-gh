use clap::{Arg, ArgMatches, Command};
use dekopon_provider_sdk::provider::Usage;

#[derive(Clone, Copy)]
pub(crate) enum Kind {
    Command,
    Flag,
    UnscopedSearch,
    Syntax,
}

#[derive(Clone, Copy)]
pub(crate) enum Arity {
    Boolean,
    Value,
}

#[derive(Clone, Copy)]
pub(crate) struct Flag {
    pub name: &'static str,
    pub short: Option<char>,
    pub arity: Arity,
}

pub(crate) struct Entry {
    pub command: &'static str,
    pub kind: Kind,
    pub reason: &'static str,
    pub flags: &'static str,
}

impl Entry {
    pub(crate) fn flags(&self) -> impl Iterator<Item = Flag> {
        self.flags.split_whitespace().map(|token| {
            let (spelling, arity) = token.split_once(':').expect("registry flag arity");
            let arity = match arity {
                "bool" => Arity::Boolean,
                "value" => Arity::Value,
                _ => unreachable!("closed registry arities"),
            };
            let (name, short) = spelling
                .split_once('/')
                .map_or((spelling, None), |(name, short)| {
                    (name, short.chars().next())
                });
            Flag { name, short, arity }
        })
    }
}

pub(crate) fn entries() -> impl Iterator<Item = Entry> {
    include_str!("unavailable.tsv").lines().map(|line| {
        let mut fields = line.split('\t');
        let kind = match fields.next().expect("entry kind") {
            "command" => Kind::Command,
            "flag" => Kind::Flag,
            "scope" => Kind::UnscopedSearch,
            "syntax" => Kind::Syntax,
            _ => unreachable!("closed entry kinds"),
        };
        let command = fields.next().expect("command");
        let reason = match fields.next().expect("reason") {
            "-" => "",
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

fn flags(command: &mut Command, entries: &[Entry]) {
    for entry in entries {
        match entry.kind {
            Kind::Command | Kind::Flag => {}
            Kind::UnscopedSearch | Kind::Syntax => continue,
        }
        for flag in entry.flags() {
            let mut arg = Arg::new(flag.name).long(flag.name).hide(true);
            arg = match flag.arity {
                Arity::Boolean => arg
                    .action(clap::ArgAction::SetTrue)
                    .num_args(0..=1)
                    .require_equals(true)
                    .default_missing_value("true"),
                Arity::Value => arg.num_args(1).allow_hyphen_values(true),
            };
            if let Some(short) = flag.short {
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
                Kind::Syntax => {}
                Kind::Command => help.push_str(&format!("  gh {path} ({})\n", entry.reason)),
                Kind::Flag => {
                    for flag in entry.flags() {
                        let name = flag.name;
                        let short = flag
                            .short
                            .map_or(String::new(), |short| format!("/-{short}"));
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
            Kind::Syntax => {}
            Kind::Command => return Some(Usage::new(message(&entry, ""))),
            Kind::Flag => {
                for flag in entry.flags() {
                    if matches!(
                        current.value_source(flag.name),
                        Some(clap::parser::ValueSource::CommandLine)
                    ) {
                        return Some(Usage::new(message(&entry, &format!(" --{}", flag.name))));
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

enum RawFlag {
    Known(Flag),
    Unknown(String),
    MissingValue(Flag),
}

fn raw_flags(args: &[std::ffi::OsString], known: &[Flag]) -> Vec<RawFlag> {
    let mut output = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let arg = arg.to_string_lossy();
        if arg == "--" {
            break;
        }
        if let Some(long) = arg.strip_prefix("--") {
            let (name, inline) = long
                .split_once('=')
                .map_or((long, false), |(name, _)| (name, true));
            match known.iter().find(|flag| flag.name == name) {
                Some(flag) => {
                    output.push(RawFlag::Known(*flag));
                    match flag.arity {
                        Arity::Boolean => {}
                        Arity::Value => {
                            if !inline && args.next().is_none() {
                                output.push(RawFlag::MissingValue(*flag));
                            }
                        }
                    }
                }
                None => output.push(RawFlag::Unknown(arg.into_owned())),
            }
        } else if let Some(shorts) = arg.strip_prefix('-') {
            for (index, short) in shorts.char_indices() {
                match known.iter().find(|flag| flag.short == Some(short)) {
                    Some(flag) => {
                        output.push(RawFlag::Known(*flag));
                        match flag.arity {
                            Arity::Boolean => {
                                if shorts[index + short.len_utf8()..].starts_with('=') {
                                    break;
                                }
                            }
                            Arity::Value => {
                                if index + short.len_utf8() == shorts.len() && args.next().is_none()
                                {
                                    output.push(RawFlag::MissingValue(*flag));
                                }
                                break;
                            }
                        }
                    }
                    None => output.push(RawFlag::Unknown(format!("-{short}"))),
                }
            }
        }
    }
    output
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
        .filter_map(|(path, entries)| {
            let words: Vec<_> = path
                .split_whitespace()
                .map(std::ffi::OsString::from)
                .collect();
            let args = argv.get(1..)?.strip_prefix(words.as_slice())?;
            Some((*path, entries, args))
        })
        .max_by_key(|(path, _, _)| path.len());
    let mut command = base();
    if let Some((path, entries, args)) = candidate {
        let mut known: Vec<_> = entries.iter().flat_map(|entry| entry.flags()).collect();
        known.push(Flag {
            name: "help",
            short: Some('h'),
            arity: Arity::Boolean,
        });
        if matches!(
            path,
            "pr checkout" | "run download" | "run watch" | "repo deploy-key add"
        ) {
            known.push(Flag {
                name: "repo",
                short: Some('R'),
                arity: Arity::Value,
            });
        }
        let raw = raw_flags(args, &known);
        let refusal = entries.iter().find_map(|entry| match entry.kind {
            Kind::Command => Some(Usage::new(message(entry, ""))),
            Kind::Flag => entry.flags().find_map(|flag| {
                raw.iter()
                    .any(|raw| match raw {
                        RawFlag::Known(found) => found.name == flag.name,
                        RawFlag::Unknown(_) | RawFlag::MissingValue(_) => false,
                    })
                    .then(|| Usage::new(message(entry, &format!(" --{}", flag.name))))
            }),
            Kind::UnscopedSearch | Kind::Syntax => None,
        });
        if let Some(refusal) = refusal {
            for flag in raw {
                match flag {
                    RawFlag::Known(_) => {}
                    RawFlag::Unknown(flag) => {
                        return Err(command.error(
                            clap::error::ErrorKind::UnknownArgument,
                            format!("unexpected argument '{flag}' found"),
                        ));
                    }
                    RawFlag::MissingValue(flag) => {
                        return Err(command.error(
                            clap::error::ErrorKind::InvalidValue,
                            format!("a value is required for '--{} <VALUE>'", flag.name),
                        ));
                    }
                }
            }
            return Ok(Some(refusal));
        }
    }
    match command.try_get_matches_from_mut(&argv) {
        Err(error) => Err(error),
        Ok(_) => Err(command.error(
            clap::error::ErrorKind::UnknownArgument,
            format!(
                "unexpected argument '{}'",
                argv.get(1)
                    .map(|arg| arg.to_string_lossy())
                    .unwrap_or_default()
            ),
        )),
    }
}
