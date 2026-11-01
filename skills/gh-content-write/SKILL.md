---
name: gh-content-write
description: Create or replace one UTF-8 repository file on an explicit existing branch through the separately granted gh.content.write capability.
---

Use this provider extension only when authorized to change one file on an existing run branch.
It is not an upstream GitHub CLI command or generic API passthrough.

```text
gh repo write-file PATH -R OWNER/REPO --branch BRANCH --message TEXT --create --body-file -
gh repo write-file PATH -R OWNER/REPO --branch BRANCH --message TEXT --sha BLOB_SHA --body-file -
```

Pipe UTF-8 content (0–4096 bytes); local file loading is unavailable. Message is required and
at most 4096 UTF-8 bytes. The serialized JSON request must fit 16 KiB, including escaping.
Exactly one of create and SHA-pinned update is required. Obtain the file blob SHA with the
separately granted `gh repo read-file PATH -R OWNER/REPO --ref BRANCH`; it is not branch HEAD.
Direct invocation requires owner, repo, path, branch, message, mode and body; update additionally
requires sha, which create refuses. Unknown fields refuse.

One PUT changes one path in one commit. No implicit read, retry, merge, branch creation, or
create-to-overwrite fallback occurs. Unrelated commits may advance the branch.
Success returns owner, repo, path, branch, blobSha and commitSha, never submitted content.
Failures leave stdout empty. Use exit status plus stderr, not a named broker detail code:

- Exit 4: `file write refused: content conflict or repository rule violation` (HTTP 409,
  internal name `content-conflict`). GitHub documents repository-rule violations as well as
  basic conflicts under 409; this does not establish a SHA mismatch.
- Exit 3: definitive API refusals, distinguished by stderr: `credential is not permitted to
  make this call` (403 without permission headers), `not found, or not visible to this credential`
  (404), or `endpoint refused the request as invalid` (422).
- Exit 5: `file write outcome is unknown; read state before deciding what to do next`
  (transport failure or unconfirmed HTTP response, internal name `write-outcome-unknown`).
  This includes HTTP 408/499, every 5xx response such as 500/502/503/504, and unexpected success
  or redirect responses; none establishes the write's final effect.

A broker-side operation timeout is a broker failure without an exit status and must also be
treated as unknown. Read state before deciding what to do next; never blindly retry an unknown
result. See the [outcome and REST documentation](../../README.md#single-file-writes-on-an-explicit-branch).

A skill grants no authority. The owner must separately grant `gh.content.write`, scoped by
principal and agent, with Contents: Write and an exact file/branch request fence. Existing
read grants remain GET-only and must pin their exact ref query.
