# Native enterprise upgrade

This is the existing program for the Rust-native platform and enterprise upgrade.

- Requirements: [platform upgrade plan](../../../docs/plans/platform-upgrade-2026-10/README.md).
- Current implementation, verification, remaining work and next action: [loop-state.json](loop-state.json).
- Working rules: [AGENTS.md](../../../AGENTS.md).

Deliver complete modules with caller wiring and necessary tests, then perform consolidated
compilation, interface checks and real service journeys. Continue independent work when one
acceptance environment is unavailable. Nonblocking cosmetic work follows functional delivery.

The ledger is the single execution record. Reuse test logs; do not create routine per-edit
handoff, resume or checkpoint files. Historical results remain tied to their tested source and
environment. Preserve required acceptance, existing changes, credentials, data and the upstream
Codex harness. Runtime/publication operations follow the user's authorized scope.
