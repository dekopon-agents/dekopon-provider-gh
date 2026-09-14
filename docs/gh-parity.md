# Flag parity with the GitHub CLI

Audited against `gh` 2.100.0 (`gh <cmd> --help`, live on the audit machine) for every `gh <area>
<verb>` this provider's command tree (`src/commands.rs`) already exposes. Per the brief, verbs the
provider has that real `gh` does not (`gh content view`, `gh branch view`, `gh commit view`, `gh
user view`, `gh pr files`, `gh pr reviews`) are provider-only inventions with no upstream flag
surface to diff against — confirmed with `gh <word> --help` returning "unknown command" for each —
and are out of scope below. Nothing was added to them.

## The live bug

Production hit `gh pr list -R scientist-hq/rx --state open --author xrl --page 1 --per-page 100`
and got `invalid-input: input does not match the capability contract`. A second live failure hit
the same wall via `--repo`/`--per-page 100 --page 1`. Root cause: `--page`/`--per-page` are this
provider's own invented pagination knob (real `gh` has neither on `pr list`/`issue list`, only
`-L/--limit`, default 30) and had no bound enforced at the clap layer — `--per-page 100` parsed
fine and only failed downstream, in native validation, against a `MAX_PER_PAGE` of 50 that
`--help` never mentioned. A model reaching for `--per-page 100` is not being exotic: GitHub's own
REST API caps `per_page` at exactly 100 on every list endpoint this provider calls.

Fixed by:
1. **`MAX_PER_PAGE` raised from 50 to 100** (`src/lib.rs`) — GitHub's real ceiling, not an
   arbitrary tighter one.
2. **Every numeric bound now enforced at the clap layer**, not just natively: `--page`, `--per-page`,
   the new `-L/--limit`, and the pull/issue number positional all carry `clap::value_parser!(...).range(...)`,
   so an out-of-range value is a named usage error at parse time —
   `error: invalid value '150' for '--per-page <N>': 150 is not in 1..=100` — instead of a generic
   `invalid-input` a model has no way to predict from `--help` text alone.
3. **`-L/--limit`** added to `pr list` and `issue list`, gh's own primary spelling, default 30
   (matching gh), mapped onto the same `perPage` capability field `--per-page` fills;
   `conflicts_with("per-page")` so at most one ever sets it.
4. **`hasMore`/truncation correctness fix**: raising the per-request ceiling to 100 while
   `MAX_LIST_ITEMS` (the output-size cap) stays 50 meant a single request could return more raw
   rows than the projection keeps. `hasMore` is now `has_next_link(&response) || raw_len >
   MAX_LIST_ITEMS`, computed before any client-side filtering, in every paginated capability
   (`pr list`, `pr files`, `pr reviews`, `issue list`, `issue comments`) — not just the one that
   was reported.
5. **`perPage` defaults changed from 20 to 30** on `pr list`/`issue list` to match gh's own default
   ("adopt gh's default even without `--foo` present").

## Cross-cutting decisions

**`--json`/`--jq`/`--template` stay rejected**, unchanged from before this PR. Reconsidered per the
brief: this provider's output is *already* a structured JSON value on every call (unlike real
`gh`, whose default output is human-formatted table text `--json` exists to escape). The shell's
own `jq` builtin already sits downstream of that JSON for exactly the field-narrowing `--json`
would otherwise buy, so wiring a second, narrower field-filter mechanism would duplicate it while
adding real risk: `pr list`/`issue list`'s output is a `{items, page, hasMore}` wrapper, not a bare
array, so filtering would need to special-case which shapes are wrapped, and any restricted
field-name allowlist would inevitably diverge from gh's own JSON field names (this provider's
output was never gh-field-identical — e.g. `headRef` vs gh's `headRefName`), risking exactly the
"a script asked for X and got something else, silently" failure this codebase's `REJECTED_FLAGS`
table already exists to prevent for `--web`/`--checkout`. `--jq`/`--template` accept arbitrary
computation and were never in scope. Kept as documented rejections, not silently dropped.

**`-R`/`--repo` value format**: real gh accepts `[HOST/]OWNER/REPO` for GitHub Enterprise hosts.
This provider talks only to `api.github.com` (see `endpoint()`), so a host segment has nowhere to
route — already failed closed before this PR (a 3-segment value hits `repo.contains('/')`), just
with a message that read like a formatting typo. Reworded to name the real reason
(`parse_repo`): *"names a host, but this provider only talks to api.github.com."* No behavior
change, message-clarity only.

**`gh pr status` / `gh pr checks` naming**: real gh's `pr checks <number>` is what this provider
spells `pr status` (visible-aliased as `checks`, both dispatch identically). Real gh's own `pr
status` is a *different* command — a no-argument dashboard of PRs relevant to the current
branch/user — which this provider has no equivalent of and doesn't need one for (there is no
"current branch" concept in a broker-authorized capability call). Left as-is: both provider
spellings already work, and swapping which is primary wasn't worth the churn for a purely
cosmetic mismatch. Documented here so it isn't rediscovered as a bug.

**`gh issue comments` (plural, list)**: this provider has a standalone list-comments subcommand;
real gh instead puts `-c/--comments` on `issue view` (also true of `pr view`). Both mechanisms now
exist side by side — `-c/--comments` wired onto `pr view`/`issue view` for parity, the standalone
list subcommands kept unchanged for full paged access with `--page`/`--per-page` (which the
`-c` inline preview doesn't offer).

**`repo view --branch` rejected, not wired or ignored.** `gh.repo.read`'s REST call
(`GET /repos/{owner}/{repo}`) has no branch parameter, and its output is metadata only (default
branch, visibility, flags) — there is no README or branch-scoped content in the response for
`--branch` to select in the first place. Silently ignoring it would be indistinguishable from it
working, since the (branch-invariant) output looks identical either way; rejecting it by name says
so.

**`--search` rejected** on both `pr list` and `issue list`. GitHub's plain list endpoints have no
free-text search parameter; honoring it would require switching to the separate search API
(different response envelope, different rate-limit bucket) — and worse, a naive implementation
would let a model's free-text query itself carry `repo:`/`org:` qualifiers that repoint the search
at a repository this invocation was never authorized for, a capability-scope escape the granted
`owner`/`repo` are supposed to bound. Rejected with a message naming the safe alternative filters.

**`pr merge --admin`/`--auto`/`--disable-auto` rejected.** `--admin` bypasses merge-queue routing
and required-check/review protections gh's own help describes; there is no REST body parameter for
either bypass on the merge endpoint (`PUT .../pulls/{n}/merge` takes only `sha`, `commit_title`,
`commit_message`, `merge_method`), and this write capability was authorized as an ordinary
conditional merge, never reviewed as an administrator-override grant. `--auto`/`--disable-auto`
toggle GitHub's auto-merge-when-ready, settable only through a GraphQL mutation this REST-only
provider never calls. All three would otherwise be a hard, unlabeled clap "unrecognized argument"
error; promoted into named `REJECTED_FLAGS` entries instead.

**`issue comment --delete-last`/`--edit-last` rejected**, not accepted as no-ops. Both change the
target of the write entirely (a DELETE or a PATCH of an existing comment, keyed off "your last
comment" — an identity this guest never learns, since the broker injects the credential and there
is no reachable whoami). Silently discarding either while a caller's `--body`/`--attach` still
posts a brand-new comment would be exactly the "script believes X happened but Y happened" failure
this codebase's own rejection philosophy exists to avoid.

**Short-flag rejection gap closed for `-w`/`-q`** (`--web`/`--jq`), added to `REJECTED_FLAGS`
alongside their long forms — checked for collisions against every short flag this PR adds; neither
letter is used anywhere in the tree. `-t` (`--template`) was *not* added as a global short reject:
gh spells `pr merge --subject` with the identical short flag `-t`, and `REJECTED_FLAGS` matches raw
argv tokens regardless of subcommand, so rejecting `-t` globally would have broken the newly-wired
`pr merge -t`. The long form `--template` stays rejected everywhere; the bare `-t` on read commands
falls through to clap's own (still safe, just less friendly) "unrecognized argument" error, same as
before this PR.

## `gh pr list`

| Flag | gh default | Disposition | Notes |
|---|---|---|---|
| `-s/--state` | open | **wire** (extended) | Enum gained `merged` (GitHub's list endpoint has no such state; requested as `closed`, filtered client-side on `mergedAt` presence) |
| `-A/--author` | — | wire (pre-existing) | Client-side post-fetch filter, unchanged; the plain PR-list endpoint has no author param |
| `-a/--assignee` | — | **wire** | Client-side post-fetch filter on the response's own `assignees[]` (no REST param on this endpoint either) |
| `-B/--base` | — | **wire** | Native REST query param, direct passthrough |
| `-H/--head` | — | **wire** | Native REST query param, qualified server-side as `owner:branch` (GitHub requires the qualified form; gh's own flag takes the bare branch) |
| `-l/--label` | — | **wire** | Client-side post-fetch filter requiring every given label (AND), repeatable flag |
| `-d/--draft` | — | **wire** | Client-side post-fetch filter |
| `--app` | — | noop | Bot-author filtering is a search-API qualifier; accepted, ignored |
| `-L/--limit` | 30 | **wire** | New; see live-bug fix above |
| `-S/--search` | — | **reject** | Scope-escape risk; see cross-cutting notes |
| `-q/--jq`, `--json`, `-t/--template` | — | reject (pre-existing) | See cross-cutting notes |
| `-w/--web` | — | reject (pre-existing) | No browser |
| *(no flag)* `perPage` default | — | **default-only** | 20 → 30, matching gh |

**Counts: 8 wired, 1 default-only, 1 noop, 5 rejected.**

## `gh pr view`

| Flag | Disposition | Notes |
|---|---|---|
| `-c/--comments` | **wire** | New optional `comments` input field; one bounded extra GET to the same comments endpoint `issue-comments.read` uses, embedded as `recentComments`/`recentCommentsTruncated` |
| `-q/--jq`, `--json`, `-t/--template`, `-w/--web` | reject (pre-existing) | See cross-cutting notes |

**Counts: 1 wired, 4 rejected.**

## `gh pr diff`

| Flag | Disposition | Notes |
|---|---|---|
| `--name-only` | **wire** | Parses `diff --git a/… b/…` header lines out of the already-fetched diff text; output shape becomes `{number, files, filesTruncated}` when set, unchanged otherwise |
| `--patch` | noop | Unified/patch format is already the only output mode |
| `--color`, `--allow-escape-sequences`, `-e/--exclude` | noop | No local diff rendering to filter or colorize |
| `-w/--web` | reject | No browser (via the global table) |

**Counts: 1 wired, 4 noop, 1 rejected.**

## `gh pr checks` (this provider's `pr status`, aliased)

| Flag | Disposition | Notes |
|---|---|---|
| `--required` | noop | Would need a second GitHub surface (branch protection) beyond Actions runs + commit statuses |
| `--watch`, `--fail-fast`, `-i/--interval` | noop | No polling loop; single stateless invocation |
| `-q/--jq`, `--json`, `-t/--template`, `-w/--web` | reject (pre-existing) | |

**Counts: 0 wired, 4 noop, 4 rejected.**

## `gh pr review`

Already at full parity before this PR (`-a/--approve`, `-c/--comment`, `-r/--request-changes`,
`-b/--body`, `-F/--body-file`) — this PR adds gh's short flags to all five. No gh equivalent exists
for this provider's own `--expected-head-sha` SHA-pin safety flag, so it needed no gh-spelled
alias here (unlike `pr merge`, below).

**Counts: 5 wired (shorts added, no new fields).**

## `gh pr merge`

| Flag | Disposition | Notes |
|---|---|---|
| `-m/--merge`, `-s/--squash`, `-r/--rebase` | wire (pre-existing) | Shorts added |
| `-b/--body`, `-F/--body-file` | **wire** | New `commitMessage` field → REST `commit_message` |
| `-t/--subject` | **wire** | New `commitTitle` field → REST `commit_title` |
| `--match-head-commit` | **wire** | Alias onto the existing `--expected-head-sha`/`expectedHeadSha` field |
| `-A/--author-email` | noop | REST merge endpoint has no author-override field; only GraphQL exposes one, and this provider is REST-only |
| `-d/--delete-branch` | noop | Wireable as a second `DELETE` after merge, but that turns one authorized write into two under the same grant; left for a future, separately-scoped capability |
| `--admin`, `--auto`, `--disable-auto` | **reject** | See cross-cutting notes |

**Counts: 7 wired, 2 noop, 3 rejected.**

## `gh issue view`

| Flag | Disposition | Notes |
|---|---|---|
| `-c/--comments` | **wire** | Same pattern as `pr view`; embeds `recentComments`/`recentCommentsTruncated` without disturbing the pre-existing numeric `comments` count field |
| `-q/--jq`, `--json`, `-t/--template`, `-w/--web` | reject (pre-existing) | |

**Counts: 1 wired, 4 rejected.**

## `gh issue list`

| Flag | gh default | Disposition | Notes |
|---|---|---|---|
| `-s/--state` | open | wire (pre-existing, correct) | |
| `-A/--author` | — | **wire** | New; GitHub's issues-list REST endpoint's real query param is `creator`, translated server-side |
| `-a/--assignee` | — | **wire** | Native REST param, direct |
| `-l/--label` | — | **wire** | Native REST param (comma-joined); GitHub's own AND semantics, no client filtering needed (unlike `pr list`) |
| `-m/--milestone` | — | **wire, numeric-only** | Accepts a milestone number or `*`/`none`; a title (which gh itself resolves via an extra lookup) is out of scope |
| `--mention` | — | **wire** | Native REST param name is `mentioned`, translated server-side |
| `--type` | — | **wire** | Native REST param, direct |
| `--app` | — | noop | Same reasoning as `pr list` |
| `-L/--limit` | 30 | **wire** | Same mechanism as `pr list` |
| `-S/--search` | — | **reject** | Same reasoning as `pr list` |
| `-q/--jq`, `--json`, `-t/--template`, `-w/--web` | — | reject (pre-existing) | |
| *(no flag)* `perPage` default | — | **default-only** | 20 → 30, matching gh |

**Counts: 8 wired, 1 default-only, 1 noop, 5 rejected.**

## `gh issue comment`

| Flag | Disposition | Notes |
|---|---|---|
| `-b/--body`, `-F/--body-file` | wire (pre-existing) | Unchanged |
| `--attach` | noop | No filesystem in this guest to read an attachment from |
| `--create-if-none` | noop | Only meaningful paired with `--edit-last`, which is rejected |
| `--yes` | noop | Inert either way once `--delete-last` is rejected |
| `--delete-last`, `--edit-last` | **reject** | See cross-cutting notes |
| `-e/--editor`, `-w/--web` | reject (pre-existing) | |

**Counts: 2 wired (pre-existing), 3 noop, 4 rejected.**

## `gh repo view`

| Flag | Disposition | Notes |
|---|---|---|
| `-b/--branch` | **reject** | See cross-cutting notes |
| `-q/--jq`, `--json`, `-t/--template`, `-w/--web` | reject (pre-existing) | |

**Counts: 0 wired, 5 rejected.**

## No upstream equivalent (confirmed, out of scope, unchanged)

`gh content view`, `gh branch view`, `gh commit view`, `gh user view`, `gh pr files`, `gh pr
reviews` — each confirmed via `gh <word> --help` returning "unknown command" on gh 2.100.0. No
gh-spelled flags exist to adopt; nothing was added or changed on these five.

## Totals

| Verb | Wired | Default-only | Noop | Rejected |
|---|---|---|---|---|
| pr list | 8 | 1 | 1 | 5 |
| pr view | 1 | 0 | 0 | 4 |
| pr diff | 1 | 0 | 4 | 1 |
| pr checks | 0 | 0 | 4 | 4 |
| pr review | 5* | 0 | 0 | 0 |
| pr merge | 7 | 0 | 2 | 3 |
| issue view | 1 | 0 | 0 | 4 |
| issue list | 8 | 1 | 1 | 5 |
| issue comment | 2* | 0 | 3 | 4 |
| repo view | 0 | 0 | 0 | 5 |
| **Total** | **33** | **2** | **15** | **35** |

\* pre-existing flags that gained gh's short-flag spelling in this PR, not new fields.

Plus the cross-cutting correctness fixes that aren't per-flag: `MAX_PER_PAGE` 50→100, clap-layer
numeric bounds on page/per-page/limit/number, and the `hasMore` truncation-awareness fix applied to
every paginated list capability.
