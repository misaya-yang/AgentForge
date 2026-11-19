# Gateway migration lane: concrete parity closures

## Scope and implementation

The original lane edits `rust/platform-native/crates/platform-gateway/src` and these evidence reports. Following the parent's full-auth-parity direction and an explicit ownership handoff from the foundation worker, this lane also edits `platform-gateway-foundation/src/assistant_catalog.rs`, `assistant_plugins.rs`, and `assistant_plugin_mutations.rs` to add the shared identity seam. No Python, schema, credential, deployment, or Cargo invocation was changed by this lane. Existing source-backed implementations were reused.

Five verified source-to-native route gaps are closed in source:

1. `GET /api/v1/setup/state`: construct the already-ported SetupApi with live shared provider metadata, host `model_setup_mode`/`default_model`, admission/RBAC, and cancellation. Previously the implementation existed but was never mounted.
2. `GET /api/v1/admin/architecture-status`: fix the host mount from the incorrect root `/admin/architecture-status` to the Python `/api/v1` prefix. Setup and architecture now use real permission-denial auditing and trusted trace IDs.
3. `GET /api/v1/assistant/sessions/{session_id}/tools`: implement the Python owner check, policy validation, ordered platform catalog, pinned snapshot projection, and live next-turn estimate through the existing Runtime model/catalog authorities. Worker failures degrade only the estimate; policy failures fail closed. Raw SQL errors are sanitized.
4. `GET /api/v1/assistant/sessions/{session_id}/metrics`: implement session ownership, limit bounds, recent-window aggregation, rounded averages, and exact empty fields.
5. `GET /api/v1/assistant/metrics/tenant`: implement tenant filtering, strict time cutoff, unique-session count, integer averages, and nullable zero/empty utilization.

The metrics collector intentionally retains the source's lazy, process-wide deque capped at 100 records. The Python application does not initialize a persistent collector or write context metrics in the gateway paths inspected. Native starts empty equivalently and exposes a producer `record` method; it does not invent data from billing or claim telemetry producers are integrated.

New public diagnostics use the existing content-request context middleware, preserving authenticated/guest resolution and request-local authority caching.

## User-context parity closure

The production GatewayGraph now injects its existing `NativeContentAccess` into the older assistant base router. Model/config/dataset reads and session CRUD/history resolve the canonical JWT, API-key or anonymous-cookie context instead of rejecting everything except JWT. The foundation tools/policies/plugin routes receive the same resolver through a narrow typed `CatalogIdentityResolver`; their explicit authenticated-user requirement remains intact.

History source access accepts resolved `ContentIdentity` directly. KnowledgeActor applies its existing conservative guest downgrade. Internal share checks no longer manufacture an Account/Principal just to pass source authorization. Legacy standalone router factories retain the verified-JWT fallback when no production resolver is supplied; the production graph always supplies the canonical resolver.

## Source evidence

- `src/api/v1/setup.py` and `src/services/llm/provider_setup.py`
- `src/api/v1/architecture_status.py`, `src/api/router.py`, `src/main.py`
- `src/api/v1/_assistant_routes/tools.py`
- `src/api/v1/_assistant_routes/metrics.py` and `schemas.py`
- `packages/ai-gateway-core/src/ai_gateway_core/metrics/context_metrics.py`
- Existing `platform-gateway-foundation/src/setup_api.rs`, `architecture_status_api.rs`, `assistant_catalog.rs`

## Inventory

`reports/rust-migration-gateway-routes.md` maps all 452 published HTTP operations across 349 frozen OpenAPI paths to 49 existing native owner files. No owner is missing from the checkout. This is an ownership inventory, not an assertion of full semantic parity. Hidden/internal registrations omitted from OpenAPI are not included in that count. Existing wildcard dispatchers implement Agent/Studio, eval, LangGraph, local-node and proxy groups; a path match alone does not establish complete branch parity.

## Regression coverage added

- `migration_host::foundation::setup_tests::setup_projects_current_tenant_configuration`
- `migration_host::foundation::setup_tests::setup_denies_before_reading_provider_state`
- `migration_host::catalog::session_tools::tests::projection_preserves_groups_and_falls_back_to_catalog`
- `migration_host::context_metrics::tests::recent_window_limit_and_tenant_isolation`
- `migration_host::context_metrics::tests::cutoff_is_strict_and_zero_utilization_is_null`

Additional auth regression coverage:

- `read_identity_tests::catalog_adapter_preserves_api_key_authority_and_ignores_forged_identity_headers`
- `read_identity_tests::catalog_adapter_never_promotes_guest_to_authenticated`
- `knowledge::read_identity_tests::resolved_guest_retains_scope_without_authority_promotion`
- `knowledge::read_identity_tests::resolved_authenticated_identity_keeps_real_scope_and_permissions`

No Cargo run was performed by this lane, per serialized-build ownership. Parent's first compiler pass found a missing sqlx-error conversion in session tools; that was fixed explicitly. Test execution, final compilation, full HTTP integration and live DB/Worker/E2E acceptance remain parent-owned and must not be represented as passed without their results.

## Remaining parity risks found

- The older assistant base JWT-only gap is repaired through the production native identity seam described above. Live API-key/cookie/JWT integration still requires end-to-end acceptance.
- Session tool next-turn estimation uses existing RuntimeModels, whose missing-model response is fail-closed unavailable, whereas Python can use capability revision 1 after a `None` model. No authority widening was introduced to emulate that edge case.
- Query validation errors use the gateway's existing sanitized error pattern; exact FastAPI validation-envelope parity is not established by these tests.
- The ownership map does not validate every migration service's behavior and is not a claim that the complete migration is finished.

## Transparent proxy and KB-tools mount audit (2026-10-15)

The earlier owner inventory exposed a real distinction: `transparent_proxy.rs` contained the transport engine but no HTTP mount. Added `migration_compat/proxy_http.rs` and production graph registration for discovery, service root, wildcard seven-method forwarding, `_health`, and `_selftest` (17 published operations). Added the missing five-method `/api/v1/kb-tools/{path}` alias to the existing signed Knowledge proxy. The KB alias retains body/delete-confirmation preparation but does not incorrectly intercept the Knowledge management-only impact route.

The proxy adapter reuses real content identity and request-scoped database-key restrictions; AgentInvoke capability, configured service auth and API-key prefix header, tenant/key/user service constraints; shared service configuration cache; ordered rate-policy batching/service overrides/fallback and response headers; live model allowlists/governance, quota failure mode, nested downgrade projection, deduplicated alerts and security recording; source request class and configured capacity budgets; native streaming/accounting/cancellation engine. Discovery whitelists configuration fields and omits upstream identifiers for non-admins. Selftest probes assistant search and first stream chunk with five-second timeout and owned stream drop. Availability uses the native engine's shared cache rather than unconditional discovery probes.

Added pure native tests for source operation matching order, streaming predicates and safe model-override projection. Existing native governance now preserves Retry-After and structured fail-closed errors. Parent compiler found `url` dependency absent in the initial adapter; explicitly added the already-locked `url = "2"` dependency. No Cargo command has been executed by this lane; all compilation and execution results remain parent-owned.

Further acceptance still required: complete mounted HTTP contract integration, source service-auth error ordering under missing configs, background availability cancellation, health/stream/selftest upstream fixtures, and full database-backed quota/rate scopes. These are substantive implemented routes, but this report does not assert full application equivalence from static inspection. SDK Python API parity is owned and actively implemented by the local-clients lane.

## Native source regression matrix files

- `crates/platform-gateway/tests/setup_source_matrix.rs`: actual nested HTTP router and method/status/body contract. Provider metadata and trusted identity are named test doubles; tests cover unconfigured/configured/custom provider IDs, missing metadata, deployment mode/default, permission denial, configured RBAC role, guest denial, and GET-only methods.
- `crates/platform-gateway/tests/proxy_source_policy_matrix.rs`: native policy/scoping/RBAC and in-memory rate store using source test fixtures. Explicitly lower-level policy coverage, not end-to-end authentication or PostgreSQL coverage.
- `crates/platform-gateway/tests/proxy_payload_source_matrix.rs`: real payload preparation, trusted identity overwrite, browser gateway-model scrubbing, domain-policy preservation, run selection, allowlist matching/denials, and HTTP Retry-After serialization.

A further static audit checked every non-parameter OpenAPI path absent as a whole literal. All 18 such paths have corresponding nested/wildcard registrations (conversations guest-session, LangGraph, eval, files, local-node pairing and image blobs). This supports route ownership completeness only; it does not replace live mounted-router tests.

Parent compile `workspace-check4.log` found the security recorder helper inserted under the wrong impl. Moved `record_decision` into `PgFoundationSecurityEvents`; all five diagnostics share that cause. Awaiting parent recheck. No lane-executed tests are reported as passing.

## Canonical invocation/task identity and startup admission

Invocation and task router adapters now consume real ContentIdentity through the existing identity authority. Genuine session JWT selection is reauthenticated by the foundation session authority; API-key/guest contexts use the existing resolved-context preflight constructor and never fabricate a Principal or SDK grant. Task owner/admin checks use real tenant/user/roles. Session-list, setup and service-list identity adapters use the same canonical context. Source-equivalent application schema admission now executes immediately after database pool acquisition, before bootstrap admin/catalog writes, route construction or workers. The migration-authority reader preserves the source Gateway epoch window 2..10; canonical epoch17 refusal remains visible rather than widening authorization.

## Native contract tools and additional source tests

`platform-openapi` now implements native `gate` and `snapshot` commands, bounded schema/request/response comparison, profile ownership validation, independent subprocess determinism, source fingerprints, output alias safety, conservative previous-declaration drift, and explicit incomplete coverage evidence. The producer only emits native source-owned mounted declarations. It does not copy missing published paths into an apparently implemented contract. Complete native Knowledge owner export remains missing, and full native verification fails closed even on artifact equality. Legacy profile names are metadata compatibility labels, never interpreter launch paths.

Native source matrices added: response alternatives (10 tests), generic contract drift (3 tests), trace feedback (7 exact source cases with extracted literal JSON fixtures), and transparent proxy streaming timeout/connection errors (2 real loopback transports, restored one-slot permit and released connection ownership). The latter retain the pre-existing sanitized native error body; source dynamic exception-string body equivalence is still unresolved. No live database, environment matrix or Cargo command was run by this lane. Parent `release-openapi-tests2.log` confirms the 10 response-alternative tests passed and 27 producer unit tests passed; newly added generic drift/snapshot/feedback tests require the next parent-owned run. Parent gateway lib run passed 106/107, with only the stale wildcard count2 vs actual3 fixed afterward and KB-tools operationId corrected.

## Agent Plugins loader and capacity inventory

New lightweight `platform-agent-plugins` crate replaces the local validator and package loader. It loads contained manifest/Skills/agent-extension/MCP declarations without executing commands or network requests, isolates component diagnostics, quarantines all duplicate agent IDs, and preserves source limits, metadata/provenance, hashing and explicit transport rules. Source callsite audit found the loader only in the validator CLI and tests, so no catalog activation path was invented. Parent reports 16 native plugin tests passed. `validate-agent-plugin` outputs the source CLI JSON with secret-bearing env/headers omitted.

`gateway_capacity_inventory` native binary now projects the exact eight source SERVICE_UPSTREAM_GROUPS through the existing native CapacityResolver; preserves mode `single-node`, epoch `uat-2026-05`, text/JSON and missing-exit2/allow-missing behavior. Historical script called a nonexistent `inventory_rows`; root explicitly authorized this minimal repair using existing source mappings and resolver defaults. No environment reads, database or admission requests occur.

## Additional mounted-handler contracts

`migration_host/native_public_contract.rs` now contributes 27 implemented operations on 15 paths: setup, session tools/context metrics, proxy discovery/root/passthrough/health/selftest, architecture status, assistant models and health reads. Diagnostic response properties follow actual native serializers; proxy bodies remain opaque. Proxy/tools/metrics routing shares this module's route constants. Export fingerprints include the handler, model and architecture source files and are checked by the native snapshot CLI. Missing other operation families remain explicit incomplete coverage.

Fixed Axum automatic HEAD exposure on six GET-only Assistant endpoints (session list/detail/history, models, config, datasets), matching source GET declarations with explicit 405. This change and the latest producer extensions await parent-owned compiler/test results.

## Native KB release and boundary gates (2026-10-15)

Implemented `ai-platform-openapi kb-release-evidence` as an offline native port of the T0 release-evidence script: manifest file SHA binding and containment, 200–400 cases, unique IDs, reviewed source provenance and 40–60% real mix, version consistency, reviewed pointer/baseline dataset binding, expectations/observations hashes, finite retrieval metrics and query-count binding. It reports PASS/BLOCKED and never modifies evidence. Seven source release fixture cases are ported in `platform-openapi/tests/kb_release_source_matrix.rs`; additional metric/path tests exercise fail-closed behavior.

Implemented `ai-platform-openapi gateway-kb-boundary` using `syn` AST traversal of string literals (including macro arguments) and serialized native DTO declarations. It scans Gateway, foundation, and Gateway-owned integrations, and learns mirror names from dedicated Knowledge-owned modules. No function exemptions were introduced. Knowledge module ownership replaces the former Python package boundary. Exact SQL/metadata table set and name heuristics are preserved. Missing scan roots and parse failures cannot silently pass. Source metadata/DDL/verb and schema mirror cases are included as native unit tests.

Integrated the dedicated Knowledge owner's 13 real Confluence/upload operation declarations by source inclusion into the native OpenAPI producer; snapshot validates their source SHA fingerprints. Full native schema coverage is still incomplete and retains the explicit incomplete marker.

These latest additions are formatted only; no Cargo invocation was performed by this lane. Root must run serialized compile/tests before any passing verification claim.

## Provider/model producer declarations

Added 18 actual mounted provider/model administration operations in `platform-openapi/src/provider_contract.rs`, sourced from native `provider_admin_api`/`model_admin_api` validation and response projection functions. The producer includes create/update constraints, nullable optional fields, API-key write-only handling, Decimal response strings, exact query requirements, template and sync projection fields, and bundled capability adapter response fields. SHA fingerprints include the adapter, both handler owners, template owner and bundled capability data. Service responses without an explicit projection (provider test/model toggle) remain opaque. This improves declaration coverage without claiming that all native routes or all response alternatives are yet declared.

## Skill registry and bounded YAML resolver

Added the actual missing core SkillRegistry API in `migration_content/skill_registry.rs`: global registration, immutable tenant/user/version caching, exact-version no-fallback reads, fresh per-run views, deterministic filtered listing and token-overlap selection, implementation-free prompt metadata, and optional database loading/saving through the existing immutable SkillRepository. Generated Skills remain disabled/proposed until existing review/evaluation/rollback gates pass. Registry validation deliberately matches the original dataclass's three required strings rather than accidentally inheriting the stricter upload permission policy. Dedicated source tenant-isolation tests are owned by the local-client lane.

Plugin parsing now normalizes YAML1.1 plain booleans, leading-zero octal, sexagesimal integers and scientific/0o spelling differences using libyaml scalar events. Quoted strings, block strings and aliases retain their semantics. Two focused parser tests were added. Timestamp/explicit-tag edge equivalence is not yet established; the source map remains partial for that loader instead of claiming full YAML dialect equivalence.

## Bootstrap optional capability parity

Sandbox transport is now optional in production composition, matching the source lifecycle which mounts the internal route but never initializes `app.state.python_code_capability`. Absent origin/token no longer blocks Gateway startup; the authenticated execute handler returns source status503 and `python capability unavailable`. Providing either value activates validation of the configured pair and retains the real HttpSandboxTransport. Artifact routes remain available independently.

Model/provider invalidation has real Redis transport under the existing Required mode. The parent updates the deployment example away from Absent; no replacement publisher was needed. Local device channel None values match explicit source main.py assignments. Legacy Confluence dynamic registrar still corresponds to the absent source module; the separate native Confluence broker is mounted and not disabled by that registrar policy.

The previously recorded missing-model estimate difference is now closed: a new optional RuntimeModels read seam preserves `None` separately from database/provider lookup failures. The concrete ModelService implements it directly. Session-tool diagnostics use revision1 for genuine absence, as the source does; mandatory Runtime model admission still rejects absence and other failures. This does not promote a nonexistent model into a runnable model or bypass capability-catalog authorization.

Added eight mounted Skills API declarations to the native OpenAPI producer, retaining multipart upload201, nullable metadata-update fields, and empty-string-valid test input. While deriving declarations, fixed actual HTTP drift: Skill test input no longer uses a nonempty-string helper; non-object update payloads no longer silently become read-only requests; Skills GET routes reject Axum's implicit HEAD.

## Remaining exported Skill APIs

Added source SkillBuilder propose/approve APIs with the exact `assistant_audit_events` writes and optional in-memory behavior, plus SkillExecutor/register_builtin with explicit in-process callback registration. Tenant instruction execution continues through the existing db:// immutable-identity checks; no tenant code loader is introduced. Builtin callback errors return the source fixed failure message without leaking the callback error. These are actual missing exported core APIs, not replacement tests. Parent compilation is pending for these newest files.

### Live document and missing dashboard WebSocket (2026-10-15)
- Added shared pure `platform-openapi/src/document.rs` extension producer, used by offline binary and live Gateway `/openapi.json`; no AST/parser dependency enters Gateway runtime through this reuse.
- Ported previously absent dashboard WebSocket into its existing foundation router: canonical handshake-header identity, platform metrics authorization, source snapshot/alerts/tenant projections, zero-run null success rate, five-second receive cadence and ping/pong. Pre-upgrade authorization rejection remains HTTP403 (the actual source ASGI behavior); a production-used helper preserves requested source close classifications4001/4003 for behavioral tests.
- Readiness now passes generated `svc-` request IDs and exposes the actual production HTTP probe for source boot tests. Auth/foundation owner owns boot/authorization test targets; compilation remains parent-serialized.
- Shared broad route registry now declares252 explicit route/method pairs from51 mounted native sources and concrete dispatcher match arms. Rich typed declarations are retained; opaque additions explicitly mark response/request details unverified.37 dispatcher boundaries remain tracked without inventing wildcard operation coverage. Snapshot validates bundled route-source hashes; no historical Python schema is embedded as implemented operations.

### Concrete remaining runtime gap closed (2026-10-15)
The native TTFT evaluator existed but actual model-plane streams had no source timing evidence. Added production first-write monotonic stamps and typed visible-delta recognition for both provider wires, exact additive timing component names and six-decimal sanitized evidence logs. Missing stamps stay `None`. `model_timing.rs` includes additive identity and tolerance tests; real model.rs stream owns the evidence lifetime through success/error/cancellation. Parent compile/test pending. Source map now separates incomplete schema declarations, verification depth, YAML scalar edges and deliberate proxy diagnostic differences from absent business implementations.
- Closed source process-wide Redis logical RTT evidence gap at the existing successful Redis rate batch boundary. The implementation already had per-instance counts; added shared aggregate/reset API only. Source counts successful script invocations, not NOSCRIPT transport recovery attempts. Both existing HTTP/multidimension paths reuse this native boundary.
- Final owned source map distinguishes implementation presence from exact retirement certification. Plugin datetime/explicit-tag typing and deliberate proxy sanitized diagnostics remain stated semantic holds; opaque/incomplete OpenAPI descriptions do not imply missing mounted business implementations. Source references were not deleted by this lane.

## Optional Qdrant gRPC and internal retrieval RPC (2026-10-15)

- Source `vector_store.py` forwards `prefer_grpc`, and both Knowledge startup paths consume it; it is an active transport option, not historical dead code.
- Shared `migration_knowledge_query/qdrant_grpc{,_values,_config}.rs` uses official qdrant-client 1.19.0 (serde only). The existing QdrantStore dispatches collection creation/info/update/deletion/existence/list, payload index creation, query and query batch, retrieve/upsert/update vectors, set payload/delete, scroll/count through it when selected. Dense, sparse, server-side BM25 documents, weighted RRF, tenant/dataset filters and metadata receipts use typed official protobuf messages. Same resilience/deadline wrapper; no HTTP fallback. HTTP remains default; gRPC default port6334 matches Python client's default.
- Dedicated Knowledge `KNOWLEDGE_QDRANT__PREFER_GRPC` no longer rejects the implemented option; both native bootstraps select it after timeout configuration. Source-only conversion tests cover identity filters, point IDs, named vectors, inference documents and weighted RRF. No live Qdrant or model requests were made.
- Gateway KnowledgeProxy supports explicit operator-selected typed internal retrieval RPC through platform-rpc. It preserves exact signed HTTP path/body/authority headers; external REST status, JSON, retry-after and bounded RAG observations remain intact. Only POST `{dataset}/retrieve` selects RPC; batch, management and streaming routes retain their declared transports. SDK tools delegate this same forwarding path. Origin policy remains private-network opt-in and configured RPC failures never downgrade to HTTP.
- Compile/test execution remains serialized by the root; this entry records implementation, not an unexecuted verification claim.

## Reqwest 0.13 retry contract review (2026-10-15)

Installed official reqwest 0.13.5 `src/retry.rs` documents automatic safe protocol-NACK retries and its default retry budget. Gateway/integrations/native SDK worker/standalone overlay SDK engine builders now explicitly select `reqwest::retry::never()` wherever it was absent. Existing custom policies were not overwritten; application-owned connect-only retries, Qdrant resilience and SSE reconnect rules remain the sole retry authority. Foundation MCP/OAuth/catalog/provider/GenericREST builders already selected this policy and were left unchanged. Exact and aliased Client::builder source scan found no remaining implicit retry policy in these owned source trees. This is a source review; root owns compilation and execution. Overlay `engine.rs` and `tools.rs` changed after the prepared external composition, so its refresh is required before verification.

## Frozen packaging checkpoint (2026-10-15 15:58 UTC)

Parent-serialized verification: full workspace check 96 passed. Gateway test batch 98 passed the selected offline targets: admission metrics 2, auth database target 1 offline passed / 7 ignored, billing shutdown 4, event schema 3, SSE heartbeat 11, task authorization 4 ignored, runtime lifecycle 10, observer 3, uncertainty 4. Ignored SQL/Redis authority and billing receipt/replay tests compiled but were not run. No live infrastructure acceptance is claimed.

Current bounded billing implementation uses existing billing_events per-observation/stage receipts atomic with quota/day/hour mutations, deterministic failed-stage payloads and tenant-scoped guarded replay. Unknown commits and nontransactional trace stages remain manual reconciliation. This is not a full durable intake outbox or complete legacy BillingInterceptor callback/Redis DLQ/retry parity. Backlog: conservatively upgrade existing failed-receipt quarantine metadata if a later identical receipt reports an uncertain outcome (current INSERT preserves first receipt); preserve atomic duplicate-mutation suppression. Remaining untranslated historical tests are recovery material, not evidence of passed parity. Files frozen for source ZIP delivery; further bug fixes deferred by owner priority.

## Post-v126 bounded fix: failed billing receipt quarantine (2026-10-15 16:17 UTC)

Changed foundation `usage_recorder.rs` and `usage_stage_receipts.rs`, plus gateway `tests/gateway_migration_parity_auth_database.rs`. The old `ON CONFLICT DO NOTHING` retained a prior safe flag when the same receipt later reported an uncertain commit; this was established by source inspection, not an executed database reproduction. Reconciliation now inserts and row-locks the tenant/request/user-scoped receipt in one transaction, then applies a production metadata merge: uncertainty is sticky, replay permission can only decrease, and malformed flags fail closed. Original receipt payload/status and atomic applied-stage receipts are preserved. No schema, retries, callback flow, or delivered v126 ZIP changed.

Parent-serialized run 108: all three `quarantine_conflict_tests` passed. These exercise the actual production merge for safe→uncertain→safe, all sixteen boolean combinations/idempotency/order independence, and malformed/unrelated payloads. The new opt-in SQL conflict/replay test is not run and compilation remains pending at this checkpoint. It asserts one receipt, unchanged quota counters, preserved payload and rejected replay after quarantine. Database concurrency/transaction acceptance remains unverified; the pre-existing full-intake-outbox and manual trace reconciliation limitations remain.
