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

pub(crate) enum Parsing {
    Flags,
    PassThrough,
    NoPositionals,
    Group,
}

pub(crate) struct Entry {
    pub command: &'static str,
    pub kind: Kind,
    pub reason: &'static str,
    pub flags: &'static str,
    pub parsing: Parsing,
    pub aliases: &'static str,
    pub inherited: &'static str,
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
            "W8-upload" => "needs local file access",
            "W10" => "needs redirects to rotating storage hosts",
            "W11" => "needs an OCI registry token exchange",
            "W12" => "needs local executable support",
            "W13" => "needs a local configuration store",
            "W14" => "needs a host-shell completion hook",
            "X1" | "X2" | "X3" | "X4" | "X5" | "X6" | "X7" => "by policy",
            _ => unreachable!("closed inventory reasons"),
        };
        let flags = fields.next().unwrap_or_default();
        let parsing = match fields.next() {
            None | Some("") => Parsing::Flags,
            Some("pass-through") => Parsing::PassThrough,
            Some("no-positionals") => Parsing::NoPositionals,
            Some("group") => Parsing::Group,
            Some(_) => unreachable!("closed registry parsing modes"),
        };
        Entry {
            command,
            kind,
            reason,
            flags,
            parsing,
            aliases: fields.next().unwrap_or_default(),
            inherited: fields.next().unwrap_or_default(),
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

fn leaf<'a>(root: &'a mut Command, path: &'static str) -> &'a mut Command {
    let mut command = root;
    for word in path.split_whitespace() {
        if command.find_subcommand(word).is_none() {
            *command = std::mem::take(command).subcommand(
                Command::new(word)
                    .hide(true)
                    .subcommand_required(true)
                    .disable_help_flag(true)
                    .disable_help_subcommand(true),
            );
        }
        command = command.find_subcommand_mut(word).expect("declared command");
    }
    command
}

fn flag_arg(flag: Flag) -> Arg {
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
        arg = arg.short(short);
    }
    arg
}

fn required_globals(mut command: Command, inherited: &[clap::Id], path: String) -> Command {
    let mut globals = inherited.to_vec();
    globals.extend(
        command
            .get_arguments()
            .filter(|arg| arg.is_global_set())
            .map(|arg| arg.get_id().clone()),
    );
    let required: Vec<_> = command
        .get_arguments()
        .filter(|arg| arg.is_required_set() && globals.contains(arg.get_id()))
        .map(|arg| arg.get_id().clone())
        .collect();
    if !required.is_empty() {
        let usage = command
            .clone()
            .bin_name(path.clone())
            .render_usage()
            .to_string();
        command =
            command.override_usage(usage.strip_prefix("Usage: ").unwrap_or(&usage).to_owned());
        // Clap validates required flags before propagating globals, so validate these from completed ArgMatches.
        for id in required {
            command = command.mut_arg(id, |arg| arg.required(false));
        }
    }
    let children: Vec<_> = command
        .get_subcommands()
        .map(|child| child.get_name().to_owned())
        .collect();
    for name in children {
        let child_path = format!("{path} {name}");
        command =
            command.mut_subcommand(name, |child| required_globals(child, &globals, child_path));
    }
    command
}

pub(crate) fn register(mut root: Command) -> Command {
    let mut help = String::from("Not available in this environment:\n");
    for (path, entries) in groups() {
        let existing = {
            let mut node = Some(&root);
            for word in path.split_whitespace() {
                node = node.and_then(|node| node.find_subcommand(word));
            }
            node.is_some()
        };
        let command = leaf(&mut root, path);
        let mut parsing = Parsing::Flags;
        for entry in &entries {
            *command = std::mem::take(command).aliases(entry.aliases.split_whitespace());
            match entry.parsing {
                Parsing::Flags => {}
                Parsing::PassThrough => parsing = Parsing::PassThrough,
                Parsing::NoPositionals => parsing = Parsing::NoPositionals,
                Parsing::Group => parsing = Parsing::Group,
            }
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
            for flag in entry.flags() {
                if command.get_arguments().any(|arg| arg.get_id() == flag.name) {
                    continue;
                }
                if flag.short == Some('h') {
                    *command = std::mem::take(command)
                        .disable_help_flag(true)
                        .arg(Arg::new("help").long("help").action(clap::ArgAction::Help));
                }
                let inherited = entry
                    .inherited
                    .split_whitespace()
                    .any(|name| name == flag.name);
                *command = std::mem::take(command).arg(flag_arg(flag).global(inherited));
            }
        }
        if !existing {
            *command = std::mem::take(command)
                .disable_help_flag(true)
                .disable_help_subcommand(true);
            if command.get_arguments().any(|arg| arg.get_id() == "help") {
                *command = std::mem::take(command).mut_arg("help", |arg| {
                    arg.action(clap::ArgAction::SetTrue)
                        .num_args(0..=1)
                        .require_equals(true)
                        .default_missing_value("true")
                });
            } else {
                let short = (!command
                    .get_arguments()
                    .any(|arg| arg.get_short() == Some('h')))
                .then_some('h');
                *command = std::mem::take(command).arg(flag_arg(Flag {
                    name: "help",
                    short,
                    arity: Arity::Boolean,
                }));
            }
            match parsing {
                Parsing::Flags => {
                    *command = std::mem::take(command)
                        .subcommand_required(false)
                        .arg(Arg::new("arguments").hide(true).num_args(0..));
                }
                Parsing::PassThrough => {
                    *command = std::mem::take(command).subcommand_required(false).arg(
                        Arg::new("arguments")
                            .hide(true)
                            .num_args(0..)
                            .allow_hyphen_values(true)
                            .trailing_var_arg(true),
                    );
                }
                Parsing::NoPositionals => {
                    *command = std::mem::take(command).subcommand_required(false);
                }
                Parsing::Group => {}
            }
        }
    }
    required_globals(root.after_help(help), &[], "gh".to_owned())
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
