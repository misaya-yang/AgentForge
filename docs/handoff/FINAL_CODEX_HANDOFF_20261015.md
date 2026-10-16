> Historical handoff, superseded by the owner’s full Rust migration and Rust-native architecture upgrade on 2026-10-15. Follow AGENTS.md and docs/migration/full-rust-status.md for the current source and verification boundaries. This earlier handoff does not certify the current revision.

# Final six-service review → Codex handoff

**Snapshot: 2026-10-15, UTC. This is a development handoff, not enterprise release acceptance.**

The user's latest instruction is a bounded final review/fix of six core service groups,
then delivery to Codex. Do not restart the older open-ended development loop. Preserve
all existing source changes and the Python hybrid architecture. Do not commit, push,
deploy, use production credentials, change security settings, spend provider funds or
operate the user's computer without the relevant authorization.

## Start here

1. Verify the archive using root `VERIFY_RECOVERY.py` / `RECOVERY_MANIFEST.json` and read
   `RESTORE.md`. Work in a fresh checkout. Do not execute historical candidate patches.
2. Read `AGENTS.md`, `docs/harness/runtime-and-secrets.md`,
   `docs/harness/commands.md`, and this document before the older chronological handoff.
3. Read the latest records in `deploy/runbooks/native-enterprise-upgrade/loop-state.json`.
   Earlier counts and top-level legacy notes do not override current source-bound records.
4. The six review bundles are at archive-root `recovery-candidate/final-review/`.
   Each records precise source scope, hashes, tests, limitations and follow-up commands.
5. Restore Runtime upstream from the immutable pin in `deploy/agent-runtime-source/lock.json`
   using the existing source-assembly procedure. The full upstream checkout and dependency
   caches are not bundled. This was also true of version124; the package is not an offline
   dependency distribution. Do not update lockfiles merely to make installation succeed.

## What was actually integrated in the final review

Root checked all preimages before integrating these **12 files**. The two Rust candidates
were deliberately NOT integrated. Cross-review was read-only, followed by bounded isolated
verification and root source-to-candidate mapping.

| Group | Main-tree result | Actual bounded verification | Not established |
|---|---|---|---|
| Python Gateway | Existing local account active/tenant and current authorization rechecked for JWT and owner-bound keys; external/no-profile and independent key contracts retained | 34 route/dependency tests; five-file Ruff; expected baseline failures | Real DB/Redis, complete credential revocation |
| Native Gateway | No main modification; isolated nonstream terminal-correctness patch | Source cross-review; rustfmt parsing/format only; eight test functions NOT run | Cargo typing/linking, real Runtime/HTTP, authorized native citation parity |
| Agent Runtime | No main modification; isolated persistent SSE cursor-gap patch | Source cross-review; rustfmt and patch applicability only; classifier test NOT run | Cargo/real PostgreSQL ordering, reconnect/restart behavior |
| SDK Worker | One stale test assertion replaced with strict AST ordering; no production change | Same 91 controlled cases, all candidate passes; baseline exactly one stale assertion failure | Current installed official SDKs, real child/provider execution |
| Knowledge Service | Durable delete/reembed batch dispatch revalidates current actor and dataset editor access rather than historical roles | 58 tests and two-file Ruff; expected baseline failures | Real Gateway/PG/Qdrant/S3, claim races, live cloud connectors |
| Local Node | Oversized receipt gets a bounded terminal outcome, preserving consumed command and preventing replay | 20 file/SQLite/runner tests and three-file Ruff; expected baseline failures | Production pairing/device transport, real host/Office workflow |

Canonical root checks after joint integration: `make harness-check architecture-boundary-gate`
passed; 634 Python files, zero architecture violations. Five pre-existing harness warnings
remain. Candidate tests used controlled dependencies; do not add their counts together as
an enterprise-acceptance score.

The local-node fallback reports oversized read-only results as `failed`; a successful
potential-side-effect result becomes `unknown`. It does not erase the reservation or retry
execution. Other outbox/storage errors still propagate.

The Knowledge batch fix checks current actor and ACL before dispatch. It does not redesign
claim leases or cancel an already-claimed operation. An independently authorized current
admin tier remains authority under the existing policy.

## Fixed continuation queue: release blockers, not an endless new backlog

### 1. Close retained Python authentication revocation gaps

The integrated admission fix is useful but does not close these confirmed source-level gaps:

- Password reset and disable→reenable lack durable token/session revision invalidation.
- Hard deletion becomes `get_user() is None`, indistinguishable from intentionally external
  identities; an old local JWT may reach that compatibility path.
- `get_user_permissions(require_current=True)` may fall back to stale `users.roles` or
  `users.permissions` when assignment-derived permissions are empty, including expiry.

Implement an explicit durable issuer/local-identity/session-revocation contract through the
canonical migration owner; do not simply reject all external identities or add a local-only
schema column. Prove old tokens fail after reset/delete/reenable and assignment expiry, while
legitimate independent service keys and external identities retain their agreed behavior.
See group `final-review-python-gateway/REVIEW.md` for exact paths and commands.

### 2. Review and compile the two isolated Rust candidates

- `final-review-native-gateway/candidate.diff`: matching successful native chat terminal
  required; failed/cancelled/unknown/EOF must not return completed HTTP200. The patch does
  not alter native control-plane ledger/accounting decisions and leaves `contexts: []`.
- `final-review-agent-runtime/candidate.patch`: durable SSE reads must not skip stored events
  when a later broadcast advances the cursor. Preserve the separate non-durable/MAX failure
  notification and verify its reconnect implications.

Check preimages before applying. Apply only in the receiving working copy, refresh the
appropriate source/overlay manifests, and compile/test before main integration. These are
source-reviewed proposals, not verified Rust implementations. Native citation projection
must independently enforce bounded authorized metadata and final source reauthorization;
never solve parity by returning raw Runtime metadata.

Use each component's pinned toolchain (the native and Runtime toolchains differ), one Cargo
job, no debug/incremental and external targets. Follow existing resource admission, not the
small net growth from an old build. Example native commands after admission:

```sh
export CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export CARGO_TARGET_DIR="$LOCAL_WORK/targets/native"
cargo check --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-gateway --all-targets
cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-gateway --lib
```

Runtime canonical flow is documented in the Runtime report and source-assembly owner:
`AI_PLATFORM_AGENT_RUNTIME_SOURCE=<clean-pinned-upstream> make agent-runtime-recovery-tests`,
then `make agent-runtime-build-local` with the same explicit source. These were not run here.

### 3. Restore official Worker dependencies and prove actual framework loops

Keep current 13 Worker modules and the frozen lock. The current environment could not resolve
`files.pythonhosted.org` in the bounded 2026-10-15 05:51 UTC DNS check. Earlier 12-module SDK
receipts do not certify current source. Follow `final-review-sdk-worker/HANDOFF.md` for frozen
install, wheel/installed identity verification, actual OpenAI/Anthropic SDK loop checks, then
real Runtime/Worker approval, cancellation, restart and provider cases. The 91 controlled
checks used existing Uvicorn0.52.1/Click8.4.2 fallback, not the lock's Uvicorn0.54.0.

### 4. Resolve native Local Node enablement and oversized-result parity

Native Local Node has the same library-level 8MiB read / 2MiB receipt mismatch, currently
without Python's new fallback. Production device pairing/composition is also intentionally
unavailable/fail-closed; no real device adapter is wired. Do not invent a daemon command or
claim host capability readiness. Implement the supported pairing/credential/device adapter
contract and native parity in an explicitly authorized environment. Real local file/process
and Office render→persist→authorized download must be tested with dedicated fixtures.

### 5. Run real integrated acceptance, then the existing journey matrix

First prove one current-source authenticated conversation on the intended native stack:
login → configured test provider → session → streamed answer → reload persisted history →
stop a second run → another turn. Then approve an actual tool once, reject with zero dispatch,
restart during pending approval, and verify one original operation/receipt without replay.

Next prove actual Knowledge import/index/retrieve → governed citation → exact historical
source using public `knowledge.get_source`, including current ACL/version revocation at each
boundary. Use real least-privilege PG roles, Qdrant and object storage. Exercise durable batch
revocation, cancellation/late claims and publication/recovery. Continue the existing J01–J30
acceptance matrix rather than inventing a new feature list. Record real run/session/operation
identities, effects, hashes, source/image identity and redacted evidence, not only health200.

Provider test credentials/subscriptions and local environment operation still require the
owner's authorized setup. Never print credentials or use production data for convenience.

## Useful verified foundation, and its limits

Before this review, integrated work includes SDK stream ownership and async image routes,
explicit skill and historical-source APIs, approval/image read recovery, scheduled action
reconciliation, governed deletion receipts, historical Knowledge preview, Python nonstream
terminal correctness, authoritative Knowledge capability identities and final-rechecked chat
citations. The composed SDK→Gateway→Knowledge→historical source suite passed 123 tests: 10
journeys through actual Python HTTP layers plus 113 existing tests. Runtime/storage/admission
were controlled adapters.

The latest full Web aggregate was 1,975 tests and seven delivery phases. It predates the later
approval and image receipt increments; those have separate focused evidence. No new full-Web
or browser result is implied. Runtime/Worker/native deployment and final enterprise acceptance
remain incomplete. Existing warnings and failed attempts are preserved in the evidence.

## Package boundary and delivery completion

The final compact handoff keeps the complete current project, current checkpoint evidence,
these six review bundles and isolated Rust proposals. Multi-version historical audit inventories,
old snapshots and diagnostic copies remain in the preserved version124 archive and Library
history; they are not needed to build the current project and are not automatically applied.
Consult the archive's exact selection/omission manifest, not a prior archive's file count.

Assistant completion for this request is: six reviews concluded, verified fixes integrated,
unverified candidates clearly isolated, necessary checks recorded, same Library archive updated
and restored/verified, and this Codex continuation delivered. It is **not** a claim that the
enterprise product is finished. After handoff, do not resume unlimited autonomous feature work
without a new user instruction.
