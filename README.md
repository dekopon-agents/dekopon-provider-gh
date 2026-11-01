# dekopon-provider-gh

The GitHub provider for [Dekopon](https://github.com/dekopon-agents/dekopon), as a WebAssembly component.

Forty-two narrow authentication, repository, pull-request, issue, and Actions capabilities with fixed request shapes, bounded
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

`--body-file -` and `gh workflow run --json` consume piped input. Proposal records only a
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
legacy inputs retain their output. Filters collect matching rows within the existing
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
groups, API restriction and a compact list of unavailable families. `gh help GROUP`
lists every unavailable command, flag and scope in that group
with its reason; repeat for each group named on the root page for the full inventory.
Supported groups also accept `gh GROUP --help`; focused leaf help lists its exclusions. Unknown commands and flags retain
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
| `gh.run.list` / `gh.run.read` | read-only |
| `gh.run.cancel` / `.force-cancel` / `.rerun` / `.delete` | external-write |
| `gh.workflow.list` / `.read` | read-only |
| `gh.workflow.run` / `.enable` / `.disable` | external-write |
| `gh.cache.list` | read-only |
| `gh.cache.delete` | external-write |
| `gh.actions.logs.read` | read-only |
| `gh.auth.status` | read-only |
| `gh.content.read` | read-only |
| `gh.content.write` | external-write |
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
and install it on the intended account and repositories. Bind it as a broker credential of kind
`githubApp` (github.com only) with `appId`, `installationId` and `privateKey`; the broker mints and renews the
installation token, optionally downscoped by `repositories` and `permissions`. This provider does
not mint or refresh tokens.

For either credential kind, use the broker's owner-only credentials file and bind its symbolic
name to each granted capability's constraint set. Bind its destinations to `api.github.com`, or
the authority in the owner's `baseUrl` (PATs only); the guest receives no token. See the
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
component fixture; it exposes no new production capability. There are now forty-two production capabilities.

For fine-grained PATs and GitHub App installations, grant repository permissions for the
capabilities you enable (Read unless marked Write):

| Capability | Fine-grained permission |
|---|---|
| `gh.auth.status`, `gh.user.read`, `gh.user-self.read` | None (public endpoint / rate limit) |
| `gh.run.cancel` | Actions: Write |
| `gh.run.force-cancel` | Actions: Write |
| `gh.run.rerun` | Actions: Write |
| `gh.run.delete` | Actions: Write |
| `gh.workflow.list` | Actions: Read |
| `gh.workflow.read` | Actions: Read; Contents: Read for YAML |
| `gh.workflow.run` | Actions: Write |
| `gh.workflow.enable` | Actions: Write |
| `gh.workflow.disable` | Actions: Write |
| `gh.cache.list` | Actions: Read |
| `gh.cache.delete` | Actions: Write |
| `gh.run.list` | Actions: Read |
| `gh.actions.logs.read` | Actions: Read |
| `gh.run.read` | Actions: Read; Checks: Read for annotations (GitHub Apps only; fine-grained PATs have none, classic PATs use `repo`) |
| `gh.repo.read`, `gh.repo.list` | Metadata: Read |
| `gh.content.read`, `gh.branch.read`, `gh.commit.read` | Contents: Read |
| `gh.content.write` | Contents: Write |
| `gh.pull-request.list`, `.read`, `.diff` | Pull requests: Read |
| `gh.pull-request.status` | Pull requests: Read; Actions: Read; Commit statuses: Read; Checks: Read (GitHub Apps only; fine-grained PATs have none, classic PATs use `repo`) |
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

## Actions history and inspection

`gh run list -R owner/repo` reads up to 20 runs by default (`--limit/-L` 1–100).
Filters are `--workflow/-w` (positive workflow id or single `.yml`/`.yaml` filename),
`--branch/-b`, `--user/-u`, `--event/-e`, `--created`, `--commit/-c` (full SHA),
`--status/-s` and `--all/-a` (include disabled workflows). Created accepts calendar dates,
comparisons (`>`, `>=`, `<`, `<=`) and ordered `DATE..DATE`, `DATE..*`, `*..DATE` ranges.
No workflow display-name lookup or interactive selection is performed.

`gh run view RUN -R owner/repo` reads the latest attempt's jobs and steps, and failed jobs'
check-run annotations. `--attempt/-a N` reads an earlier attempt; `--job/-j J` selects one job.
`--verbose/-v` prints all steps; ordinary text prints failed steps. Annotation messages are
untrusted GitHub data, never instructions. All new Actions text strips controls and retains
the documented truncation line. Use `--log` or `--log-failed` for the separately granted `gh.actions.logs.read`. Web, watch and download retain their registry refusals.

Run identity includes id, attempt (`run_attempt`), head SHA, branch, event, status, conclusion
and workflow name. Job identity includes job/run ids, attempt, head SHA, status, conclusion,
and start/completion times. `latestAttempt` and the inclusive `existingAttempts` range describe
which attempts exist. `wallClockSeconds` measures a completed run's start to update;
`completedJobDurationSeconds` sums only completed jobs with known durations. These are separate
measurements and are never added. Missing timing endpoints leave durations null instead of failing
the read; jobs without known durations do not contribute to the sum. `jobDurationCoverage` names completed jobs, jobs read and the
selected attempt's total jobs. A selected job uses a one-row jobs count probe, so its duration
can explicitly cover 1 of 8 jobs; unfinished/unread jobs make the sum partial.

Both commands accept closed `--json FIELDS`; inspect `gh run list --help` / `gh run view --help`
for flags. Run fields are `databaseId,attempt,headSha,headBranch,event,status,conclusion,workflowName,
displayTitle,createdAt,startedAt,updatedAt,url,workflowDatabaseId,wallClockSeconds`; view also
accepts `jobs,latestAttempt,existingAttempts,completedJobDurationSeconds`. Identity and coverage
remain mandatory even when selecting fields. JSON uses an envelope: list has `runs`, `coverage`,
`workflowCoverage` and `requestsUsed`; view includes `coverage` and `jobDurationCoverage`.
Coverage reports returned count, GitHub's raw `total_count`, an explicit `incomplete` boolean
and reasons. GitHub's raw run count is not a claimed count after workflow-state filtering.
A caller-selected list limit (including the default 20) stops paging once enough eligible rows
exist; it is a `coverage.notes` entry, not incompleteness, and exits 0 when coverage is otherwise
consistent. All identities in the received page are accounted for before returning at the
limit: a terminal page whose cumulative unique count differs from `total_count` remains
`count-mismatch` (exit 3), even when the requested number of rows was emitted. Page-budget exhaustion
before the limit, count drift, and unread annotation successors are incomplete and return
`incomplete-results` (exit 3) after bounded rows. A failed later run page preserves rows and
reports `page-failed` plus its typed `pageError` code.
A deliberately selected job names `selected-job` scope in coverage and may exit 0 when that
selected read is complete, while job-duration coverage explicitly remains partial. `--exit-status` returns `run-failed` (exit 1) for a completed unsuccessful attempt;
incomplete results take precedence. Success, neutral and skipped are not failed conclusions.

Grant `gh.run.list` and `gh.run.read` separately. Both need only GET on the existing API authority
and Actions: Read; view additionally needs Checks: Read (GitHub Apps only) for annotations. List enforces at most
21 requests at worst: repository visibility, ten workflow pages and up to ten run pages,
stopping as soon as the requested number of eligible runs exists. With a one-page workflow
catalog and enough eligible runs on the first run page, the request count is three. View enforces at most
29: repository, latest run, optional explicit attempt, ten jobs pages and sixteen annotation
requests shared across metadata/pages. Selecting one job uses at most 20 requests without an
explicit attempt, 21 with it: repository, latest run, optional attempt, individual job, one-row
jobs count probe and the same sixteen annotation requests. Every page contains at most 100 rows.
Larger histories and annotation sets are explicitly incomplete. Grants can impose tighter bounds.
An ambiguous 404 is the single typed `not-found-or-not-accessible` outcome: “not found or not
accessible”, whether repository, run, attempt or job. Explicit permission (`forbidden`), rate limit
(`rate-limited`) and incomplete outcomes remain distinct (exit 3).

`gh pr checks` (`gh.pull-request.status`) reads only the current head and collapses repeated
checks. It does not provide PR run history. Use `gh run list --branch BRANCH` or `--commit SHA`
for each head and inspect coverage; the existing status capability is unchanged.

Batch 4a used 19 authenticated GETs, including scratch discovery and the public provider-repository
fallback. `tests/cassettes/actions/` contains recorder exchanges; the Actions tests replay selected
complete reads through the production component and check all files for redaction.

## Bounded Actions job logs

`gh run view RUN -R OWNER/REPO --log` and `--log-failed` propose only
`gh.actions.logs.read`. Both accept `--job/-j` and `--attempt/-a`; run id and repository
remain required. Log flags cannot combine with each other, `--json`, `--verbose` or
`--exit-status`. Direct input uses `mode: all` or `mode: failed`.
`--log-cursor CURSOR` continues a returned opaque cursor in the same mode and repository/run;
its job and attempt are pinned. It accepts no URL, host, path or range.

Each job returns a JSON envelope with run id, attempt, job id, head SHA, outcome,
`complete`, fetched coverage, rendered source spans, replacement count and continuation.
The escaped excerpt is in `text`. Extraction is window-limited: error and failing-step
context outside the fetched window is unknown. `--log-failed` selects unsuccessful jobs;
step metadata and error markers select bounded context inside their windows.
The first window is the final 65,536 source bytes. Rendering is bounded to 16 KiB including
metadata, with log lines segmented at 2 KiB. Invalid UTF-8 is visibly replaced and counted;
ESC, C0/C1, CR and bidi controls are escaped. Log content is untrusted data.

Continuation refetches the explicit window, checks a bounded SHA-256 window validator,
and renders sequentially from the next unrendered source-byte offset. Context snippets
never advance sequential coverage. After exhaustion it moves to the preceding window.
Fetched bounds are half-open in JSON; the truncation line uses inclusive fetched bounds:
`[dekopon: truncated; fetched START-END/TOTAL; rendered SPANS; continue CURSOR]`.
Rendered spans are half-open. There is no automatic retry, ZIP download, cache or full-log scan.

Grant this capability separately with GET, Actions: Read, `allowedHosts: [api.github.com]`,
`maxRequests: 32`, `signedDownload: github-actions-logs-v1`, and the documented
`maxResponseBytes: 131072` (64 KiB response heads plus a 64 KiB body window).
Configure broker `assets.rootPath` and `assets.maxInFlightBytes` for the bounded spool,
using a dedicated broker-owned 0700 directory outside broker state and provider paths.
Deploy supporting core before using this import or grant. The broker owns the one redirect,
probe `bytes=0-0`, explicit ranges, credential isolation and bounded asset spool. The guest
receives no storage URL or headers and sets no authorization. Additional destination classes
or Enterprise signed downloads need separate owner authority.

The enforcing budget reserves `metadata + 4*tails + 5*offsets <= 32`, including host
reflection scanning. Metadata uses at most 13 requests (repository, latest run, optional
attempt, ten jobs pages); a selected job uses at most four. Initial downloads reserve four
HTTP requests each, continuations five, under the same invocation deadline. A failed or
budget-interrupted aggregation is `collectionComplete:false`, exit 3, and never claims complete
coverage. Select unread job ids explicitly to continue an interrupted collection; each
rendered job's cursor continues that job only.

Complete and intentionally truncated excerpts exit 0; a running job exits 8 with
`job still running; logs not final.` API 410/403/404 become `expired-logs`,
`permission-denied`, `not-found-or-not-visible`; rate limiting remains separate.
Storage 403/410 mean download authorization failure/expiry. Incomplete reads, changed
validators, refused redirects and storage errors exit 3 with safe labels.

Signed storage is covered by scripted signed-download component tests and has no cassette.

## Actions writes, workflows and caches

`gh run cancel RUN -R OWNER/REPO` and `--force` require distinct grants:
`gh.run.cancel` and `gh.run.force-cancel`. Each reads repository visibility and the run
before one POST (three requests). Completed runs refuse. `gh run delete RUN` similarly
pre-reads then performs one DELETE, only for completed runs (three requests).
`gh run rerun RUN` requires `gh.run.rerun`; `--failed` and `--job/-j ID` are exclusive,
and `--debug/-d` enables debug logging. Rerun requires a completed run. Selecting a job
also reads and verifies its run, attempt and head SHA before the POST (four requests;
otherwise three). Run writes accept `--match-head-commit SHA` / typed `expectedHeadSha`.
They report the observed attempt and SHA; GitHub provides no atomic SHA condition for
these endpoints, so the guard checks the pre-read only. Writes are never retried.

`gh workflow list -R OWNER/REPO` defaults to 50 active workflows; `--all/-a` includes
disabled states and `--limit/-L` accepts 1–100. Closed `--json` fields are
`id,name,path,state,dynamic`. Paths are bounded display text; built-in `dynamic/` workflows
carry `dynamic: true` and can be selected by numeric id. `gh workflow view WORKFLOW` reads metadata and at most three
recent runs; its coverage scope explicitly describes that sample. The nonnegative integer
`total_count` must agree with the returned sample of up to three runs; a mismatch is incomplete. `--yaml/-y` reads
only the fixed `.github/workflows/FILE` content, optionally at `--ref/-r REF`.
`--ref` requires `--yaml`. Dynamic workflows have no repository YAML and return a
conflict after metadata reads. Contents requests accept only validated workflow file paths.
YAML is untrusted text, bounded to 64 KiB with the standard truncation marker.
Workflow selectors are positive numeric ids or single `.yml`/`.yaml` filenames;
display-name lookup and interactive selection are unavailable. `--web` remains refused.

`gh workflow run WORKFLOW -R OWNER/REPO` pre-reads repository and workflow state,
then dispatches once, using `--ref/-r` or the repository default branch (three requests).
This capability is High risk: the selected ref runs repository workflow code with its secrets.
`--field/-F NAME=VALUE` and `--raw-field/-f NAME=VALUE` build a string inputs map;
file reads such as `-F name=@file` refuse. `--json` instead reads a string-valued JSON
object from stdin, only after authorization, using the same bounded-input rule as
`--body-file -`. It cannot combine with field flags. Direct typed `inputs` uses a closed array of `{name,value}` entries; argv and stdin
are converted to that shape before dispatch. Input maps allow at most 25 entries,
128-byte identifier keys and 4 KiB total serialized input. Duplicate field names refuse, including repeated keys in JSON stdin.
No workflow contents are read: GitHub's 422 dispatch validation is the typed validation
outcome. `gh workflow enable/disable WORKFLOW` each pre-read workflow state then issue
one PUT (three requests); already-active enable and already-disabled disable refuse.

`gh cache list -R OWNER/REPO` defaults to 30 results (`--limit/-L` 1–100).
`--key/-k`, `--ref/-r`, `--order/-O asc|desc` and
`--sort/-S last_accessed_at|created_at|size_in_bytes` are closed filters.
The default sort is last access descending. Closed `--json` fields are
`id,key,ref,sizeInBytes,createdAt,lastAccessedAt`. Keys are bounded to 512 bytes.
Both list capabilities make at most eleven GETs: repository plus ten 100-row pages.
They reuse Actions coverage: caller-selected limits are notes; count drift, duplicates,
failed successor pages and exhausted budgets are explicitly incomplete.

`gh cache delete ID|KEY -R OWNER/REPO` first discovers cache identities through at most
ten pages; id deletion uses one DELETE by id, key deletion one DELETE by exact key,
optionally with `--ref/-r` (at most twelve requests). Numeric argv selects an id;
direct typed input can explicitly select a numeric key. Key deletion requires complete
discovery of at most twenty matching targets before its single write. `--all/-a` requires complete
discovery of at most twenty targets before deleting each id once (at most 31 requests).
Larger or inconsistent discovery returns incomplete before any deletion.
`--succeed-on-no-caches` makes an empty match succeed without a write. A later failure
can leave earlier deletions applied; there is no retry or rollback.

New operations distinguish permission/invisible repository, target not found, inapplicable
state/conflict, rate limit, dispatch validation and incomplete results. GitHub response
text is untrusted, projected and bounded; upstream error bodies are never echoed.
Existing run reads retain their existing error and output contract.

Grant each new id separately; none is enabled in the example by default. Every grant
uses only the existing GitHub API authority, GET for pre-reads, and the listed write
method. The coordinator's `actions-grants` OWNER-QUEUE item owns deployment grants;
this PR writes no homelab grant.

| Capability | Effect | Methods | Maximum requests | Fine-grained permission |
|---|---|---|---:|---|
| `gh.run.cancel` | external-write | GET, POST | 3 | Actions: Write |
| `gh.run.force-cancel` | external-write | GET, POST | 3 | Actions: Write |
| `gh.run.rerun` | external-write | GET, POST | 4 | Actions: Write |
| `gh.run.delete` | external-write | GET, DELETE | 3 | Actions: Write |
| `gh.workflow.list` | read-only | GET | 11 | Actions: Read |
| `gh.workflow.read` | read-only | GET | 3 | Actions: Read; Contents: Read for YAML |
| `gh.workflow.run` | external-write | GET, POST | 3 | Actions: Write |
| `gh.workflow.enable` | external-write | GET, PUT | 3 | Actions: Write |
| `gh.workflow.disable` | external-write | GET, PUT | 3 | Actions: Write |
| `gh.cache.list` | read-only | GET | 11 | Actions: Read |
| `gh.cache.delete` | external-write | GET, DELETE | 31 | Actions: Write |

The provider and isolated fixture use published Dekopon 0.64.0 dependencies.
The SDK supports up to 64 capabilities; this provider exports 42.

Batch 4b recorded four authenticated GETs against `dekopon-agents/gh-parity-scratch`
with cassette `4308b585`: repository, empty workflow/cache lists and a missing workflow
read. Scratch has no workflows, so successful workflow/YAML reads and every write are
covered by scripts, not claimed as live evidence. `tests/cassettes/actions-b4b/` replays
these reads through the production component and shares the credential-window redaction test.

## Single-file writes on an explicit branch

`gh repo write-file PATH -R OWNER/REPO --branch BRANCH --message TEXT (--create | --sha BLOB_SHA) --body-file -`
is a provider extension, not an upstream gh command or `gh api` route. Pipe UTF-8 content;
empty content is allowed. The [embedded skill](skills/gh-content-write/SKILL.md) describes
inputs, bounds and outcomes. Success returns only repository, path, branch, blob and commit identifiers.

Update pins the file blob SHA from a separate content read, never branch HEAD. An unrelated
commit may advance the branch. Each call sends one Contents PUT and changes one path in one
commit; there is no implicit read, retry, merge, create-to-overwrite fallback, batch atomicity,
or exactly-once guarantee. Callers distinguish failures by exit status plus stable stderr:

| Outcome inside the provider | Exit status | Stderr without permission headers |
|---|---:|---|
| `content-conflict` (HTTP 409) | 4 | `file write refused: content conflict or repository rule violation` |
| `forbidden` (HTTP 403) | 3 | `credential is not permitted to make this call` |
| `not-found` (HTTP 404) | 3 | `not found, or not visible to this credential` |
| `unprocessable` (HTTP 422) | 3 | `endpoint refused the request as invalid` |
| `write-outcome-unknown` (transport failure or unconfirmed/unreadable HTTP reply) | 5 | `file write outcome is unknown; read state before deciding what to do next` |

HTTP 408/499, every 5xx response (including 500/502/503/504), and unexpected success or
redirect responses are unknown: a received response does not establish whether the write
committed. A confirmed 201 create or 200 update whose reply cannot be decoded or whose
path/type/identifiers fail validation is also unknown: the commit may have happened even though
the provider cannot report it. These outcomes are never retried automatically.

Other exit-3 messages for 401, rate limiting, or permission/scope diagnostics are also definitive
API refusals.

The broker retains the status and message; its generic detail label is `provider-exit`, not the
provider's internal outcome name. A broker-side operation timeout instead arrives as a broker
failure without an exit status: also treat it as unknown and read state before deciding what to
do next. Do not blindly retry either unknown result. Stdout stays empty on failure; neither
upstream response bodies nor submitted content are printed.

GitHub's [published REST description](https://github.com/github/rest-api-description/blob/main/descriptions/api.github.com/api.github.com.json)
for this endpoint includes both `basic-error` and `repository-rule-violation-error` under HTTP 409.
Therefore `content-conflict` can also mean a repository rule (ruleset) refusal; it does not establish a
SHA mismatch. See the [Contents REST reference](https://docs.github.com/en/rest/repos/contents#create-or-update-file-contents).

Grant `gh.content.write` separately: external-write, Medium risk, Contents: Write, PUT only,
one request, existing GitHub API authority and broker-owned credential. The
[disabled fence fixture](examples/content-write/README.md) pins authenticated principal, agent,
file URL and run branch. It includes a separate GET read fence with the exact encoded ref query.
The PR reviewer grants no file writes. CI creates the branch; this capability creates neither
branches nor repositories and supplies no workflow, secret, PR, delete or multi-file operation.
