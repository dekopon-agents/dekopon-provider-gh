# dekopon-provider-gh

The GitHub provider for [Dekopon](https://github.com/dekopon-agents/dekopon), as a WebAssembly component.

Twenty-seven narrow authentication, repository, pull-request, and issue capabilities with fixed request shapes, bounded
output projections, and SHA-pinned review/merge writes. There is deliberately no arbitrary
`gh api` passthrough: a path-level escape hatch would collapse per-capability policy into
"everything the credential can reach".

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

`gh version`, `gh licenses`, `gh actions`, and `gh accessibility` print local component information without a capability or HTTP call.

`gh --help` and `gh pr view --help` render on stdout at status 0; `gh bogus` and a missing argument
render a usage error on stderr at status 2, so `$(gh bogus)` captures nothing while the error still
reaches the model. Neither renders from the network or grants anything.

Every supported operation maps to exactly one `gh.*` capability. The rewrite is a pure function that
*proposes*: the broker then authorizes it on the identical path a direct
`gh.pull-request.read --number 7` takes. Naming a capability the caller was not granted produces a
denial, not an escalation.

`--body-file -` is the one argument whose value is piped into the word. Proposal records only a
marker; the authorized invocation reads at most 4 KiB of UTF-8 from stdin before any HTTP call.
Nothing piped is a usage error rather than an empty comment. `gh api` accepts only four named GET routes (`user`, `users/LOGIN`, `repos/OWNER/REPO/branches/BRANCH`,
`repos/OWNER/REPO/commits/REF`). `gh api user` requires the separate `gh.user-self.read` grant;
`gh api users/LOGIN` requires `gh.user.read`. No arbitrary route or API flag is accepted.

Supported `gh pr list/view`, `gh issue list/view`, `gh repo view/list/read-file/read-dir` commands
accept closed, comma-separated `--json FIELDS`; bare, unknown and repeated fields refuse as usage.
Without `--json`, these commands print tab-separated rows (files print decoded content).
`gh pr list` accepts `--state merged`, filtering closed pulls to merged results, and both
PR and issue lists accept `--state all`. PR lists accept base/head, label, assignee, draft,
app-author and scoped search filters. Issue lists accept assignee, label, author, mention,
milestone, app-author, issue type and scoped search filters. New fields are optional;
legacy inputs retain their requests and output. Filters collect matching rows within the existing
ten-request bound. A search-backed PR list uses the search result's issue-shaped projection;
GitHub does not include head/base refs in that response, so those selected fields are null.
`pr view --comments` and `issue view --comments` render the existing bounded comments page in text.
`pr diff --patch` selects the patch Accept header; `--name-only` extracts names from the same
single diff GET. They cannot be combined. Repeated/comma-separated `--exclude` globs filter
file sections, matching a full path or basename. Diff commands retain their JSON result envelope.
`--allow-escape-sequences` on `pr diff` and `repo read-file` is a documented no-op:
base output already preserves ESC, so output bytes are identical with or without the flag.
`pr merge --body` (or `--body-file -`) and `--subject` set the merge commit message/title.
`gh pr list`, `gh issue list`, and `gh repo list OWNER` collect up to 100 matching rows
(default 30) across at most ten REST pages; an exhausted list may return fewer rows.
`repo list` uses GitHub's public `/users/OWNER/repos` endpoint and cannot enumerate
private repositories inaccessible there. `--source`/`--fork`, `--language`, repeated `--topic`,
`--archived`/`--no-archived`, and `--visibility` filter that public list. Deprecated `--private`
and `--public` select visibility; they do not enable private enumeration. Source/fork,
archived/no-archived, private/public, and visibility with either deprecated alias are mutually exclusive. An interrupted collection prints no partial rows.
`gh pr view --json files` collects all file pages, up to GitHub's 3,000-file cap.
It needs one metadata GET plus up to 30 file GETs under the same grant. Each response
uses the grant's per-response byte limit; a failed page emits no partial JSON.
Next links must retain the origin, base prefix, pull number and `per_page=100`;
paths may use the requested owner/repository or `/repositories/ID` with the ID from
the metadata response’s `base.repo.id`. Only the consecutive `page` changes. Reviews and comments retain their existing one-page bounds.
A text field cut at its byte bound ends, in JSON and text alike, with the line
`[dekopon: truncated to N bytes of M]`.
Upstream commands and flags classified as Backburner or Excluded refuse before HTTP with
`gh <cmd> [flag or scope]: not available in this environment (<reason>)`. The registry in
[`src/unavailable.tsv`](src/unavailable.tsv) supplies both refusals and help. `gh --help` is a bounded overview with the supported
groups, API restriction and a compact list of unavailable families. `gh GROUP --help`
(or `gh help GROUP`) lists every unavailable command, flag and scope in that group
with its reason; repeat for each group named on the root page for the full inventory.
Focused leaf help also lists its exclusions. Unknown commands and flags retain
clap usage errors. Unbuilt Buildable commands retain their base clap usage errors unless an actual
refused flag is present alongside valid upstream options. Required arguments on supported
commands stay required; flag refusals name the canonical long flag and scope refusals name the
missing `--owner` or `-R` scope. Registry arities distinguish booleans from value-taking options.
Syntax rows supply clap's closed option grammar for unbuilt commands. Hidden command and
flag declarations let clap resolve aliases, inherited options, values, shorthand clusters, and
separators before selecting a refusal. They expose no new capability inputs or HTTP authority.
The component uses the standard SDK export and clap parser, with no second argv tokenizer.
Supported upstream shorthands include `-F` for `--body-file`, review `-a/-c/-r`, merge `-m/-r/-s`,
list `-s`, and PR-list `-A`, alongside `-R`, `-L`, and `-b`. The four finite `gh api` reads
return bounded upstream JSON objects. Direct typed invocation retains JSON output; failures
write to stderr with a nonzero status. Filter stdout with the shell's
`jq` builtin.

## Capabilities

| Capability | Effect |
|---|---|
| `gh.auth.status` | read-only |
| `gh.content.read` | read-only |
| `gh.pull-request.list` / `.read` / `.diff` / `.status` | read-only |
| `gh.pull-request.approve` / `.comment` / `.request-changes` / `.merge` | external-write |
| `gh.repo.read` / `gh.repo.list` / `gh.branch.read` / `gh.commit.read` / `gh.user.read` / `gh.user-self.read` | read-only |
| `gh.issue.read` / `.list` | read-only |
| `gh.issue.comment` | external-write |
| `gh.issue.comment-edit` / `.comment-delete` / `.comment-edit-or-create` | external-write |
| `gh.search.prs` / `.issues` / `.repos` / `.code` / `.commits` | read-only |

`gh.pull-request.status` backs numbered `gh pr checks` with real check runs (including third-party
integrations), legacy commit statuses, and Actions workflow names at the pull's head SHA. The
credential must be able to read all three endpoints; a refusal is an error, not a passing check.
The read grant needs up to ten GET requests for bounded pagination. Text checks exit 8 while
pending, 1 for failed or empty checks, and 3 for upstream, broker HTTP or ceiling failures;
selected `--json` checks exit 0 and expose the verdict in `bucket`.

Scoped search requires exactly one `--owner OWNER` or `-R OWNER/REPO` (`search repos` requires `--owner`). `--owner` matches personal and organization owners. Searches are bounded to ten REST requests and at most 100 matches. A shorter response can reflect GitHub's indexing delay; incomplete results and an inconsistent index count fail rather than being presented as a complete array. GitHub exposes at most the first 1,000 indexed matches. Query terms cannot set a repository or owner scope.

Search qualifier flags are closed typed fields: logins, SHAs, dates/ISO times, counts/ranges,
enums and quoted free text. Counts and dates support `>`, `>=`, `<`, `<=`, closed `N..M`
and open `N..*`/`*..N` ranges. A free-text value becomes one quoted term, so a label such as
`area:db` or `x repo:elsewhere/repo` cannot change scope. Quotes, backslashes and controls in
these values refuse before HTTP. Raw list `--search` text retains the shipped scope-word rule;
its text plus supplied typed terms on new search paths must fit 512 bytes, excluding
provider-appended scope/kind terms. Existing queries without new query terms retain their
legacy author/state accounting. REST-only filters do not acquire a search-query length limit.
Issue semantic/hybrid searches use the fixed `search_type` REST parameter and return at most one
page, up to 100 results. They exclude `--include-prs`, explicit `--sort` and explicit `--order`.
An app filter and author filter are mutually exclusive on issue/PR lists and searches.

`issue comment --edit-last` edits the current user's latest comment; `--delete-last --yes`
deletes it. `--edit-last --create-if-none` selects the distinct edit-or-create capability.
Lookup walks at most ten backward pages of 100 comments, followed by at most one mutation.
All three new capabilities need POST to the fixed GraphQL endpoint, up to eleven requests;
only edit and edit-or-create accept a body or piped body. Delete cannot include a body.
`--yes` requires delete-last, and create-if-none requires edit-last. These operate on issue
comments even for PR numbers and do not depend on the PR head. With no matching comment,
plain edit/delete return `not-found`; edit-or-create posts one new comment.

`pr checks --required`, `pr merge --author-email`, `repo view --branch` and
`repo read-file --output` remain unbuilt: their implementations would exceed the corresponding
base grant. Required checks retain the existing GraphQL-only refusal. Batch 2 merge modes,
new commands, local uploads and arbitrary API access remain unavailable as before.

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

Grant each capability separately. `gh pr merge N -R owner/repo` requires exactly one of
`--merge`, `--squash`, or `--rebase`; the typed merge input requires `mergeMethod`.
Review submission pins the head observed by the provider but cannot prevent a push between
its pre-read and submission. `--match-head-commit SHA` on merge and all three review events refuses a head that moved
before the pre-read; GitHub's merge SHA guard also checks the subsequent write.
Only `gh.pull-request.comment` accepts an optional `comments` array of 1–50
`{path, line, side?, startLine?, body}` items. Paths use the content-path validation;
line is positive, side defaults to RIGHT (LEFT is also accepted), and an optional positive
startLine must be below line on that same side. Each body retains the 4 KiB review-body bound.
The review body is required without comments and optional with comments. The entire serialized
line-comment review request must fit 16384 bytes, including the pinned commit ID; validation
and the size check run before any HTTP. The POST uses `start_line` and `start_side` for ranges.
Approval and request-changes do not accept this array. Their existing grants stay unchanged.

Grant request counts for the operations actually used: PR read permits up to 33 provider requests
(metadata, 30 file pages, reviews and comments), while issue read permits two, diff/content one,
and lists/search/checks ten each. The broker's default 32-call ceiling can refuse the final
combined PR-read expansion; this existing behavior is unchanged. Examples are deployment choices,
not capability bounds. Hosts remain the existing GitHub authority or the owner-set baseUrl.

The [reviewer example](examples/pr-summarizer-linter/README.md)
shows the broker grant steps. `gh.auth.status` is read-only, Low risk, and needs one allowed GET.
It reports quota and available credential-header metadata, not repository visibility or credential
kind. `gh auth status --active` (`-a`) is a no-op for the single broker-bound credential.
`--json limit,remaining,reset,tokenExpiration,scopes` selects a closed subset of those fields;
`{}` and plain `gh auth status` retain their existing full JSON response. The optional internal
`_cliOutput` selection follows the existing command convention; it adds no HTTP authority.

GraphQL-backed reads need `POST` in `allowedMethods`, even though their capability effect is
read-only. The HTTP grant cannot distinguish a query from a mutation: fixed component-owned
documents and typed variables provide that guarantee. No existing read in this batch switches
from REST to GraphQL or changes its grant. The foundation helper is exercised by an isolated
component fixture; it exposes no new production capability. There are now twenty-seven production capabilities.

For fine-grained PATs and GitHub App installations, grant repository permissions for the
capabilities you enable (Read unless marked Write):

| Capability | Fine-grained permission |
|---|---|
| `gh.auth.status`, `gh.user.read`, `gh.user-self.read` | None (public endpoint / rate limit) |
| `gh.repo.read`, `gh.repo.list` | Metadata: Read |
| `gh.content.read`, `gh.branch.read`, `gh.commit.read` | Contents: Read |
| `gh.pull-request.list`, `.read`, `.diff` | Pull requests: Read |
| `gh.pull-request.status` | Pull requests: Read; Checks: Read; Actions: Read; Commit statuses: Read |
| `gh.pull-request.approve`, `.comment`, `.request-changes` | Pull requests: Write |
| `gh.pull-request.merge` | Pull requests: Write; Contents: Write |
| `gh.issue.read`, `.list` | Issues: Read |
| `gh.issue.comment` | Issues: Write |
| `gh.issue.comment-edit` | Issues: Write; Pull requests: Read when targeting a PR number |
| `gh.issue.comment-delete` | Issues: Write; Pull requests: Read when targeting a PR number |
| `gh.issue.comment-edit-or-create` | Issues: Write; Pull requests: Read when targeting a PR number |

| Error code | Message | What to do |
|---|---|---|
| `request-too-large` | `the review request would exceed 16384 bytes` | Shorten the line-comment bodies or submit fewer comments; no request was sent (exit 2). |
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
The foundation suite builds an isolated GraphQL fixture using the pinned toolchain and
`wasm-tools`; its test-only `gh.foundation.read-repository-node` capability is absent from the production manifest.
The fixture is selected only by the test build cfg `gh_foundation_fixture`, not a default or
published Cargo feature. CI applies the root deny policy to both Cargo lockfiles.

`tests/cassettes/gh/` holds GitHub exchanges recorded with `cassette record --upstream
gh=https://api.github.com` and `baseUrl: http://127.0.0.1:<port>/gh`, the port you pass to
`--listen`; `tests/cassette.rs` replays them through the `baseUrl` setting. The recorder saves
`authorization` as `[redacted]`. `tests/cassettes/have-flags.json` maps the batch 1 exchanges to
production-capability replays; setup-only recordings are excluded. The batch used 46 authenticated
calls, including scratch setup. The redaction test always checks authorization and, when
`GH_CASSETTE_TOKEN` is set, rejects every eight-byte token window without printing the token.

## License

MIT or Apache-2.0, at your option.
