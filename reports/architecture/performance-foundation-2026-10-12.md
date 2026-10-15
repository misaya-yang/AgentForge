# Performance foundations, 2026-10-12

This checkpoint implements bounded performance work after the explicit user priority. It is not an enterprise capacity or latency acceptance.

## Native streaming framing

The shared OpenAI-compatible, Dify Chat and official SDK line framer now keeps consume/scan cursors, eliminating repeated suffix movement and repeated partial-line scans. Real production transport tests: 9 passed. All adapter regressions: 29 passed. Exact-source isolated helper: 6 passed. Source hashes were unchanged during native tests.

Instrumented deterministic comparisons: 4,096 short lines in one 8,192-byte chunk moved 16,773,120 suffix bytes before versus zero after; a 65,536-byte unfinished line across 1,024 fragments examined 33,652,736 delimiter bytes before versus 65,536 after. These are algorithm-operation counts, not wall-clock speedups. UTF-8, CRLF, EOF, permits, pull behavior and cancellation were checked. A subsequent six-file change now bounds staged unfinished lines: SDK/OpenAI 4 MiB, Dify 1 MiB plus its six-byte SSE prefix. It releases response/permit/buffer on over-limit or invalid UTF-8, while retaining cancellation/resume behavior. Dify assembled-event accounting also includes inserted newlines. One upstream raw chunk, output strings and allocator overhead remain outside the staging bound. Actual production adapter tests now pass 42 cases including 13 additions; the complete integrations library gate passed 50 cases. These are correctness checks, not end-to-end RSS measurements.

## Knowledge parent lookup fanout

Parent folding reuses the resolved retrieval query concurrency in a fixed worker pool, instead of spawning a query task per unique parent. The tenant/dataset scoped reader, ordering, scores, deduplication and per-parent fallback remain intact. Exact old/new synthetic-I/O comparison on 2,000 children/1,000 parents returned identical results, statistics and scoped calls; peak active queries and tasks changed from 1,000 to the selected limit of three.

Ten isolated new regressions passed, followed by 57 real pytest unit tests and four subtests, including the existing parent-child unit suite. Actual production module bodies were imported unchanged; package namespace shims excluded unrelated application startup. Full retrieval pipeline, live PostgreSQL and latency/throughput acceptance were not executed. This bounds per-request fanout but neither removes N+1 queries nor establishes global pool admission.

## Measurement harness

The new enterprise performance contract and bounded loopback GET harness passed 26 offline regressions. The harness separates successful latency, rejection/errors, nearest-rank percentile populations, closed-loop throughput, client queueing and exact Gateway PID CPU/RSS/FD/socket observations. Model, SSE, server queue, KB and pool metrics are explicitly unavailable for these JSON profiles. No live load baseline or numerical enterprise SLO is claimed.

## Recovery and evidence

The execution workspace was replaced during work; cause is unproven. The just-saved version49 archive was recovered and all 5,209 manifest file hashes plus ZIP SHA/CRC verified. Toolchains and locked dependencies were restored from official sources. Historical fixtures, credentials and build artifacts were deliberately absent from that source backup; previous real-database results remain historical, not current runnable fixtures.

External receipts: native-restored/stream-framing-tests.json, native-restored/adapter-regressions.json, gateway-hot-path-next/root-result.json, knowledge-performance-next/root-real-pytest.log, and performance-baseline-next/root-tests-correct-cwd.log under platform-verification. The Runtime stable deferred-backend host executable build passed with exact source/lock identity, exit 0 in 1,677.143 seconds and sampled peak RSS 3,178,284 KiB. ELF SHA256: 001d183a927239f92bfbe6cc0a6eaa024867e2348b278ed40758e6a951007107. Runtime startup, HTTP, provider and release acceptance remain unverified. A restored native Gateway executable was then rebuilt for the bounded-line source: exit0 in117.293s, SHA256 d883279ee8a2da2e4097a83a68887efb1dc7b7768a304c2747b9775220d8b92b. ELF and source identity verified; process/HTTP acceptance not claimed.

Fresh private Unix-socket PostgreSQL startup failed with operation-not-permitted, also through the supported reviewed execution route. All owned processes stopped and synthetic directories were removed. No alternative transport/security workaround was attempted. Canonical bootstrap, Redis and live HTTP baseline were never reached. Independent source and offline/native tests continue.

## Actual local native framing timings

Three fresh processes, three warmups and twelve alternating measured pairs per process completed with identical full-output checks. The exact historical framing branch and current bounded helper were compiled; async/HTTP/reqwest owners were excluded. Each timing includes fresh allocation/drop and common output checksumming. Nine native helper tests and eight separate current cap cases passed.

| Synthetic case | Baseline median ms/replay | Current median ms/replay | Median paired baseline/current time ratio |
| --- | ---: | ---: | ---: |
| coalesced_tiny_4096 | 0.302681 | 0.145399 | 2.054 |
| fragmented_64k_eof | 17.665696 | 0.126902 | 140.124 |
| mixed_json_coalesced | 0.941826 | 0.438289 | 2.182 |
| mixed_json_fragmented | 0.528819 | 0.437300 | 1.207 |

These are local batch-average framing measurements, not Gateway throughput, per-request P95/P99 or provider speedups. Short batches and uncontrolled shared-host variation limit precision; all cases, including the smaller mixed-fragmented improvement, are retained. Exact fixtures, source hashes, raw trials and summary are under platform-verification/framing-performance-next/results/run-1.

## Web request and loading performance

Task detail polling now stops for terminal/unknown/exhausted-error states; result reads enable only for the matching completed task. Task GETs propagate AbortSignal and background refresh keeps existing details mounted. Pinned Node26.11.1/pnpm12.11.2 full checks passed with300 tests before the next optimization.

Image compression is now dynamically imported only for an image exceeding the existing compression threshold, inside the existing original-file fallback. Same-path repeated production builds were byte-identical. Assistant dependency closure saves19,600 gzip bytes; restored/GFM saves19,599 bytes, with GFM219,248→199,649 bytes. Public entry remains328,641 bytes and still has little budget headroom. Exact510-file candidate passed type-check, lint,310 tests, build/budget and i18n; root integrated its exact hashes and reran10 focused tests. Browser and live upload acceptance remain unverified. Evidence: platform-verification/web-bundle-next/receipts/aggregate.json.
