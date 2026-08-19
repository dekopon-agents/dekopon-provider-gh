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
and the `examples/rubber-stamper` walkthrough.

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

## Building

Requires the pinned `wasm-tools`, because component encoding is not stable across versions:

```console
cargo install wasm-tools --version 1.236.1 --locked
./build.sh
```

`cargo test` runs the subcommand table and capability mapping natively; nothing contacts GitHub.

## License

MIT or Apache-2.0, at your option.
