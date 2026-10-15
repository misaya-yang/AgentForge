# Independent Agent Runtime CLI Migration

- Owner: Product/engineering
- Initial base: `1713b3b4c1f222ff23c62ac144da91f5f002bb81`; synchronized to upgrade SHA `f6390bca`
- Worktree: `/Users/yang/projects/AI--Platfform-cli-runtime-client-2026-08-30`
- Upstream source: `codex-harness@94cbbddafc1776d5e377bca1b05932c697e82238`

## Goal

Deliver an independent local Agent Runtime CLI with CLI-owned provider profiles, composed codex-harness behavior, direct Responses access, and bounded Chat Completions compatibility.

## Non-goals

- Do not send CLI provider credentials or config to the hosted Gateway Runtime.
- Do not expose the hosted Runtime `/internal/v1` API or change its leases,
  PostgreSQL state, Gateway model plane, Worker, Compose, or database.
- Do not implement tools, approvals, resume, compaction, or another Agent loop
  in TypeScript.
- Do not promise lossless Chat conversion for images, hosted tools, or provider
  extensions that cannot be represented by the bounded adapter.
- Do not publish, push, merge, deploy, or run host Cargo without authorization.

## Authorization

- Safe local reads, CLI-owned edits, and non-destructive offline checks may proceed.
- Native Rust builds run only in the repository's Docker/hosted-CI paths.
- Docker/provider operations are serialized by the integration owner. The
  current running Compose project belongs to `/Users/yang/projects/AI--Platfform`.
- Provider secret values may be read only at live execution time and must never
  be printed, stored, committed, or included in evidence.

## Product Contract

```text
sdk/cli launcher + strict provider profile
  -> isolated ~/.ai-gateway-cli/runtime
  -> composed codex CLI / InProcessAppServerClient
  -> one local Agent loop
  -> Responses provider directly
     OR loopback Responses-to-Chat adapter -> Chat Completions provider
```

- Product config stores credential environment-variable names, not values.
- The generated Runtime TOML is upstream-compatible and excludes credential
  variables from model-generated child processes.
- Direct Responses uses upstream `ModelProviderInfo` URL/header/query/retry and
  stream semantics.
- Chat compatibility accepts only losslessly representable text/function-tool
  input and emits ordered Responses SSE. Unsupported input fails closed.
- TUI, exec JSONL, approvals, interruption, resume, tools, and persistence are
  native Rust behavior, not launcher behavior.
- Native artifacts are tied to the fixed upstream SHA plus overlay SHA.

## Phase Map

| Phase | Feature | Contract | Depends on |
| --- | --- | --- | --- |
| CLI-00 | CLI-F001 | [Freeze independent product and source contract](phase-00-freeze-upstream-and-current-cli-compatibility-contracts.md) | none |
| CLI-01 | CLI-F002 | [Independent provider config and launcher](phase-01-normalize-standalone-cli-configuration-and-third-party-api-compatibility.md) | CLI-00 |
| CLI-02 | CLI-F003 | [Composed native Runtime packaging](phase-02-integrate-thread-turn-item-streaming-transport-with-agent-runtime.md) | CLI-01 |
| CLI-03 | CLI-F004 | [Chat Completions compatibility](phase-03-preserve-interactive-exec-resume-interrupt-and-tool-workflows.md) | CLI-01 |
| CLI-04 | CLI-F005 | [Deterministic regression gates](phase-04-add-deterministic-offline-compatibility-and-failure-path-tests.md) | CLI-02, CLI-03 |
| CLI-05 | CLI-F006 | [CANDIDATE_80 live acceptance](phase-05-reach-candidate-80-through-serialized-cli-runtime-and-provider-acceptance.md) | CLI-04 |
| CLI-06 | CLI-F007 | [RELEASE_100 provider matrix and rollback](phase-06-reach-release-100-through-regression-rollback-documentation-and-final-review.md) | CLI-05 |

`loop-state.json` is the only execution-status authority.

## Milestones

- `CANDIDATE_80`: product config/launcher/adapter tests and bundle pass; native
  composed CLI builds; one Responses or Chat provider completes text, function
  tool approval/deny, interrupt, and resume with no secret leak.
- `RELEASE_100`: both native Responses and Chat-only provider profiles pass the
  required fault/proxy/resume/tool matrix on final per-platform artifacts, with
  native source identity and rollback receipts and no release-required skips.

## Operating Rules

1. Read state, handoff, init, and active phase; inspect Git status/log.
2. Run the active phase's smallest falsifying check before and after edits.
3. Keep one writer worktree. Terra agents remain read-only explorers/reviewers.
4. A Node/mock pass does not prove the composed Rust binary or a real provider.
5. Continue after failure only with a changed, evidence-backed hypothesis.
6. Do not mark a feature passing until its stated user-visible steps ran.
7. Stop on success, a named blocker, or an authority boundary.

## Validation Boundary

The harness validator checks structure and metadata only. It does not execute
the native Runtime, call a provider, or prove tool/approval/interrupt behavior.
