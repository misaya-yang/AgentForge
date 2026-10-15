# Reranker bake-off evidence boundary

These reports are development evidence only. The fixed candidate set contains
12 machine-generated, human-review-pending cases. A report whose local
`promotable` field is true demonstrates that the adapter beat the identity
baseline on this small fixture; it does **not** satisfy the T0 release gate,
authorize a serving-default change, or replace the required 200–400 reviewed
golden cases and frozen real-corpus baseline.

`2026-08-29-run3` is the acceptance rerun against the configured live
DashScope reranker. Earlier runs are retained only as diagnostic history.
