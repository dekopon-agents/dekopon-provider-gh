use std::collections::{BTreeMap, BTreeSet};

use dekopon_broker_host::{BrokerHostLimits, BrokerProviderRegistry};
use dekopon_gh_provider::Gh;
use dekopon_provider_sdk::{CommandRunOutcome, provider};

#[expect(
    dead_code,
    reason = "Tests consume the production refusal registry data."
)]
#[path = "../src/unavailable.rs"]
mod unavailable;

struct NoHttp;

impl provider::Port for NoHttp {
    fn now_unix_millis(&mut self) -> u64 {
        panic!("help read the clock")
    }
    fn now_nanos(&mut self) -> u64 {
        panic!("help read the clock")
    }
    fn fill_random(&mut self, _: &mut [u8]) {
        panic!("help requested entropy")
    }
    fn settings(&mut self) -> Option<String> {
        panic!("help read settings")
    }
    fn send(&mut self, _: provider::Request) -> Result<provider::Response, provider::HttpError> {
        panic!("help sent HTTP")
    }
    fn stream(
        &mut self,
        _: provider::StreamedRequest<'_>,
    ) -> Result<provider::StreamedResponse, provider::HttpError> {
        panic!("help streamed HTTP")
    }
}

fn registry(runtime: &tokio::runtime::Runtime) -> BrokerProviderRegistry {
    let component = std::env::var_os("DEKOPON_PROVIDER_COMPONENT")
        .expect("DEKOPON_PROVIDER_COMPONENT must name the freshly built component");
    runtime
        .block_on(BrokerProviderRegistry::load(
            [std::path::PathBuf::from(component)],
            BrokerHostLimits::default(),
        ))
        .expect("load real component")
}

fn page(
    runtime: &tokio::runtime::Runtime,
    registry: &BrokerProviderRegistry,
    argv: &[&str],
) -> String {
    let argv: Vec<_> = argv.iter().map(|word| (*word).to_owned()).collect();
    let native = provider::with_port(NoHttp, || {
        dekopon_provider_sdk::__typed_run_command::<Gh>(&argv, false)
    });
    let CommandRunOutcome::Rendered {
        stdout,
        stderr,
        status,
    } = runtime
        .block_on(registry.run_command("gh", &argv, false))
        .expect("import-free help")
    else {
        panic!("help proposed or failed: {argv:?}")
    };
    assert_eq!(status, 0, "{argv:?}: {stderr}");
    assert!(stderr.is_empty(), "{argv:?}: {stderr}");
    let CommandRunOutcome::Rendered {
        stdout: native_stdout,
        stderr: native_stderr,
        status: native_status,
    } = serde_json::from_str(&native).expect("native outcome")
    else {
        panic!("native help proposed or failed: {argv:?}")
    };
    assert_eq!(
        (native_stdout, native_stderr, native_status),
        (stdout.clone(), stderr, status)
    );
    assert!(!stdout.contains("[--help truncated"), "{argv:?}");
    stdout
}

#[test]
fn the_real_component_root_is_complete_below_the_core_help_limit() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let registry = registry(&runtime);
    let root = page(&runtime, &registry, &["--help"]);
    eprintln!(
        "root help: {} bytes; target <= 1800; hard limit < 2048",
        root.len()
    );
    assert!(root.len() < 2048, "root help: {} bytes", root.len());
    for group in [
        "api",
        "auth",
        "pr",
        "repo",
        "issue",
        "search",
        "version",
        "licenses",
        "actions",
        "accessibility",
    ] {
        assert!(
            root.lines()
                .any(|line| line.trim_start().starts_with(&format!("{group} "))),
            "missing {group}: {root}"
        );
    }
    assert!(root.contains("no raw API passthrough"));
    for route in [
        "user",
        "users/LOGIN",
        "repos/OWNER/REPO/branches/BRANCH",
        "repos/OWNER/REPO/commits/REF",
    ] {
        assert!(root.contains(route));
    }
    assert!(root.contains("gh help GROUP"));
    assert!(root.contains("full inventory"));
    assert!(!root.contains("gh pr view --web"));
}

#[test]
fn every_visible_registry_exclusion_has_a_reason_on_real_component_group_help() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let registry = registry(&runtime);
    let root = page(&runtime, &registry, &["--help"]);
    let groups: BTreeSet<_> = unavailable::entries()
        .map(|entry| entry.command.split_whitespace().next().unwrap())
        .collect();
    let mut pages = BTreeMap::new();
    for group in groups {
        assert!(root.contains(group), "root must name {group}");
        let help = page(&runtime, &registry, &["help", group]);
        if [
            "api",
            "auth",
            "pr",
            "repo",
            "issue",
            "search",
            "accessibility",
        ]
        .contains(&group)
        {
            assert_eq!(help, page(&runtime, &registry, &[group, "--help"]));
        }
        pages.insert(group, help);
    }
    for entry in unavailable::entries() {
        let group = entry.command.split_whitespace().next().unwrap();
        let help = &pages[group];
        let mut subjects = vec![];
        match entry.kind {
            unavailable::Kind::Syntax => continue,
            unavailable::Kind::Command => subjects.push(format!("gh {}", entry.command)),
            unavailable::Kind::Flag => {
                for flag in entry.flags() {
                    let short = flag
                        .short
                        .map_or(String::new(), |short| format!("/-{short}"));
                    subjects.push(format!("gh {} --{}{short}", entry.command, flag.name));
                }
            }
            unavailable::Kind::UnscopedSearch => {
                subjects.push(format!("gh {} without --owner or -R", entry.command))
            }
        }
        for subject in subjects {
            let line = format!("{subject} ({})", entry.reason);
            assert!(
                help.lines().any(|candidate| candidate.trim() == line),
                "missing {line} on {group} help"
            );
        }
    }
    let leaf = page(&runtime, &registry, &["pr", "view", "--help"]);
    assert!(leaf.contains("gh pr view --web/-w (needs a browser or local application)"));
}

#[test]
fn broker_host_cache_preserves_the_real_component_root_page_byte_for_byte() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let registry = registry(&runtime);
    let root = page(&runtime, &registry, &["--help"]);
    let cached = registry.command_word_help();
    eprintln!("broker-host =0.58.0 cached {} root bytes", root.len());
    assert_eq!(cached.get("gh"), Some(&root));
    assert!(!cached["gh"].contains("[--help truncated"));
}

#[test]
fn non_help_usage_and_refusal_selection_remain_unchanged_in_the_real_component() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let registry = registry(&runtime);
    let run = |words: &[&str]| {
        let argv: Vec<_> = words.iter().map(|word| (*word).to_owned()).collect();
        let native = provider::with_port(NoHttp, || {
            dekopon_provider_sdk::__typed_run_command::<Gh>(&argv, false)
        });
        [
            serde_json::from_str::<CommandRunOutcome>(&native).expect("native outcome"),
            runtime
                .block_on(registry.run_command("gh", &argv, false))
                .expect("import-free non-help command"),
        ]
    };
    for (argv, usage) in [
        (
            vec!["auth", "git-credential", "--definitely-a-typo"],
            "Usage: gh auth git-credential",
        ),
        (vec!["bogus"], "Usage: gh <COMMAND>"),
        (
            vec!["pr", "view", "--web"],
            "Usage: gh pr view [OPTIONS] --repo <OWNER/REPO> <NUMBER>",
        ),
        (
            vec![
                "pr",
                "view",
                "7",
                "-R",
                "octo/hello",
                "--definitely-a-typo",
                "--web",
            ],
            "Usage: gh pr view [OPTIONS] --repo <OWNER/REPO> <NUMBER>",
        ),
    ] {
        for outcome in run(&argv) {
            let CommandRunOutcome::Rendered {
                stdout,
                stderr,
                status,
            } = outcome
            else {
                panic!("clap usage must take precedence over refusal: {argv:?}")
            };
            assert_eq!(status, 2, "{argv:?}");
            assert!(stdout.is_empty(), "{argv:?}");
            let usage_lines: Vec<_> = stderr
                .lines()
                .filter(|line| line.starts_with("Usage:"))
                .collect();
            assert!(!usage_lines.is_empty(), "{argv:?}: {stderr}");
            for line in usage_lines {
                assert_eq!(line, usage, "{argv:?}");
            }
        }
    }
    for (argv, refusal) in [
        (
            vec!["auth", "git-credential"],
            "gh auth git-credential: not available in this environment (needs a local git checkout)",
        ),
        (
            vec![
                "pr",
                "view",
                "7",
                "-R",
                "octo/hello",
                "--jq",
                ".title",
                "--web",
            ],
            "gh pr view --web: not available in this environment (needs a browser or local application)",
        ),
        (
            vec!["pr", "view", "7", "-R", "octo/hello", "--jq", "--web"],
            "gh pr view --jq: not available in this environment (by policy)",
        ),
        (
            vec!["search", "prs", "term"],
            "gh search prs without --owner or -R: not available in this environment (by policy)",
        ),
    ] {
        for outcome in run(&argv) {
            let CommandRunOutcome::Failed { error } = outcome else {
                panic!("registry refusal must remain selected: {argv:?}")
            };
            assert_eq!(error.code, "usage", "{argv:?}");
            assert_eq!(error.message, refusal, "{argv:?}");
        }
    }
}
