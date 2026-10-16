# Native Redis Streams durability upgrade

User-authorized architecture upgrade, 2026-10-15. This supersedes the initial literal Python connector port's non-atomic XADD/XACK behavior. Implementation: `platform-gateway-foundation/src/message_queue_transport.rs` and child Rust/Lua files. Production GenericRestAdapterFactory already constructs this connector lazily. No constructor network access.

## Connected behavior

- Publish uses atomic bounded admission. Retry/dead-letter transfer uses a single Redis Lua operation that verifies the pending entry's consumer and delivery generation, appends all preserved business fields plus retry metadata, then acknowledges the old entry. The root original ID survives subsequent retries.
- Repeating a transfer after an ambiguous network outcome sees the absent pending entry and does not append another retry. Reclaimed ownership/generation fences stale handlers. This is **at-least-once** delivery; business effects still need durable idempotency keyed by original stream/ID (or an application business key). No exactly-once or transactional business-effect guarantee.
- Consumption returns one message, matching the API's single-message shape; COUNT no longer silently strands additional fetched messages. Retry stream retains priority. Before fresh reads, a bounded unfiltered XPENDING scan (at most32 entries, advancing cursor) with explicit idle-age checks and XCLAIM recover abandoned pending deliveries. Active local IDs are excluded. Reclaim idle defaults to 60 seconds and must exceed expected handler duration unless handlers tolerate concurrent retry effects.
- Queue admission refuses full streams rather than trimming pending/unconsumed messages. Safe pruning respects every existing consumer group's delivered watermark and oldest pending message. A stalled group or full DLQ therefore backpressures producers/failed deliveries, preserving PEL ownership for retry. Creating a new consumer group later cannot reconstruct already-pruned history; this is a work queue, not indefinite event storage.
- Limits: max_queue_length=10000 (1..1000000), max_inflight=128 (1..4096), max_payload_bytes=1048576 (1..16777216), reclaim_idle_ms=60000 (1..86400000), drain_timeout_ms=30000 (1..300000). Error text is bounded to 16KiB. Local polling is interruptible; requested blocking is capped at 30 seconds. Redis connection/query operations each have a five-second bound.
- Close marks draining before waiting; new publish/consume calls fail closed, existing ack/fail calls can settle. Drain expiry never acknowledges unfinished work; Redis PEL remains recoverable. Production shutdown is connected through RestTransport/ProtocolAdapter/GenericRest/Bulkhead to registry.shutdown(), before Gateway DB/Redis resource closure; the foundation lifecycle owner implemented that shared wiring.
- Unknown local delivery ack/fail is rejected, replacing the old synthetic empty-payload retry. The legacy stream/message-ID path remains same-instance. Consume now adds an optional opaque delivery_receipt accepted by ack/fail on any authorized replica; stream/message/payload/attempts/original_stream stay intact and original_id is retained. The receipt binds version, service, group, stream, message ID, per-delivery random owner, and delivery generation. Redis PEL ownership validates its unpredictable UUID bearer capability; it is not a signed JWT and must not be logged or disclosed. Existing service authorization still applies. A receipt cannot authorize another service/group/message or a reclaimed lease. Remote fail reloads business fields from Redis with the same fence rather than synthesizing missing payload. Local permits are reaped in bounded batches of32 after remote settlement; empty per-delivery Redis consumers are cleaned up. Repeated remote fail after an ambiguous committed result returns already_settled=true rather than inventing a destination receipt.
- Generic REST operation bodies now reach consume/ack/fail arguments; publish continues treating its body as the payload.

## Redis deployment requirements

Redis >=6.2 is required for exclusive pending cursor ranges and MINID trimming. The unfiltered32-entry PEL scan checks idle age in Lua before XCLAIM; it deliberately avoids XPENDING IDLE, whose filter can scan beyond COUNT. The configured production stack must authorize EVAL, XGROUP, XINFO, XLEN, XTRIM, XDEL, XPENDING, XCLAIM, XREADGROUP, XADD, XACK. No script loading or external code download occurs.

Atomic transfer across source/retry/DLQ requires one Redis instance or co-located hash slots. For Redis Cluster use a shared hash tag, e.g. `{orders}:normal`, `queue:retry:{orders}:normal`, `queue:dlq:{orders}:normal`; cross-slot calls fail closed. The existing redis::Client connector is not a cluster discovery client. Redis persistence/replication configuration still determines survival under server/power failure. Lua is atomic with respect to other commands, not rollback: fallible destination type/capacity checks run before XADD, and the prevalidated source XACK immediately follows it. Administrative group destruction/reset and externally deleting pending stream entries are outside this connector's safety contract.

## Offline evidence

Fifteen injected-backend contracts cover business payload/factory, retry priority, ambiguous-response retry idempotency, full destination preserving PEL, stale-delivery fencing, inflight capacity release, graceful drain, timeout preserving PEL, interruptible polling, invalid configuration, unknown settlement, and actual Generic REST ack/shutdown dispatch, cross-replica settlement/capacity release, durable failure reload, and receipt tamper/binding rejection. An additional actual loopback RESP test exercises the real redis-rs connection, EVAL argument framing, reclaim response decoding, and fenced transfer command shape. These are not live Redis/Lua execution or crash-failover proofs. Root owns serialized Cargo execution; no Redis/DB/provider was run for this change.

## Official pinned dependency compatibility, checked 2026-10-15

The current Cargo.lock resolves stable releases, not prereleases:

| Component | Pinned | Published package MSRV |
| --- | --- | --- |
| Rust native toolchain | 1.99.0 | n/a |
| axum | 0.8.9 | 1.80 |
| tokio | 1.53.2 | 1.71 |
| tower | 0.5.3 | 1.64.0 |
| tonic | 0.14.6 | 1.88 |
| redis | 1.7.1 | 1.88 |

Exact registry package manifests were inspected locally against the lock; official documentation release pages were checked: [Rust 1.99 announcement](https://blog.rust-lang.org/2026/10/01/Rust-1.99.0/), [axum 0.8.9](https://docs.rs/crate/axum/0.8.9), [Tokio 1.53.2](https://docs.rs/crate/tokio/1.53.2), [Tower 0.5.3](https://docs.rs/crate/tower/0.5.3), [Tonic 0.14.6](https://docs.rs/crate/tonic/0.14.6), [redis 0.32.7](https://docs.rs/crate/redis/0.32.7). The crates.io API page was unavailable through the browser, so no unverified yanked/security status is asserted. axum declares Tokio ^1.44 and Tower ^0.5.2; these locked versions satisfy that published compatibility. All listed MSRVs are below native Rust1.99. The user-authorized modern-stack upgrade selects redis1.7.1, published2026-09-25, MSRV1.88. Parent coordinated all five native consumers through workspace redis=1.7.1; overlay pins are unchanged. Narrow source adaptations are complete: borrowed value conversion helpers, server/I/O error variants, preservation of the registry package’s required ConnectionLike pipeline method, and explicit MQ connection configuration. Parent lock/build verification is pending. See [official1.7.1 package](https://docs.rs/crate/redis/1.7.1), [tagged manifest](https://raw.githubusercontent.com/redis-rs/redis-rs/redis-1.7.1/redis/Cargo.toml), and [migration changelog](https://raw.githubusercontent.com/redis-rs/redis-rs/redis-1.7.1/redis/CHANGELOG.md). The original0.32.7 compatibility inspection above is historical evidence.

Redis primary references: [Lua atomic execution](https://redis.io/docs/latest/interact/programmability/eval-intro/), [XPENDING idle ranges](https://redis.io/docs/latest/commands/xpending/), [XCLAIM delivery counters](https://redis.io/docs/latest/commands/xclaim/). Final compile/test compatibility remains the parent-controlled Cargo gate, not merely this version comparison.

## Registry routing and verification update

ServiceRegistry.adapter_cache/get_adapter is process-local, while GenericRestAdapter.invoke carries ordinary service requests; no enterprise replica-affinity guarantee exists. The optional receipt closes that actual routing gap without changing legacy call signatures. Receipt authority relies on a fresh UUIDv4 PEL consumer per delivery, so knowing one receipt cannot derive another delivery owner. Group/stream checks remain configured-service scoped. New calls remain bounded by the existing service admission layer; no new signing key or account is introduced.

Parent run foundation-native37 compiled all14 MQ contracts:13 passed, including all3 cross-replica cases; the drain-interruption case exposed a50ms polling sleep exceeding the configured25ms drain budget. Fixed with a registered Tokio Notify waiter and close-time wakeup, preserving the deadline and preventing lost notifications. Rerun pending. Deployment script contracts43/43 passed in parent run36 after anchored top-level Compose section parsing.

redis1.7.1 MQ connection bounds are explicit:5s connection/response deadlines,128 concurrent driver requests,32 buffered pipeline commands,64KiB write-buffer backpressure threshold. Existing usage-event-bus XREADGROUP blocks for1000ms, below the bounded response deadline. No native Redis PubSub or unbounded blocking command was found in this workspace scan, so no infinite response timeout was introduced.

The registry package compiler required ConnectionLike::req_packed_commands despite the browsed latest documentation omitting it; the fixture method was restored from its exact compiled signature. Parent check40 reached final test compilation with production reqwest0.13/redis1.7 adaptations compiled; final rerun pending. A15th MQ regression covers delayed stale settlement versus a newly acquired local lease.

## Final locked transport stack (2026-10-15 13:24 UTC)

The updated native Cargo.lock was inspected directly: axum0.8.9, tokio1.53.2, tower0.5.3, tonic0.14.6, redis1.7.1, reqwest0.13.5, and direct tokio-tungstenite0.30.0. Axum retains its supported transitive tokio-tungstenite0.29.0; this is not a hidden direct dependency downgrade. Official exact release pages were checked for [reqwest0.13.5](https://docs.rs/crate/reqwest/0.13.5) and [tokio-tungstenite0.30.0](https://docs.rs/crate/tokio-tungstenite/0.30.0). The lock is resolved; parent full check41 and post-upgrade tests are still pending. No further feature/version expansion is planned.

Parent workspace-check41 completed successfully across all targets with the final locked transport stack (log ends `Finished dev profile` after3m32s, no compiler errors). This establishes build compatibility, not live Redis behavior. Post-upgrade MQ/loopback test execution is still awaited.

Parent foundation-native43 execution passed all15 MQ integration contracts on the final Redis1.7.1/reqwest0.13/tokio-tungstenite0.30 stack, including the drain notification fix, all cross-replica receipt cases, and delayed stale-settlement cache race. The separate library loopback RESP test compiled in check41 but was not selected by run43; its execution remains requested. No live Redis/Lua server acceptance or failover test was run.
