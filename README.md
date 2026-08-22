# dekopon-provider-gh

The GitHub provider for [Dekopon](https://github.com/dekopon-agents/dekopon), as a WebAssembly component.

Nineteen narrow repository, pull-request, and issue capabilities with fixed request shapes, bounded
output projections, and SHA-pinned review/merge writes. There is deliberately no `gh.api.*`
passthrough: a path-level escape hatch would collapse per-capability policy into "everything the
credential can reach".

The component never sets `authorization`. The broker injects a destination-bound credential at the
native HTTP boundary, where no guest can observe it.

## Command words

The component exports `resolve-command`, so a model can type GitHub-CLI spellings:

```
gh pr view 7 -R owner/repo
gh pr review 7 -R owner/repo --approve
gh issue comment 3 -R owner/repo --body "..."
```

Each maps to exactly one `gh.*` capability. The rewrite is a pure function that *proposes*: the
broker then authorizes it on the identical path a direct `gh.pull-request.read --number 7` takes.
Naming a capability the caller was not granted produces a denial, not an escalation.

Flags that would change what a command means — `--json`, `--jq`, `--web`, `--checkout` — are
rejected by name rather than accepted as no-ops. Output is always a structured JSON value; filter it
with the shell's `jq` builtin.

## Capabilities

| Capability | Effect |
|---|---|
| `gh.content.read` | read-only |
| `gh.pull-request.list` / `.read` / `.files` / `.diff` / `.reviews` / `.status` | read-only |
| `gh.pull-request.approve` / `.comment` / `.request-changes` / `.merge` | external-write |
| `gh.repo.read` / `gh.branch.read` / `gh.commit.read` / `gh.user.read` | read-only |
| `gh.issue.read` / `.list` / `gh.issue-comments.read` | read-only |
| `gh.issue.comment` | external-write |

## Using it

The component grants nothing on its own. An operator points `dekopon-brokerd` at it and writes a
constraint set per capability — allowed hosts, methods, request counts, timeouts, and the symbolic
credential to inject. See [the broker's configuration reference](https://github.com/dekopon-agents/dekopon/blob/main/crates/dekopon-brokerd/README.md)
and [`examples/pr-summarizer-linter/`](examples/pr-summarizer-linter/README.md), an end-to-end
walkthrough of a Slack-driven pull-request reviewer built on these capabilities. It moved here from
the dekopon tree with the provider, because it exercises this component rather than dekopon's own
machinery.

Drop `gh-provider.wasm` into a provider directory the broker loads:

```yaml
providers:
  - /opt/dekopon/providers
```

## Releases

Each tag publishes `gh-provider.wasm` two ways:

- a **release asset** with a `.sha256` alongside it and a provenance attestation, verifiable with
  `gh attestation verify gh-provider.wasm --repo dekopon-agents/dekopon-provider-gh`;
- an **OCI artifact** at `ghcr.io/dekopon-agents/provider-gh`, pullable by tag or digest.

The release workflow rebuilds the component a second time into a clean target directory and
byte-compares before publishing, so a tag that ships is a tag that reproduced. Attestation proves
who built the artifact; the rebuild proves what it was built from.

## Building

Both pins are exact, because neither Rust codegen nor component encoding is stable across
versions and the build asserts its own reproducibility:

```console
rustup toolchain install 1.97.0 --profile minimal
cargo install wasm-tools --version 1.236.1 --locked
./build.sh
```

`build.sh` is a self-contained port of dekopon's `examples/providers/build-component.sh`, and it
keeps every mechanism that made the in-tree component reproducible: a `rustc` proxy that
normalizes `-Cmetadata` to a fixed salt (`dekopon-provider-repro-v1`), `--remap-path-prefix` for
the source root, the Cargo home and the toolchain sysroot, `-Ccodegen-units=1`, and a final scan
that fails the build if any local path survives into the component. Given the same source and the
same two pins, it lands on the same bytes on any machine.

`cargo test` runs the subcommand table and capability mapping natively; nothing contacts GitHub.

## License

MIT or Apache-2.0, at your option.
