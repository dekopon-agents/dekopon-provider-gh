# PR summarizer and linter

A maintainer sends a Slack DM — “summarize and lint PR 7 in owner/repo” — and receives a concise
answer while the pull request gets one `COMMENT` review containing the summary, actionable lint
findings, and check status.

The gateway authenticates the message and vouches for the sender; it decides nothing. The broker
maps that Slack subject to the principal `maintainer`, checks Cedar policy, exposes five narrow
capabilities, injects a GitHub token bound to `api.github.com`, executes the `gh` WebAssembly
component, and hash-links every decision and result into an audit chain naming the person who
asked. The token is never visible to the model, shell session, agent, or component that uses it.

The provider still exports twenty-nine production capabilities; this example grants seven.
Upstream shorthands select the same grants: `gh pr review ... -c -b <text>` is equivalent to
`--comment --body <text>`, and `-F -` reads a piped body. Unsupported upstream operations refuse
before HTTP; `gh --help` lists their reasons. `gh version`, `licenses`, `actions`, and
`accessibility` are pure local output. Future GraphQL-backed reads need an explicit POST grant;
this example's existing REST read grants stay GET-only.

This workflow can submit COMMENT reviews, including optional line comments. It cannot approve, request changes, merge, or edit/delete issue comments. Those are separate `gh`
capabilities absent from the catalog, policy, and broker constraint sets, and tests prove the
omission against the checked-in provider manifest.

| File | What it is | Who reads it |
|---|---|---|
| [`dekopon.yaml`](dekopon.yaml) | Catalog: the agent, its standing orders, and the capability words it may spell | `dekopon-gatewayd` |
| [`broker.yaml`](broker.yaml) | Identities, mappings, providers, and execution constraints | `dekopon-brokerd` |
| [`policies.cedar`](policies.cedar) | Who may drive the agent and which actions it may reach | `dekopon-brokerd` |
| [`broker-credentials.yaml.example`](broker-credentials.yaml.example) | GitHub token template | `dekopon-brokerd` |
| [`gatewayd.yaml`](gatewayd.yaml) | Transport, model, route, and session bounds | `dekopon-gatewayd` |

Nothing here is a mock: these are the files a deployment runs, and the constraint sets in
`broker.yaml` mirror this provider's manifest field for field, which the broker checks at startup
and refuses to start on. The allow/deny table these files declare was asserted by
`crates/dekopon-brokerd/tests/examples.rs` while the provider lived in the dekopon tree; the
example came here with the component, and nothing in dekopon exercises it any more.

## What “lint” means here

The agent reviews bounded GitHub data: pull-request metadata, changed-file patches, a unified diff,
real check runs (including third-party integrations), legacy commit statuses, Actions workflow names, and selected files at the observed head SHA. It
can identify likely correctness,
security, reliability, maintainability, and test-coverage problems. It cannot check out the branch,
run repository commands, or claim that a compiler or project linter passed. Its standing orders
require it to say when truncation or missing context limits a conclusion.

Repository text is untrusted throughout. A pull-request description, diff, source file, or check
message can influence the model’s proposal, but cannot assert identity, widen policy, select a
credential, or authorize the resulting comment.

## 1. Create the Slack app

Follow [dekopon's Slack example](https://github.com/dekopon-agents/dekopon/blob/main/examples/slack/README.md): create the app from
[`manifest-agent.yaml`](https://github.com/dekopon-agents/dekopon/blob/main/examples/slack/manifest-agent.yaml), generate the app-level token (`xapp-…`, scope
`connections:write`), install it for the bot token (`xoxb-…`), and find the workspace `T…` team ID
and permitted sender’s `U…` member ID. Socket Mode needs no public HTTP endpoint or inbound firewall
rule. Slack shows its native Working/Stop session while an authorized review runs; a workspace
without the Agent feature degrades to the temporary `:tangerine:` reaction.

## 2. Create the GitHub token

Create a fine-grained personal access token scoped to only the repositories this agent may review:

- **Contents: Read-only** — `gh.content.read`
- **Pull requests: Read and write** — pull-request reads and one `COMMENT` review
- **Checks: Read-only** — check runs returned by `gh.pull-request.status`
- **Actions: Read-only** — workflow names and events for Actions check runs
- **Commit statuses: Read-only** — legacy statuses returned by `gh.pull-request.status`

Verify the chosen credential can read the check-runs endpoint for these repositories. A 403 or
broker refusal is an error, not evidence that checks passed; checks-only integrations are included.

GitHub has no comment-only pull-request permission. Dekopon narrows that provider permission by
exposing comment, approval, request-changes, and merge as separate capabilities. This deployment
constrains only comment. The token’s own repository scope remains important: Cedar does not inspect
provider input paths and therefore cannot restrict `owner/repo` itself.

Classic PAT and GitHub App installation-token setup, plus the credential error messages, are
covered in the [provider authentication guide](../../README.md#credentials-and-authentication-status).
`gh auth status --json limit,remaining,reset` uses the separate read-only `gh.auth.status` capability. This five-capability reviewer
does not grant it: to enable it, an owner must add it to the catalog and policy and give it its own
Low-risk, read-only constraint set with the same credential and one GET request. A successful
quota check does not establish access to a particular repository.

Then create the owner-only credentials file:

```console
cp broker-credentials.yaml.example broker-credentials.yaml
chmod 600 broker-credentials.yaml
$EDITOR broker-credentials.yaml
```

`chmod 600` is enforced, not suggested. The broker rejects group or world readability, symlinks,
hard links, and wrong ownership. `broker-credentials.yaml` is ignored by Git.

## 3. Adjust the placeholders

| Placeholder | File | Replace with |
|---|---|---|
| `/home/xavier/.local/{run,state}/dekopon/…` | `broker.yaml` | Your socket, audit, and checkpoint paths |
| `/home/xavier/.local/run/dekopon/broker.sock` | `gatewayd.yaml` | The same socket path |
| `uid: 501` | `broker.yaml` | Your UID (`id -u`) |
| `serverUid: 501` | `gatewayd.yaml` | The same UID |
| `slack.t0123abcd` | `broker.yaml` | `slack.` plus the lowercased team ID |
| `slack.t0123abcd.u0123abcd` | `broker.yaml` | Lowercased `slack.<team>.<user>` |
| `github_pat_XXXX…` | `broker-credentials.yaml` | The fine-grained token |

The principal `maintainer`, gateway principal `dekopon-gatewayd`, and agent
`pr-summarizer-linter` are internal names repeated across configuration and policy. Rename each
only if every occurrence changes together.

Relative paths resolve against the configuration file’s directory, so the provider component,
policy, credentials file, and catalog work from a checkout without edits.

```console
mkdir -p ~/.local/run/dekopon ~/.local/state/dekopon
chmod 700 ~/.local/run/dekopon ~/.local/state/dekopon
chmod 600 broker.yaml policies.cedar gatewayd.yaml
```

The catalog is validated where it is read: `dekopon-gatewayd` loads `catalogPath` at startup and refuses
to start naming every problem at once. It proves the agent's own metadata and its skill files, and
nothing about permission — capabilities and providers come from the broker, which builds them from
the provider manifest and its own constraint sets.

## 4. Run the broker

```console
dekopon-brokerd --config broker.yaml
```

```json
{"level":"INFO","event":"broker_started","audit_records":0,"audit_head":"none","target":"dekopon_brokerd"}
```

Before binding its socket, the broker strictly validates policy, checks that every permitted
capability has a matching constraint set, loads the destination-bound credential, and verifies the
provider manifest. A mismatch is a startup failure, not a surprise during a review.

## 5. Run the gateway

```console
export DEKOPON_GATEWAYD_SLACK_APP_TOKEN=xapp-...
export DEKOPON_GATEWAYD_SLACK_BOT_TOKEN=xoxb-...
dekopon-gatewayd --config gatewayd.yaml
```

```json
{"level":"INFO","event":"gateway_broker_ready","capability.count":0,"target":"dekopon_gatewayd"}
{"level":"INFO","event":"gateway_transport_connected","transport":"workspace-slack","kind":"slackSocketMode","target":"dekopon_gatewayd::transport::recovery"}
{"level":"INFO","event":"gateway_started","transport.count":1,"route.count":1,"target":"dekopon_gatewayd"}
```

`capability.count: 0` is intentional. The startup probe asks what the gateway’s own direct identity
may do; both Cedar statements require an attested sender and agent. The gateway reaches the five
capabilities only while vouching for an allowed Slack subject. `gh repo list OWNER` is a separate
`gh.repo.list` read grant; this example does not grant it.

## 6. Summarize and lint a pull request

> **maintainer:** Summarize and lint PR 7 in owner/repo, then post the result.

A session typically proposes commands like these through the sandboxed `gh` builtin:

```console
$ gh pr view 7 -R owner/repo --json number,title,state,headRefOid,baseRefName,url
{"baseRefName":"main","headRefOid":"4f2a91c8e3b7d05a6c1f8e2b9d4a70c3e5f81b26","number":7,"state":"OPEN",
 "title":"Cache the provider registry between invocations","url":"https://github.com/owner/repo/pull/7"}

$ gh pr diff 7 -R owner/repo
{"diff":"diff --git a/... b/...\n...","diffTruncated":false,"number":7}

$ gh pr checks 7 -R owner/repo --json name,bucket,workflow
[{"bucket":"pass","name":"ci / validate","workflow":"Validate"}]
# Without --json, checks print TSV; failed checks exit 1 and pending checks exit 8.

$ gh repo read-file crates/dekopon-broker-host/src/lib.rs -R owner/repo \
    --ref 4f2a91c8e3b7d05a6c1f8e2b9d4a70c3e5f81b26
...decoded file content...

$ gh pr review 7 -R owner/repo --comment \
    --body "## Summary ...\n\n## Lint findings ...\n\n## Checks ..."
{"reviewId":2418809931,"state":"COMMENTED","pullNumber":7,
 "headSha":"4f2a91c8e3b7d05a6c1f8e2b9d4a70c3e5f81b26","author":"review-bot",...}
```

Each `gh` command is one capability proposal, not an operating-system process. There is no real
`gh` executable, checkout, or filesystem. Arbitrary `gh api` routes are refused because a generic
GET would collapse separately authorized capabilities into everything the token can read.
The finite `gh api user` route requires its own `gh.user-self.read` grant, which this reviewer
example does not request.

The comment capability pre-reads the pull request and pins the head it observed in its POST.
The agent supplies the metadata head SHA using `--match-head-commit`, so a moved head refuses
before the POST. A push between the provider pre-read and submission is not prevented. The agent’s orders
say not to retry: the new revision needs a new review. Drafts may receive comments, but closed or
merged pull requests are refused.

The final Slack answer reports what was posted and its review ID, after which the Agent session
returns to `active` (or the fallback reaction is removed). On an internal failure it instead
receives the fixed sentence `The agent could not complete this request.`; model, provider, and
transport error text never leaks back into chat.

The route is `mode: persistent`, so a follow-up like “explain the first finding” can use the compact
`(question, answer)` pair kept in gateway memory. Tool calls and GitHub output are not retained.
Nothing is written to disk or sent to the broker, and an idle timeout or changed grant drops the
history.

## 7. Inspect the audit chain

```console
tail -1 ~/.local/state/dekopon/audit.jsonl | jq .
```

The terminal comment record has this shape (hashes abbreviated):

```json
{
  "sequence": 10,
  "previousHash": "sha256:1d0a…",
  "event": {
    "type": "execution",
    "invocation": "dekopond-session-9f1c4a7b0e35d268-5",
    "trace": "dekopond-session-9f1c4a7b0e35d268",
    "principal": "maintainer",
    "actor": { "type": "agent", "agent": "pr-summarizer-linter" },
    "via": "dekopon-gatewayd",
    "attested_subject": "slack.t0123abcd.u0123abcd",
    "capability": "gh.pull-request.comment",
    "provider": "gh",
    "policy_revision": "pr-summarizer-linter-2026-01",
    "policy_ids": ["pr-summarizer-linter-gh-surface"],
    "effect": "external-write",
    "risk": "Medium",
    "credential": "github-pat",
    "outcome": "Succeeded",
    "output_digest": "sha256:9ab4…",
    "http_calls": [
      { "method": "GET", "authority": "api.github.com", "status": 200,
        "credentialInjected": true },
      { "method": "POST", "authority": "api.github.com", "status": 200,
        "credentialInjected": true }
    ]
  },
  "recordHash": "sha256:c7e2…"
}
```

The chain records trusted attribution, policy identity, bounded HTTP metadata, outcome, and output
digest. It does not record the Slack message, repository path, diff, source, comment body, URL path
or query, headers, token, or provider output. Earlier decision/execution pairs share the trace ID.

## Refusals worth knowing

| Symptom | Likely cause |
|---|---|
| `You're not authorized to use this agent.` | Subject outside the attestor grant, no identity mapping, or no `agent.prompt` policy |
| Authorized sender, zero capabilities | Capability policy’s `context.agent` or `context.via` does not match the session policy |
| `head-changed` | New commits arrived after inspection; start a new review instead of retrying |
| `pr-closed` | Pull request is closed or merged; no comment was posted |
| Broker says a capability has no constraint set | Policy and `broker.yaml` disagree; startup failed closed |
| Broker says credential is unknown or host is uncovered | Credentials file is absent/misnamed or lacks `api.github.com` (or the `providerSettings.gh.baseUrl` authority) |
| `invalid-settings` on every `gh` call | `providerSettings.gh` has a key other than `baseUrl`, or `baseUrl` carries a query, fragment or userinfo |
| Gateway exits naming an environment variable | Slack or model credential variable is unset; only its name is logged |

Approval, request-changes, and merge do not produce runtime provider errors here: they are absent
from the session. A model-written command using one reports the exact missing capability and exits
`127`; policy and constraints independently omit it as defense in depth.

## Current limitation

`dekopon-gatewayd` and `dekopon-brokerd` run under one UID in this example because the broker socket is
owner-only. The attestor grant therefore provides attribution and deny-by-default shape, not
isolation from another process under that UID. A dedicated gateway UID remains committed direction.

Prompt injection is not solved. Repository text can manipulate model output and remain in a
persistent conversation until its bounds evict it. Containment comes from explicit capabilities,
trusted identity mapping, per-message authorization, broker-held credentials, constrained provider
execution, and audit — not from trusting the summary.

## Related

- [dekopon's Slack example](https://github.com/dekopon-agents/dekopon/blob/main/examples/slack/README.md) — app manifest, tokens, and Slack identifiers.
- [the provider](../../README.md) — the narrow capabilities and no raw passthrough.
- [`gatewayd.md`](https://github.com/dekopon-agents/dekopon/blob/main/docs/gatewayd.md) — routing, sessions, and conversation bounds.
- [`dekopon-brokerd`](https://github.com/dekopon-agents/dekopon/blob/main/crates/dekopon-brokerd/README.md) — broker configuration and recovery.
- [`security-model.md`](https://github.com/dekopon-agents/dekopon/blob/main/docs/security-model.md) — trust boundaries and limitations.
- [`examples/local/dekopon.yaml`](https://github.com/dekopon-agents/dekopon/blob/main/examples/local/dekopon.yaml) — a smaller catalog-only reviewer declaration.

`gh pr view --json files` reads up to 3,000 files with one metadata request and up to
30 file requests. Operators using this expansion need a larger `gh.pull-request.read`
request grant than this metadata-only example; 32 requests and 1 MiB per response
accommodate files plus either reviews or comments. Requesting all three with 30 file
pages needs 33 requests and fails at the broker’s default 32-request ceiling.
A byte or request refusal fails the whole read without partial output.

## Batch 1 grant choices

The five pre-Actions capability ids retain their grants. The PR read grant allows two GETs, so text
`pr view --comments` can fetch its single comments page; the agent's normal metadata selection
still makes one request. Diff name-only, patch and exclusions retain the one-GET grant.
The COMMENT grant still allows one pre-read and one POST, with a 16384-byte request ceiling.
Optional line-comment arrays fit that grant and require a positive line and validated content path;
the review body can accompany them. The provider checks the aggregate serialized limit before HTTP.

The three new issue-comment capabilities are deliberately absent from this reviewer. An owner
who enables `gh.issue.comment-edit`, `gh.issue.comment-delete`, or `gh.issue.comment-edit-or-create`
must add each chosen id separately to the catalog, Cedar actions and broker constraint sets:
external-write, Medium risk, Issues: Write, allowedMethods `[POST]`, allowedHosts `[api.github.com]`,
and up to eleven requests (ten lookup pages plus one mutation). PR numbers additionally need
Pull requests: Read for GraphQL lookup. They use fixed documents and never accept a query or API path.
They edit/delete issue comments and have no PR-head dependency. The compound edit-or-create id
has its own grant because its fallback creates a comment. No read capability gains POST.

## Actions read grants

The example additionally grants `gh.run.list` (21 GETs) and `gh.run.read` (29 GETs),
with Actions: Read and Checks: Read for extracted annotations. There are seven grants total.
Use run list by PR branch or each head SHA to investigate history: `pr checks` reads only the
current head and collapses repeated checks. Inspect coverage before claiming a complete history.
Run view supports explicit attempts and jobs; a selected job reports its completed duration
against the entire selected attempt's job count. Logs and artifacts are not fetched.
These example grants do not change deployed homelab grants.
