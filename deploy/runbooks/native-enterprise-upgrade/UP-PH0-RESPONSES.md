# Ph0 Responses accepted-turn replay repair

State and complete contract: [loop-state.json](loop-state.json), support package
`UP-PH0-RESPONSES`. This closes an observed compatibility defect while preserving
the full U-GW-05 inbound API scope and original acceptance requirements.

## Discovery checkpoint — 2026-10-17

Independent review of U-PE-02 found that a required historical Responses contract
still applies to the native route: observation failure after `start_turn` must not
cause a duplicate execution when the caller retries the same idempotency key/body.
Current native source aborts the claim on stream-open/read failure and EOF; its
generic `finish` also discards 5xx. This is source-confirmed, not a reproduced live
incident. The documentation requirement has been restored and the defect remains open.

Next: freeze response error shapes and existing pre-accept retry behavior; exercise
the real production route with a counted Runtime start and failing observation stream.
Repair cache retention for accepted failures without asserting a fabricated terminal.
Real Redis and live paired-service evidence remain separately required.

## Implementation checkpoint

The three post-accept stream-open/read/EOF paths now retain 503/502 plus run/session
identity through a private cache store. Generic pre-accept finish/abort behavior is
unchanged; a cache-write failure retains the original claim and reports storage
unavailability. Five cache-helper regressions are compiling with one Rust job, no
debug/incremental output and an external target. They are not route/auth/store/live
acceptance; counted production POST and real Redis/paired-service cases remain required.

Clean Gateway compilation also required a missing product PNG. The native include path
now contains the exact historical Git blob: SHA-256
`da1d9304549d9c1e153c37472748ec949a87f3af1b395412e17a451c9b41a6ff`,
identical in both historical watermark locations. No retired Python code was restored.

## Measured helper checkpoint

Gateway library/test compilation completed in 4m13s with one Cargo job. All five
selected cache-helper tests passed; 146 unrelated library tests were filtered out.
Independent source review found no blocker/high in the bounded fix.

Actual authenticated POST start-count, real Redis and paired-service retry scenarios
remain required and unrun. This package is externally blocked on the pending local
environment choice; it is not accepted. Continue independent U-PE-03 work.

[Source-bound receipt and limits](../../../reports/platform-upgrade-20261017/UP-PH0-RESPONSES.json).
