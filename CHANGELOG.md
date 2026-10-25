# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.11.0] - 2026-10-25

### Added

- Inventory-backed unavailable command and flag refusals with matching help and preserved clap typo errors.
- Upstream flag shorthands and closed JSON selection for auth status.
- Local version, licenses, actions and accessibility output.

### Changed

- Document POST grants for fixed-document GraphQL reads.
- Identify unavailable flags and scopes while preserving required arguments, readable help and base clap errors.
- Preserve upstream flag arity, option values, shorthand clusters and usage errors in gh refusal handling.
- Add scoped PR and issue list filters and state all.
- Add all typed search qualifier flags with closed validation.
- Add public-endpoint repository filters and escape-sequence no-op flags.
- Add single-request patch, name-only and glob exclusions to PR diffs.
- Render PR and issue comments in text.
- Support review head guards, bounded line comments and merge commit text.
- Add separate issue comment edit, delete and edit-or-create capabilities.
- Keep a typed issue label containing a comma as one scoped search term.
- Preserve legacy search query errors and accounting while bounding complete typed queries on new search paths.

### Fixed

- Read complete pull-request file lists through validated GitHub pagination without partial output on failure.

## [0.10.0] - 2026-10-16

### Changed

- gh now refuses removed CLI words and routes finite API reads and numbered checks through existing capabilities.
- Finite read commands support closed `--json` selections and non-TTY text output.
- Pull request and issue reads print upstream-shaped selected JSON or command-specific text, with bounded view details.
- Repository content commands refuse mismatched file and directory responses.
- `gh pr list` and `gh issue list` collect up to 100 matching rows across bounded pages without partial output.
- `gh repo list OWNER` enumerates public repositories through a separate read grant.
- `gh search prs`, `issues`, `repos`, `code` and `commits` accept explicit owner or repository scope with bounded results.
- `gh pr checks` now reports third-party checks and exits 8 while pending, 1 on failure or no checks, and 3 on provider-observed errors.
- `gh api user` returns the authenticated GitHub identity under its own read grant.
- Pull request merge requires an explicit merge method.
- Three redundant read grants are no longer declared.

### Fixed

- Finite `gh api` user, branch and commit reads now return upstream-shaped JSON.
- Build with patched rustls 0.23.46 and Wasmtime 49.0.3.

## [0.9.0] - 2026-10-12

### Added

- Inspect credential quota, expiration, and scopes with `gh auth status`.

### Changed

- Move to provider SDK and testkit 0.45.0 to support additional GitHub capabilities.

### Fixed

- Explain rejected credentials, required permissions or scopes, and ambiguous not-found responses.
- Reject `providerSettings.gh.baseUrl: null` as invalid settings instead of using the default.

## [0.8.0] - 2026-10-08

### Added

- Owner setting `providerSettings.gh.baseUrl` for GitHub Enterprise Server or a recording proxy, defaulting to `https://api.github.com`.

### Changed

- Move to provider SDK and testkit 0.38.0.

### Removed

- The model-facing `endpoint` input on every capability; a call can no longer choose its origin.

## [0.7.0] - 2026-10-04

### Changed

- Move to provider SDK and testkit 0.33.0 and HTTP 1.1.0 imports; preserve all nineteen GitHub capabilities and their authorization boundaries.

## [0.6.0] - 2026-10-03

### Changed

- Move to provider SDK and testkit 0.31.0 with typed stdio streams and brokered HTTP 1.2.0; retain the nineteen GitHub capabilities and their authorization boundaries.

## [0.5.0] - 2026-09-20

### Changed

- Move to provider SDK 0.18.0 and HTTP 1.1.0 bindings; no caller-facing behavior changes.
