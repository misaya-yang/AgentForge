# 01 Baseline and findings

历史评审资料：描述 2026-10-16 的源码快照和技术调研。当前执行入口是 [主计划](README.md)，不按本文件的旧缺口重新开工。
status: proposed · domain_id: platform-upgrade-2026-10 · last_verified: 2026-10-16 (HEAD `22d9ce0`)

## 1. Method and limits

Reviewed: `AGENTS.md`, `CLAUDE.md`, the harness docs, the docs index, ADR-004…012, the vNext PRD
(01, 07), the 2026-10-12 cross-service evolution design, the full-Rust migration status, the
active `native-enterprise-upgrade` ledger and the 2026-08-17 microservice performance review.
Code: every native crate manifest; the composition roots, middleware, routing, admission,
accounting, model-plane, retrieval, ingestion, Runtime HTTP/SSE, sandbox and auth modules;
the primary Compose file and overlays; the Dockerfiles, Helm chart, CI workflows, web package and
key web modules; the SDK tree; the database baseline and migrations.

Not done: no builds, tests, gates, live calls, profiling or line-by-line audit. Findings marked
SRC are source facts at HEAD `22d9ce0`, not observed incidents. Prior findings from vNext
[01](../agent-platform-vnext-2026-09/01-current-state-and-gaps.md) cite deleted Python files; §4
re-checks a sample against the native code.

### Size snapshot (navigation only)

| Area | Tracked lines |
| --- | --- |
| Native Rust workspace (`rust/platform-native`) | 363,075 (19 crates; Gateway ≈ 250k across three crates) |
| Runtime overlay, tracked files (`rust/agent-runtime-overlay`) | 91,311 Rust lines; [manifest](../../../rust/agent-runtime-overlay/manifest.json) lists 148 overlay files (111 platform-owned, 37 modified upstream) |
| Web console (`web/src`) | 124,781 TS/TSX |
| Independent CLI (`sdk/cli/src`) | 7,053 TS/TSX |
| Database SQL (`database/`) | 28,960 (163 tables across `gateway` 82, `assistant` 48, `knowledge` 33) |
| Public OpenAPI (`sdk/openapi.json`) | 3.1.0, 349 paths, 400 schemas |

## 2. Version baseline: current → target

"Keep" means current as of 2026-10-16; still track patch releases through Renovate.

| Component | Current (SRC) | Target (TARGET, WEB-checked) | Notes |
| --- | --- | --- | --- |
| Rust (native workspace) | 1.99.0, edition 2024 | Keep; follow the six-weekly stable train | 1.99.0 shipped 2026-10-01 |
| Rust (Runtime/Worker) | 1.95.0 | Follow the upstream Codex pin; stay ≤ 2 minors behind native | Two toolchains are acceptable while the overlay is upstream-pinned |
| Tokio / Tower / Axum | 1.53.2 / 0.5.3 / 0.8.9 | Keep (Axum 0.9 unreleased) | Re-evaluate when Axum 0.9 ships |
| reqwest / tonic / prost | 0.13.5 / 0.14.6 / 0.14.4 | Keep latest patch | — |
| sqlx | 0.8 | **0.9.x** | Released 2026-05-06; project moved to `transact-rs`; per-crate `sqlx.toml` |
| redis-rs | 1.7.1 | Keep | Works with Valkey |
| OpenTelemetry Rust | 0.31, trace only | **0.32** with traces, metrics and logs | Crates are not 1.0 yet; pin the minor |
| jsonwebtoken | 9.3, HS256 | **11.x**, explicit crypto provider, EdDSA/ES256 | Pluggable providers (aws-lc-rs / RustCrypto) |
| Password hashing | bcrypt 0.17, cost 12 | **Argon2id** (`argon2`), rehash on next login | Keep bcrypt verification for migration |
| Credential encryption | `fernet` 0.2.2 (Python-compatible) | AES-256-GCM-SIV or XChaCha20-Poly1305 envelope encryption, KMS-wrapped keys with key IDs | Decrypt-only Fernet path during migration |
| YAML | `serde_yaml` 0.9 (archived upstream) in 7 crates, plus `unsafe-libyaml` | A maintained fork (e.g. `serde_yaml_ng` or `serde_norway`) after a `cargo-vet` review | Check RustSec for any fork before adoption |
| Python AST in eval | `rustpython-parser` 0.4 | `tree-sitter-python` or remove | Upstream parser development moved elsewhere |
| `rusqlite`, `zip`, `jsonschema`, `scraper`, `quick-xml`, `image` | Older pins | Current releases via Renovate | Archive and decoder crates carry security fixes |
| Candle / tokenizers | 0.11 / 0.22 | Keep; evaluate `ort` only if reranker CPU latency is the bottleneck | — |
| PostgreSQL | 16 (Compose, CI); 15 in the monitoring overlay | **18.x now**; 19 after GA (scheduled 2026-11-19) plus its first minor | `uuidv7()`, async I/O, faster major upgrades |
| Redis | 7-alpine | **Valkey 9.x** (BSD-3) or Redis 8 under AGPLv3 | Licence choice recorded in P-ADR-11 |
| Qdrant | server 1.18.2, client 1.19.0 | **1.19.2** server | Code already uses the Query API (`query_points`) |
| Object store (dev) | RustFS 1.0.0 | Keep for dev; any S3-compatible service in production | — |
| Tracing backends | Tempo 2.5.0; Jaeger 1.51, OTel Collector 0.88, Prometheus 2.47, Grafana 10.2, Alertmanager 0.26 in the monitoring overlay | One current OTel Collector, Tempo, Prometheus 3 (or Mimir), Loki, current Grafana; drop Jaeger v1 | [08](08-platform-engineering.md) |
| Container base | `debian:bookworm-slim` with `curl` and `poppler-utils` in every image; the Runtime's final stage is `rust:1.95.0-bookworm` | Per-binary images on `debian:trixie-slim` or distroless `cc`; poppler only in parser images | Debian 12 moved to LTS on 2026-07-12 |
| Python (Capability Worker image) | `python:3.11-slim-bookworm` | 3.14 on Debian 13 with hash-locked wheels; 3.15 after its first patch release | 3.11 is security-only until 2027-10 |
| Desktop shell | egui/eframe 0.33 | Tauri 2 around the React console (decided by P-ADR-8) | [06](06-clients-and-edge.md) |
| Web server | `nginx:1.27-alpine`, runs as root on port 80 | `nginx-unprivileged` on the 1.30 stable line, port 8080 | — |
| Node / pnpm | 26.11.1 / 12.11.2 | Keep (Node 26 enters LTS late October 2026) | `@types/node` is still ^22 |
| TypeScript | ~6.0.3 | **7.0** for type-checking | Keep the TS 6 API package for typed lint rules until they support 7 |
| React / Vite | 19.2.7 / ^8.0.16 | Keep; enable React Compiler 1.0 (`reactCompilerPreset` with `@rolldown/plugin-babel`, since plugin-react v6 dropped Babel); move `manualChunks` to `codeSplitting` | [05](05-web-console.md) |
| UI libraries | antd 6, Radix, Tailwind 4, framer-motion, two date and two markdown libraries | One component system; `motion`; one date library; remove unused packages | [05](05-web-console.md) |
| OpenAPI | 3.1.0, custom source generator | **3.2.0** (`itemSchema` for SSE) | Generated SDKs |
| MCP | Client pins `2025-11-25` and rejects any other negotiated revision | Negotiate a supported set that includes **2026-07-28** (stateless) | Dual-revision window (F-32) |
| A2A | Not present | **v1.0** adapter (Ph3) | — |
| Codex upstream | `rust-v0.162.1` source lock; Compose defaults still name `94cbbdda` images | Build the current lock (Ph0), then monthly sync windows | About 16 upstream releases in the 30 days before this review; [03](03-agent-execution.md) |
| GitHub Actions | Tag-pinned (`@v4`) | SHA-pinned, Renovate-managed | — |

## 3. Findings register

Severity: **P0** release/safety blocker · **P1** correctness, security or major capability gap ·
**P2** structural or operational debt · **P3** hygiene. "Fix" points to a work package.

| ID | Sev | Area | Finding (SRC) | Evidence | Fix |
| --- | --- | --- | --- | --- | --- |
| F-01 | P0 | Deploy | The `frontend` service defaults to `${KNOWLEDGE_IMAGE:-ai-platform/knowledge:rust-local}`. The build overlay builds the web image into that tag, and `knowledge-worker` uses the same tag. Pull mode runs the Knowledge image as the frontend; build mode lets the web and Knowledge builds overwrite each other's tag. | [docker-compose.yml:399](../../../docker-compose.yml), [:564](../../../docker-compose.yml); [build overlay:21–33](../../../deploy/compose/docker-compose.build.yml) | U-PE-01 |
| F-02 | P1 | Deploy | The Knowledge API defaults to `ghcr.io/…/ai-gateway-knowledge-service:2.0.0` while its worker defaults to the local Rust image. API and worker of one code unit can run different versions, and the published tag may predate the Rust binary path its entrypoint names. Runtime and Worker default to `local-94cbbddafc17-…` images built from the previous upstream revision, while the source lock targets `092d3acd`; the Runtime compatibility notes say the older binary cannot decode newer history records (active blocker `assistant-current-binary`). | [docker-compose.yml:434](../../../docker-compose.yml) vs [:564](../../../docker-compose.yml); [:600, :653](../../../docker-compose.yml) | U-PE-01 |
| F-03 | P1 | Billing | `model_pricing` is unique on `model` alone. `update_pricing` upserts `ON CONFLICT (model)` (overwriting `provider`) and returns JSON error objects instead of a typed error. vNext D19 survived the port. | [model_pricing.rs:306–330, 429](../../../rust/platform-native/crates/platform-gateway/src/migration_host/model_pricing.rs); [init.sql:5941](../../../database/baselines/2026_08_post_kb_v1/init.sql) | U-GW-01 |
| F-04 | P1 | Trust | One static `AI_PLATFORM_INTERNAL_TOKEN` is shared by Gateway, Knowledge, Runtime and Worker. One HMAC capability-lease secret is shared by Gateway, Runtime and Worker: the Runtime mints a per-call lease and the Worker verifies it, so any holder, including the verifier, can mint a lease for any tenant, run or capability. Model leases are HMAC-signed by the Gateway. | [docker-compose.yml:290, 607, 663](../../../docker-compose.yml); [capability_execution.rs:248–269](../../../rust/agent-runtime-overlay/kernel-rs/ai-platform-agent-runtime/src/capability_execution.rs); [`LeaseSigner` model.rs:26](../../../rust/platform-native/crates/platform-gateway/src/migration_agent/model.rs) | U-GW-02, U-PE-12 |
| F-05 | P1 | Data | Every Compose service connects with the bootstrap superuser credentials although scoped roles and grants exist in the baseline. | [docker-compose.yml:204, 448, 617, 672](../../../docker-compose.yml); [grants.sql](../../../database/baselines/2026_08_post_kb_v1/grants.sql) | U-DATA-02 |
| F-06 | P1 | Sandbox | The Capability Worker container runs with `seccomp=unconfined` so that its supervisor can create user namespaces. Python isolation is `unshare` user/net/mount/pid namespaces, rlimits, dropped capabilities with `no_new_privs`, and a hand-written seccomp filter that allows by default and denies about 16 namespace, mount and fork syscalls. It has no filesystem LSM (Landlock), no syscall allowlist and no cgroup limits, and it shares a kernel and container with brokers that hold the internal token and lease secret. The Python module denylist is not a security boundary. | [docker-compose.yml:647](../../../docker-compose.yml); [python_code_execution.rs:44](../../../rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-worker/src/python_code_execution.rs); [python_supervisor.rs:39, :231–380](../../../rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-worker/src/python_supervisor.rs) | U-CW-01, U-CW-03, U-CW-04 |
| F-07 | P1 | Web security | The console keeps the bearer JWT in `localStorage`/`sessionStorage`, where any XSS can read it. | [web/src/lib/api.ts:51–85](../../../web/src/lib/api.ts); [useAuthStore.ts:65–71](../../../web/src/store/useAuthStore.ts) | U-GW-03, U-WEB-01 |
| F-08 | P1 | Identity | User JWTs are HS256 with one shared secret. There is no OIDC/SAML SSO, SCIM, MFA or passkey support. | [platform-auth/src/lib.rs:153, 183, 192](../../../rust/platform-native/crates/platform-auth/src/lib.rs) | U-GW-03, U-GW-04 |
| F-09 | P1 | Tasks | Generic `/api/v1/tasks` defaults to an in-memory, destructively consumed queue; a durable ledger mode exists but is opt-in and pending schema acceptance ([design](../../design/generic-task-durable-rollout.md)). (Known: evolution F7.) | [bootstrap/core.rs:110–115](../../../rust/platform-native/crates/platform-gateway/src/migration_host/bootstrap/core.rs) | U-GW-11 |
| F-10 | P1 (HA) | Runtime | Turn cancellations, per-thread gates and the event broadcast live in process memory, which forces a single Runtime instance. | [http_service.rs:64–74](../../../rust/agent-runtime-overlay/kernel-rs/ai-platform-agent-runtime/src/http_service.rs) | U-RT-02, U-RT-03 |
| F-11 | P1 | Governance | The "official SDK" engine is a native loop that receives provider keys and calls providers directly; framework IDs are routing labels only. This diverges from ADR-012's wording and bypasses the Gateway model plane. | [sdk-agent-engine COMPATIBILITY.md:3–12](../../../rust/agent-runtime-overlay/kernel-rs/ai-platform-sdk-agent-engine/COMPATIBILITY.md) | U-SW-01 |
| F-12 | P1 | Deploy | The Helm chart describes a Python gateway; it has no Runtime, Capability Worker or Knowledge Worker; it runs databases as in-chart StatefulSets and uses `tag: latest`. | [values.yaml:13](../../../deploy/helm/ai-gateway/values.yaml); [templates](../../../deploy/helm/ai-gateway/templates/) | U-PE-30 |
| F-13 | P1 | Observability | The monitoring overlay is 2023-era (PostgreSQL 15, Collector 0.88, Prometheus 2.47, Grafana 10.2, Jaeger 1.51) and duplicates the app services. Tempo and Jaeger are two competing trace backends. | [docker-compose.monitoring.yml:63–190](../../../docker/monitoring/docker-compose.monitoring.yml) | U-PE-10 |
| F-14 | P2 | Observability | Native OTel exports traces only (`features = ["trace"]`); metrics use a separate registry; no OTLP logs; no GenAI semantic conventions. | [foundation Cargo.toml:9–11](../../../rust/platform-native/crates/platform-gateway-foundation/Cargo.toml) | U-PE-11 |
| F-15 | P2 | Modularity | `platform-knowledge-service` depends on the whole `platform-gateway` crate. The Knowledge domain lives in `platform-gateway-integrations`. | [Cargo.toml:15](../../../rust/platform-native/crates/platform-knowledge-service/Cargo.toml) | U-KN-01 |
| F-16 | P2 | Typing | Python semantics are embedded in the core: `python_json` canonical form, Python float spelling, CPython int limits, wall-clock breaker; routing/failover operate on `Value` records. | [python_json.rs](../../../rust/platform-native/crates/platform-gateway-foundation/src/python_json.rs); [model_failover.rs:1–20](../../../rust/platform-native/crates/platform-gateway-foundation/src/model_failover.rs); [circuit_breaker.rs:52](../../../rust/platform-native/crates/platform-gateway-foundation/src/circuit_breaker.rs) | U-GW-17, P-ADR-3 |
| F-17 | P2 | Product law | The quiz domain is built into foundation, Gateway and Worker, although [platform law §4](../../harness/platform-architecture.md) places artifact domains in Agent Plugins. The `ai-quiz` plugin depends on a "built-in generate_quiz tool". | `quiz_*` in [foundation](../../../rust/platform-native/crates/platform-gateway-foundation/src/) and [gateway](../../../rust/platform-native/crates/platform-gateway/src/); quiz_capabilities.rs (`rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-worker/src/quiz_capabilities.rs`, historical `22d9ce0`); [plugin.json](../../../agent-plugins/ai-quiz/plugin.json) | U-GW-18 |
| F-18 | P2 | Performance | The retrieval result cache is a `VecDeque` behind an async mutex, scanned linearly (≤ 2,000 entries) on every lookup. | service.rs:35 (`rust/platform-native/crates/platform-gateway-integrations/src/migration_knowledge_query/service.rs`, historical `22d9ce0`) | U-KN-07 |
| F-19 | P2 | Capability | Multimodal dataset retrieval is disabled in this release. | service.rs:219 (`rust/platform-native/crates/platform-gateway-integrations/src/migration_knowledge_query/service.rs`, historical `22d9ce0`) | U-KN-06 |
| F-20 | P2 | Efficiency | Durable workers poll (Knowledge dispatch every 1 s, recovery every 60 s); no LISTEN/NOTIFY wake-ups anywhere. | worker.rs:28–29 (`rust/platform-native/crates/platform-gateway-integrations/src/migration_knowledge_ingest/worker.rs`, historical `22d9ce0`) | U-KN-03, U-RT-04 |
| F-21 | P2 | Data | 163 tables with no partitioning and no row-level security. Usage, trace, audit and event tables grow without bound in the OLTP schema. | [init.sql](../../../database/baselines/2026_08_post_kb_v1/init.sql) (0 `PARTITION BY`, 0 `CREATE POLICY`) | U-DATA-03, U-DATA-04 |
| F-22 | P2 | Build | The native Dockerfile exists in five near-identical copies: two are byte-identical and three differ by one `ENV` line and a `final` stage. One builder compiles seven binaries for every image, so every image ships all binaries plus poppler and curl. The Runtime image's final stage is the full `rust:1.95.0-bookworm` toolchain image, and all images use Debian 12 bookworm. | Dockerfile (`Dockerfile`, historical `22d9ce0`), [deploy/platform-native/Dockerfile](../../../deploy/platform-native/Dockerfile), apps/knowledge-service/Dockerfile (`apps/knowledge-service/Dockerfile`, historical `22d9ce0`), apps/sdk-agent-worker/Dockerfile (`apps/sdk-agent-worker/Dockerfile`, historical `22d9ce0`), docker/migrate/Dockerfile (`docker/migrate/Dockerfile`, historical `22d9ce0`); [Dockerfile.runtime:79](../../../deploy/agent-runtime-source/Dockerfile.runtime) | U-PE-20, U-RT-08 |
| F-23 | P2 | Supply chain | 49 action references are pinned by mutable tags, and `ci.yml` has no `permissions:` block. There is no `cargo-deny`/RustSec, image scanning or signing (SBOM and provenance are produced). CI forces `CARGO_BUILD_JOBS=1`, and arm64 Rust images build under QEMU. | [ci.yml](../../../.github/workflows/ci.yml); [docker-publish.yml](../../../.github/workflows/docker-publish.yml) | U-PE-13, U-PE-21 |
| F-24 | P2 | Licensing | Root `LICENSE` is MIT; Rust crates declare Apache-2.0; the overlay derives from an Apache-2.0 upstream; the CLI package says MIT. | [LICENSE](../../../LICENSE); [Cargo.toml:7](../../../rust/platform-native/Cargo.toml); [sdk/cli/package.json:32](../../../sdk/cli/package.json) | U-PE-15 |
| F-25 | P2 | Web | Unused dependencies (`@langchain/core`, `@langchain/langgraph-sdk`, `marked`, `react-grid-layout`, `react-resizable`); two component systems (antd imported in 45 files, Radix in 11, Tailwind with `cva` alongside); two date libraries (dayjs 13 files, date-fns 1); `framer-motion` in 28 files; `@types/node` ^22 against Node 26. | [web/package.json:24–25, 52, 55, 59, 74](../../../web/package.json) | U-WEB-03 |
| F-26 | P2 | Dependencies | The archived `serde_yaml` is used in seven crates (plus `unsafe-libyaml`); `rustpython-parser` 0.4 is unmaintained. | Crate manifests under [rust/platform-native/crates](../../../rust/platform-native/crates/) | U-PE-14 |
| F-27 | P2 | Config | 208 variables in `.env.example`, mixing nested `GATEWAY_*__*`, flat provider names and a JSON options file. | [.env.example](../../../.env.example); [native-options.example.json](../../../deploy/platform-native/native-options.example.json) | U-GW-19 |
| F-28 | P2 | SDKs | The Python and Rust SDK directories are pointer READMEs; Java and Dart are hand-written; nothing is generated from the 349-path OpenAPI document. | [sdk/](../../../sdk/) | U-CL-10 |
| F-29 | P2 | Capability | The inbound LLM surface is `/v1/responses` plus registered transparent-proxy routes. There is no unified Chat Completions, Embeddings, Models or Anthropic Messages endpoint. | Route literals under [platform-gateway/src](../../../rust/platform-native/crates/platform-gateway/src/) | U-GW-05 |
| F-30 | P2 | Safety | No guardrail stage (prompt-injection, PII/secret or content-safety classification) on the request or response path. | — (absence across Gateway crates) | U-GW-09 |
| F-31 | P3 | Docs | Docs drift. `architecture.md` says Python is the default (lines 24, 33). ADR-008 says "polyglot is permanent" (line 60). `CLAUDE.md` lists a missing `skills/` directory (line 23). The agent-runtime-source README still describes a "130-file" overlay (line 186; the manifest has 148). READMEs call deleted Python scripts ([agent-runtime-source README:22](../../../deploy/agent-runtime-source/README.md), [authority README:8](../../../database/authority/README.md), [performance contract:16](../../design/enterprise-performance-contract.md)). Compose (line 445), Helm (line 13) and nginx (line 145) comments name Python/FastAPI. | as listed | U-PE-02 |
| F-32 | P1 | Interop | The MCP client sends `initialize` with exactly `2025-11-25` and fails with `MCP_PROTOCOL_VERSION_MISMATCH` on any other answer. Servers on the stateless `2026-07-28` revision (no `initialize`) and servers that negotiate an older revision cannot be registered. | [mcp_http.rs:31, 534–537](../../../rust/platform-native/crates/platform-gateway-foundation/src/mcp_http.rs); [mcp_gateway_broker.rs:452–453](../../../rust/platform-native/crates/platform-gateway-foundation/src/mcp_gateway_broker.rs) | U-GW-10 |
| F-33 | P1 | Governance | Knowledge holds its own provider credentials and calls embedding, rerank, OCR/VLM and judge providers directly. Those costs bypass Gateway quotas and price receipts (platform law L5). | [docker-compose.yml:456–483](../../../docker-compose.yml) | U-KN-10, U-GW-05 |
| F-34 | P1 | Capacity | The Capability Worker runs every capability execution through one process-wide slot (`Semaphore::new(1)`). One Python run (up to 300 s), Office render or slow Local Node call blocks every other tenant's tool calls. | [http_service.rs:91](../../../rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-worker/src/http_service.rs), [execution_run.rs:52](../../../rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-worker/src/http_service/execution_run.rs), [python_code_execution.rs:39](../../../rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-worker/src/python_code_execution.rs) | U-CW-02 |
| F-35 | P2 | Efficiency | Runtime SSE: every subscriber re-reads PostgreSQL every 500 ms in addition to one process-wide broadcast channel (capacity 256) that each subscriber filters by thread. The per-thread gate map is never pruned. | [events.rs:64, :85](../../../rust/agent-runtime-overlay/kernel-rs/ai-platform-agent-runtime/src/http_service/events.rs), [http_service.rs:57, :74, :211](../../../rust/agent-runtime-overlay/kernel-rs/ai-platform-agent-runtime/src/http_service.rs) | U-RT-04 |
| F-36 | P2 | Hardening | Only the Capability Worker drops capabilities and sets `no-new-privileges`. Gateway, Runtime, Knowledge, migrate and web containers run without `cap_drop`, a read-only root filesystem or a PID limit. | [docker-compose.yml:643–647](../../../docker-compose.yml) vs the other service blocks | U-PE-22 |
| F-37 | P1 | Web security | The web image serves from `nginx:1.27-alpine`, a 2024 mainline branch that no longer receives fixes. The stable branch is 1.30.x (WEB: 1.30.1 and 1.30.2 shipped security fixes in May 2026). The CSP allows any HTTPS/WSS `connect-src` and `img-src` and inline styles; the deprecated `X-XSS-Protection` header is still sent; there is no `Permissions-Policy` or COOP header. | [web/Dockerfile:16](../../../web/Dockerfile); [web/nginx.conf:54–58](../../../web/nginx.conf) | U-WEB-01, U-WEB-09 |
| F-38 | P1 | Recovery | `make backup` is a gzipped `pg_dump` of PostgreSQL only. There is no WAL archiving or point-in-time recovery, no Qdrant snapshot (vectors are rebuilt only by re-embedding), no object-store coverage and no backup encryption. The README §8 targets (RPO ≤ 5 min, RTO ≤ 30 min) are not reachable with this. | [Makefile:277–284](../../../Makefile), [scripts/new/backup.sh:122](../../../scripts/new/backup.sh) | U-DATA-06 |
| F-39 | P1 | Data security | Qdrant runs without an API key or JWT access control (the client supports a key; Compose sets none). All services share one bridge network, so every container, including the web edge and the Capability Worker, can read and write every tenant's vectors directly. | [docker-compose.yml:85–106, :701–704](../../../docker-compose.yml); qdrant.rs:130 (`rust/platform-native/crates/platform-gateway-integrations/src/migration_knowledge_query/qdrant.rs`, historical `22d9ce0`) | U-DATA-08, U-PE-22 |
| F-40 | P1 | Coordination | One Redis instance (128 MB, `allkeys-lru`) holds session allowlists, capacity leases, rate-limit windows, usage streams and caches. Under memory pressure, eviction can silently drop leases and rate windows (over-admission) or sessions (forced logout). | [redis-entrypoint.sh:41–45](../../../scripts/new/redis-entrypoint.sh); [sessions.rs:27](../../../rust/platform-native/crates/platform-auth/src/sessions.rs), [capacity_admission.rs:457–463](../../../rust/platform-native/crates/platform-gateway-foundation/src/capacity_admission.rs) | U-DATA-05 |

## 4. Re-check of earlier findings against the native code

| Earlier ID (vNext 01) | Status in Rust (SRC) | Evidence |
| --- | --- | --- |
| D19 global price overwrite by model name | **Still present** → F-03 | `model_pricing.rs:429` |
| D22 dispatcher retries any exception | **Fixed.** Only safe methods retry, and only connect, protocol or pool failures; an uncertain unsafe call becomes `UpstreamOutcomeUnknown`. | [dispatcher.rs:140–195](../../../rust/platform-native/crates/platform-gateway-foundation/src/dispatcher.rs) |
| D09 cross-tenant role-name dataset grant | **Fixed.** Role grants apply only when the actor's tenant matches the dataset's. | store.rs:27–60 (`rust/platform-native/crates/platform-gateway-integrations/src/migration_knowledge_query/store.rs`, historical `22d9ce0`) |
| D28 hard-coded SLO/SOC-2 claims on Login | **Removed** (no matches in `Login.tsx`) | [Login.tsx](../../../web/src/pages/Login.tsx) |
| 2026-08-17 review: dual rate limiting | **Fixed** in native: a single HTTP rate authority charges the dimensions once | [rate.rs:1–3](../../../rust/platform-native/crates/platform-gateway/src/migration_host/rate.rs) |

The remaining vNext findings should be re-checked the same way when their work packages
activate. The native port preserved behaviour on purpose, so Python-era defects can survive.

## 5. Strengths to preserve

- **Authority model:** signed immutable launches, model leases and capability descriptors; exact
  argument hashing; one-time approvals; dispatch fences; the unknown-effect barrier.
- **Durable execution evidence:** the Runtime's append-only event log with cursor replay, the
  persistence poison fence and owner-fenced recovery.
- **Knowledge lifecycle:** generation-based publication, durable ingestion claims, hybrid
  retrieval with Qdrant-native or application RRF, rerank degradation semantics and interactive
  deadlines.
- **Request governance:** permits that follow body lifetime, separate header and idle deadlines,
  bounded WebSocket sessions and graceful drain.
- **Database authority:** frozen baselines, measured fingerprints, ACL receipts and
  successor-proposal workflow.
- **Supply-chain identity for the Runtime:** source locks, receipts, SBOMs and paired image checks.
- **Engineering harness:** affected-gate selection, boundary gates, evidence receipts and a
  single-writer discipline.

<details>
<summary>原评审决策、风险与外部资料（只作背景，不产生开工前置任务）</summary>

### Proposed ADR topics

These are the original decision topics. Reuse accepted decisions and implemented contracts.
Write a new ADR only for an unresolved material architecture or compatibility choice; routine
implementation and documentation cleanup do not wait for ADR numbering or a review ceremony.

| ID | Decision | Supersedes / amends | Needed by |
| --- | --- | --- | --- |
| P-ADR-1 | Rust-native platform baseline; architecture entrypoint rewritten for native defaults | ADR-008 §2 ("polyglot is permanent"), vNext DEC-01, `architecture.md` §1–3 | Ph0 |
| P-ADR-2 | Service identity and signed authority: workload identity (mTLS), Ed25519 leases with key IDs, verify-only consumers, attenuable capability tokens | ADR-007 transport notes; internal token usage | Ph1 |
| P-ADR-3 | Canonical JSON v2 (RFC 8785 JCS) for hashes and signatures; `python_json` frozen as the v1 verifier | ADR-011 canonical hash notes | Ph1 |
| P-ADR-4 | Native crate topology and process roles; Knowledge independent of the Gateway crate | Stage F of the cross-service evolution | Ph2 |
| P-ADR-5 | Price book and usage ledger v2 (provider/model/version/effective range; cached and reasoning token components) | Current `model_pricing` contract | Ph1 |
| P-ADR-6 | SDK execution scope: native engine through the Gateway model plane; A2A for external frameworks | ADR-012 | Ph2 |
| P-ADR-7 | Sandbox isolation tiers: seccomp + Landlock baseline; gVisor/Kata/microVM tier | Capability Worker deployment notes | Ph1 (baseline), Ph3 (tier) |
| P-ADR-8 | Desktop shell: Tauri 2 vs the existing egui app | vNext DEC-07 vs `platform-desktop` | Ph2 |
| P-ADR-9 | Enterprise identity: OIDC/SAML SSO, SCIM 2.0, passkeys, BFF session cookies, EdDSA API tokens | `platform-auth` HS256 profile | Ph1–Ph2 |
| P-ADR-10 | Telemetry: OTLP for all signals, pinned GenAI semantic conventions, telemetry data tier | Monitoring Compose overlay | Ph1 |
| P-ADR-11 | Coordination substrate: Valkey 9; durable-versus-ephemeral rule for streams and caches | Redis 7 profile | Ph1 |
| P-ADR-12 | LLM inbound API surface; passthrough-first protocol translation policy | Transparent proxy and Responses notes | Ph2 |

### Review risks

| Risk | Likelihood / impact | Mitigation |
| --- | --- | --- |
| Codex overlay drift (148 overlay files, 37 of them modified upstream files, against a moving upstream) | High / High | Seam contract tests, App Server schema-diff gate (exists), scheduled sync windows, push generic hooks upstream (U-RT-07) |
| Refactoring destabilizes acceptance | Medium / High | Strangler order; golden fixtures, OpenAPI and event diffs as hard gates; one domain at a time; feature flags per tenant |
| Single-writer and build-capacity bottleneck | High / Medium | Hosted CI with sccache and nextest first (U-PE-03); serialize only shared manifests and locks |
| Protocol churn (MCP 2026-07-28 removes sessions; A2A v1.0; GenAI semconv still "Development") | Medium / Medium | Pin versions, adapters at the edge, dual-revision windows, contract fixtures per revision |
| Client breakage from auth changes (cookies, token formats) | Medium / Medium | Dual-mode windows, per-tenant switches, SDK deprecation policy |
| Large-table migrations (partitioning, RLS) | Medium / High | Online attach-partition migrations, rehearsal on restored snapshots, PITR before cut-over |
| Licence or distribution changes in infrastructure | Medium / Medium | Prefer foundation-governed projects (Valkey, CloudNativePG, Qdrant OSS), mirror images, SBOM licence gate |
| Sandbox runtimes unavailable on customer hosts | Medium / Medium | Capability detection with a fail-closed tier; documented minimum host profile |
| Model and provider churn | High / Medium | Catalog sync (exists), versioned routing policy, eval-gated promotion |

### External references (2026-10-16)

Versions and dates change quickly. Re-check each one when a work package is activated.

- PostgreSQL 19 schedule: RC1 planned 2026-10-15; GA moved to 2026-11-19 — [pgsql-announce thread](https://www.postgresql.org/message-id/f161fbb7-e193-42eb-bec7-4cfc1af74a50%40postgresql.org), [release notes draft](https://www.postgresql.org/docs/19/release-19.html)
- MCP specification `2026-07-28` (stateless requests, `server/discover`, extensions, lifecycle policy) — [MCP blog](https://blog.modelcontextprotocol.io/tags/protocol/), [breaking-changes summary](https://www.developersdigest.tech/blog/mcp-2026-07-28-breaking-changes)
- A2A protocol v1.0 under the Linux Foundation — [LF press release](https://linuxfoundation.org/press/a2a-protocol-surpasses-150-organizations-lands-in-major-cloud-platforms-and-sees-enterprise-production-use-in-first-year), [specification overview](https://tyk.io/learning-center/a2a-protocol-architecture-and-technical-specification/)
- Axum 0.8.9 is the latest release; 0.9 is unreleased — [docs.rs](https://docs.rs/crate/axum/)
- SQLx 0.9.0 released 2026-05-06 — [release discussion](https://github.com/launchbadge/sqlx/discussions/4271)
- Qdrant 1.19 (Turbo4 storage, memory tiers, per-tenant IDF); 1.19.2 on 2026-10-05 — [Qdrant blog](https://qdrant.tech/blog/qdrant-1.19.x/), [releases](https://github.com/qdrant/qdrant/releases)
- OpenTelemetry GenAI conventions still mostly "Development" — [Greptime overview](https://www.greptime.com/blogs/2026-05-09-opentelemetry-genai-semantic-conventions), [Dash0 explainer](https://www.dash0.com/knowledge/opentelemetry-genai-semantic-conventions-explained)
- OpenTelemetry Rust 0.32 line — [release tracker](https://releasealert.dev/github/open-telemetry/opentelemetry-rust)
- Valkey 9 (BSD-3) and Redis 8 (tri-licence including AGPLv3) — [overview](https://www.cloudmagazin.com/en/2026/04/14/valkey-9-ga-what-the-cache-fork-means-for-dach-ops-teams-18/), [Valkey 9.0 notes](https://blog.remirepo.net/post/2025/10/17/Valkey-version-9.0)
- TypeScript 7.0 (native compiler) released July 2026 — [InfoQ](https://infoq.com/news/2026/08/typescript-7-released/)
- Kubernetes Agent Sandbox (SIG Apps; gVisor/Kata through RuntimeClass) — [Kubernetes blog](https://kubernetes.io/zh-cn/blog/2026/03/20/running-agents-on-kubernetes-with-agent-sandbox/), [project docs](https://agent-sandbox.sigs.k8s.io/docs/getting_started/overview/)
- Gateway API Inference Extension — [Kubernetes blog](https://kubernetes.io/blog/2025/06/05/introducing-gateway-api-inference-extension/), [KubeCon EU 2026 recap](https://www.codecentric.de/en/knowledge-hub/blog/kubecon-europe-2026-ai-agents-go-to-production)
- agentgateway (Rust MCP/A2A/LLM proxy, Linux Foundation) — [docs](https://agentgateway.dev/docs/standalone/latest/faqs/)
- Rust 1.99.0 released 2026-10-01 — [coverage](https://www.warp2search.net/story/rust-1990-ships-with-stable-cvariadic-functions/)
- OpenAPI 3.2.0 (`itemSchema` for SSE/JSONL) — [OpenAPI Initiative](https://www.openapis.org/?p=4078), [SSE in 3.2](https://developerhub.io/blog/event-streaming-in-openapi-3-2-what-changed-and-why-it-matters/)
- Node.js 26 enters LTS late October 2026 — [Node.js releases](https://nodejs.org/en/about/previous-releases)
- jsonwebtoken 11 with pluggable crypto providers — [docs.rs](https://docs.rs/crate/jsonwebtoken/latest)
- Embedding and reranker landscape 2026 — [Zilliz benchmark](https://www.zilliz.com/blog/choose-embedding-model-rag-2026), [Qwen3 Embedding paper](https://arxiv.org/abs/2506.05176v3)
- Document parsing (PaddleOCR-VL 1.6, MinerU, Granite-Docling) — [benchmark write-up](https://www.besthub.dev/articles/open-source-pdf-parser-benchmark-mineru-granite-docling-paddleocr-vl-and-xberg-9f184b033c66), [PaddleOCR docs](https://www.paddleocr.ai/latest/en/)
- pgvector 0.8.x and VectorChord 1.x — [pgvector changelog](https://pgxn.org/dist/vector/CHANGELOG.html), [VectorChord on PGXN](https://pgxn.org/dist/vchord/1.1.0/)
- Guardrail models (Llama Prompt Guard 2, Llama Guard 4, LlamaFirewall) — [Prompt Guard](https://developer.meta.com/ai/docs/model-cards-and-prompt-formats/prompt-guard/), [LlamaFirewall paper](https://arxiv.org/pdf/2505.03574)
- React Compiler 1.0, Vite 8, Rolldown 1.0 — [React blog](https://uk.react.dev/blog/2025/10/07/react-compiler-1), [Vite 8 announcement](https://vite.dev/blog/announcing-vite8), [Rolldown 1.0](https://voidzero.dev/posts/announcing-rolldown-1-0)
- Vite 8 `codeSplitting` replacing `manualChunks`; plugin-react v6 without Babel — [Rolldown option reference](https://rolldown.rs/reference/OutputOptions.advancedChunks), [migration write-up](https://dev.classmethod.jp/articles/vite7-to-vite8-migration-pitfalls/), [React Compiler with plugin-react v6](https://recca0120.github.io/en/2026/04/14/react-compiler-vite-v6/)
- TypeScript 7.0 GA without a stable programmatic API until 7.1 — [migration readiness](https://ecorpit.com/typescript-7-migration-readiness-eslint-astro-blockers-2026/), [TypeScript blog](https://devblogs.microsoft.com/typescript/progress-on-typescript-7-december-2025)
- Debian 12 security support handed to LTS on 2026-07-12 — [Debian announcement](https://www.debian.org/News/2026/20260712.en.html)
- Python 3.15.0 final scheduled for 2026-10-09 after rc3 — [python.org rc3](https://www.python.org/downloads/release/python-3150rc3/), [endoflife.ai](https://endoflife.ai/python/3.15)
- nginx 1.30 stable branch and May 2026 security releases — [nginx community announcement](https://community.nginx.org/t/nginx-1-30-0-stable-version-released/9417), [release notes](https://releasebot.io/updates/nginx)
- Codex release cadence (`rust-v0.162.1`, 2026-10-09) — [release tracker](https://www.havoptic.com/releases/openai-codex/2026/10)
- Qdrant tiered multitenancy (1.16) and 1.19 memory tiers — [Qdrant 1.16](https://qdrant.tech/blog/qdrant-1.16.x/), [memory tiers](https://qdrant.tech/documentation/ops-configuration/memory-tiers/index.md)
- PostgreSQL 19 draft notes (REPACK CONCURRENTLY, sequence replication, WAIT) — [release notes draft](https://postgresql.org/docs/release/19.0), [feature status](https://www.devx.com/cybersecurity/postgresql-19-new-features-release-date/)
- CloudNativePG and the Barman Cloud plugin — [1.28 release notes](https://cloudnative-pg.io/docs/1.30/release_notes/old/v1.28)
- MinIO Community Edition archived; self-hosted S3 alternatives — [status summary](https://glukhov.org/data-infrastructure/object-storage/minio-dead/), [alternatives](https://glukhov.org/data-infrastructure/object-storage/self-hosted-s3-alternatives/)
- Agent Client Protocol and the Codex adapter — [Zed ACP](https://zed.dev/acp), [Codex in Zed](https://zed.dev/blog/codex-is-live-in-zed)
- Tauri 2 changelog (IPC ACL change, updater) — [Tauri releases](https://tauri.app/release/tauri/all-versions/)

</details>
