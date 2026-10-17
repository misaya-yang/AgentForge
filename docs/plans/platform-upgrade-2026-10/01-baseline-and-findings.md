# 01 Baseline and findings

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
| F-17 | P2 | Product law | The quiz domain is built into foundation, Gateway and Worker, although [platform law §4](../../harness/platform-architecture.md) places artifact domains in Agent Plugins. The `ai-quiz` plugin depends on a "built-in generate_quiz tool". | `quiz_*` in [foundation](../../../rust/platform-native/crates/platform-gateway-foundation/src/) and [gateway](../../../rust/platform-native/crates/platform-gateway/src/); [quiz_capabilities.rs](../../../rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-worker/src/quiz_capabilities.rs); [plugin.json](../../../agent-plugins/ai-quiz/plugin.json) | U-GW-18 |
| F-18 | P2 | Performance | The retrieval result cache is a `VecDeque` behind an async mutex, scanned linearly (≤ 2,000 entries) on every lookup. | [service.rs:35](../../../rust/platform-native/crates/platform-gateway-integrations/src/migration_knowledge_query/service.rs) | U-KN-07 |
| F-19 | P2 | Capability | Multimodal dataset retrieval is disabled in this release. | [service.rs:219](../../../rust/platform-native/crates/platform-gateway-integrations/src/migration_knowledge_query/service.rs) | U-KN-06 |
| F-20 | P2 | Efficiency | Durable workers poll (Knowledge dispatch every 1 s, recovery every 60 s); no LISTEN/NOTIFY wake-ups anywhere. | [worker.rs:28–29](../../../rust/platform-native/crates/platform-gateway-integrations/src/migration_knowledge_ingest/worker.rs) | U-KN-03, U-RT-04 |
| F-21 | P2 | Data | 163 tables with no partitioning and no row-level security. Usage, trace, audit and event tables grow without bound in the OLTP schema. | [init.sql](../../../database/baselines/2026_08_post_kb_v1/init.sql) (0 `PARTITION BY`, 0 `CREATE POLICY`) | U-DATA-03, U-DATA-04 |
| F-22 | P2 | Build | The native Dockerfile exists in five near-identical copies: two are byte-identical and three differ by one `ENV` line and a `final` stage. One builder compiles seven binaries for every image, so every image ships all binaries plus poppler and curl. The Runtime image's final stage is the full `rust:1.95.0-bookworm` toolchain image, and all images use Debian 12 bookworm. | [Dockerfile](../../../Dockerfile), [deploy/platform-native/Dockerfile](../../../deploy/platform-native/Dockerfile), [apps/knowledge-service/Dockerfile](../../../apps/knowledge-service/Dockerfile), [apps/sdk-agent-worker/Dockerfile](../../../apps/sdk-agent-worker/Dockerfile), [docker/migrate/Dockerfile](../../../docker/migrate/Dockerfile); [Dockerfile.runtime:79](../../../deploy/agent-runtime-source/Dockerfile.runtime) | U-PE-20, U-RT-08 |
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
| F-39 | P1 | Data security | Qdrant runs without an API key or JWT access control (the client supports a key; Compose sets none). All services share one bridge network, so every container, including the web edge and the Capability Worker, can read and write every tenant's vectors directly. | [docker-compose.yml:85–106, :701–704](../../../docker-compose.yml); [qdrant.rs:130](../../../rust/platform-native/crates/platform-gateway-integrations/src/migration_knowledge_query/qdrant.rs) | U-DATA-08, U-PE-22 |
| F-40 | P1 | Coordination | One Redis instance (128 MB, `allkeys-lru`) holds session allowlists, capacity leases, rate-limit windows, usage streams and caches. Under memory pressure, eviction can silently drop leases and rate windows (over-admission) or sessions (forced logout). | [redis-entrypoint.sh:41–45](../../../scripts/new/redis-entrypoint.sh); [sessions.rs:27](../../../rust/platform-native/crates/platform-auth/src/sessions.rs), [capacity_admission.rs:457–463](../../../rust/platform-native/crates/platform-gateway-foundation/src/capacity_admission.rs) | U-DATA-05 |

## 4. Re-check of earlier findings against the native code

| Earlier ID (vNext 01) | Status in Rust (SRC) | Evidence |
| --- | --- | --- |
| D19 global price overwrite by model name | **Still present** → F-03 | `model_pricing.rs:429` |
| D22 dispatcher retries any exception | **Fixed.** Only safe methods retry, and only connect, protocol or pool failures; an uncertain unsafe call becomes `UpstreamOutcomeUnknown`. | [dispatcher.rs:140–195](../../../rust/platform-native/crates/platform-gateway-foundation/src/dispatcher.rs) |
| D09 cross-tenant role-name dataset grant | **Fixed.** Role grants apply only when the actor's tenant matches the dataset's. | [store.rs:27–60](../../../rust/platform-native/crates/platform-gateway-integrations/src/migration_knowledge_query/store.rs) |
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
