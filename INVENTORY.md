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
