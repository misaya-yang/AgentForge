# Runtime history compatibility and rollback (U-RT-05)

Implementation date: 2026-10-17. Runtime execution/acceptance has not run for this
change. Real N-1 raw rollout evidence is missing and the new release gate remains
blocked until authenticated external evidence is supplied.

## Persistence seam

`PostgresThreadStore::append_items_in_transaction` adds the private top-level
`ai_platform_history` field to each newly persisted native rollout JSON payload.
It contains `format=ai-platform/rollout-writer/v1`, `upstream_revision` and
`schema_sha256`. Values are compiled from `history_writer_identity.json`, rather
than taken from tenant requests or mutable runtime environment. The ordinary
payload hash covers the stamp. There is no SQL/column/grant migration, public
protocol change, second kernel or new execution path.

The original native `type`/`payload`/upstream metadata remain in place, so scoped
SQL readers for root attribution and invocation output retain their JSON paths.
Normal `load_history` and cold `recover_claim` both use `history_codec::decode_rollout`.
It validates a present private envelope, strips only that field and delegates to
the actual pinned upstream `RolloutItem` deserializer. Existing untagged data is
still read natively; malformed/future private metadata is never silently treated
as legacy. Unknown native variants remain errors. No existing row is rewritten,
no original payload hash is repaired, and historical writer identity is never
inferred from a later process. Event keys, owner/fence, scope and immutable append
conflict semantics are unchanged.

Upstream/schema changes must refresh the compiled identity alongside the owning
source lock. The offline gate compares the compiled identity, manifest candidate
and actual source lock; stale constants block promotion. A source-only overlay
refresh does not invent a new upstream/schema version. If frozen compatibility is
unproved, a valid metadata stamp is provenance, not permission to execute or a
claim that an older decoder supports every new variant.

## Blocking gate

The crate's auto-discovered `ai-platform-history-gate` binary compiles the exact
production codec via a source module include. It uses existing dependencies; no
Cargo manifest or lock changes are needed. Primary must build it in the composed
controlled Rust 1.95.0 source workflow, and run its unit tests explicitly (the
current Docker library-only test target does not automatically run bin tests).
No implementation agent ran a build or added a separate compiler workflow.

`make agent-runtime-release-gate` now calls `make agent-runtime-history-gate` after
source admission. The gate runs only an explicitly supplied prebuilt decoder and
cannot silently build, skip, use a projection as raw history or substitute a
fixture when N-1 evidence is missing:

```sh
AI_PLATFORM_HISTORY_DECODER=/absolute/composed-target/ai-platform-history-gate \
AI_PLATFORM_HISTORY_CORPUS=/external/private-corpus/corpus.json \
AI_PLATFORM_HISTORY_REPORT_DIR=/external/cache/history-evidence \
  make agent-runtime-history-gate
```

The checked-in manifest is intentionally empty/blocked, with provenance and exact
input contract documented in `tests/fixtures/agent-runtime/history/README.md`.
The gate enforces immutable file/source/capture hashes, real previous identity,
eight required native scenarios, actual upstream decode and semantic roundtrip.
It writes only redacted counts, hashes and compiled identity. The wrapper
invalidates old PASS before a missing binary/input can fail. Compilation, unit
coverage, corpus decode and live turn replay remain separate evidence layers.

## Rollback drill

The existing new→old→new image rehearsal now first runs the same gate in rollback
direction. Supply `AI_PLATFORM_HISTORY_ROLLBACK_DECODER` compiled against the
**actual frozen N-1 upstream/dependency closure** and its source-bound writer
identity. Do not change the current decoder's constants to impersonate old code.
The rollback manifest cases contain real N-written native records and authenticated
capture/source receipts. A missing frozen decoder, corpus or scenario blocks that
in-place rehearsal before any environment or running service mutation.

Until backward decoding is proved, rollback is **stop writes, then restore data**:

1. Pin the entire admitted old/current release units, source/schema fingerprints,
   scoped roles, images and backup identities. Freeze new Assistant turn starts
   and stop the Runtime/Worker writers through the existing owner-checked local
   control workflow. Wait for all known work/unknown effects to be reconciled;
   never retry unknown writes or authorize data rollback while writers run.
2. Use the owning native database authority/runbook to create a consistent,
   encrypted external backup of the scoped platform database and required
   artifact/object-store data. Retain checksums and epoch verification. No new
   direct SQL privilege shortcut or fingerprint repair is introduced here.
3. Restore that backup into the existing authority-approved isolated drill target
   with the frozen epoch/grants. Run frozen verification and the real frozen
   decoder on the restored raw history. Compare ordered item IDs, payload hashes,
   sequence/counts and exact original tenant/user/session/thread attribution
   against the captured pre-upgrade data. Never restore over the live database
   merely to test this step, create duplicate dev infrastructure or delete volumes.
4. On the restored drill target, run old-image history/resume, tool receipt,
   approval/cancel and no-unknown-write-replay checks. Record actual image/source,
   backup/data hashes and commands. Only then use the existing release-unit
   rollback procedure for an operator-authorized real rollback.
5. Return to the admitted current release/data pair through the same controlled
   workflow, verify health and original journeys, and only then reopen writes.

This document is the required restore drill protocol, not a receipt that a backup,
restore or rollback executed. An old image alone is not a safe rollback after new
history has been written. Native unknown effects and external side effects are
not undone by restoring a database. Required corpus, frozen decoder, actual
restore and original product journeys remain **NOT_RUN / missing evidence**.

## Implemented tests and unrun checks

Production codec tests reuse an exact, provenance-pinned existing upstream
legacy response record. They verify legacy acceptance, private-stamp roundtrip
and upstream-wire transparency; negative mutations reject bad payloads, unknown
native variants and malformed/unknown writer envelopes. These mutations are unit
inputs, not fabricated historical samples. Gate tests reject path/hash tampering
and scenario labels with no supporting native records. Neither type of test is
accepted N-1 evidence. Primary must run library/bin tests, actual corpus gates,
existing recovery/store contracts and complete live upgrade/rollback journeys.
