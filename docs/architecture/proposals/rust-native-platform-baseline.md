# P-ADR-1: Rust-native platform baseline

**Status:** Proposed (successor draft; permanent ADR number and acceptance remain with the owner)
**Date:** 2026-10-17
**Deciders:** Repository owner and platform maintainers

## Context

The owner authorized the Rust-native upgrade on 2026-10-15 and adopted the
[platform upgrade plan](../../plans/platform-upgrade-2026-10/README.md) for execution.
Current first-party Gateway, Knowledge, SDK Worker, native clients, migration authority and
operational tooling are Rust. Python service implementations and backup trees have
been removed. Python remains only as an optional user-code sandbox runtime. Web and the
independent CLI launcher are TypeScript; Java/Dart SDKs retain their own client languages.

[ADR-008 §2](../ADR-008-bounded-contexts-no-new-services.md#2-polyglot-is-a-permanent-property-not-a-transition)
records permanent Python Gateway/Knowledge ownership. Its three bounded contexts,
single managed kernel and evidence requirement for new services remain useful;
its language/default-deployment clauses no longer describe current source. Accepted
ADRs remain immutable historical decisions. This draft names the exact successor
scope rather than rewriting ADR-008 or pretending source migration proves acceptance.

## Decision proposed

Supersede ADR-008 §1's Python Gateway/Knowledge implementation cells and §2's Python
service ownership with the current Rust-native baseline. Retain gateway-control,
agent-execution and knowledge contexts, ADR-006/007's single managed Codex kernel and
ADR-009's independently owned local CLI. React/TypeScript remains the console surface.

Keep domain, contract and process boundaries distinct. Knowledge still links the
Gateway crate graph through a named transitional exception; U-KN-01 will remove it
only with unchanged wire hashes and measured compile scope. Do not rename ported
modules or replace Python-compatible hashes/numbers without their own versioned
migration and compatibility proof.

Base Compose consumes versioned images; opt-in build overlays compile native
binaries. Runtime/Worker keep their pinned upstream toolchain and controlled source
assembly. Source/config identity, compilation, image admission and live product
acceptance remain separate gates in the existing execution ledger.

## Options considered

| Option | Complexity and cost | Compatibility | Assessment |
| --- | --- | --- | --- |
| Incremental Rust-native baseline | Existing native code/tooling; migration work remains | Preserve APIs, canonical history, hashes and tenant policy | Proposed; matches current source and owner direction |
| Restore Python services | Deleted implementations and duplicate maintenance | Another unaccepted rollback/profile surface | No implementation or authorization to restore |
| Rewrite/split services now | New contracts/processes and operational cost | Broad acceptance risk without measured capacity need | Retain ADR-008's evidence requirement |

## Trade-offs and consequences

One native implementation reduces duplicate operational paths. Two pinned Rust
toolchains remain because upstream owns the Runtime toolchain. Migration-named
modules, untyped domain records and transitional dependencies remain explicit debt.
Compilation and line-count changes alone do not prove better quality or performance.

Deployment gaps remain: current images and live journeys are unaccepted; public
Compose still has superuser DSN defaults; the Helm chart lacks the complete native topology;
Runtime/Worker schema admission is incomplete. Each retains its package and original
gate. This proposal does not authorize production changes, new paid resources,
publication, signature changes or deletion of compatibility code.

## Action items

1. Owner assigns a permanent ADR number and records acceptance or revisions.
2. U-PE-02 aligns live architecture/commands and checks documentation truth.
3. Existing Ph0 packages prove current-source images, complete journeys and baselines.
4. Later packages evolve typed domains, trust and scale under their declared gates.

Execution status belongs only to
[`native-enterprise-upgrade/loop-state.json`](../../../deploy/runbooks/native-enterprise-upgrade/loop-state.json).
