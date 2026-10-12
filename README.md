# dekopon-provider-gh

The GitHub provider for [Dekopon](https://github.com/dekopon-agents/dekopon), as a WebAssembly component.

Twenty narrow authentication, repository, pull-request, and issue capabilities with fixed request shapes, bounded
output projections, and SHA-pinned review/merge writes. There is deliberately no `gh.api.*`
passthrough: a path-level escape hatch would collapse per-capability policy into "everything the
credential can reach".

The component never sets `authorization`. The broker injects a destination-bound credential at the
native HTTP boundary, where no guest can observe it.

## The `gh` command word

The component exports `run-command`, so the word behaves like the upstream command-line tool:

```
gh --help
gh auth status
gh pr view 7 -R owner/repo
gh pr review 7 -R owner/repo --approve
gh issue comment 3 -R owner/repo --body "..."
echo "ship it" | gh pr review 7 -R owner/repo --comment --body-file -
```

`gh --help` and `gh pr view --help` render on stdout at status 0; `gh bogus` and a missing argument
render a usage error on stderr at status 2, so `$(gh bogus)` captures nothing while the error still
reaches the model. Neither renders from the network or grants anything.

Every other argv maps to exactly one `gh.*` capability. The rewrite is a pure function that
*proposes*: the broker then authorizes it on the identical path a direct
`gh.pull-request.read --number 7` takes. Naming a capability the caller was not granted produces a
denial, not an escalation.

`--body-file -` is the one argument whose value is piped into the word. Proposal records only a
marker; the authorized invocation reads at most 4 KiB of UTF-8 from stdin before any HTTP call.
Nothing piped is a usage error rather than an empty comment. There is no `gh api`, so nothing pipes into a passthrough —
a path-level escape hatch would collapse per-capability policy into "everything the credential can
reach", and typing it says so.

Flags that would change what a command means — `--json`, `--jq`, `--web`, `--checkout` — are
rejected by name rather than accepted as no-ops. Successful invocations write bounded projected
JSON to stdout; failures write to stderr with a nonzero status. Filter stdout with the shell's
`jq` builtin.

## Capabilities

| Capability | Effect |
|---|---|
| `gh.auth.status` | read-only |
| `gh.content.read` | read-only |
| `gh.pull-request.list` / `.read` / `.files` / `.diff` / `.reviews` / `.status` | read-only |
| `gh.pull-request.approve` / `.comment` / `.request-changes` / `.merge` | external-write |
| `gh.repo.read` / `gh.branch.read` / `gh.commit.read` / `gh.user.read` | read-only |
| `gh.issue.read` / `.list` / `gh.issue-comments.read` | read-only |
| `gh.issue.comment` | external-write |

`gh.pull-request.status` reads the pull request's head, then lists GitHub Actions workflow runs and
legacy commit statuses at that SHA. This deliberately avoids the Checks REST API: GitHub documents
`checks:read` for fine-grained personal access tokens but does not expose that permission in the
token editor. The replacement uses the available **Actions: Read-only** and **Commit statuses:
Read-only** permissions, stays entirely on bounded GET requests, and reports the two sources
separately rather than pretending workflow runs are check runs. Checks-only third-party integrations
remain outside this capability. The constraint set must allow three GETs (pull metadata, Actions,
then commit statuses); a two-request constraint from v0.1.0 fails closed before the final read.

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

### Credentials and authentication status

Create a [personal access token](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/managing-your-personal-access-tokens)
in GitHub Settings → Developer settings → Personal access tokens. For a fine-grained PAT, select
its resource owner, expiration, and only the repositories the provider should reach; grant the
repository permissions needed by your enabled capabilities (see the map below). Complete
organization approval when required. For a classic PAT, choose the scopes the endpoints need
(`repo` for private repository access), set an expiration, and authorize organization SSO when
required. Classic scopes can cover more repositories than a fine-grained token.

For a GitHub App, register the App with the repository permissions your enabled capabilities need
and install it on the intended account and repositories. An owner-side process uses the App's
private key to sign a JWT and
[generate an installation access token](https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-an-installation-access-token-for-a-github-app).
Store the installation token in the broker's credential binding, not the JWT or private key.
Installation tokens expire after one hour; renew them owner-side and restart the broker to load the
replacement before expiration. This provider does not mint or refresh tokens.

For either credential kind, use the broker's owner-only credentials file and bind its symbolic
name to each granted capability's constraint set. Bind its destinations to `api.github.com`, or
the authority in the owner's `baseUrl`; the guest receives no token. See the
[credential template](examples/pr-summarizer-linter/broker-credentials.yaml.example) and
[broker constraints](examples/pr-summarizer-linter/broker.yaml). Never put a token in capability
input, model text, or provider settings.

Grant each capability separately; the [reviewer example](examples/pr-summarizer-linter/README.md)
shows the broker grant steps. `gh.auth.status` is read-only, Low risk, and needs one allowed GET.
It reports quota and available credential-header metadata, not repository visibility or credential
kind.

For fine-grained PATs and GitHub App installations, grant repository permissions for the
capabilities you enable (Read unless marked Write):

| Capability | Fine-grained permission |
|---|---|
| `gh.auth.status`, `gh.user.read` | None (public endpoint / rate limit) |
| `gh.repo.read` | Metadata: Read |
| `gh.content.read`, `gh.branch.read`, `gh.commit.read` | Contents: Read |
| `gh.pull-request.list`, `.read`, `.files`, `.diff`, `.reviews` | Pull requests: Read |
| `gh.pull-request.status` | Pull requests: Read; Actions: Read; Commit statuses: Read |
| `gh.pull-request.approve`, `.comment`, `.request-changes` | Pull requests: Write |
| `gh.pull-request.merge` | Pull requests: Write; Contents: Write |
| `gh.issue.read`, `.list`, `gh.issue-comments.read` | Issues: Read |
| `gh.issue.comment` | Issues: Write |

| Error code | Message | What to do |
|---|---|---|
| `unauthorized` | `credential rejected: expired, revoked or malformed` | Replace or renew the broker-bound token and restart the broker to load it. |
| `forbidden` | `credential lacks a permission this call needs; GitHub accepts: <permissions>` | Check the fine-grained PAT or App permissions GitHub names, repository selection, and organization approval. |
| `forbidden` | `credential lacks a scope; GitHub accepts: <scopes>; credential has: <scopes>` | Compare accepted classic PAT scopes with held scopes; grant only what this call needs. Missing held-scope metadata is reported as `not reported`. |
| `forbidden` | `credential is not permitted to make this call` | Check credential permissions, repository access, and organization restrictions; GitHub supplied no accepted-permission or accepted-scope header. |
| `not-found` | `not found, or not visible to this credential` | Check the resource spelling and the token's repository access; a 404 does not prove absence. |
| `rate-limited` | `endpoint rate limit is exhausted` | Wait for the quota reset or GitHub's retry guidance before retrying. |

Accepted GitHub permissions take precedence over OAuth scopes when both headers are present.
A 403 with `x-ratelimit-remaining: 0` and every 429 retain the rate-limit error. Diagnostics quote
only the named permission/scope headers, never the response body.

### `baseUrl`

Requests go to `https://api.github.com`. To reach GitHub Enterprise Server, or a
[`cassette`](https://github.com/dekopon-agents/cassette) recorder, the owner sets one key in
`broker.yaml`:

```yaml
providerSettings:
  gh:
    baseUrl: https://ghe.example.com/api/v3
```

`baseUrl` is an `http://` or `https://` URL with an optional path prefix and no userinfo, query or
fragment; one trailing `/` is dropped. An invalid value fails `invalid-settings` before any request.
No capability input names the origin, so a model cannot choose where a call goes. The setting
only moves where the component sends: the capability's `allowedHosts` (plus
`allowPlaintextLoopback` for an `http://` loopback recorder) and the credential's `destinations`
still decide what a call may reach, so a new `baseUrl` needs both updated to its authority.

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
rustup toolchain install 1.98.1 --profile minimal
cargo install wasm-tools --version 1.259.0 --locked
../provider-workflows/build.sh
```

`build.sh` now lives in [`dekopon-agents/provider-workflows`](https://github.com/dekopon-agents/provider-workflows),
cloned next to this repository, and is shared across every Dekopon provider: a `rustc` proxy that
normalizes `-Cmetadata` to a fixed salt (`dekopon-provider-repro-v1`), `--remap-path-prefix` for
the source root, the Cargo home and the toolchain sysroot, `-Ccodegen-units=1`, and a final scan
that fails the build if any local path survives into the component. Given the same source and the
same two pins, it lands on the same bytes on any machine. CI runs the same script as the
`ci / validate` check.

`DEKOPON_PROVIDER_COMPONENT="$PWD/gh-provider.wasm" cargo test --locked --workspace` runs native
contract tests and real-component conformance against the built artifact; nothing contacts GitHub.

`tests/cassettes/gh/` holds GitHub exchanges recorded with `cassette record --upstream
gh=https://api.github.com` and `baseUrl: http://127.0.0.1:<port>/gh`, the port you pass to
`--listen`; `tests/cassette.rs` replays them through the `baseUrl` setting. The recorder saves
`authorization` as `[redacted]`.

## License

MIT or Apache-2.0, at your option.
