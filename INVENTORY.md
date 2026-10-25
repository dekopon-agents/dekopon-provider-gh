# Upstream gh inventory: corrected label rows

This checked-in excerpt records the owner's 2026-10-25 correction to the
`gh` v2.102.0 inventory supplied with the parity brief. Other classifications
remain as briefed. Label colour is repository metadata, not terminal colour.
These commands remain Buildable and retain their base clap behaviour until built.

| Command | Upstream | Buildable flags | Backburner flags | API calls | Class | Proposed capability |
|---|---|---|---|---|---|---|
| `gh label create` | `label/create/create.go:58` | `--description/-d`, `--force/-f`, `--color/-c` | none | POST `/repos/{o}/{r}/labels`; `--force` PATCH on 422 | Buildable | `gh.label.create` |
| `gh label edit` | `label/edit/edit.go:34` | `--description/-d`, `--name/-n`, `--color/-c` | none | PATCH `/repos/{o}/{r}/labels/{name}` | Buildable | `gh.label.edit` |
