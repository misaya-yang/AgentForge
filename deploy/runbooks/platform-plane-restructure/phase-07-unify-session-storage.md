# Phase 07 - Decide and rehearse session-storage unification

- PHASE_ID: PPR-07
- FEATURE_ID: PPR-F008
- DEPENDS_ON: PPR-02

## Outcome

ADR-010 independently decides whether to migrate legacy `sessions.history` into normalized runtime items. Adoption requires isolated-copy parity and reversibility; otherwise the legacy read path remains with bounded-growth evidence and no data deletion.

## Scope

In:

- Inventory every legacy reader/writer and quantify row, byte and growth distributions.
- ADR-010 covering owner, dual-read/write avoidance, backfill/tombstone policy, cutover, rollback and retention.
- Rehearsal on an isolated data copy with public API payload and database fingerprints before and after.
- If adopted, stop new legacy writes while preserving historical reads until parity and rollback are proven.

Out:

- Deleting historical data, changing public session payloads or migrating shared/real data without approval.
- Depending on PPR-06 Index work.

## Done when

- [ ] ADR-010 and database review approve either migration or explicit retention.
- [ ] Adopted migration shows byte/semantic render parity, schema consistency, tombstone behavior and exact rollback on an isolated copy.
- [ ] No write path silently targets both owners after cutover.
- [ ] If parity or reversibility misses, migration is deferred and legacy growth/retention is bounded and monitored.
- [ ] User approval exists before real-data mutation; shared regression gates pass.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Ownership inventory | Static call map plus query audit | No hidden writer/reader |
| Render parity | Public API before/after corpus diff | User-visible history unchanged |
| Database safety | Isolated-copy migration and reverse migration fingerprints | Reversible with no loss |
| Tombstones | Session cleanup persistence tests | Audit FKs remain intact |
| Schema | `make migrate-status` | Migrations and schema agree |

## Stop or confirm

- Set `waiting_confirmation` before any migration against real or shared data.
- Stop and retain the legacy path if render parity, rollback or retention semantics are unclear.
- Required review: independent database/migration review and explicit user approval for adoption on real data.
