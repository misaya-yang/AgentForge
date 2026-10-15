# Phase 06 - Scale Index capacity without quality drift

- PHASE_ID: PPR-06
- FEATURE_ID: PPR-F007
- DEPENDS_ON: PPR-02

## Outcome

Knowledge ingestion scales only after profiling identifies its binding resource; worker concurrency preserves claim safety and retrieval isolation, while native kernels ship only with byte-identical boundaries and material benefit.

## Scope

In:

- Profile CPU, IO, queue wait, embedding/provider wait and database contention on the named PPR-00 workload.
- Scale `knowledge-worker` beyond one only if worker concurrency addresses the measured ceiling.
- Conditional Rust/PyO3 kernels for chunk, tokenize, hash or dedupe when CPU is the binding constraint.
- Claim/lease fuzz, ingestion throughput and concurrent retrieval interference evidence.

Out:

- Retrieval profiles, recall width, RRF weights, rerank policy or other quality tuning.
- Reimplementing BM25/sparse retrieval or creating another ingestion service.

## Done when

- [ ] Profiling names the binding resource and precommits material throughput/CPU gates before implementation.
- [ ] If worker scaling is adopted, N greater than one never double-claims and a 200-page ingest raises retrieval p99 by at most 10%.
- [ ] If a native kernel is adopted, corpus boundaries are byte-identical and measured throughput or CPU improvement clears the precommitted materiality gate.
- [ ] If the ceiling is external IO/provider/database or benefit is immaterial, the corresponding change is measured-not-adopted.
- [ ] Quality fixtures, rollback, independent review and shared regression gates pass.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Profile | CPU/IO/queue/provider/database spans | Correct owner and bottleneck |
| Claim safety | N-worker concurrent claim and crash/retry fuzz | No duplicate processing |
| Interference | 200-page ingest during retrieval load | Retrieval p99 delta at most 10% |
| Kernel parity | Corpus-wide Python/Rust boundary and digest diff | No forced re-embed |
| Quality | `make rag-eval-regression-gate` | Retrieval behavior unchanged |

## Stop or confirm

- Stop rather than change claim ownership or chunk boundaries to make a benchmark pass.
- Ask before Docker topology changes or any migration/re-embedding of real datasets.
- Required review: independent data, claim/lease and retrieval review; native code adds security review.
