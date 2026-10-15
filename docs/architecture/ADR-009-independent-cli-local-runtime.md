# ADR-009: Independent CLI owns a local Runtime and provider profiles

**Status:** Accepted

**Date:** 2026-08-30

**Deciders:** Product owner and AI Gateway maintainers

**Scope:** Independent CLI product, local Agent Runtime, provider configuration, and third-party API compatibility

## Context

The legacy TypeScript CLI is a remote Gateway chat client. That is no longer
the intended product: the CLI must run independently of the Web/Gateway model
configuration and use provider profiles owned by the CLI itself.

The hosted `ai-platform-agent-runtime` cannot safely fill that role. It is a
private, multi-tenant service that requires Gateway-issued identity, model
leases, PostgreSQL state, and the private model plane. Passing long-lived
provider credentials from a desktop CLI into that service would contradict its
authorization and persistence boundaries.

The pinned `codex-harness` source already contains the required standalone
execution model: the TUI and `codex exec` start one in-process App Server,
load local model-provider configuration, and own Thread/Turn/Item, tools,
approvals, interruption, compaction, resume, and output projection.

## Decision

The independent CLI runs the lock-pinned composed Rust CLI locally. The native
binary embeds the App Server and remains the only Agent loop for the CLI
product. It does not call the Gateway or the hosted Runtime.

`sdk/cli` becomes a thin product launcher and provider-compatibility layer:

- it owns an isolated `~/.ai-gateway-cli` home and a strict provider profile;
- it generates the upstream-compatible, non-secret Runtime `config.toml`;
- it launches the composed native CLI with the isolated home;
- it never implements thread state, tool selection, approvals, or another loop.

The previous remote Gateway chat client remains reachable only through the
explicit `ai-gateway gateway` compatibility subcommand and retains its separate
`~/.ai-gateway` configuration. It is not a provider path for this local product.

Provider profiles may select:

1. `responses`: the native Runtime calls a provider's OpenAI-compatible
   `/responses` endpoint directly using upstream `ModelProviderInfo` semantics;
2. `chat_completions`: a loopback-only adapter translates the representable
   Responses text/function-tool subset to `/chat/completions` and projects the
   provider stream back to strict Responses events.

The Chat adapter fails closed for inputs it cannot represent losslessly. It is
not a remote service and is stopped with the CLI process.

## Credentials and local capabilities

- Product configuration stores environment-variable names, never secret
  values. Secrets are not accepted in argv, static headers, or query params.
- Direct Responses credentials enter only the local native process. Generated
  shell-environment policy excludes key/token/secret/password/credential
  variables from model-generated commands.
- Chat-only credentials remain in the adapter process. The native child sees
  only a random, per-launch loopback token.
- Static secret-bearing headers are rejected; secret headers reference named
  environment variables.
- Provider URLs require TLS, except explicitly opted-in loopback development
  profiles.
- Local tools and approvals use the embedded App Server contracts. The legacy
  Node MCP manager is not a Runtime tool plane.

## Hosted platform boundary

ADR-007 remains authoritative for the hosted product. Gateway still owns
provider credentials, quota, billing, tenant policy, and model leases for the
hosted Runtime. This ADR creates a separate local product deployment model; it
does not add a mode to the hosted Runtime and does not alter its API.

Remote standalone Runtime is deferred. It would require a stable authenticated
transport plus a credential broker/delegation design and cannot be obtained by
exposing the existing `/internal/v1` API.

## Consequences

- The CLI gains upstream TUI/exec behavior and provider configurability without
  duplicating the Agent loop.
- Native packages must be built from the exact upstream source plus overlay and
  carry source identity per platform. Build/package gates derive that identity
  from the source receipt, overlay manifest, and lock rather than copied code
  constants.
- Responses-compatible providers are the native path. Chat compatibility is
  intentionally bounded and may reject provider-specific or hosted-tool
  extensions.
- CLI state is local and separate from hosted Gateway sessions. Cross-product
  session synchronization is not implied.

## Verification

- Deterministic launcher/config/adapter tests with no real secrets.
- Hosted Rust fmt/check/tests for the composed source crates.
- Docker-contained native build and source-identity smoke; never host Cargo in
  this repository.
- One real Responses provider and one real Chat-only provider journey covering
  text, function tool, approval, interruption, and resume before release.
