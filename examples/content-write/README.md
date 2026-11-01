# Disabled single-file run-branch fixture

This directory is not loaded by the PR reviewer or any deployment. An owner deliberately
combines `constraints.yaml` with their broker configuration and loads `policies.cedar` only
after replacing the synthetic attestor gateway, principal, agent, repository and exact CI-created branch.
Bind `github-app` to the existing broker-owned GitHub App credential, downscoped to the one
repository and Contents: Write. Do not add Workflows permission. Bootstrap
`config/mutable/agent.yaml` as plain data; never interpret it as arbitrary Kubernetes workload
or RBAC configuration. This fixture creates no credential, installation, branch or live grant.

Keep both policy layers: the permit pins authenticated identity and agent, while the guarded
request forbid checks every emitted PUT's exact host/path, absent query and JSON branch.
`allowedHosts: [api.github.com]` and disabled plaintext require HTTPS on port 443. Buffered
JSON must fit 16 KiB. Missing branch, malformed JSON, non-record or opaque body refuses;
policy evaluation errors deny. Changing `baseUrl` requires an owner-authored matching fence,
not a model input. The separate content read permits only GET with the exact serialized
`ref=runs%2Fcontent-123` query; a no-query write fence must never be copied onto reads.

From the provider root, after the pinned build:

```console
DEKOPON_PROVIDER_COMPONENT="$PWD/gh-provider.wasm" cargo test --locked --test content_write
DEKOPON_PROVIDER_COMPONENT="$PWD/gh-provider.wasm" cargo test --locked --test content_write_broker
```

The first command uses HttpScript and the production component. The second exercises the real
0.64.0 broker request-policy seam, with a local fake GitHub and the same policy text, substituting
only the loopback host and `/gh` prefix. An isolated test component emits malformed/opaque bodies;
its raw-request inputs and implementation are absent from the production artifact. Denied cases assert
zero egress; the admitted request asserts its exact URI/body and one egress.

The daemon smoke uses only a local fake and a temporary synthetic bearer credential, deleted
after the owned daemon exits:

```console
DEKOPON_PROVIDER_COMPONENT="$PWD/gh-provider.wasm" cargo run --locked --example content-write-smoke -- /path/to/dekopon-brokerd-0.64.0
```
