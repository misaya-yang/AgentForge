# Phase 09 - Release and roll back the adopted topology

- PHASE_ID: PPR-09
- FEATURE_ID: PPR-F010
- DEPENDS_ON: PPR-03, PPR-04, PPR-05, PPR-06, PPR-07, PPR-08

## Outcome

The release matrix is generated from what prior phases actually adopted—not the aspirational diagram—and a reviewed serial gate plus digest-pinned rollback preserves product behavior, sessions and the execution ledger.

## Scope

In:

- Classify every prior phase as adopted, measured-not-adopted, deferred or waived with evidence.
- Extend the release gate only for adopted deployment units and data owners across Edge, Control, Data, Index and Governance; include storage substrate health and fingerprints without calling it an application plane.
- Current-to-frozen-to-current rehearsal using the exact shipped images, config fingerprints and schema state.
- Product, failure, security, resource and rollback receipts.

Out:

- New functionality, late topology changes or using stale image receipts.

## Done when

- [ ] Every selected unit and cross-plane contract has a serial gate; non-adopted experiments are absent from the default path.
- [ ] Shipped image, config and schema digests match the rollback bundle.
- [ ] Approved current-to-frozen-to-current rehearsal preserves session, audit and execution-ledger fingerprints exactly.
- [ ] Live product checks have at least the baseline pass count, zero failures and no skip beyond the named allowlist.
- [ ] Independent release/security review approves the evidence and the closeout distinguishes all four outcome classes.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Generated matrix | Compare adopted decisions to release targets | No aspirational/stale service is gated |
| Serial contract | Extended canonical release target | Ordered health and contract coverage |
| Evidence freshness | Image/config/schema digests versus bundle | What was tested is what ships |
| Rollback | Approved round trip with before/after fingerprints | Operational reversibility |
| Product | Full live suite and failure matrix | User-visible behavior survives |

## Stop or confirm

- Set `waiting_confirmation` before deploy, image swap, rollback rehearsal or shared-state mutation.
- Stop on any fingerprint mismatch; do not rerun unchanged until it passes.
- Required review: independent release and security approval.
