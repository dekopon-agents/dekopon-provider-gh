# Guidance for coding agents

The GitHub provider for Dekopon: one Wasm component, nineteen narrow capabilities, no `gh.api.*`
passthrough. [README.md](README.md) is the design; read the section your change touches.

## Contract

- This file and the documents it links are the whole contract. A cloud agent in a bare checkout has
  no owner memories, machine instructions or sibling repositories, and needs none: clone what a
  gate needs (below), and name every gate you could not run.
- Core's [boundaries](https://github.com/dekopon-agents/dekopon/blob/main/AGENTS.md#boundaries-that-must-survive),
  [proportionate remedies](https://github.com/dekopon-agents/dekopon/blob/main/AGENTS.md#proportionate-remedies)
  and [Rust guidelines](https://github.com/dekopon-agents/dekopon/blob/main/AGENTS.md#rust-guidelines)
  apply here. This file adds only what is particular to this repository.
- Tags, releases and version bumps need the owner's authorization for that version. A PR never
  pushes `main`.

## Boundaries particular to gh

- The component never sets `authorization`. The broker injects a destination-bound credential at
  the native HTTP boundary; no capability input, output or error carries token bytes.
- No capability input names the origin. `baseUrl` is an owner setting in `broker.yaml`, never a
  model-facing field.
- A write stays narrow and SHA-pinned. A new capability gets its own id, its own manifest entry
  and its own grant; never widen an existing one into a passthrough.

## Build and verify

A PR passes `ci / validate` before merge, the shared job in
[provider-workflows](https://github.com/dekopon-agents/provider-workflows). Run its steps before a push:

```console
git clone --depth 1 https://github.com/dekopon-agents/provider-workflows ../provider-workflows
cargo fmt --all --check
cargo deny --all-features check bans licenses sources advisories
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked --package dekopon-gh-provider --lib --target wasm32-unknown-unknown -- -D warnings
../provider-workflows/build.sh
DEKOPON_PROVIDER_COMPONENT="$PWD/gh-provider.wasm" cargo test --locked --workspace
```

Pins are exact (`rust-toolchain.toml`, wasm-tools 1.259.0) because `build.sh` asserts
reproducible bytes. Nothing in the suite contacts GitHub.

## Change rules

- No comments or doc comments by default; core's
  [Comments](https://github.com/dekopon-agents/dekopon/blob/main/AGENTS.md#comments) rule decides
  the exceptions. Clap `///` is help text and stays.
- Test names are sentences that state the invariant.
- A component test reads `DEKOPON_PROVIDER_COMPONENT` with `expect`: no fallback file, early
  return or skip. A native-only test carries a native name.
- A test that drives a request asserts what it caused: the full URI with any `baseUrl` prefix,
  exactly one call, and no guest-sent `authorization`. Name what each harness observes
  (`HttpScript`, `tests/cassettes/`) before claiming coverage.
- Cassettes are recorded with `cassette record`, which saves `authorization` as `[redacted]`.
  Never commit a real token or a recording made without the recorder.
- A change to a setting or capability updates `examples/pr-summarizer-linter/` in the same PR.
- Leave `CHANGELOG.md` to the release-prep commit (`chore: prepare gh provider X.Y.Z`); the PR body
  says what changed.
- A fleet re-pin moves `dekopon-provider-sdk` and `dekopon-provider-sdk-testkit` together, exact
  (`=X.Y.Z`); regenerate `Cargo.lock` with Cargo, never by hand.
