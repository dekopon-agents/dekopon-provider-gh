# Upstream gh inventory: corrected label rows

This checked-in excerpt records the owner's 2026-10-25 correction to the
`gh` v2.102.0 inventory supplied with the parity brief. Other classifications
remain as briefed. Label colour is repository metadata, not terminal colour.
These commands remain Buildable and retain their base clap behaviour until built.

| Command | Upstream | Buildable flags | Backburner flags | API calls | Class | Proposed capability |
|---|---|---|---|---|---|---|
| `gh label create` | `label/create/create.go:58` | `--description/-d`, `--force/-f`, `--color/-c` | none | POST `/repos/{o}/{r}/labels`; `--force` PATCH on 422 | Buildable | `gh.label.create` |
| `gh label edit` | `label/edit/edit.go:34` | `--description/-d`, `--name/-n`, `--color/-c` | none | PATCH `/repos/{o}/{r}/labels/{name}` | Buildable | `gh.label.edit` |

## Local attachment correction

The coordinator classifies upstream `--attach` local file uploads as Backburner
on `gh issue comment`, `gh issue create`, `gh issue edit`, `gh pr comment`,
`gh pr create`, and `gh pr edit`. Each refuses with the flag named and reason
`needs local file access`; it adds no capability or file upload support.

## Batch 1 Have flags

The base refusal/syntax registry remains authoritative; the appended inventory correction wins
where that registry is silent. The six local `--attach` refusals above retain their exact reason.
Built flags are declared in the visible clap grammar rather than hidden syntax/refusal rows.

| Have command | Available additions |
|---|---|
| `pr list` | base, head, label, assignee, search, draft, app, state all |
| `issue list` | assignee, label, author, mention, milestone, app, search, type, state all |
| `pr view`, `issue view` | comments in text, using the existing comments page |
| `pr diff` | patch, name-only from the same diff GET, exclude, allow-escape-sequences (no-op) |
| `pr merge` | body, body-file `-`, subject |
| `pr review` | owner fold-in match-head-commit on all three events |
| `repo list` | source, fork, language, topic, visibility, archived, no-archived, private, public; same public endpoint |
| `repo read-file` | allow-escape-sequences (no-op) |
| `search code/commits/issues/prs/repos` | every qualifier flag, order/sort, and issue search-type/include-prs; existing scope stays fixed |
| `issue comment` | edit-last, delete-last, yes, create-if-none; three separate write ids |

`gh.pull-request.comment` additionally accepts the owner fold-in line-comments array, with the
same pinned reviews POST and a 16384-byte serialized request limit. It is not the unbuilt
`pr comment` command, which posts an issue comment.

These Buildable flags remain unbuilt under the base-grant rule: checks required (new POST),
merge author-email (new POST), repo view branch (extra GET), repo read-file output (asset attach).
Checks required retains its existing special refusal; the others retain clap errors.
Batch 2 merge modes and all other Buildable commands remain unbuilt.

Upstream v2.102.0 derives diff names from the diff body, contrary to the original inventory's
files-endpoint hint. It maps app filters to author:app/name rather than app:. Upstream silently
prefers name-only over patch; this contract instead requires their explicit combination refusal.
Free-text quoting and stricter typed grammars follow the batch contract rather than upstream's
permissive strings. Private repository enumeration remains unavailable under the base grant.
