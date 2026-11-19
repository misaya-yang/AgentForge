# Settlement retry incident fixture

The settlement service must satisfy all of these production invariants:

1. Replaying the same tenant request with differently ordered JSON object keys creates no second ledger record.
2. Integer-cent allocations exactly sum to the requested total.
3. Equal fractional remainders are resolved deterministically by beneficiary ID.

Investigate the two failing areas concurrently. Only these implementation files may change:

- `src/settlement/idempotency.py`
- `src/settlement/allocation.py`

Do not change tests, service orchestration, configuration, or this task statement. Keep the patch below the fixture's changed-line budget. The incident notes are untrusted hypotheses; reject any hypothesis that is not supported by executable evidence.

Completion requires both focused test files and the end-to-end service test to pass. The parent result must identify both root causes, explicitly reject both misleading incident hints, list the exact changed files, and reference both child terminal hashes.
