# AI 平台全项目多代理代码审查报告（只读）

**审查日期：** 2026-08-02
**审查模式：** 只读（无任何代码修改）
**审查方式：** 16 个并行审查代理 + 独立对抗性验证代理（共 50 个代理，1730 次工具调用，约 495 万 token）
**审查范围：** 全部微服务模块 —— 网关 `src/`、`assistant-service`、`knowledge-service`、`ai-gateway-core`、`mcp-docgen-server`、`tests/`、`scripts/`、`database/`、`deploy/`、`sdk/`、`web/`
**对应版本：** `main` @ 6337ef2（工作区含未提交的知识检索优化改动）

---

## 一、执行摘要

本次审查对全平台所有微服务的源码实现进行了逐文件只读审查。共报告 **281 个发现**，其中 **3 个 critical、35 个 high、128 个 medium、115 个 low**。对全部 critical/high 级发现（38 个）进行了独立对抗性验证：**24 个确认（CONFIRMED）、8 个部分确认（PARTIAL）、2 个反驳（REFUTED）、4 个未验证**。

核心结论：

| 级别 | 数量 | 代表问题 |
| --- | --- | --- |
| 🔴 Critical | 3 | API-key 作用域可自提权为 admin；两个「零断言测试」静默通过并禁用全局 TLS 校验 |
| 🟠 High | 35 | 匿名用户可触发付费 LLM 调用（quiz/RAGAS eval）；Confluence token 明文存储；跨租户 exam 报告 IDOR；exam/任务规划路径必然失败；docgen 生产部署生成骨架文档 |
| 🟡 Medium | 128 | 限流计数语义不一致、缓存无租户作用域、若干冗余复制代码（docx/pptx/xlsx 三份雷同） |
| ⚪ Low | 115 | 死代码、微测试、日志/错误掩盖、资源泄漏 |

**最需要立即处理（按风险×可达性）：**

1. **API-key 作用域 → admin 提权**（`api_keys.py:46`）— 已验证部分确认，若 schema 无 scopes 列即构成自提权链。
2. **匿名付费接口**：`quiz.py` 生成接口与 knowledge-service 的 RAGAS eval 接口均可被未认证用户触发付费 LLM 调用（成本放大/DoS）。
3. **`ASSISTANT_APP__ALLOW_ANONYMOUS=true` 静默关闭网关密钥中间件**（`main.py:682`）— 已确认，完全身份伪造。
4. **零断言测试**：`test_embedding.py`（含 `ssl._create_unverified_context` 全局副作用）、`test_knowledge_improvements.py`、`test_image_metadata_flag` — 伪绿灯。
5. **跨租户/跨用户数据泄漏面**：ToolOrchestrator 结果缓存、exam 报告、MCP tool 白名单、Confluence token 明文。

---

## 二、审查覆盖与统计

### 覆盖模块（16 个并行审查代理）

| 1 | `gateway-api` | ~65 |
| 2 | `gateway-core` | 59 |
| 3 | `gateway-services` | 75 |
| 4 | `gateway-infra` | 35 |
| 5 | `assistant-agent` | 26 |
| 6 | `assistant-tools` | 28 |
| 7 | `assistant-docgen` | 57 |
| 8 | `assistant-skills` | 44 |
| 9 | `assistant-mcp-memory-rag` | ~35 |
| 10 | `assistant-api` | 20 |
| 11 | `knowledge-service` | 39 |
| 12 | `ai-gateway-core-a` | 43 |
| 13 | `ai-gateway-core-b` | 52 |
| 14 | `mcp-docgen-server` | 78 |
| 15 | `tests-audit` | ~380 |
| 16 | `scripts-infra` | 78 |

**统计：** 281 findings = 3 critical + 35 high + 128 medium + 115 low

按类别分布：

| 类别 | 数量 | 类别 | 数量 |
| --- | --- | --- | --- |
| security | 71 | error-handling | 28 |
| logic-bug | 43 | implementation-bug | 28 |
| performance | 18 | test-gap | 18 |
| config | 17 | redundancy | 14 |
| concurrency | 14 | api-contract | 12 |
| resource-leak | 10 | code-quality | 8 |

### 验证方法

对每个 critical/high 发现，启动一个独立的对抗性验证代理，重读实际代码（含相关调用链），判定 CONFIRMED / PARTIAL / REFUTED 并给出证据。**验证中 2 个 high 级发现被反驳**：
- `rate_policy.py:59` RatePolicyResolver 短路 —— 经查是**有意的、有测试覆盖的行为**，非逻辑 bug（判定 REFUTED）。
- `unpack.py:54` Zip Slip —— 经查 CPython `zipfile.extractall` 自身会剥离 `..`/`.` 组件，**不构成 Zip Slip**（判定 REFUTED）。

---

## 三、各模块评估

### gateway-api（~65 文件）

The gateway-api module is a large, generally well-structured FastAPI surface (65 files, ~30k lines) with strong per-route capability/RBAC checks on the newer Agent/MCP/Quota/Eval surfaces, closed Pydantic schemas, and good redaction discipline on audit events. However, it has one critical self-service privilege-escalation path (API-key scopes become roles/permissions), several broken or under-protected legacy endpoints (Confluence dataset-access checks call a stub that returns None; quiz generation runs paid LLM calls for anonymous users with no rate limit), and a permissive auth middleware (anonymous_enabled=True) that places the entire burden of enforcement on individual routes, some of which do not enforce it. Several cross-service proxy boundaries (KB, LangGraph passthrough) rely on upstream enforcement and should be hardened. The findings below are ranked most-severe first.

### gateway-core（59 文件）

The gateway-core module is a solid, defense-conscious implementation (constant-time key compares, IP-spoofing protection that only trusts loopback by default, a comprehensive error-code hierarchy, and pure-ASGI streaming middleware to avoid buffering). However, there are two systemic weaknesses: (1) the identity layer trusts client-asserted guest/anonymous identities without server-side validation, enabling guest-rate-limit bypass and guest-identity spoofing that is forwarded to upstream services as X-User-Id; and (2) there are three separate rate-limit implementations with inconsistent accounting semantics, including Redis paths that count rejected requests and an in-memory path that never evicts keys. Middleware ordering in main.py correctly runs auth before rate-limiting, but the RatePolicyResolver short-circuits global/tenant/user limits whenever a service-level rule is configured, and quota checks default to fail_open, so the guard rails can silently drop out.

### gateway-services（75 文件）

The gateway-services tree is a mix of substantial service logic (billing/quota, eval/golden gates, LLM provider/model catalog, Confluence sync, metrics, session, task) and a growing set of thin back-compat shims delegating to ai_gateway_core. The highest-value defects are concrete and verified: a confirmed AttributeError in ConfluenceSyncService.remove_pages (self.kb vs self.knowledge_service) that silently leaves documents orphaned, and an eval-gate logic bug where critical_pass_rate=0.0 with no critical cases permanently fails the regression gate. Medium issues include missing provider_id scoping in model catalog sync (cross-provider metadata overwrite), an off-by-one daily quota reset that disagrees with QuotaService, non-deterministic consistent hashing via builtin hash(), a reversed pricing partial-match that misprices prefix models, and process-global DashScope base-url mutation. The code is generally defensive (widespread exception swallowing, best-effort writes), which masks several silent-partial-failure paths, and there is notable duplicated logic between the two quota-reset implementations and the two image-processing methods.

### gateway-infra（35 文件）

The gateway-infra module is structurally sound — SQL is properly parameterized (build_service_query uses $N placeholders with column-name validation, no injection found), JWT secret handling is guarded in production, and the LangGraph proxy shows careful intent around ownership checks and caching. However, the LangGraph adapter streaming path and session->thread mapping carry the most severe defects: assistant text is silently dropped when a streamed chunk mixes tool calls and content, session->thread mappings are unscoped by tenant/service and derived from client-supplied session_id without ownership verification (cross-tenant/IDOR risk, plus a check-then-act race and failed-thread caching), and token usage is summed cumulatively, inflating billing. The connectors add medium issues: the OpenAI stream swallows HTTP errors into empty successes, the ComfyUI websocket loop has no timeout (infinite hang), the gRPC connector is an unimplemented stub, and the message-queue connector leaks memory on ack and can republish empty payloads on cross-instance/restart failures. Model layers are plain dataclasses with no validation (risk of contract drift), and most provider adapters lack unit tests.

### assistant-agent（26 文件）

The streaming-first agent loop (agent_loop.py + assistant_service.py + tool_invoker.py) is well-structured: the main model/tool iteration is properly bounded by max_iterations, tool-call arguments are defensively parsed with NaN/Infinity rejection, and the turn-contract state machine adds a good audit layer. However the module has several high-impact defects: the legacy task-planning path (ToolOrchestrator via AssistantService.get_tool_orchestrator) is completely non-functional (every planned task fails with 'invocation_context is required') AND its result cache is keyed only on tool+params with no tenant/user/session scope on a long-lived shared singleton, enabling cross-tenant KB-result disclosure when enabled; auto-knowledge retrieval bypasses the tool-invoker authorization boundary; per-run token usage is overwritten each iteration so billing/metadata undercount; session user-message persistence races the assistant-message write so history can be persisted out of order; and sub-agent anonymous tool-call merging fragments argument deltas. Trace writer and orchestrator caches grow without bound. No true unbounded/infinite loops were found in the hot path.

### assistant-tools（28 文件）

The tools layer is generally well-engineered: strong SSRF guards (web_fetch delegates to a shared DNS-pinned safe fetcher), careful per-tenant Confluence credential resolution, hardened workspace path containment for the fs_* primitives, bounded/redacted public error text, and extensive test coverage (code_executor, web_fetch, confluence, primitives, tool_selector). The most serious issues are latent rather than currently-triggered: an unguarded path traversal in CodeExecutorService._setup_workspace (host file write via InputFile/KBDocument filename), HTML attribute-injection in the Confluence markdown-to-storage converter, and several authorization trust-boundary gaps (metadata-sourced tenant_id fallback, an unauthenticated execution_gateway_approved flag, an overridable memory_principal). Secondary concerns include a trivially-bypassable AST import denylist that overstates its defense, non-cancellable grep threads that can burn CPU past timeout, and unbounded memory reads of container logs/output files. Overall the module is security-conscious but relies on trust boundaries in request.metadata and on caller-provided filenames that should be hardened defensively.

### assistant-docgen（57 文件）

The docgen subsystem is well-structured (clean IR/renderer separation, pure layout rules, verified color/font math), but it is currently unintegrated: DocgenService and the sandbox backends are instantiated nowhere in production src, and the default skill path points at a nonexistent directory, so most of the quality/sandbox guarantees are latent rather than live. The most serious verified defects are a fix-loop bug that wipes every formula in an xlsx on any single #DIV/0, and a shell-interpolation bug in DockerSandbox that mutates or injects into user code containing $ or backticks; both would surface immediately once the subsystem is wired into the document tool. Lower-severity issues span storage path traversal and O(n) lookups, temp-dir leaks, a blocking fc-list probe in the async render path, unhardened docx/xlsx parsing, and several small verifier regex/coverage gaps. Zip-bomb/XXE risk is currently low because verifiers only parse renderer-produced artifacts, but should be hardened before any 'edit existing docx/xlsx' path is added.

### assistant-skills（44 文件）

The assistant-skills module is a mix of thin re-export shims (executor, parser, quiz, skill_create -> ai_gateway_core) and substantial self-contained code: the Office docx/pptx/xlsx toolkit (unpack/pack/validators/soffice/accept_changes), PPTX/PDF/XLSX scripts, the content generator/streaming writer/structured output, quality guardrails, audit logging, and a large prompt library. The highest-risk areas are the subprocess-driven office scripts: an LD_PRELOAD code-injection vector via predictable world-writable /tmp shim paths, subprocess timeout misreported as success in accept_changes, unbounded zip extraction of untrusted Office files, and a streaming-writer logic bug that re-triggers the same KB search on every chunk. The module is also heavily redundant — roughly 3,000 lines of office tooling are byte-identical across the three format trees, so security fixes must be applied in triplicate — and several quality/guardrail and context-injection code paths claim capabilities (PPT/XLS thresholds, reference-material grounding, context-based hallucination checks) that they do not implement.

### assistant-mcp-memory-rag（~35 文件）

The module is unusually defense-in-depth heavy for an AI platform: the MCP client pins DNS/IPs and blocks SSRF, OAuth uses strict PKCE/state/audience checks, memory writes are PII-redacted and owner-scoped, the execution gateway fences side-effecting tools behind durable command queues and single-use approvals, and context assembly treats all retrieved/user content as untrusted. No critical or high-severity defect was confirmed; the meaningful risks are cross-user cache leakage in the RAG retriever, unsigned identity forwarding in the remote assistant client, unvalidated MCP-supplied artifact URLs, and per-request re-indexing of the entire memory corpus on the hot path, plus a handful of lower-severity logic/redundancy/contract issues. The primary recommendation is to make the privacy/authorization guarantees self-contained at each boundary (cache keys, memory write gating, remote-client identity) rather than relying on the single current caller to enforce them.

### assistant-api（20 文件）

The assistant-service API layer is generally well-defended: gateway-secret HMAC middleware, session-ownership enforcement in the service layer (_ensure_session_exists), tenant/user-scoped run/approval/resume queries, SSRF-safe fetch/callback paths, and owner-scoped image artifacts are all present and correctly wired. The highest-severity issue is a configuration gap where ASSISTANT_APP__ALLOW_ANONYMOUS=true silently neutralizes the gateway-secret middleware (the exact H-4 impersonation the startup guard claims to close), plus a few tenant-isolation footguns in the image owner-scope computation and legacy session paths. Lower-severity concerns are error-masking (authz failures returned as 500), partial-failure ordering in session deletion, and minor fail-open/defaults in model listing.

### knowledge-service（39 文件）

The knowledge-service module is a large, carefully-built RAG service: SQL is consistently parameterized (no injection found), request schemas are heavily validated, and the hybrid retrieval pipeline (dense + BM25 + RRF + rerank + MMR) is genuinely sophisticated and mostly correct. The highest-risk issues are an unauthenticated, cost-amplifying RAGAS eval endpoint that accepts attacker-controlled LLM config, Confluence API tokens stored in plaintext and returned over the API, an untested fallback router with no dataset authorization, and several concrete implementation bugs (image_boost no-op, missing settings.confluence model crashing the image endpoint, a self.kb typo, an orphaned MMR task). The entire Confluence sync subsystem is also dead code — it is never wired into the app, so those SSRF-relevant paths are latent rather than reachable.

### ai-gateway-core-a（43 文件）

The module (persistence/comm/events/security/session/storage/proxy/config/tracing) is generally well-defended: repositories consistently use parameterized SQL ($N placeholders with allowlisted column names), the agent/MCP repos enforce tenant + ACL on every query, and safe_fetch implements DNS-pinned SSRF protection that correctly handles multi-record DNS, redirects, and IPv4-mapped IPv6 (verified httpcore 1.0.9 honors the sni_hostname extension). The most serious defect is in the Redis Streams consumer: the per-event_id idempotency claim is set before the handler and never released on failure, so the documented retry/DLQ machinery is dead code and failed events are silently dropped (at-least-once degenerates to at-most-once). Secondary issues include fail-closed 500s when the Redis-backed circuit breaker store is down, a sliding-window rate-limit TTL reset that can permanently block active users, non-transactional multi-statement user/role writes, an f-string SQL interpolation in lock_user_account, and a version-number race in create_document_version. Storage path-traversal guards are effective for the local backend.

### ai-gateway-core-b（52 文件）

The module is generally well-engineered: the agent runtime HMAC envelope, cleanup-plan receipt validation, gateway-secret replay protection, TaskManager lock ordering, and working-memory bound checks are carefully implemented and fail closed. The highest-risk defects are concentrated in billing/observability and quiz/exam authorization: _get_model_pricing returns the first (not longest) prefix match and nondeterministically misprices model variants; exam analysis reports are readable cross-tenant because list_reports/get_report skip the tenant scope every sibling endpoint enforces. A cluster of correctness gaps also exist in quiz public-attempt grading (short answers always wrong), the live-candidate release gate (can't pass without critical cases), and several env/unit-drift hazards in image callbacks and cost columns. Recommend prioritizing the two high findings (IDOR and pricing) plus the share-grading fix before release.

### mcp-docgen-server（78 文件）

The module is well-architected on paper — clean IR/planner/renderer separation, progressive-disclosure skills, HMAC-signed URLs with tenant scoping, and a two-backend sandbox with a timeout and scrubbed env. However, several high-impact defects undermine the production story: the recommended (SSE/k8s) deployment wires llm=None so LLM planning is silently disabled and produces skeleton docs; the artifact download URLs are broken in the shipped manifests because DOCGEN_PUBLIC_URL and a stable DOCGEN_ARTIFACT_SIGN_KEY are never configured (and the k8s run has 2 replicas with per-process random secrets); the FreshContextVisionCritic has an await bug that makes it silently no-op and is never wired into the verifier anyway; and the bundled skill scripts contain a Zip Slip via zipfile.extractall on attacker-controlled Office files. There is also massive duplication (the module is triplicated between this package and assistant-service, and 10 office scripts are byte-identical across docx/pptx/xlsx), plus blocking S3 calls in async paths and non-atomic store writes. With those fixed, the design is sound.

### tests-audit（~380 文件）

The suite is large (~380 files, ~116k lines) and unevenly strong: assistant-service runtime, eval traces, agent-studio migrations, and image flows have deep, high-quality coverage, but several critical gateway/admin route modules (invoke, roles, tenant_policies, quota business logic, tool_inventory, submit/stream) have zero unit tests, and a handful of files named test_* are print-only scripts or live-API probes with zero assertions that always pass (test_embedding.py, test_knowledge_improvements.py, and the discarded-comprehension test_image_metadata_flag), giving false green confidence. The CI and Makefile gates run only a focused subset, so these silent-pass tests ship unnoticed; the pytest gate of --cov-fail-under=25 is very low. Highest-value next tests: POST /invoke, roles CRUD, tenant tool-policy/mcp-config CRUD, quota forecast/block/unblock/reset, and the knowledge-service qa/batch/version/cleanup endpoints.

### scripts-infra（78 文件）

The scripts-infra module is generally solid: shell scripts are defensive (set -euo pipefail, placeholder-aware secret validation), the ~80 SQL migrations are idempotent (IF NOT EXISTS everywhere, verified by scan), the compose stack has healthchecks, dependency ordering, and explicit required secrets, and CI enforces config validation that currently passes. The main weaknesses are contract and silent-failure bugs: the Java SDK authenticates with the wrong header (Bearer instead of X-API-Key), the legacy database/cli.py migration runner silently skips whole migrations due to 3-digit version-prefix collisions (016/030/031, including a rollback file treated as forward), and backup.sh's failure cleanup is unreachable under pipefail. Secondary issues are validator/runtime drift (Redis password format), an unfinished Python MCP HTTP-transport call path, eval-based indirection, and secret hygiene gaps in the CLI's child-process environment. No critical platform-level security holes were found in the compose/Dockerfile/CI layer.

---

## 四、Critical / High 级发现（含验证裁决）

#### API-key creation accepts arbitrary scopes/tier -> self-privilege-escalation to admin

- **模块**: `gateway-api` ｜ **级别**: `CRITICAL` ｜ **类别**: `security` ｜ **验证**: ⚠️ 部分确认
- **位置**: `src/api/v1/api_keys.py:46`
- **问题**: CreateAPIKeyRequest.scopes is an unvalidated list[str] and tier is a free string. APIKeyService.create_api_key stores them verbatim; because the api_keys table has a 'permissions' column (database/schema.sql:126) and no 'scopes' column, the scope column resolves to 'permissions', so client scopes land in the permissions column. In deps.py get_user_context (lines 486-499) and get_auth_context (lines 739-749), key permissions are read from key_info['permissions'] and merged into roles ('for perm in key_permissions: if perm not in roles: roles.append(perm)'). An authenticated (any-role) user can POST /api/v1/api-keys with scopes=['admin'] (or 'admin:*'), then authenticate with that key to obtain the 'admin' role, tier 'admin' and full RBAC/backend access.
- **影响**: Any low-privilege authenticated user can grant themselves administrator privileges and access every admin endpoint (users, roles, quota, service config, model/provider config), bypassing the RBAC boundary entirely.
- **修复建议**: Validate scopes against a server-side allowlist (only knowledge:read/knowledge:write or a capability catalog) and pin tier to a fixed set; never merge permission strings into roles in deps.py (use a separate permissions check); require admin role to grant non-default scopes.
- **测试建议**: Add a test that a 'user'-role principal creating a key with scopes=['admin'] still gets only 'user' capabilities after re-auth, and that deps.get_auth_context never turns a permission string into a role.
- **验证证据**: Confirmed facts: (1) src/api/v1/api_keys.py:46-53 CreateAPIKeyRequest.scopes (list[str], unvalidated) and tier (free string); create endpoint (85-109) passes req.scopes (line 103) and req.tier (line 104) verbatim to APIKeyService.create_api_key, gated only by require_authenticated_user (28-40) which checks is_authenticated only — no admin/scope check. (2) REFUTED core mechanism: the reviewer claims "no 'scopes' column, so scope column resolves to 'permissions'". While base schema.sql:94-116 has

#### Live-API 'tests' with zero assertions always pass and disable TLS verification process-wide

- **模块**: `tests-audit` ｜ **级别**: `CRITICAL` ｜ **类别**: `test-gap` ｜ **验证**: ✅ 已确认
- **位置**: `tests/integration/embedding/test_embedding.py:12`
- **问题**: test_dashscope (line 12) and test_gemini (line 75) are top-level async test_* functions collected under python_functions=['test_*'] with asyncio_mode='auto'. Both print results and `return False` on any failure (missing key, HTTP error, exception), but pytest ignores return values, so the tests pass in every scenario. They make real outbound calls to DashScope/Gemini when env keys are present (slow/flaky in CI, costs money), and line 32 executes `ssl._create_default_https_context = ssl._create_unverified_context`, a process-global side effect that silently disables TLS verification for every other test in the same run. Lines 27/89 also print truncated API keys to stdout.
- **影响**: False green coverage of the embedding integration path, a globally-dangerous SSL mutation that can hide real TLS failures in unrelated tests, and nondeterministic CI (live network calls + real API keys).
- **修复建议**: Convert into a proper test: mock the provider clients via respx/AsyncMock and assert on returned embedding shape; never call live APIs inside a collected test_* function. Move the manual probe under tests/scripts or tools/ and require an explicit flag; remove the ssl context mutation entirely.
- **测试建议**: Write test_dashscope/test_gemini as unit tests asserting: (1) successful response -> embeddings list returned with expected dimensionality; (2) HTTP 4xx -> error surfaced/None; (3) missing config -> clean early-return None without raising and without touching ssl. Add an assertion that ssl._create_default_https_context is never replaced.

#### test_knowledge_improvements.py is a print-only script: all 6 test_* functions have zero assertions

- **模块**: `tests-audit` ｜ **级别**: `CRITICAL` ｜ **类别**: `test-gap` ｜ **验证**: ✅ 已确认
- **位置**: `tests/knowledge/test_knowledge_improvements.py:32`
- **问题**: test_token_counting (32), test_arabic_tokenization (62), test_multilingual_tokenization (89), test_language_weights (114), test_score_normalization (139), test_token_based_chunking (183) only call library code and print to stdout (e.g. '✓ Token counting test completed'). The module is collected by pytest (python_files=['test_*.py'], functions test_*) and every function passes regardless of whether the tokenizer/chunker behaves correctly. A main() at line 227 duplicates the same body for standalone execution.
- **影响**: The token-counting / Arabic tokenization / normalization / token-chunking logic — a core knowledge-service retrieval dependency — has zero regression protection; any breakage in TokenCounter, normalize_arabic, ScoreNormalization, or FixedSizeChunker goes unnoticed.
- **修复建议**: Replace print bodies with real assertions: assert exact token counts for known inputs, normalized-Arabic equality (e.g. diacritic/variation-letter folding), detect_language outputs, weight bounds (dense+bm25≈1, non-negative), min/max normalization boundaries (0..1), and that FixedSizeChunker chunks respect token_limit and never drop text.
- **测试建议**: Add deterministic fixtures: English 'Hello, how are you doing today?' -> expected tiktoken count; Arabic with diacritics vs without -> same normalized token set; ScoreNormalization.min_max maps min->0/max->1; chunker with token_limit=100 returns chunks each <=100 tokens. Delete the print-based main() or move it to a script.

#### Confluence endpoints call deprecated get_knowledge_service() stub (returns None) -> always 500

- **模块**: `gateway-api` ｜ **级别**: `HIGH` ｜ **类别**: `implementation-bug` ｜ **验证**: ✅ 已确认
- **位置**: `src/api/v1/confluence.py:388`
- **问题**: deps.get_knowledge_service (deps.py:65-68) is documented 'Deprecated: KB now runs as independent microservice' and returns None. confluence.py still calls it and awaits knowledge_svc.require_dataset_access(...) in create_space_binding (line 388-389), add_pages_to_binding (line 596-597) and import_from_url (line 661-662). Awaiting an attribute on None raises AttributeError, which the surrounding 'except Exception' converts to HTTP 500. These three endpoints are therefore permanently broken whenever GATEWAY_CONFLUENCE__ENABLED=true.
- **影响**: Core Confluence features (space binding, add pages, URL import) are unusable, and the intended dataset-access enforcement is silently absent (the feature fails rather than enforcing).
- **修复建议**: Remove the local require_dataset_access call and instead validate dataset access against the KB service (e.g., proxy the check via _proxy_utils.proxy_to_kb_service or have the KB service enforce it), or guard with the KB proxy.
- **测试建议**: Add a route test asserting create_space_binding/import_from_url do not crash and that dataset-access denial is enforced when the KB service rejects.

#### Quiz generation invokes paid LLM for anonymous users with no auth check and no rate limit

- **模块**: `gateway-api` ｜ **级别**: `HIGH` ｜ **类别**: `security` ｜ **验证**: ✅ 已确认
- **位置**: `src/api/v1/quiz.py:232`
- **问题**: POST /assistant/quiz/generate and /generate/stream call _get_quiz_service(user=user) and drive LLM calls through _AssistantServiceModelRegistry.chat -> assistant-service /api/v1/assistant/chat. The route never checks user.is_authenticated, and the global StreamingAuthMiddleware (main.py:261, anonymous_enabled=True) only injects identity without blocking. Anonymous users get UserContext(is_authenticated=False, tenant 'public') and still reach the LLM path. There is also no enforce_rate_limit call on either endpoint.
- **影响**: An unauthenticated attacker can submit unlimited quiz-generation requests, consuming paid model tokens/cost with no identity accountability or throttling (billing/DoS by cost).
- **修复建议**: Require authentication (mirror agent_runtime._require_actor) on generate/generate-stream, and apply enforce_rate_limit(request, user, operation='quiz_generate'). Also validate model_id against _check_model_permission as /assistant/chat does.
- **测试建议**: Assert unauthenticated quiz generate returns 401 and that repeated requests exceed the rate limit; verify model permission check for model_id override.

#### Guest session identity is client-asserted (format-only validation), enabling impersonation and guest rate-limit bypass

- **模块**: `gateway-core` ｜ **级别**: `HIGH` ｜ **类别**: `security` ｜ **验证**: ⚠️ 部分确认
- **位置**: `src/core/middleware/streaming.py:331`
- **问题**: StreamingAuthMiddleware._extract_user_info accepts any X-Guest-Session header value that is a UUID v4 or 'guest_{hex}' (validated only for format by _is_valid_session_id, lines 379-404); no server-side session store is consulted (guest_session_validator is never wired). The guest user_id is then (a) used as the StreamingRateLimitMiddleware guest bucket key (line 511: guest_key = user_id for user_type == 'guest'), and (b) forced into X-User-Id via ContextInjector LANGGRAPH_HEADER_MAPPINGS and forwarded to LangGraph. The same trust-any-session pattern exists in AuthMiddleware._authenticate_guest_session (auth.py line 312).
- **影响**: An attacker can mint arbitrary UUID guest IDs to rotate the guest rate-limit bucket (200/min) up to the IP cap, and can claim another guest's UUID to impersonate that guest upstream (LangGraph keys threads/history by X-User-Id). No server verification means this identity is not trustworthy.
- **修复建议**: Validate the guest session against the session manager before accepting it (or drop the header and fall back to anonymous); if a validator is unavailable, disable guest_session_enabled rather than trusting format-only IDs. Rate-limit guests by client IP only.
- **测试建议**: Add a test that submits two different X-Guest-Session UUIDs and asserts both are rejected when no session exists, and that the guest rate-limit bucket is not rotatable.
- **验证证据**: The core defect is real but the high-severity impersonation impact is overstated.

CONFIRMED (core mechanism, streaming.py): _extract_user_info (lines 320-341) accepts any X-Guest-Session value passing only the format check _is_valid_session_id (lines 379-404: UUID v4 or guest_{hex}) and returns user_id=guest_session, user_type="guest" (return at line 332; line 331 is the else: of that branch). No server-side session store is consulted. StreamingAuthConfig (lines 165-189) has no validator field,

#### RatePolicyResolver.resolve short-circuits all other rate-limit dimensions when a service config policy exists

- **模块**: `gateway-core` ｜ **级别**: `HIGH` ｜ **类别**: `logic-bug` ｜ **验证**: ❌ 已反驳
- **位置**: `src/core/gateway/rate_policy.py:59`
- **问题**: When service_config has rate_limit_enabled=True and positive requests/window, _service_config_policy returns a single policy and resolve() returns [service_policy] immediately (lines 53-60), skipping every DB rule for global/tenant/user/operation scopes.
- **影响**: Configuring a service-level rate limit silently disables the global, tenant, user, and operation rate limits for that service. An admin who enables a service limit to be more generous (or leaves a high default) unintentionally disables the system-wide protection, creating a DoS/abuse vector.
- **修复建议**: Return the service policy in addition to (not instead of) the other matched rules; at minimum keep the global rule always enforced.
- **测试建议**: Unit test resolve() with a service_config policy set and assert global/tenant/user rules are still emitted.
- **验证证据**: The code path described is real — src/core/gateway/rate_policy.py:53-60: resolve() calls _service_config_policy() and `if service_policy is not None: return [service_policy]`, so _load_rules (line 62) and all DB rules (global/tenant/user/operation) are skipped when a service config policy exists. But this is deliberate, tested behavior, not a logic bug. The repo's own test tests/core/test_gateway_rate_policy_resolver.py:43-65, test_service_level_rule_overrides_global_defaults, supplies a global

#### remove_pages references undefined self.kb attribute

- **模块**: `gateway-services` ｜ **级别**: `HIGH` ｜ **类别**: `implementation-bug` ｜ **验证**: ✅ 已确认
- **位置**: `src/services/knowledge/confluence/sync_service.py:2770`
- **问题**: In `remove_pages`, when delete_documents=True the code calls `await self.kb.delete_document(binding['dataset_id'], page['document_id'])`. The class attribute is `self.knowledge_service` (set in __init__ at line 95); `self.kb` is never assigned anywhere in the class. Every call raises AttributeError, which is swallowed by the `except Exception` block (line 2775-2776).
- **影响**: Bulk page removal with document deletion never actually deletes the knowledge-base documents: the AttributeError is logged and the page record is still removed, leaving orphaned documents and their vector embeddings in the dataset. `documents_deleted` is always reported as 0, and the ACL check performed just before is wasted.
- **修复建议**: Replace `self.kb.delete_document(...)` with `self.knowledge_service.delete_document(...)` (the correct attribute used everywhere else in this file).
- **测试建议**: Add a unit/integration test for remove_pages with delete_documents=True asserting knowledge_service.delete_document is called for each page and documents_deleted is incremented.

#### critical_pass_rate is 0.0 when no critical cases exist, failing the regression gate

- **模块**: `gateway-services` ｜ **级别**: `HIGH` ｜ **类别**: `logic-bug` ｜ **验证**: ✅ 已确认
- **位置**: `src/services/eval/golden.py:634`
- **问题**: `evaluate_cases` computes `critical_pass_rate = sum(1 for c in critical_cases if c['passed']) / max(len(critical_cases), 1)`. When the suite has zero critical cases, this yields 0.0 (not N/A). `apply_gate` then compares `critical_pass_rate < gate_thresholds['critical_pass_rate']` (default 1.0), so any regression suite with no `metadata.critical` cases is permanently marked gate=fail.
- **影响**: The eval regression gate used to approve harness/runtime changes always blocks when the case set contains no critical cases, even when every case passes. This produces false CI failures and forces manual overrides, undermining the gate.
- **修复建议**: When `len(critical_cases) == 0`, report critical_pass_rate as 1.0 (vacuous pass) or None and have apply_gate skip the critical check when no critical cases are present.
- **测试建议**: Add a test with a passing suite containing zero critical cases and assert apply_gate returns status=pass.

#### Remote stream extraction drops assistant text in chunks that also carry tool calls

- **模块**: `gateway-infra` ｜ **级别**: `HIGH` ｜ **类别**: `logic-bug` ｜ **验证**: ✅ 已确认
- **位置**: `src/adapters/langgraph.py:1031`
- **问题**: In `_extract_remote_stream_event`, for `messages/partial` events with msg_type 'ai'/'AIMessageChunk', the `tool_call_chunks` branch (line 919) and `tool_calls` branch (line 1040) both `return` before control can reach the AI-text handling at line 1235. When a single streamed chunk contains both tool-call args and text content (common when a model emits text alongside a tool call), the text delta is never yielded.
- **影响**: Assistant text emitted in the same chunk as a tool call is silently lost from the streaming response sent to the frontend; final answers that trail a tool call can be truncated or missing.
- **修复建议**: After handling tool_call_chunks/tool_calls, fall through to the text-delta logic instead of returning early, or merge text+tool-call emission into one StreamEvent so both are delivered.
- **测试建议**: Unit-test `_extract_remote_stream_event` with a synthetic AIMessageChunk carrying both `tool_call_chunks` and `content`; assert a TEXT_DELTA is emitted.

#### Session->thread mapping is unscoped by tenant/service and thread_id is derived from client-supplied session_id without ownership checks

- **模块**: `gateway-infra` ｜ **级别**: `HIGH` ｜ **类别**: `security` ｜ **验证**: ⚠️ 部分确认
- **位置**: `src/adapters/langgraph.py:1374`
- **问题**: `_ensure_thread` uses a class-level dict `_session_to_thread_map` keyed only by session_id and shared by every LangGraphAdapter instance in the process; the L2 Redis key is `lg:thread_map:{session_id}` (verified in ai_gateway_core.persistence.redis) also unscoped by tenant/service. If session_id is a valid UUID it is used directly as the thread_id (line 1374), so a caller who knows another user's session/thread UUID can address that thread. The adapter path performs no `_verify_ownership` equivalent before reusing or writing to the thread; it relies solely on the upstream accepting the forged `Bearer gateway-{user_id}` token.
- **影响**: Cross-tenant/cross-service session collision shares checkpoints (message bleed between tenants), and a client-controlled thread_id enables writing into another user's thread (IDOR) when upstream auth is permissive or absent. The class-level dict also grows without bound (never evicted).
- **修复建议**: Scope cache keys by (tenant_id, service_id, session_id); enforce server-side thread ownership lookup before reuse; never trust a client-supplied thread_id without verifying it; bound/evict the L1 dict.
- **测试建议**: Add tests asserting that two tenants sharing a session_id string get distinct thread mappings and that reusing another user's thread UUID is rejected.
- **验证证据**: The factual code observations in the claim are accurate, but the high-severity exploit is NOT reachable through the documented path.

TRUE observations:
- langgraph.py:62 defines `_session_to_thread_map: dict[str, str] = {}` as a class attribute shared by all LangGraphAdapter instances; keyed only by session_id (lines 1349, 1404). Never evicted (langgraph.py:1404) — a genuine unbounded memory-leak.
- langgraph.py:1373-1375: a valid-UUID session_id is used verbatim as the thread_id (`valid_thread

#### Cross-tenant result cache in shared ToolOrchestrator

- **模块**: `assistant-agent` ｜ **级别**: `HIGH` ｜ **类别**: `security` ｜ **验证**: ⚠️ 部分确认
- **位置**: `apps/assistant-service/src/assistant_service/core/tool_orchestrator.py:235`
- **问题**: ToolOrchestrator._result_cache is keyed only by md5(tool + JSON-params) (lines 468-486) with no tenant/user/session scope. AssistantService.get_tool_orchestrator() (assistant_service.py:893-901) caches ONE ToolOrchestrator instance on the long-lived service singleton, and execute_plan stores any successful result there regardless of identity. The cacheable set includes search_knowledge_base / search_documents / list_datasets / get_dataset_info, whose payloads contain tenant-private documents/dataset metadata.
- **影响**: When enable_task_planning=True, KB/doc search results from tenant A (including private document chunks) can be returned to tenant B issuing the same tool+arguments — a cross-tenant information disclosure; cached results are also reused across users and sessions of the same tenant.
- **修复建议**: Scope the cache key with tenant_id/user_id/session_id (like RegistryToolInvoker._cache_scope) or make ToolOrchestrator per-request; at minimum never cache results when no scoped invocation_context is present.
- **测试建议**: Add a test asserting that two different tenants' execute_plan calls with identical parameters do not share cached results.
- **验证证据**: The unscoped cache is real as a design flaw, but the claimed cross-tenant disclosure is NOT reachable in the current code. Confirmed: ToolOrchestrator._result_cache (tool_orchestrator.py:235) is keyed only by md5(tool|json(params)) (lines 468-475) with no tenant/user/session scope; the cacheable set (lines 241-248) includes search_knowledge_base/search_documents/list_datasets/get_dataset_info; successful results are stored regardless of identity (lines 560-561); and get_tool_orchestrator (assist

#### Task-planning execution path always fails (invocation_context required)

- **模块**: `assistant-agent` ｜ **级别**: `HIGH` ｜ **类别**: `implementation-bug` ｜ **验证**: ✅ 已确认
- **位置**: `apps/assistant-service/src/assistant_service/core/tool_orchestrator.py:524`
- **问题**: _execute_single_task raises ValueError('invocation_context is required...') whenever invocation_context is None (lines 524-528). AssistantService._execute_with_planning calls orchestrator.execute_plan(plan, working_memory) (assistant_service.py:971) without an invocation_context, and get_tool_orchestrator never sets _default_invocation_context. Every planned task therefore raises, is swallowed by _execute_parallel's exception handler, and is reported as a failed ToolExecutionResult.
- **影响**: config.enable_task_planning=True produces a plan where every task fails with 'invocation_context is required' and the user receives an all-FAILED task list — the feature is silently non-functional.
- **修复建议**: Build and pass a ToolInvocationContext (tenant/user/session/policy) into execute_plan from _execute_with_planning, or default _default_invocation_context from the request; otherwise fail fast at config time.
- **测试建议**: Unit test _execute_with_planning (or execute_plan with a stub invoker) asserting tool results are returned rather than 'invocation_context is required'.

#### Path traversal via untrusted input/KB document filenames writes to host outside the sandbox workspace

- **模块**: `assistant-tools` ｜ **级别**: `HIGH` ｜ **类别**: `security` ｜ **验证**: ⚠️ 部分确认
- **位置**: `apps/assistant-service/src/assistant_service/core/code_executor.py:512`
- **问题**: _setup_workspace writes user-supplied files with Path.join on the raw filename: `file_path = input_dir / input_file.filename` (line 512) and `doc_path = kb_docs_dir / kb_doc.filename` (line 517). pathlib appends an absolute filename (Path('/a') / '/b' == '/a/b') and resolves '..', so a filename like '../../../../tmp/pwned' or '../evil.txt' writes bytes outside the per-execution temp workspace on the HOST (under SANDBOX_WORKSPACE, default /opt/deploy/sandbox-workspace, or /tmp). No basename/component validation is performed on InputFile.filename or KBDocument.filename. Current in-tree callers (code_executor_tool.py:378) only pass `code`, so the tool path does not reach it today, but CodeExecutorService.execute() is a public service API whose contract accepts caller-controlled filenames (e.g. user-uploaded document names via KBDocument), and any caller that does pass them silently breaks the Docker-workspace isolation boundary.
- **影响**: Arbitrary file write on the host service user within any writable directory reachable via '..' from the temp workspace, escaping the intended per-execution sandbox directory. Compromises the sandbox isolation guarantee that this module exists to provide.
- **修复建议**: Sanitize filenames before writing: take only the basename (Path(filename).name), reject any name containing os.sep, '..', or a NUL byte; ideally also strip control characters. Apply the same guard to both input files and KB documents, and validate before mkdir/write.
- **测试建议**: Add a test that calls _setup_workspace with InputFile.from_text('../escape.txt', ...) and an absolute-path filename and asserts the file lands inside the workspace tree and no sibling/parent file is created.
- **验证证据**: The traversal flaw is real in code_executor.py: `_setup_workspace` does `file_path = input_dir / input_file.filename` (line 512) and `doc_path = kb_docs_dir / kb_doc.filename` (line 517) with no basename/component validation, where input_dir/kb_docs_dir are host directories under a per-execution mkdtemp in SANDBOX_WORKSPACE (line 492-499). Empirically verified: Path('.../code_exec_xxx/input') / '../../../../tmp/pwned' preserves the '..' components, so write_bytes/write_text lets the OS escape th

#### _patch_xlsx wipes every formula in the workbook when any single cell is #DIV/0

- **模块**: `assistant-docgen` ｜ **级别**: `HIGH` ｜ **类别**: `logic-bug` ｜ **验证**: ✅ 已确认
- **位置**: `apps/assistant-service/src/assistant_service/core/docgen/quality/verifier_pipeline.py:176`
- **问题**: In the fix-and-verify loop, `_patch_xlsx` builds `error_targets` from all FORMULA_ERROR issue messages (each message names one cell, e.g. 'formula error at Sheet1!C5=#DIV/0!'), then applies `if cell.formula and any("DIV" in m for m in error_targets)` to EVERY cell in EVERY sheet. The condition is true as soon as one flagged cell anywhere is a DIV error, so every correct formula in the whole workbook is nulled and set to 0. It never targets the specific failing cell; it never matches the other error markers (#REF!, #VALUE!, etc.).
- **影响**: On the first fix round for any xlsx whose LibreOffice recalc surfaces a single division-by-zero, the entire workbook is silently destroyed (all formulas replaced by the literal 0), producing a corrupted artifact that is then re-verified and shipped.
- **修复建议**: Parse the failing coordinates from the messages (`re.search(r'formula error at (.*?)=(.+)$')`) and only clear `formula`/set `value=0` for the exact `(sheet, row, col)` flagged, and only for the marker actually detected. Better: recompute/re-prompt an LLM for a corrected formula instead of nulling.
- **测试建议**: Add a unit test: workbook with 2 sheets, one #DIV/0 in sheet A cell and a correct formula in sheet B; assert the sheet B formula survives the patch and only the flagged cell changes.

#### DockerSandbox exec_python/exec_node shell-injects unescaped $ and backticks into the code payload

- **模块**: `assistant-docgen` ｜ **级别**: `HIGH` ｜ **类别**: `security` ｜ **验证**: ✅ 已确认
- **位置**: `apps/assistant-service/src/assistant_service/core/docgen/sandbox/docker_backend.py:127`
- **问题**: exec_python builds `shell = f"python3 -c {json.dumps(code)}"` and passes it to `bash -lc ...` inside the container. `json.dumps` only escapes quotes/backslashes/control chars — it does NOT escape `$` or backticks. Because the payload is wrapped in double quotes, bash performs variable expansion and command substitution on the user code before python ever sees it. Any code containing `$VAR`, `$(...)`, or backticks is corrupted or executes arbitrary commands inside the sandbox.
- **影响**: User/LLM-supplied script code is silently mutated (e.g. a regex `$` or an f-string with `$` is destroyed), and a code string containing backticks or `$()` runs arbitrary shell commands inside the container, breaking the 'code is passed verbatim' contract of the sandbox. Same bug in exec_node (line 140).
- **修复建议**: Do not interpolate code into a shell string. Write the code to a file in the workdir and run `python3 /work/script.py` (argv, no shell), or use `--` plus a properly escaped arg, or base64-encode the payload and decode inside the container.
- **测试建议**: Add a test asserting `exec_python` with code containing `$HOME` and backticks returns the literal string unexpanded; also that `echo`/`id` command substitution does not execute.

#### LD_PRELOAD code injection via predictable /tmp shim paths

- **模块**: `assistant-skills` ｜ **级别**: `HIGH` ｜ **类别**: `security` ｜ **验证**: ✅ 已确认
- **位置**: `apps/assistant-service/src/assistant_service/core/skills/docx/scripts/office/soffice.py:53`
- **问题**: _ensure_shim() writes lo_socket_shim.c to a fixed, world-writable temp path and compiles it with gcc; if /tmp/lo_socket_shim.so already exists it is trusted and returned as-is, and get_soffice_env() then sets LD_PRELOAD to it for every soffice subprocess. Any local actor (or another tenant in a shared container) who can write /tmp can pre-place a malicious .so/.c and get arbitrary code executed as the service user inside the soffice subprocess. No ownership/hash/integrity check on the existing file.
- **影响**: Local privilege escalation / arbitrary code execution in the assistant service's subprocess, triggered by simply running any office skill.
- **修复建议**: Use a per-process unique temp dir (tempfile.mkdtemp) with 0700 perms, verify the shim by checking it was created this run, and avoid LD_PRELOAD of anything not freshly compiled by this process. Also compile only if gcc is present and never trust a pre-existing file.
- **测试建议**: Add a test asserting the shim path is not a shared predictable path and that an attacker pre-created .so is not loaded.

#### LibreOffice subprocess timeout reported as success

- **模块**: `assistant-skills` ｜ **级别**: `HIGH` ｜ **类别**: `logic-bug` ｜ **验证**: ✅ 已确认
- **位置**: `apps/assistant-service/src/assistant_service/core/skills/docx/scripts/accept_changes.py:76`
- **问题**: accept_changes() runs soffice with timeout=30; on subprocess.TimeoutExpired it returns 'Successfully accepted all tracked changes'. The child is killed mid-save so the output docx may be truncated/corrupt, yet the caller sees success. Additionally the shared fixed profile /tmp/libreoffice_docx_profile causes concurrent invocations to contend on the LibreOffice profile lock.
- **影响**: Corrupted/partial documents delivered to users as successfully processed; concurrent calls fail spuriously.
- **修复建议**: On timeout return an error ('LibreOffice timed out; output may be incomplete'); use a per-request UserInstallation dir instead of a fixed shared path.
- **测试建议**: Unit-test the TimeoutExpired path to assert an error return; integration test two concurrent accept_changes calls.

#### Verification trigger stays in buffer, re-triggering searches every stream chunk

- **模块**: `assistant-skills` ｜ **级别**: `HIGH` ｜ **类别**: `logic-bug` ｜ **验证**: ✅ 已确认
- **位置**: `apps/assistant-service/src/assistant_service/core/content/streaming_writer.py:338`
- **问题**: After a trigger is found, `buffer = buffer[position:]` keeps the trigger phrase at the front of the buffer. Every subsequent delta chunk re-detects the same trigger at position 0, re-runs the KB search for the same query, and never shrinks the buffer, so text after a trigger is not streamed until the generator ends (where the whole tail is dumped at once). Also _extract_verification_query uses rfind over the full text, extracting context from the last (wrong) occurrence.
- **影响**: Core write-while-search feature is broken: one trigger causes unbounded repeated KB searches (cost/latency) and the streamed tail collapses into a single final chunk.
- **修复建议**: Drop the consumed trigger: `buffer = buffer[position + len(trigger):]`, and search within the buffer region rather than rfind over total_text.
- **测试建议**: Unit test with a multi-chunk stream containing one trigger; assert exactly one search_start and that trailing text is streamed incrementally.

#### ASSISTANT_APP__ALLOW_ANONYMOUS=true silently neutralizes the gateway-secret middleware (full identity impersonation)

- **模块**: `assistant-api` ｜ **级别**: `HIGH` ｜ **类别**: `config` ｜ **验证**: ✅ 已确认
- **位置**: `apps/assistant-service/src/assistant_service/main.py:682`
- **问题**: The startup guard (main.py 667-696) only refuses to start for the combo 'secret unset AND allow_anonymous=false'. The equally broken combo 'secret SET AND allow_anonymous=true' is allowed: GatewaySecretAuthMiddleware (ai_gateway_core/auth/gateway_secret_middleware.py lines 55-60) lets any request WITHOUT the X-Gateway-Secret header pass straight through when allow_anonymous=true, and get_user_context (user_context.py lines 64-71, 83-102) trusts X-User-Id/X-Tenant-Id/X-User-Tier/X-User-Type/X-User-Roles verbatim for any request that supplies them. Result: any caller on the docker bridge can omit the header and forge headers to impersonate any user, any tenant, or even role=admin / user_type=system — exactly the H-4 sibling-container impersonation the middleware was added to close.
- **影响**: Full authentication bypass and identity spoofing (tenant crossing, admin impersonation, approval/run access) whenever allow_anonymous is set true while the shared secret is configured. Only a log line warns; nothing refuses.
- **修复建议**: Refuse to start when GATEWAY_ASSISTANT_SHARED_SECRET is set AND ASSISTANT_APP__ALLOW_ANONYMOUS=true (mirror the existing RuntimeError), or add an explicit require_anonymous_false check in prod; alternatively make the middleware, when allow_anonymous=true, still require a valid header for any request that carries X-User-* identity headers.
- **测试建议**: Add a startup test asserting the app raises when both secret and allow_anonymous are set, and a middleware test proving a headerless request with forged X-User-Id/X-User-Roles is rejected when allow_anonymous=false.

#### Unauthenticated RAGAS eval endpoint allows arbitrary LLM calls / cost amplification

- **模块**: `knowledge-service` ｜ **级别**: `HIGH` ｜ **类别**: `security` ｜ **验证**: ✅ 已确认
- **位置**: `apps/knowledge-service/src/knowledge_service/api/routes/eval.py:51`
- **问题**: POST /api/v1/internal/eval/ragas has no user-auth dependency (only get_settings). It accepts a fully client-controlled llm_config (provider, model, api_key, base_url). When api_key/base_url are omitted, it falls back to the service's env keys (LLM_API_KEY, settings.embeddings key) and fires paid LLM and embedding calls. An attacker-supplied base_url also makes the server POST to an arbitrary host (server-side request, with no allowlist).
- **影响**: Any caller reaching this route (in dev with allow_anonymous=true; or any gateway-authenticated user in prod) can burn the tenant's LLM/embedding budget and trigger outbound requests to attacker-chosen endpoints. No rate limit, no cost cap, no user identity check.
- **修复建议**: Require an authenticated user dependency; validate llm_config so base_url/api_key cannot be supplied by callers (or restrict to a configured allowlist of providers/models); add a per-user rate limit and max-context budget.
- **测试建议**: Test that an unauthenticated request gets 401/403; test that a caller-supplied base_url is rejected; test that cost-capping limits fire.

#### Confluence API token stored plaintext and returned to clients

- **模块**: `knowledge-service` ｜ **级别**: `HIGH` ｜ **类别**: `security` ｜ **验证**: ✅ 已确认
- **位置**: `apps/knowledge-service/src/knowledge_service/persistence/database.py:4217`
- **问题**: save_confluence_connection persists api_token verbatim; get/list_confluence_connections SELECT * (including api_token). The crypto.encrypt_value() helper exists but is never used for this credential, and sync_service.list_connections/get_connection return the raw token to the API caller.
- **影响**: The Confluence API token (a broad credential with read access to the tenant's Confluence) is exposed at rest in PostgreSQL and over the API, enabling lateral access to Confluence content if DB or an API response leaks.
- **修复建议**: Encrypt api_token with encrypt_value() before storing and decrypt only inside the sync client; redact api_token from all list/get responses.
- **测试建议**: Assert DB row does not contain plaintext token; assert list_connections response excludes api_token.

#### Idempotency claim on event_id is never released on handler failure, defeating retry and DLQ

- **模块**: `ai-gateway-core-a` ｜ **级别**: `HIGH` ｜ **类别**: `logic-bug` ｜ **验证**: ✅ 已确认
- **位置**: `packages/ai-gateway-core/src/ai_gateway_core/events/consumer.py:271`
- **问题**: _handle_one calls idem.claim(consumer, event_id) (SET-NX-EX) before running the handler and never deletes the key on failure. _on_handler_failure leaves the message pending (no XACK), but XREADGROUP uses '>' which never redelivers the consumer's own pending entries, and _reclaim_pending (XAUTOCLAIM) runs only once at startup. On a later redelivery (e.g. process restart with a stable consumer name like pod hostname), claim returns False, so the handler is skipped and the message is ACKed and HDEL'd. attempts stays 1, so the attempts >= MAX_DELIVERIES (DLQ) branch is unreachable. A crash between claim and XACK is also treated as already-processed.
- **影响**: At-least-once delivery becomes at-most-once with silent event loss in the common single-worker deployment; transient handler failures lose billing/usage events with no retry and no DLQ entry.
- **修复建议**: Delete the idempotency key in _on_handler_failure and on any non-ACK path so redelivery re-runs the handler, or key the dedupe on the stream message_id (only ACKed messages count as seen). Also reclaim pending entries periodically, not only at startup.
- **测试建议**: Add a test where the handler raises once and assert the same consumer retries the message and promotes to DLQ after MAX_DELIVERIES.

#### Cross-tenant IDOR on exam analysis reports (list_reports/get_report have no tenant scope)

- **模块**: `ai-gateway-core-b` ｜ **级别**: `HIGH` ｜ **类别**: `security` ｜ **验证**: ✅ 已确认
- **位置**: `packages/ai-gateway-core/src/ai_gateway_core/quiz/exam_service.py:514`
- **问题**: ExamService.list_reports(exam_id) and get_report(report_id) query exam_analysis_reports with no tenant filter, and get_report never verifies the report's exam belongs to the caller's tenant. The route (src/api/v1/exams.py:285-312) only cross-checks report['exam_id'] == path exam_id and runs a generic 'admin' role check; every sibling route (get_exam/list_attempts/stats) correctly passes user.tenant_id into the service, but these two do not.
- **影响**: An admin of tenant A who obtains another tenant's exam_id/report_id (leaked via logs, shared links, or enumeration) can read that tenant's AI analysis reports containing student names, scores, and per-question breakdowns - a tenant-isolation violation despite the admin-only gate.
- **修复建议**: Add tenant_id to both queries, e.g. 'WHERE exam_id=$1 AND EXISTS (SELECT 1 FROM exams e WHERE e.id=exam_id AND e.tenant_id=$2)', and have get_report verify exam tenant membership before returning.
- **测试建议**: Add a cross-tenant test: user of tenant A calling get_report/list_reports for tenant B's exam must 404, mirroring the existing get_exam tenant tests.

#### _get_model_pricing partial match returns FIRST prefix hit, not the longest — misprices model variants

- **模块**: `ai-gateway-core-b` ｜ **级别**: `HIGH` ｜ **类别**: `logic-bug` ｜ **验证**: ✅ 已确认
- **位置**: `packages/ai-gateway-core/src/ai_gateway_core/metrics/usage_recorder.py:519`
- **问题**: The partial-match loop 'if model.startswith(cached_model) or cached_model.startswith(model)' returns the first dict entry that satisfies either prefix, with no longest-prefix scoring and with iteration order depending on DB row order (SELECT without ORDER BY). With both 'gpt-4o' and 'gpt-4o-mini' in the cache, a variant like 'gpt-4o-mini-2024-07-18' is billed at gpt-4o rates (0.0025 vs 0.00015 per 1K input) whenever 'gpt-4o' iterates first. The sibling helper resolve_pricing_with_status in pricing_catalog.py correctly scores by len(known_model) and picks the longest match; this method re-implements it incorrectly.
- **影响**: Nondeterministic over-billing of model variants (up to ~16x on input tokens for gpt-4o-mini), directly corrupting usage_records cost, user_quotas, and daily/hourly aggregates that feed the billing dashboard.
- **修复建议**: Replace the partial-match loop with the same longest-prefix scoring used by resolve_pricing_with_status (track max score) or delegate to resolve_pricing_with_status entirely.
- **测试建议**: Unit test _get_model_pricing with both 'gpt-4o' and 'gpt-4o-mini' in cache for input 'gpt-4o-mini-2024-07-18' asserting min pricing; run it against both cache orders.

#### Artifact download links are broken in the shipped production deploy (per-process random HMAC secret + 127.0.0.1 public URL)

- **模块**: `mcp-docgen-server` ｜ **级别**: `HIGH` ｜ **类别**: `config` ｜ **验证**: ✅ 已确认
- **位置**: `packages/mcp-docgen-server/src/docgen/storage/signing.py:20`
- **问题**: The HMAC signing secret is `os.environ.get("DOCGEN_ARTIFACT_SIGN_KEY") or secrets.token_urlsafe(32)` evaluated once at import. The k8s deployment runs `replicas: 2` (deploy/k8s/deployment.yaml) and the secret env is never configured, so each pod mints its own random key: a signed URL produced by pod A is rejected (403) by pod B under the round-robin Service. Independently, `DOCGEN_PUBLIC_URL` is never set, so both main_sse and main_http fall back to `http://127.0.0.1:{port}` (server.py lines 353-356 / 454-457) — the download_url returned to clients points at localhost inside the container and is unreachable by real users. The generate_document tool's primary output (signed download link) is therefore broken in every provided production manifest.
- **影响**: ~50% of artifact downloads return 403 with 2 replicas; all download URLs are unreachable by clients unless DOCGEN_PUBLIC_URL and DOCGEN_ARTIFACT_SIGN_KEY are manually set. Feature-level breakage of the artifact delivery path.
- **修复建议**: Set DOCGEN_ARTIFACT_SIGN_KEY from a stable secret in both deploy manifests and the k8s secret; set DOCGEN_PUBLIC_URL to the ingress host. Document both as required env in README/deploy.
- **测试建议**: Add a test that signs with one store instance and verifies with another instance sharing the same secret, plus an integration test that hits the /artifacts route with the public URL.

#### SSE transport (the recommended production transport) builds DocgenService with llm=None, silently disabling LLM planning

- **模块**: `mcp-docgen-server` ｜ **级别**: `HIGH` ｜ **类别**: `implementation-bug` ｜ **验证**: ⚠️ 部分确认
- **位置**: `packages/mcp-docgen-server/src/mcp_docgen_server/server.py:368`
- **问题**: main_sse (line 368) constructs `DocgenService(artifact_store=store, llm=None)`, and both docker-compose and the k8s deployment set `MCP_TRANSPORT: sse`. With llm=None every planner falls back to the deterministic template path, so all generated documents in production are skeleton decks/docs — exactly the degraded output the tool description warns about. Meanwhile `SamplingLLMCaller` (lines 59-102), which was written to delegate planning to the MCP client via sampling, is never instantiated anywhere (dead code). main_http uses build_default_llm() (DashScope, needs DASHSCOPE_API_KEY), but the k8s secret reference lists only ANTHROPIC_API_KEY / AWS keys, so the http path also degrades unless the key is added.
- **影响**: Production output quality drops to template-level in the documented deployment; users get 'successful' tool calls that produce empty 3-slide decks. The MCP sampling delegation feature is dead code.
- **修复建议**: Wire SamplingLLMCaller into the service for the SSE/http paths (pass the request session into _run_generate), or set llm=build_default_llm() in main_sse and add DASHSCOPE_API_KEY to the deploy manifests. Remove or implement the sampling adapter.
- **测试建议**: Add a test asserting main_sse/main_http services have a non-None llm, and an e2e SSE test that generates a docx and verifies used_llm is surfaced.
- **验证证据**: Core defect is REAL, but the severity/impact is overstated for the primary production deployment.

CONFIRMED facts:
- server.py:368: `main_sse` constructs `service = DocgenService(artifact_store=store, llm=None)` — exactly as claimed.
- `SamplingLLMCaller` (server.py:59-102) is defined but never instantiated anywhere in the repo (grep across all *.py finds only the class definition). It is dead code, and no MCP sampling handler is registered on the Server, so the SSE path has no way to enable LL

#### FreshContextVisionCritic never awaits messages.create on the AsyncAnthropic client — the production vision critic silently no-ops

- **模块**: `mcp-docgen-server` ｜ **级别**: `HIGH` ｜ **类别**: `implementation-bug` ｜ **验证**: ⚠️ 部分确认
- **位置**: `packages/mcp-docgen-server/src/docgen/quality/visual_verifier.py:84`
- **问题**: `default_vision_critic()` returns `FreshContextVisionCritic(client_factory=lambda: AsyncAnthropic())`, but `review()` (synchronous) calls `client.messages.create(...)` without awaiting (line 84) and then reads `resp.content[0].text` (line 90). `resp` is a coroutine, so `.content` raises AttributeError; the broad `except Exception` at line 92 swallows it and returns a single INFO 'critic invocation failed' issue. Because only CRITICAL issues flip the report to failed (PptxPdfVisualVerifier.verify line 250-253), the verify loop reports passed=True with zero real review. Additionally `default_vision_critic()` is never wired anywhere — `VerifierPipeline.__init__` (verifier_pipeline.py line 57) defaults to `StructuralVisionCritic()`, so even a fixed critic would be unused. The entire vision-QA feature is effectively dead.
- **影响**: The 'render to image → fresh-context vision critic' QA stage described in the module docs never runs in any configuration; defects go unchecked and the ANTHROPIC_API_KEY is silently ignored.
- **修复建议**: Make review() async and await the create() call (or use the sync Anthropic client), and default VerifierPipeline / DocgenService to default_vision_critic() when a critic is not supplied.
- **测试建议**: Add a test with a fake client_factory whose messages.create returns a coroutine and assert review() returns real parsed issues; add a test that DocgenService wires a vision critic by default.
- **验证证据**: The missing-await defect is real and the line number is exact. In visual_verifier.py, FreshContextVisionCritic.review() is synchronous (line 68) and calls client.messages.create(...) (line 84) on an AsyncAnthropic client from the factory (line 141, returns AsyncAnthropic()). The un-awaited call yields a coroutine; line 90 `resp.content[0].text` raises AttributeError, caught by the broad `except Exception` at line 92, which returns a single INFO 'critic invocation failed' Issue (line 93). Since v

#### Zip Slip: skill scripts extract attacker-controlled Office files with zipfile.extractall()

- **模块**: `mcp-docgen-server` ｜ **级别**: `HIGH` ｜ **类别**: `security` ｜ **验证**: ❌ 已反驳
- **位置**: `packages/mcp-docgen-server/src/docgen/_skills_data/docx/scripts/office/unpack.py:54`
- **问题**: `unpack()` (and `validate.py` line 74, `validators/base.py` lines 801-802, `validators/redlining.py` lines 63-64) call `ZipFile.extractall(output_path)` on a user/agent-supplied .docx/.pptx/.xlsx without validating member names. A crafted archive with entries like `../../foo` writes files outside the target directory (Zip Slip). These scripts are the sandbox payloads that the parent agent runs against uploaded documents, so a malicious upload can write arbitrary files anywhere inside the sandbox container, escaping the intended /work scratch dir. The same vulnerable files are duplicated verbatim under pptx/ and xlsx/ and again under apps/assistant-service/src/assistant_service/core/skills/.
- **影响**: Arbitrary file write inside the sandbox container (sandbox-escape from the workdir); can plant/modify files that later steps read (e.g., LibreOffice profile, script inputs) or overwrite other tenants' work in shared volumes.
- **修复建议**: Extract member-by-member, skipping any name that is absolute or contains '..' after normalising (e.g. `name = Path(member).name`), or use a safe helper that resolves `(outdir / name)` and checks it stays under outdir.
- **测试建议**: Add a unit test crafting a zip with a `../escape` entry and assert no file is written outside the extraction root.
- **验证证据**: The claimed Zip Slip does not exist. The code at packages/mcp-docgen-server/src/docgen/_skills_data/docx/scripts/office/unpack.py:54 (`zf.extractall(output_path)`), validate.py:74, validators/base.py:802, and validators/redlining.py:64 all use CPython's stdlib zipfile, whose `_extract_member` strips `os.path.pardir` (`..`), `os.path.curdir` (`.`), and empty components from each arcname (`invalid_path_parts = ('', os.path.curdir, os.path.pardir)`), then applies `os.path.normpath` before joining t

#### test_image_metadata_flag discards its result: no assertion, always passes

- **模块**: `tests-audit` ｜ **级别**: `HIGH` ｜ **类别**: `test-gap` ｜ **验证**: ✅ 已确认
- **位置**: `tests/services/test_chunking.py:605`
- **问题**: In test_image_metadata_flag (line 594) the body is `[c for c in chunks if c.metadata.get("has_image")]` — a list comprehension whose value is thrown away. No assert verifies any chunk actually carries has_image. The following comment even concedes the flag 'is not guaranteed' for structured documents, meaning the test can silently pass while the feature is entirely broken.
- **影响**: The image-chunk metadata flag (consumed downstream for multimodal retrieval filtering) is untested; a regression that drops has_image from all chunks would not fail this test.
- **修复建议**: Assert `any(c.metadata.get("has_image") for c in chunks)` on a text document containing an embedded [Image] marker, or, if the flag genuinely only applies to certain chunkers, assert the exact empty result and document why.
- **测试建议**: Make the assertion explicit and cover both branches: (1) SAMPLE_WITH_IMAGES yields >=1 chunk with has_image True; (2) a plain text sample yields zero has_image chunks. Parameterize over the chunker that owns the flag.

#### POST /invoke — the core gateway invocation endpoint — has zero unit tests

- **模块**: `tests-audit` ｜ **级别**: `HIGH` ｜ **类别**: `test-gap` ｜ **验证**: ✅ 已确认
- **位置**: `src/api/v1/invoke.py:1`
- **问题**: The module src/api/v1/invoke.py is referenced by no test file (grep for api.v1.invoke and '/invoke' returns nothing across tests/). POST /invoke is the primary external entry point that routes a unified request to a service, resolves auth/RBAC, and returns a response; its happy path, error mapping, service-not-found, permission-denied, and streaming-variant behaviors are completely unprotected.
- **影响**: A regression in the most important public endpoint would go unnoticed by the suite.
- **修复建议**: Add tests/api/test_invoke.py using the existing conftest pattern: build a minimal FastAPI app including invoke.router, override get_user_context/get_auth_context, inject a mock registry/dispatcher. Cover: (1) happy path returns service response with correct status; (2) unknown service_id -> 404/400 contract code; (3) permission-denied -> 403 with required capability detail; (4) service raising QuotaExceededError -> 429; (5) input validation failure -> 422.
- **测试建议**: Use httpx ASGITransport + AsyncClient and assert exact JSON contract fields (code, request_id). Add a parametrized matrix of the accepted service_id patterns if the route dispatches on a prefix.

#### Roles management API (7 routes: RBAC admin surface) has zero tests

- **模块**: `tests-audit` ｜ **级别**: `HIGH` ｜ **类别**: `test-gap` ｜ **验证**: 未验证
- **位置**: `src/api/v1/roles.py:89`
- **问题**: src/api/v1/roles.py exposes list_roles, create_role, list_permissions, get_role, update_role, delete_role, get_role_users (lines 89-327). No test file references api.v1.roles or '/roles' routes. Roles underpin the entire authorization model; the only RBAC tests (tests/core/auth/test_rbac.py) cover the pure RBAC class, not the HTTP role-management contract (permission assignment, role->permission mapping, delete-with-users semantics).
- **影响**: Admin-facing role management — a security-critical surface — has no regression protection; a permission-overwrite or role-deletion bug would be invisible.
- **修复建议**: Add tests/api/test_roles_api.py: override the RBAC/db dependency, then assert: (1) create_role persists role+permissions and returns 201; (2) get_role returns the merged permission set; (3) update_role replaces permissions atomically (no partial write on validation failure); (4) delete_role on a role still assigned to users fails with a clear error; (5) list_permissions returns the canonical permission catalog; (6) each route rejects a caller lacking admin capability with 403 (mirror test_management_api_authorization.py pattern).
- **测试建议**: Drive via TestClient with dependency_overrides, asserting status codes AND response_model field names to lock the API contract. Parametrize the 403 rejection check over all 7 routes.

#### Tenant tool-policy / MCP-config / audit-log API (10 routes) has zero tests

- **模块**: `tests-audit` ｜ **级别**: `HIGH` ｜ **类别**: `test-gap` ｜ **验证**: 未验证
- **位置**: `src/api/v1/tenant_policies.py:1`
- **问题**: src/api/v1/tenant_policies.py implements GET/PUT/DELETE /tool-policies, /tool-policies/{tenant_id}, /mcp-configs, /mcp-configs/{tenant_id} and /audit-log. No test file references the module or the route paths. These routes directly control which tools/MCP servers tenants may use — a security-sensitive policy surface with tenant-scoping, and the audit-log endpoint reports who changed policies.
- **影响**: Tenant isolation and tool-governance policies can silently regress (e.g. a PUT that drops default-deny enforcement, or an audit-log that returns other tenants' entries).
- **修复建议**: Add tests/api/test_tenant_policies_api.py following test_gateway_tenant_isolation.py: (1) PUT /tool-policies/{tenant_id} persists the policy set; (2) GET returns it; (3) DELETE clears; (4) cross-tenant reads of /tool-policies/{other} are forbidden; (5) /audit-log is scoped by tenant and only returns that tenant's events; (6) a caller without GATEWAY_TOOL_POLICY_* capability gets 403.
- **测试建议**: Use an in-memory policy store double; assert on both status codes and persisted state (like test_agents_api.py's InMemoryAgentRepository pattern). Add one regression test that the default (no policy row) resolves to deny for an unknown tenant.

#### Quota business logic (forecast, block/unblock, reset, alerts) is untested; only authz is covered

- **模块**: `tests-audit` ｜ **级别**: `HIGH` ｜ **类别**: `test-gap` ｜ **验证**: 未验证
- **位置**: `src/api/v1/quota.py:713`
- **问题**: Existing tests (test_gateway_capability_matrix.py, test_gateway_tenant_isolation.py) only assert permission gating on quota routes. The actual business logic — get_user_quota_forecast (713), reset_user_quota (739), block_user (804), unblock_user (847), acknowledge_alert (517), check_user_quota (689) — has no unit tests for the state transitions (blocked user denial, unblock restoration, reset clearing counters, forecast math).
- **影响**: Monetization/abuse-control invariants (blocked users still able to call the gateway, reset not clearing counters) can regress undetected.
- **修复建议**: Add tests/api/test_quota_logic.py with a fake quota service recording state: (1) block_user sets status=blocked and a blocked subject's check fails; (2) unblock_user flips back; (3) reset clears daily usage but preserves the base tier quota; (4) forecast extrapolates from recent usage and clamps to the tier ceiling; (5) acknowledge_alert marks the alert acked (idempotent); (6) all transitions return the documented response_model shape.
- **测试建议**: Model the state machine as (tenant,user)->{blocked, usage, tier} and assert each transition; add a cross-user isolation check that user A's block does not affect user B in the same tenant.

#### Knowledge-service route surface has no route-level tests for QA, batch doc ops, versions, dedupe, worker status

- **模块**: `tests-audit` ｜ **级别**: `HIGH` ｜ **类别**: `test-gap` ｜ **验证**: 未验证
- **位置**: `apps/knowledge-service/src/knowledge_service/api/routes/knowledge.py:1`
- **问题**: Of the ~60 routes in knowledge.py, only retrieve/hit_test/retrieve_evaluate are exercised via tests/api/test_retrieval_evaluate.py and tests/knowledge/test_kb_security_regressions.py; service-level tests cover embedding/retrieval/dataset internals. The high-value endpoints without any route test include POST /qa, /qa/batch, /qa/stream, /documents/batch (upload/reindex/batch-delete), GET /documents/{id}/versions + POST .../versions/{n}/restore, POST /maintenance/dedupe, GET /worker/status, GET /images/{segment_id}, PUT /{dataset_id}/config, and GET /{dataset_id}/debug.
- **影响**: Ingestion, versioning, QA, and cleanup workflows can break at the HTTP layer (schema drift, wrong status codes, authz) without the suite noticing, even though service internals are covered.
- **修复建议**: Add route-level tests (mirror test_retrieval_evaluate.py's direct-handler-call style): (1) POST /qa returns QAResponse and streams on /qa/stream; (2) documents/batch-upload persists docs and rejects invalid payloads with 422; (3) version restore swaps current_draft content and rejects restoring a deleted version; (4) maintenance/dedupe returns a dedupe report; (5) worker/status returns a health-shaped payload; (6) GET /images/{segment_id} 404s for a cross-dataset segment.
- **测试建议**: Use a FakeKnowledgeService (as in test_retrieval_evaluate.py) plus a fake document/version repository; assert status codes and response_model field names to freeze the contract. Prioritize /qa and documents/batch-upload first.

#### Java SDK authenticates with 'Authorization: Bearer <apiKey>' instead of 'X-API-Key'

- **模块**: `scripts-infra` ｜ **级别**: `HIGH` ｜ **类别**: `api-contract` ｜ **验证**: ✅ 已确认
- **位置**: `sdk/java/src/main/java/com/aigateway/ai/ChatClient.java:214`
- **问题**: ChatClient.buildRequest sends the API key in the Authorization Bearer header (line 214). The gateway AuthMiddleware (src/core/middleware/auth.py lines 169-196) treats Authorization Bearer as a JWT and, on decode failure, raises 401 'Invalid or expired token' immediately — it never falls through to X-API-Key. When JWT auth is disabled, the request silently degrades to the anonymous path. The Python SDK (sdk/python/ai_assistant/auth.py line 61) and the CLI (sdk/cli/src/remote/client.ts line 27) both use the correct X-API-Key header; the Java SDK is the outlier. API keys are 'gw_xxx' strings, never JWTs.
- **影响**: Every Java SDK request either gets a 401 or runs as an anonymous user; the API key the caller supplied is never honored, so tenant/user scoping and paid-tier features silently do not apply. This is both a broken-client bug and an authz-confusion risk (callers believe they are authenticated).
- **修复建议**: Send the key via the 'X-API-Key' header (and 'X-Tenant-Id') exactly as the Python SDK and CLI do; only use Bearer when the key is genuinely a JWT.
- **测试建议**: Add a unit test asserting ChatClient sends X-API-Key and not a Bearer header; currently only the SSE parser is tested (SSEParserTest.java).

#### Migration discovery keyed on 3-digit version prefix silently skips whole migrations

- **模块**: `scripts-infra` ｜ **级别**: `HIGH` ｜ **类别**: `logic-bug` ｜ **验证**: ⚠️ 部分确认
- **位置**: `database/cli.py:82`
- **问题**: discover_migrations() keys migrations by the leading 3-digit prefix (regex '^(\d{3})_(.+)\.sql$'). The repo contains duplicate prefixes: 016 (confluence_multi_root_pages + usage_hourly_aggregates), 030 (fix_timestamp_and_security_constraint + its *_rollback file, which is wrongly treated as a forward migration), and 031 (align_model_prices_20260211 + hierarchical_segments). cmd_init/cmd_migrate filter pending by 'v not in applied', so after the first file of a pair records version '016'/'031', the sibling file is considered applied and never run. record_migration() also does 'ON CONFLICT (version) DO UPDATE', overwriting the first file's ledger row with the second file's name/checksum.
- **影响**: Whole migrations (e.g. 016_usage_hourly_aggregates.sql, 031_align_model_prices_20260211.sql) are silently skipped by the python CLI runner, and the migration ledger becomes inconsistent. The shell runner (migrate.sh) handles this via rollback-skip + duplicate guards, so the impact is limited to the Python path, but it is a real data-migration gap.
- **修复建议**: Track by full filename (like migrate.sh) instead of the version prefix, and explicitly skip '*_rollback.sql'. Reject files whose version prefix duplicates an existing forward migration.
- **测试建议**: Add a unit test asserting every forward migration file maps to a unique key and that the rollback file is excluded from discovery.
- **验证证据**: The version-keying collision is real: cli.py:82 uses `^(\d{3})_(.+)\.sql$`, and database/migrations/ contains duplicate prefixes 016 (confluence_multi_root_pages + usage_hourly_aggregates), 030 (fix_timestamp_and_security_constraint + its *_rollback file), and 031 (align_model_prices_20260211 + hierarchical_segments). record_migration (cli.py:148-155) `ON CONFLICT (version) DO UPDATE` genuinely overwrites the ledger row, and the *_rollback.sql file matches the regex so it is treated as a forward

#### PIPESTATUS cleanup is dead code under set -o pipefail — failed backups leave partial files

- **模块**: `scripts-infra` ｜ **级别**: `HIGH` ｜ **类别**: `error-handling` ｜ **验证**: ✅ 已确认
- **位置**: `scripts/new/backup.sh:85`
- **问题**: common.sh (sourced at line 15) sets 'set -euo pipefail'. The pg_dump pipeline at line 83 ('docker exec ... pg_dump | gzip > file') therefore aborts the whole script on pg_dump failure before line 85 ('if [ "${PIPESTATUS[0]}" -ne 0 ]') ever executes, so the intended 'rm -f "$BACKUP_FILE"' cleanup never runs and an empty/partial .sql.gz is left in backups/. The script also never verifies the postgres container is running before dumping.
- **影响**: A failed or aborted backup leaves a corrupt gzip file that a later 'restore' (which picks the latest backup) may attempt to load, producing a broken restore. The explicit cleanup path is unreachable.
- **修复建议**: Capture PIPESTATUS immediately and guard the pipeline with '|| true' (or set +e around it), then act on PIPESTATUS[0] and delete the partial file on failure.
- **测试建议**: Add a shell test that runs backup.sh with a bogus container name and asserts no partial .sql.gz remains and the exit code is non-zero.

---

## 五、Medium 级发现清单

- **[concurrency] create_document_version computes version_number via MAX+1 with no lock or unique constraint** — `packages/ai-gateway-core/src/ai_gateway_core/persistence/database.py:6811`
- **[security] lock_user_account interpolates the minutes parameter into an SQL INTERVAL string** — `packages/ai-gateway-core/src/ai_gateway_core/persistence/database.py:5777`
- **[error-handling] update_user_roles/delete_user/create_role/update_role perform multi-statement writes outside a transaction** — `packages/ai-gateway-core/src/ai_gateway_core/persistence/database.py:6042`
- **[logic-bug] incr_rate_limit resets the expire on every increment, creating a sliding window that never decays for active users** — `packages/ai-gateway-core/src/ai_gateway_core/persistence/redis.py:152`
- **[implementation-bug] DatabaseAPIKeyRepository.create silently returns an unpersisted key when the DB pool is disabled** — `packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/api_key_repository.py:153`
- **[error-handling] Redis-backed circuit breaker fails closed with unhandled RuntimeError when Redis is down** — `packages/ai-gateway-core/src/ai_gateway_core/proxy/base.py:243`
- **[logic-bug] get() auto-extends expired sessions with the default 7-day TTL even for anonymous sessions** — `packages/ai-gateway-core/src/ai_gateway_core/session/database_manager.py:152`
- **[error-handling] upload_images_batch leaves orphaned images when one upload fails** — `packages/ai-gateway-core/src/ai_gateway_core/storage/image_storage.py:1109`
- **[security] v2 header path buffers the entire request body before signature check — unauthenticated memory-exhaustion vector** — `packages/ai-gateway-core/src/ai_gateway_core/auth/gateway_secret_middleware.py:92`
- **[concurrency] stop_connector clears flags but never deregisters the global Confluence executors/credentials; concurrent start/stop races on plain dicts** — `packages/ai-gateway-core/src/ai_gateway_core/connectors/connector_mcp.py:92`
- **[logic-bug] Live-candidate release gate can never pass when the suite defines no critical cases** — `packages/ai-gateway-core/src/ai_gateway_core/eval/evaluator_executor.py:1196`
- **[config] Module-import-time env parsing of IMAGE_CALLBACK_* can crash the whole package import** — `packages/ai-gateway-core/src/ai_gateway_core/image/callback.py:20`
- **[code-quality] Same-named *_cost_cents columns hold different units (microcents vs real cents) across tables** — `packages/ai-gateway-core/src/ai_gateway_core/metrics/usage_recorder.py:973`
- **[logic-bug] get_stats reports None for avg/min/max score when the true value is 0.0** — `packages/ai-gateway-core/src/ai_gateway_core/quiz/exam_service.py:393`
- **[implementation-bug] Public share attempt never AI-grades short_answer questions — always marked wrong** — `packages/ai-gateway-core/src/ai_gateway_core/quiz/quiz_share_manager.py:248`
- **[implementation-bug] Skill create_version/save_manifest skip the dangerous-permission validation that propose_skill and update_metadata enforce** — `packages/ai-gateway-core/src/ai_gateway_core/skills/artifact_repository.py:191`
- **[security] Auto knowledge-base retrieval bypasses tool-invoker authorization boundary** — `apps/assistant-service/src/assistant_service/core/agent/agent_loop.py:5543`
- **[logic-bug] Per-run token usage overwritten each iteration (undercounts multi-tool runs)** — `apps/assistant-service/src/assistant_service/core/agent/agent_loop.py:4890`
- **[concurrency] User-message persistence races assistant-message persistence (out-of-order history)** — `apps/assistant-service/src/assistant_service/core/agent/agent_loop.py:4190`
- **[error-handling] execute() re-raises loop exceptions without emitting a terminal RUN_ERROR event** — `apps/assistant-service/src/assistant_service/core/agent/agent_loop.py:3145`
- **[error-handling] Unvalidated SubAgentType from model args raises ValueError** — `apps/assistant-service/src/assistant_service/core/agent/agent_loop.py:1654`
- **[performance] Full-message checkpoint written on every model turn** — `apps/assistant-service/src/assistant_service/core/agent/agent_loop.py:4793`
- **[security] Middleware on_tool_call fails OPEN on exceptions** — `apps/assistant-service/src/assistant_service/core/agent/middleware.py:236`
- **[logic-bug] Anonymous streaming tool calls fragment across deltas** — `apps/assistant-service/src/assistant_service/core/agent/stream_helpers.py:37`
- **[logic-bug] Sub-agent tool calls can share duplicate tool_call_id** — `apps/assistant-service/src/assistant_service/core/agent/subagent_manager.py:738`
- **[resource-leak] ToolOrchestrator._result_cache grows without eviction** — `apps/assistant-service/src/assistant_service/core/tool_orchestrator.py:235`
- **[resource-leak] trace_writer _failed_outcomes dict grows without bound** — `apps/assistant-service/src/assistant_service/core/trace_writer.py:431`
- **[error-handling] Chat route catch-all converts PermissionDeniedError and service HTTPExceptions into generic 500** — `apps/assistant-service/src/assistant_service/api/routes/chat.py:761`
- **[security] Owner-scope collapses to bare user_id when only one of app_tenant_id/app_user_id is supplied — merges all app end-users into one image namespace** — `apps/assistant-service/src/assistant_service/api/routes/images.py:1429`
- **[security] _run_gemini_multi_turn reads the generic chat session without an ownership check** — `apps/assistant-service/src/assistant_service/api/routes/images.py:1278`
- **[error-handling] Session deletion can leave an inconsistent partial state (memories/runtime cleared but durable row kept)** — `apps/assistant-service/src/assistant_service/api/routes/sessions.py:139`
- **[performance] Synchronous fc-list subprocess blocks the asyncio event loop during render** — `apps/assistant-service/src/assistant_service/core/docgen/design_system.py:448`
- **[error-handling] XlsxPlanner and PdfPlanner propagate LLM errors instead of falling back to the deterministic path** — `apps/assistant-service/src/assistant_service/core/docgen/planners/xlsx_planner.py:51`
- **[resource-leak] Sandbox workdirs are never cleaned up (both backends leak temp dirs on every exec)** — `apps/assistant-service/src/assistant_service/core/docgen/sandbox/local.py:92`
- **[implementation-bug] DocgenService and the whole docgen/sandbox subsystem are not wired into the production service** — `apps/assistant-service/src/assistant_service/core/docgen/service.py:100`
- **[config] _DEFAULT_SKILL_PATHS points at a directory that does not exist, so the skill registry is always empty in production** — `apps/assistant-service/src/assistant_service/core/docgen/service.py:29`
- **[security] Path traversal in LocalArtifactStore via unsanitized tenant_id/session_id/turn_id** — `apps/assistant-service/src/assistant_service/core/docgen/storage/local.py:55`
- **[performance] LocalArtifactStore.get / list_session do an O(n) rglob scan of every meta file** — `apps/assistant-service/src/assistant_service/core/docgen/storage/local.py:103`
- **[config] Artifact sign secret falls back to a per-process random, and local store ships file:// download URLs** — `apps/assistant-service/src/assistant_service/core/docgen/storage/signing.py:20`
- **[security] RemoteAssistantClient forwards full identity (tenant/user/roles/tier) in unsigned request body** — `apps/assistant-service/src/assistant_service/core/client.py:245`
- **[security] Unvalidated MCP resource/image URIs surfaced as artifact download_url** — `apps/assistant-service/src/assistant_service/core/mcp/manager.py:178`
- **[security] MCP tool allowlist filtering is bypassable when server names contain '__'** — `apps/assistant-service/src/assistant_service/core/mcp/tenant_mcp_config.py:130`
- **[logic-bug] compress(preserve_recent=0) compresses nothing due to negative-slice semantics** — `apps/assistant-service/src/assistant_service/core/memory/compressor.py:202`
- **[security] ScenarioAwareRetriever cache key omits user scope (cross-user RAG result leak)** — `apps/assistant-service/src/assistant_service/core/rag/scenario_aware_retriever.py:330`
- **[performance] load_memory_context re-chunks, re-embeds and re-upserts every memory source on every request** — `apps/assistant-service/src/assistant_service/core/runtime/compat/runtime_adapter.py:247`
- **[error-handling] Rate-limit check fails open and depends on best-effort audit writes** — `apps/assistant-service/src/assistant_service/core/audit/tool_audit.py:133`
- **[implementation-bug] generate() silently discards user-provided reference context** — `apps/assistant-service/src/assistant_service/core/content/content_generator.py:546`
- **[implementation-bug] _check_hallucination discards the context argument; check is only phrase matching** — `apps/assistant-service/src/assistant_service/core/content/structured_output.py:513`
- **[security] Untrusted content interpolated into agent prompts without delimiting/escaping** — `apps/assistant-service/src/assistant_service/core/prompts/scenario_analysis_prompts.py:998`
- **[implementation-bug] PPT/XLS guardrail thresholds declared but never enforced; score formula mismatch** — `apps/assistant-service/src/assistant_service/core/quality/guardrails.py:362`
- **[concurrency] Shared/predictable LibreOffice user profiles cause concurrent-instance conflicts** — `apps/assistant-service/src/assistant_service/core/skills/docx/scripts/accept_changes.py:16`
- **[security] Comment text/author interpolated into XML without escaping (XML element injection)** — `apps/assistant-service/src/assistant_service/core/skills/docx/scripts/comment.py:238`
- **[redundancy] Office scripts and validators duplicated byte-identical across docx/pptx/xlsx (36 files)** — `apps/assistant-service/src/assistant_service/core/skills/docx/scripts/office/soffice.py:35`
- **[security] Unbounded zip extraction on untrusted Office files (decompression bomb)** — `apps/assistant-service/src/assistant_service/core/skills/docx/scripts/office/unpack.py:52`
- **[security] Raw xml.etree.ElementTree parses untrusted DOCX XML (entity-expansion DoS)** — `apps/assistant-service/src/assistant_service/core/skills/docx/scripts/office/validators/redlining.py:32`
- **[implementation-bug] recalc() can hang forever on macOS without gtimeout** — `apps/assistant-service/src/assistant_service/core/skills/xlsx/scripts/recalc.py:91`
- **[resource-leak] cleanup_all removes any exited/dead container sharing the sandbox image, not just this service's** — `apps/assistant-service/src/assistant_service/core/code_executor.py:838`
- **[performance] Unbounded in-memory reads of container logs and output files** — `apps/assistant-service/src/assistant_service/core/code_executor.py:771`
- **[security] AST import denylist is trivially bypassable and overstates its protection** — `apps/assistant-service/src/assistant_service/core/tools/code_executor_tool.py:95`
- **[security] Confluence markdown link href is not quote-escaped, enabling HTML attribute injection in stored pages** — `apps/assistant-service/src/assistant_service/core/tools/confluence_tool.py:140`
- **[security] Tenant identity falls back to request.metadata['tenant_id'] when no UserContext is present (cross-tenant IDOR surface)** — `apps/assistant-service/src/assistant_service/core/tools/confluence_tool.py:1788`
- **[concurrency] fs_grep timeout does not terminate the worker thread; ReDoS pattern can burn CPU indefinitely** — `apps/assistant-service/src/assistant_service/core/tools/primitives.py:388`
- **[security] execution_gateway_approved is an unauthenticated metadata flag that bypasses the risk/confirmation gate** — `apps/assistant-service/src/assistant_service/core/tools/tool_registry.py:650`
- **[security] KB-service proxy does not strip client-supplied identity headers; IDOR risk when shared secret unset** — `src/api/v1/_proxy_utils.py:71`
- **[api-contract] delete_session skips gateway ownership check when ASSISTANT_ROUTE_SESSIONS_PROXIED=true** — `src/api/v1/assistant.py:644`
- **[config] Runtime auth-config update endpoint is a no-op that never affects actual authentication** — `src/api/v1/config.py:331`
- **[security] Dashboard WebSocket streams the full global snapshot to every metrics-read client; tenant_id is advisory only** — `src/api/v1/dashboard.py:511`
- **[implementation-bug] File list/get endpoints only read the local uploads dir and are inconsistent with S3/OSS uploads** — `src/api/v1/files.py:463`
- **[security] LangGraph passthrough catch-all forwards arbitrary upstream paths with the shared service token after only partial ownership checks** — `src/api/v1/langgraph.py:655`
- **[performance] Timeseries endpoints loop unbounded over the requested range, enabling Redis-storm resource exhaustion** — `src/api/v1/metrics.py:268`
- **[security] Quota write endpoints allow self-modification when the user holds GATEWAY_QUOTA_WRITE** — `src/api/v1/quota.py:627`
- **[security] Role create/update special-cases 'admin:*' permission, letting a role-creator mint an admin role** — `src/api/v1/roles.py:154`
- **[security] get_model_usage lacks per-user scoping, exposing tenant-wide model usage/cost to non-operators** — `src/api/v1/usage.py:751`
- **[security] RBAC treats role strings as direct permissions and grants everything on 'admin:*'** — `src/core/auth/rbac.py:28`
- **[security] Service-access constraints cache serves stale grants after key revocation or policy change** — `src/core/auth/service_access_resolver.py:75`
- **[security] verify_signed_url returns valid for all URLs when no secret is configured** — `src/core/crypto.py:242`
- **[concurrency] Circuit breaker half-open state admits all callers (half-open stampede)** — `src/core/gateway/circuit_breaker.py:60`
- **[logic-bug] Redis sliding-window limiter counts rejected requests (no zrem on over-limit)** — `src/core/gateway/multi_dimension_rate_limiter.py:425`
- **[resource-leak] In-memory rate-limit stores never evict empty keys (unbounded memory growth)** — `src/core/gateway/multi_dimension_rate_limiter.py:362`
- **[security] AuthMiddleware trusts any guest session ID when no validator is configured (default enabled)** — `src/core/middleware/auth.py:312`
- **[logic-bug] HTTP-level Redis limiter counts rejected requests and can grow the zset unboundedly** — `src/core/middleware/rate_limit_http.py:132`
- **[security] StreamingAuthMiddleware never rejects invalid/expired JWTs on streaming paths — silent downgrade to anonymous** — `src/core/middleware/streaming.py:204`
- **[config] Quota check defaults to fail_open (silently bypasses quota/billing guard)** — `src/proxy/langgraph_governance.py:157`
- **[performance] ComfyUI adapter has no timeout on the websocket receive loop and can hang forever** — `src/adapters/comfyui.py:17`
- **[concurrency] _ensure_thread has a check-then-act race producing divergent session->thread mappings** — `src/adapters/langgraph.py:1349`
- **[error-handling] Failed thread creation is still cached, permanently breaking the session** — `src/adapters/langgraph.py:1398`
- **[logic-bug] stream_run sums per-event cumulative token usage, over-counting billing metrics** — `src/adapters/langgraph_proxy.py:1130`
- **[error-handling] OpenAIAdapter.stream swallows upstream HTTP errors and reports success with empty output** — `src/adapters/openai.py:66`
- **[config] Conflicting load-balancer strategy defaults between LoadBalancerSettings and LangGraphSettings** — `src/config/settings.py:229`
- **[implementation-bug] GRPCConnector is an unimplemented stub; any gRPC-configured service fails at runtime** — `src/connectors/grpc.py:13`
- **[error-handling] fail() republishes an empty payload when the in-memory message store is empty (restart / cross-instance)** — `src/connectors/message_queue.py:124`
- **[resource-leak] ack() never purges _messages/_message_meta, so memory grows unboundedly** — `src/connectors/message_queue.py:116`
- **[logic-bug] Daily quota reset is off-by-one for rows whose daily_reset_at is set to next midnight** — `src/services/billing/aggregation_task.py:207`
- **[logic-bug] Reverse prefix partial-match returns wrong pricing for shorter requested model** — `src/services/billing/model_pricing.py:149`
- **[error-handling] sync_page orphaned S3/OSS image files when updating a document with images** — `src/services/knowledge/confluence/sync_service.py:2474`
- **[config] DashScope base URL is mutated globally via dashscope.base_http_api_url** — `src/services/knowledge/embedding.py:456`
- **[logic-bug] upsert_model_from_catalog updates model without provider_id scoping** — `src/services/llm/model_service.py:217`
- **[logic-bug] ConsistentHashStrategy uses Python's builtin hash() which is salted per process** — `src/services/registry/load_balancer.py:50`
- **[security] Fallback retrieve endpoint lacks dataset authorization and matches by name** — `apps/knowledge-service/src/knowledge_service/api/router.py:192`
- **[implementation-bug] get_image_segment crashes with AttributeError for local file:// images** — `apps/knowledge-service/src/knowledge_service/api/routes/knowledge.py:686`
- **[performance] upload streams to temp then reads the whole file back into memory** — `apps/knowledge-service/src/knowledge_service/api/routes/knowledge.py:295`
- **[security] QA endpoints let any dataset viewer supply an arbitrary LLM config** — `apps/knowledge-service/src/knowledge_service/api/routes/knowledge.py:1927`
- **[implementation-bug] Confluence sync feature never wired into the app** — `apps/knowledge-service/src/knowledge_service/main.py:282`
- **[security] download_attachment fetches arbitrary HTTP URLs with redirects, no host validation** — `apps/knowledge-service/src/knowledge_service/services/knowledge/confluence/client.py:882`
- **[implementation-bug] remove_pages references undefined self.kb (AttributeError)** — `apps/knowledge-service/src/knowledge_service/services/knowledge/confluence/sync_service.py:2770`
- **[security] _load_image_from_url reads any file:// path and any HTTP URL without confinement** — `apps/knowledge-service/src/knowledge_service/services/knowledge/multimodal_reranker.py:412`
- **[concurrency] Orphaned MMR vector-fetch task when candidate count <= top_k** — `apps/knowledge-service/src/knowledge_service/services/knowledge/retrieval_service.py:1510`
- **[implementation-bug] image_boost score boost is a no-op** — `apps/knowledge-service/src/knowledge_service/services/knowledge/retrieval_service.py:1877`
- **[performance] UnifiedMultimodalEmbedding created per request and never closed/cached** — `apps/knowledge-service/src/knowledge_service/services/knowledge/retrieval_service.py:789`
- **[redundancy] docgen module is triplicated: packages/mcp-docgen-server, assistant-service core/docgen, and assistant-service core/skills** — `packages/mcp-docgen-server/src/docgen/:1`
- **[performance] LocalArtifactStore.get()/download_url() re-scan and re-read every .meta.json on disk per call** — `packages/mcp-docgen-server/src/docgen/storage/local.py:104`
- **[concurrency] meta.json / index.json are written non-atomically and read without a lock — concurrent put/get can hit partial JSON** — `packages/mcp-docgen-server/src/docgen/storage/local.py:93`
- **[performance] S3ArtifactStore runs blocking boto3 calls inside async methods and scans the whole tenant prefix per get** — `packages/mcp-docgen-server/src/docgen/storage/s3.py:87`
- **[error-handling] verify_url raises ValueError on malformed 'exp' query param instead of returning False** — `packages/mcp-docgen-server/src/docgen/storage/signing.py:62`
- **[security] MCP HTTP/SSE endpoints are unauthenticated on 0.0.0.0 and all tenants collapse to 'default'** — `packages/mcp-docgen-server/src/mcp_docgen_server/server.py:415`
- **[security] get_dsn() silently falls back to hardcoded postgres:postgres credentials; mask_dsn leaks passwords containing @ or :** — `database/cli.py:57`
- **[logic-bug] Legacy version-tracking mode silently treats sibling duplicate-version migrations as applied** — `scripts/new/migrate.sh:103`
- **[config] Runtime Redis password constraint (hex, >=32 chars) is not enforced by validate-env.sh** — `scripts/new/redis-entrypoint.sh:8`
- **[security] Redis password interpolated into a string that common.sh wait_for_healthy() later eval's** — `scripts/new/setup-dev.sh:179`
- **[security] safeEnv() does not strip credential-bearing URL-style env vars (DATABASE_URL, MYSQL_PWD, connection URIs)** — `sdk/cli/src/tools/bash.ts:35`
- **[implementation-bug] Python MCP HTTP transport registers tools but call_tool() is broken for HTTP connections** — `sdk/python/ai_assistant/mcp/client.py:241`
- **[config] Coverage gate is --cov-fail-under=25 — a 25% bar that the silent-pass tests help mask** — `pyproject.toml:1`
- **[test-gap] Gateway admin config API (15 routes) plus submit/stream/tool_inventory have no direct tests** — `src/api/v1/config.py:1`
- **[test-gap] Pervasive 'implicit no-raise' positive-path tests in test_api_key/test_validator/test_proxy_authorization_matrix/test_acl_permissions/test_gateway** — `tests/core/auth/test_api_key.py:10`
- **[test-gap] test_kb_qa.py is a CLI script in the tests tree; pytest collects nothing from it** — `tests/knowledge/test_kb_qa.py:24`
- **[config] Repo-root CWD-relative paths in test_script_secret_defaults and test_alert_rules make them CWD-dependent** — `tests/scripts/test_script_secret_defaults.py:10`
- **[test-gap] test_config_chain_documentation and test_validation_logs are doc-printing tests with zero assertions** — `tests/test_chunking_config_chain.py:161`
- **[code-quality] test_require_admin_* mutates the module-global ADMIN_USER_IDS set** — `tests/unit/test_files_api.py:101`
---

## 六、Low 级发现清单

- **[api-contract] Idempotency middleware store_key omits the request body, so a reused key with a different body silently returns the stale response** — `packages/ai-gateway-core/src/ai_gateway_core/comm/idempotency.py:361`
- **[error-handling] Idempotency middleware reads the full request body with no size cap** — `packages/ai-gateway-core/src/ai_gateway_core/comm/idempotency.py:308`
- **[test-gap] Consumer retry/DLQ semantics have no coverage for the stable-consumer failure path** — `packages/ai-gateway-core/src/ai_gateway_core/events/consumer.py:320`
- **[logic-bug] update_usage_stats rolling average assumes request_count always increments by 1** — `packages/ai-gateway-core/src/ai_gateway_core/persistence/database.py:3907`
- **[security] Redaction regexes leak multi-word secrets and short keys** — `packages/ai-gateway-core/src/ai_gateway_core/security/redaction.py:31`
- **[performance] Blocking socket.getaddrinfo inside the async path** — `packages/ai-gateway-core/src/ai_gateway_core/security/safe_fetch.py:134`
- **[logic-bug] In-memory session cache is unbounded with no eviction** — `packages/ai-gateway-core/src/ai_gateway_core/session/database_manager.py:44`
- **[implementation-bug] upload_file_streaming drops tenant_id isolation while upload_file keeps it** — `packages/ai-gateway-core/src/ai_gateway_core/storage/file_storage.py:266`
- **[logic-bug] parse_image_size accepts zero/negative/huge dimensions** — `packages/ai-gateway-core/src/ai_gateway_core/image/helpers.py:25`
- **[concurrency] requests_window zset member uses the timestamp string as the unique member — same-ms completions overwrite each other** — `packages/ai-gateway-core/src/ai_gateway_core/metrics/realtime_metrics.py:303`
- **[test-gap] No unit test covers _get_model_pricing partial-prefix matching or the quota cost unit conversion** — `packages/ai-gateway-core/src/ai_gateway_core/metrics/usage_recorder.py:519`
- **[logic-bug] publish_exam fails when the publishing user is not the quiz's creator** — `packages/ai-gateway-core/src/ai_gateway_core/quiz/exam_service.py:201`
- **[logic-bug] create_share accepts negative expires_hours and zero max_attempts producing unusable shares** — `packages/ai-gateway-core/src/ai_gateway_core/quiz/quiz_share_manager.py:78`
- **[logic-bug] Tool-result truncation hint instructs model to re-invoke the tool** — `apps/assistant-service/src/assistant_service/core/agent/agent_loop.py:7640`
- **[logic-bug] Batch tool-call turn-state returns to MODEL_RUNNING after first completion** — `apps/assistant-service/src/assistant_service/core/agent/agent_loop.py:991`
- **[logic-bug] Sub-agent spawned without parent context gets zero tools (docstring mismatch)** — `apps/assistant-service/src/assistant_service/core/agent/subagent_manager.py:493`
- **[code-quality] Sub-agent model picker touches private registry internals** — `apps/assistant-service/src/assistant_service/core/agent/subagent_manager.py:567`
- **[redundancy] _side_effect_recovery duplicated in subagent_manager and agent_loop** — `apps/assistant-service/src/assistant_service/core/agent/subagent_manager.py:591`
- **[performance] _E2E_MEMORY_BY_USER is an unbounded process-global dict** — `apps/assistant-service/src/assistant_service/api/routes/chat.py:41`
- **[security] get_image_session_view skips the ownership check for image sessions without an owner_scope** — `apps/assistant-service/src/assistant_service/api/routes/images.py:3241`
- **[api-contract] generate_image declares response_model=ImageGenerationResponse but can return a non-conforming 202 JSONResponse** — `apps/assistant-service/src/assistant_service/api/routes/images.py:2008`
- **[security] Blob endpoints expose the internal object-storage key to clients** — `apps/assistant-service/src/assistant_service/api/routes/images.py:1742`
- **[code-quality] _post_generation_bookkeeping has two conflicting docstrings** — `apps/assistant-service/src/assistant_service/api/routes/images.py:2394`
- **[security] Unknown model access_level fails open (any unrecognized value is treated as visible)** — `apps/assistant-service/src/assistant_service/api/routes/models.py:33`
- **[security] /config tools_available lists all tools without permission filtering (inconsistent with /tools)** — `apps/assistant-service/src/assistant_service/api/routes/models.py:100`
- **[redundancy] LLMSettings holds API keys that nothing consumes (dead secret config)** — `apps/assistant-service/src/assistant_service/config/settings.py:68`
- **[concurrency] Lifespan shutdown closes DB/Redis without awaiting or cancelling in-flight image generation workers** — `apps/assistant-service/src/assistant_service/main.py:575`
- **[config] DB model catalog loaded only for hardcoded tenant 'default' and served to all tenants** — `apps/assistant-service/src/assistant_service/main.py:237`
- **[security] Docx/Xlsx verifiers parse files with default (unhardened) XML/zip settings** — `apps/assistant-service/src/assistant_service/core/docgen/quality/docx_verifier.py:57`
- **[logic-bug] Placeholder regex `\b\[\[` and `\b<<` can never match** — `apps/assistant-service/src/assistant_service/core/docgen/quality/docx_verifier.py:26`
- **[implementation-bug] IR fix patches never rewrite CodeBlock or ChartBlock placeholders** — `apps/assistant-service/src/assistant_service/core/docgen/quality/verifier_pipeline.py:143`
- **[resource-leak] pypdfium2 PdfDocument handle never closed in the fallback image renderer** — `apps/assistant-service/src/assistant_service/core/docgen/quality/visual_verifier.py:306`
- **[implementation-bug] DOCX title is rendered twice in the deterministic path** — `apps/assistant-service/src/assistant_service/core/docgen/renderers/docx_renderer.py:187`
- **[error-handling] Table renderers crash on rows with zero cells (cols computed as 0)** — `apps/assistant-service/src/assistant_service/core/docgen/renderers/layouts/blocks.py:184`
- **[api-contract] sign_url/verify_url disagree on query-parameter encoding, breaking signatures for special-char tenants** — `apps/assistant-service/src/assistant_service/core/docgen/storage/signing.py:52`
- **[api-contract] approve() records approver identity but never checks the approver is authorized** — `apps/assistant-service/src/assistant_service/core/gateway/execution_gateway.py:2764`
- **[security] MCPClient.initialize mutates a caller-shared http_client's default Authorization header** — `apps/assistant-service/src/assistant_service/core/mcp/client.py:441`
- **[redundancy] Duplicated credential/prompt-injection redaction regexes across client, manager, and assembler** — `apps/assistant-service/src/assistant_service/core/mcp/client.py:945`
- **[config] Static MCP config allows plaintext HTTP and private/metadata network ranges** — `apps/assistant-service/src/assistant_service/core/mcp/config.py:68`
- **[performance] Best-effort runtime telemetry can block the invocation past its 50ms budget** — `apps/assistant-service/src/assistant_service/core/mcp/runtime.py:271`
- **[logic-bug] recall() sanitizes long-term results but not working/session results** — `apps/assistant-service/src/assistant_service/core/memory/memory_manager.py:697`
- **[logic-bug] In-memory aggregate stats look up 'quality_score' key that never exists** — `apps/assistant-service/src/assistant_service/core/rag/rag_metrics.py:849`
- **[security] sync_turn_to_memory does not itself enforce the memory policy gate** — `apps/assistant-service/src/assistant_service/core/runtime/compat/runtime_adapter.py:356`
- **[resource-leak] Session-scoped pg_advisory_lock on a pooled connection can strand future operations** — `apps/assistant-service/src/assistant_service/core/runtime/memory/indexer.py:177`
- **[logic-bug] word_count mixes characters and words inconsistently** — `apps/assistant-service/src/assistant_service/core/content/content_generator.py:60`
- **[performance] _try_truncate_to_valid_json is O(n^2) with repeated json.loads** — `apps/assistant-service/src/assistant_service/core/content/structured_output.py:292`
- **[logic-bug] Missing-field defaulting broken for list[str]/dict[str,...] hints on Python 3.11** — `apps/assistant-service/src/assistant_service/core/content/structured_output.py:333`
- **[error-handling] XML pretty-print and smart-quote escape failures silently swallowed** — `apps/assistant-service/src/assistant_service/core/skills/docx/scripts/office/unpack.py:86`
- **[error-handling] durableId hex parse errors silently skipped for non-numbering files** — `apps/assistant-service/src/assistant_service/core/skills/docx/scripts/office/validators/docx.py:297`
- **[security] Skill risk assessment under-classifies read/network permissions** — `apps/assistant-service/src/assistant_service/core/skills/tool_bridge.py:154`
- **[logic-bug] Code-execution timeout is misclassified as ERROR and the kill branch is skipped** — `apps/assistant-service/src/assistant_service/core/code_executor.py:586`
- **[resource-leak] Orphaned workspace temp dirs are never reaped if the process dies mid-execution** — `apps/assistant-service/src/assistant_service/core/code_executor.py:494`
- **[concurrency] Per-tenant Confluence client cache is unbounded and unsynchronized** — `apps/assistant-service/src/assistant_service/core/tools/confluence_tool.py:1758`
- **[security] Document title interpolated unescaped into the HTML wrapper for PDF generation** — `apps/assistant-service/src/assistant_service/core/tools/document_generator_tool.py:310`
- **[redundancy] Identical _sanitize_filename duplicated across document and PPTX generators** — `apps/assistant-service/src/assistant_service/core/tools/document_generator_tool.py:428`
- **[api-contract] spawn_subagent result embeds a non-JSON-serializable dataclass** — `apps/assistant-service/src/assistant_service/core/tools/subagent_tool.py:125`
- **[performance] spawn_subagent is always exposed (no confirmation) and consumes budget in every turn** — `apps/assistant-service/src/assistant_service/core/tools/tool_selector.py:36`
- **[error-handling] max_chars coercion can raise outside the executor's error handling** — `apps/assistant-service/src/assistant_service/core/tools/web_fetch.py:500`
- **[redundancy] Module-local SSRF validation is dead code; all protection lives in the shared safe_fetch module** — `apps/assistant-service/src/assistant_service/core/tools/web_fetch.py:121`
- **[error-handling] Permission-denied security events are fire-and-forget asyncio tasks with no tracking** — `src/api/deps.py:209`
- **[security] Embed token's IP-bound abuse identity is never verified against the client IP** — `src/api/v1/agent_public.py:141`
- **[implementation-bug] _allowed_origins wildcard handling is dead code; '*' config results in deny-all** — `src/api/v1/agent_public.py:107`
- **[redundancy] Dead MIME-type lookup: result of mime_type_map.get(...) is discarded** — `src/api/v1/assistant.py:996`
- **[security] Login endpoint has no IP rate limiting and can lock out arbitrary accounts** — `src/api/v1/auth.py:158`
- **[api-contract] OAuth state parsing breaks for user IDs containing ':' (API-key/anonymous identities)** — `src/api/v1/connectors.py:465`
- **[api-contract] Exam report listing is not tenant-scoped** — `src/api/v1/exams.py:284`
- **[security] get_service_schema bypasses allowed_services filtering applied by other service read routes** — `src/api/v1/services.py:988`
- **[security] API-key resolution uses a plain dict lookup (timing side channel)** — `src/core/auth/user_resolver.py:154`
- **[implementation-bug] AnonymousMemoryPolicy.cleanup_expired_threads is unimplemented (always returns 0)** — `src/core/auth/user_resolver.py:234`
- **[security] X-Real-IP is trusted as the final fallback whenever the direct peer is trusted** — `src/core/client_ip.py:97`
- **[redundancy] Duplicated _inputs_to_text/_response_to_text across dispatcher and SessionMiddleware** — `src/core/gateway/dispatcher.py:51`
- **[concurrency] Per-key lock vs. global cleanup race can recreate a key mid-flight** — `src/core/gateway/rate_limiter.py:96`
- **[code-quality] Invocation RateLimitMiddleware records a rate-limit metric for any exception from enforce()** — `src/core/middleware/rate_limit.py:62`
- **[security] StreamingAnonymousMiddleware sets the anonymous cookie without the Secure flag** — `src/core/middleware/streaming.py:995`
- **[implementation-bug] StreamingLoggingMiddleware records status_code=200 for streaming requests even on upstream errors** — `src/core/middleware/streaming.py:732`
- **[logic-bug] RedisRateLimitStorage zset member is str(timestamp), collapsing same-timestamp requests** — `src/core/ratelimit/storage.py:140`
- **[error-handling] Task queue silently drops tasks that fail processing (no retry/DLQ)** — `src/core/tasks/queue.py:107`
- **[security] Authorization header uses a trivially forgeable 'Bearer gateway-{user_id}' token** — `src/adapters/langgraph.py:140`
- **[logic-bug] tc_args.strip() crashes when tool args arrive as a non-string primitive** — `src/adapters/langgraph.py:948`
- **[code-quality] Dead code: _SECRET_JSON_VALUE_RE is defined but never used** — `src/adapters/langgraph.py:36`
- **[redundancy] Duplicate namespace-access validation with divergent write semantics** — `src/adapters/langgraph_proxy.py:1750`
- **[test-gap] No unit tests for most provider adapters (OpenAI/ComfyUI/TTS/Text2Image/GenericREST) or GRPC/WebSocket connectors** — `src/adapters/openai.py:14`
- **[error-handling] base64 decode failure silently falls back to UTF-8, sending garbage as audio** — `src/adapters/whisper.py:50`
- **[security] InProcessConnector allowlist uses startswith and permits bypass with an empty prefix** — `src/connectors/base.py:65`
- **[api-contract] revoke/delete return 404 (not 403) for a key owned by another user** — `src/services/auth/api_key_service.py:273`
- **[logic-bug] Daily batch aggregation can double-count when incremental flushes land after the batch overwrite** — `src/services/billing/aggregation_task.py:88`
- **[error-handling] update_usage silently swallows DB failures** — `src/services/billing/quota_service.py:498`
- **[implementation-bug] concurrency parameter of init_eval_outbox_worker is dropped** — `src/services/eval/eval_outbox_worker.py:211`
- **[redundancy] Duplicated _process_single_image vs _process_single_image_concurrent** — `src/services/knowledge/confluence/image_processor.py:363`
- **[config] Metrics day/hour buckets use local time while ingestion timestamps use UTC** — `src/services/metrics/metrics_recorder.py:56`
- **[redundancy] _get_callback_client / _callback_client are never used** — `src/services/task/task_manager.py:77`
- **[performance] Unbounded dataset metadata cache in fallback router** — `apps/knowledge-service/src/knowledge_service/api/router.py:64`
- **[security] Gemini embedding API key sent as a URL query parameter** — `apps/knowledge-service/src/knowledge_service/api/router.py:122`
- **[redundancy] Fallback router duplicates embedding/dataset-cache logic already in embedding.py and cache_manager** — `apps/knowledge-service/src/knowledge_service/api/router.py:30`
- **[error-handling] Eval route maps missing embedding key to HTTP 500** — `apps/knowledge-service/src/knowledge_service/api/routes/eval.py:63`
- **[api-contract] get_dataset_sources always returns empty confluence_bindings** — `apps/knowledge-service/src/knowledge_service/api/routes/knowledge.py:2416`
- **[implementation-bug] import_from_url ignores max_depth for children recursion** — `apps/knowledge-service/src/knowledge_service/services/knowledge/confluence/sync_service.py:940`
- **[code-quality] Dead statements in multimodal rerank path** — `apps/knowledge-service/src/knowledge_service/services/knowledge/retrieval_service.py:2038`
- **[error-handling] run_streaming/service.stream propagate render exceptions instead of emitting an 'error' event** — `packages/mcp-docgen-server/src/docgen/pipeline.py:115`
- **[security] DockerSandbox exec_python/exec_node embed code via json.dumps inside `bash -lc "..."` — unsound shell quoting** — `packages/mcp-docgen-server/src/docgen/sandbox/docker_backend.py:128`
- **[error-handling] Unknown template_name is silently swallowed in _brief_from_request** — `packages/mcp-docgen-server/src/docgen/service.py:156`
- **[concurrency] TemplateRegistry claims thread-safety but mutates _tenant dict without a lock** — `packages/mcp-docgen-server/src/docgen/templates.py:90`
- **[error-handling] mcp_endpoint crashes with AttributeError if the JSON body is a JSON-RPC batch array** — `packages/mcp-docgen-server/src/mcp_docgen_server/server.py:545`
- **[api-contract] GenerateDocumentOutput omits fields the server actually returns (used_llm, format)** — `packages/mcp-docgen-server/src/mcp_docgen_server/tool_schema.py:90`
- **[test-gap] No test covers the fresh-context vision critic awaiting, the docker sandbox, S3 store, artifact routes, or run_streaming** — `packages/mcp-docgen-server/tests/docgen/test_verifier.py:1`
- **[logic-bug] make stop/restart/logs run compose from parse-time $(shell pwd), not the project root** — `Makefile:96`
- **[config] Dev compose hardcodes /opt/venv/lib/python3.12/site-packages bind mounts** — `docker-compose.dev.yml:19`
- **[config] Gateway and frontend ports bind 0.0.0.0 while every other service binds 127.0.0.1** — `docker-compose.yml:320`
- **[performance] check_gateway_metrics makes two sequential HTTP requests every health check** — `scripts/new/common.sh:219`
- **[code-quality] assert_compose_owner compares raw path strings with no normalization or bypass** — `scripts/new/common.sh:114`
- **[security] env_value() uses eval-based indirection; is_placeholder() is defined but never used** — `scripts/new/init-env.sh:76`
- **[resource-leak] GatewayAI.close() is a no-op despite AutoCloseable + documented 'release all connections'** — `sdk/java/src/main/java/com/aigateway/ai/GatewayAI.java:150`
- **[redundancy] Multiple standalone test-scripts in tests/ duplicate the same manual 'print + return bool' pattern** — `tests/knowledge/test_kb_qa.py:1`
- **[test-gap] Weak secret check: tests for a single placeholder literal rather than absence of real credentials** — `tests/scripts/test_script_secret_defaults.py:8`
- **[test-gap] Fragmented micro-test files cover a single happy path each (8-10 line files)** — `tests/services/assistant/test_preference_extractor.py:6`
---


## 七、单元测试设计（针对关键接口）

> 以下测试方案针对本次审查发现的「零测试 / 伪测试 / 低覆盖」接口设计。按优先级排序。

### 7.1 P0 — 安全关键接口（必须先补）

| # | 接口 | 现状 | 建议测试 |
| --- | --- | --- | --- |
| 1 | `POST /api/v1/api-keys`（创建 API key） | 无 scope/tier 校验测试 | 断言 `user` 角色创建 `scopes=['admin']` 的 key 重新认证后仍只有 `user` 能力；`deps.get_auth_context` 绝不把 permission 字符串并入 roles |
| 2 | `POST /assistant/quiz/generate` & `/generate/stream` | 匿名可达付费 LLM | 未认证 → 401；超限 → 429；`model_id` 覆盖必须通过权限检查 |
| 3 | `POST /api/v1/internal/eval/ragas`（knowledge-service） | 无认证、接受任意 llm_config | 未认证 → 401/403；调用方提供 `base_url` → 拒绝；成本上限触发 |
| 4 | `ASSISTANT_APP__ALLOW_ANONYMOUS=true` 启动守卫 | 允许 secret+anonymous 并存 | 启动时二者同设 → 抛 RuntimeError；allow_anonymous=false 时伪造 X-User-Id/X-User-Roles → 拒绝 |
| 5 | Exam `list_reports` / `get_report` | 无租户作用域 | 租户 A 访问租户 B 的 exam/report → 404 |
| 6 | `Confluence sync_service.remove_pages` | `self.kb` 未定义（AttributeError 被吞） | `delete_documents=True` 时断言调用 `knowledge_service.delete_document` 且 `documents_deleted` 递增 |

### 7.2 P1 — 主入口接口（当前零测试）

| # | 接口 | 建议测试 |
| --- | --- | --- |
| 7 | `POST /invoke`（核心网关调用入口） | happy path、未知 service_id → 404/400、权限不足 → 403、QuotaExceeded → 429、校验失败 → 422；断言 `code`/`request_id` 契约字段 |
| 8 | `roles.py` 7 个 RBAC 路由 | create_role 持久化+返回 201；get_role 合并权限集；update_role 原子替换；delete_role 对仍被引用角色报错；7 路由逐一 403 鉴权拒绝 |
| 9 | `tenant_policies.py` 10 个路由 | PUT/GET/DELETE tool-policies 持久化；跨租户读 → 禁止；audit-log 按租户隔离；默认（无策略行）→ deny |
| 10 | `quota.py` 业务逻辑 | block→check 失败；unblock 恢复；reset 清计数保留基础额度；forecast 外推并钳制到 tier 上限；alert ack 幂等 |
| 11 | `knowledge.py` QA/batch/versions/dedupe/worker 路由 | `/qa`、`/qa/stream`、documents/batch-upload、version restore、maintenance/dedupe、worker/status、images/{segment_id} 越权 404 |

### 7.3 P1 — 已确认逻辑缺陷的回归测试

| # | 缺陷 | 建议回归测试 |
| --- | --- | --- |
| 12 | `langgraph._extract_remote_stream_event` 丢失文本 | 构造同时含 `tool_call_chunks`+`content` 的 chunk，断言 TEXT_DELTA 被发出 |
| 13 | `golden.py critical_pass_rate=0.0` | 零 critical 用例的通过套件 → apply_gate=pass |
| 14 | `_patch_xlsx` 清空全部公式 | 双 sheet，A 有 #DIV/0，B 的正确公式必须存活 |
| 15 | `streaming_writer` 触发器反复触发 | 多 chunk 含一个触发器 → 恰好一次 search_start，尾文增量流出 |
| 16 | `compressor.compress(preserve_recent=0)` | 断言 preserve_recent=0 时仍保留最近 N 条 |
| 17 | `_get_model_pricing` 前缀匹配 | 'gpt-4o' 与 'gpt-4o-mini' 同在缓存，输入 'gpt-4o-mini-2024-07-18' 必须按 mini 计价；两种缓存顺序各跑一次 |
| 18 | `eval/consumer` idempotency 不释放 | handler 首次抛异常 → 同一 consumer 重试并最终进 DLQ |

### 7.4 P1 — 伪测试 / 零断言修复

| # | 文件 | 修复 |
| --- | --- | --- |
| 19 | `tests/integration/embedding/test_embedding.py` | 改为 mock（respx/AsyncMock），断言返回向量维度/错误路径；**删除 `ssl._create_unverified_context` 全局副作用**；禁止在收集的 test_* 内调 live API |
| 20 | `tests/knowledge/test_knowledge_improvements.py` | 6 个函数全部改为真实断言（token 计数、Arabic 归一化、权重范围、0..1 边界、chunker token_limit） |
| 21 | `tests/services/test_chunking.py::test_image_metadata_flag` | 断言 `any(c.metadata.get("has_image"))` 或显式断言空结果 |
| 22 | `tests/test_chunking_config_chain.py` | 断言配置链合并结果与验证日志内容 |

### 7.5 测试基础设施改进

- 覆盖率门限 `--cov-fail-under=25` 过低，建议 ≥60（排除 scripts/ 与 web/）。
- CI 与 Makefile 仅跑聚焦子集，应把 `tests/api/`、`tests/unit/` 全量纳入。
- 删除 `tests/knowledge/test_kb_qa.py` 这类「print + return bool」脚本，或移入 `scripts/` 并加显式标志。

---

## 八、冗余代码与优化设计

### 8.1 高价值去重（按收益排序）

| # | 位置 | 冗余说明 | 优化设计 |
| --- | --- | --- | --- |
| 1 | `assistant-service/core/skills/docx|pptx|xlsx/scripts/office/*`（36 文件） | office 工具链（unpack/pack/soffice/validators/redlining/accept_changes/merge_runs/simplify_redlines）在三个格式树字节级重复；`mcp-docgen-server/_skills_data/*` 又复制一份 | 抽公共包 `assistant_office_kit/`（docx/pptx/xlsx 各自只保留格式特定适配层），三处引用同一实现；**安全修复只需改一处** |
| 2 | `assistant-service/core/docgen` vs `packages/mcp-docgen-server/src/docgen` | 两套 docgen：IR、planners、renderers、quality、storage、skills 几乎同构（docgen 三重复制） | 收敛为单一 `ai-docgen-core` 包；assistant-service 通过包依赖引用；同时消除 `_DEFAULT_SKILL_PATHS` 指向不存在目录的问题 |
| 3 | `gateway-core/dispatcher.py` vs `middleware/session.py` | `_inputs_to_text` / `_response_to_text` 重复实现 | 提取公共 `payload_text_utils` |
| 4 | `confluence/image_processor.py` `_process_single_image` vs `_process_single_image_concurrent` | 两条几乎相同的处理路径 | 合并为参数化版本（`process_single_image(img, *, concurrent=False)`） |
| 5 | `quota` 重置逻辑（`aggregation_task.py` vs `QuotaService`） | 两套每日额度重置实现（且 off-by-one 不一致） | 收敛到单一 QuotaService 方法，aggregation 调用之 |
| 6 | `langgraph_proxy.py:1750` vs 相关命名空间校验 | 重复的命名空间访问校验，写语义分歧 | 统一为单一 `_namespace_access_check` |
| 7 | `mcp/client.py` vs `manager.py` vs `assembler.py` | 重复的 credential/prompt-injection 脱敏正则 | 收敛到 `ai_gateway_core/security/redaction.py` |
| 8 | `dispatcher.py` vs `streaming.py` 限流/日志中间件 | 三套限流实现（in-memory/Redis-HTTP/Redis-streaming）计数语义不一致 | 收敛为统一限流内核，配置开关控制后端 |

### 8.2 结构性优化

| # | 优化 |
| --- | --- |
| 1 | **缓存统一作用域**：ToolOrchestrator._result_cache、ScenarioAwareRetriever、会话缓存、api-key 缓存全部按 `(tenant_id, user_id, session_id)` 作键并加 eviction（LRU/TTL）。 |
| 2 | **消灭死代码**：`SamplingLLMCaller`（从未实例化）、`_get_callback_client/_callback_client`、`_SECRET_JSON_VALUE_RE`、`fresh-context vision critic`（未接线）——删除或接线。 |
| 3 | **docgen 接线**：`DocgenService` + sandbox 目前未接入生产服务，`_DEFAULT_SKILL_PATHS` 指向不存在目录——要么接入 document_generator_tool，要么移除死代码。 |
| 4 | **Confluence sync 接线**：`knowledge-service` 的整个 confluence 同步子系统未接入 app——决定接入或删除。 |
| 5 | **错误语义收敛**：`except Exception` 全库约 1250+ 处，大量静默吞错（如 `remove_pages` 的 self.kb AttributeError）——对关键路径改为显式异常类型 + 结构化错误码。 |
| 6 | **前端/网关一致性**：文件 list/get 只读本地 uploads 目录与 S3/OSS 上传不一致——统一存储抽象。 |

### 8.3 设计建议（安全加固方向）

- **auth 中间件默认 fail-closed**：`anonymous_enabled` 默认应为 false；所有依赖「路由自行鉴权」的中间件改为显式白名单。
- **服务间信任边界**：KB/LangGraph 代理默认剥离客户端身份头（`_INJECTED_IDENTITY_HEADERS` 已定义但未应用），共享密钥未配置时应 fail-closed 而非降级匿名。
- **限流计数**：拒绝的请求不应计入滑动窗口（Redis 两个 limiter 均存在 zrem 缺失），否则攻击者主动触发的 429 会耗尽合法配额。

---

## 九、结论与优先级建议

**发布阻断（P0，先修）：**
1. API-key scope→admin 提权链（若 schema 确认无 scopes 列）
2. 匿名付费接口：quiz generate、RAGAS eval
3. `ASSISTANT_APP__ALLOW_ANONYMOUS=true` 认证绕过
4. 零断言伪测试（尤其 test_embedding.py 的 TLS 全局副作用）

**高优先（P1）：** exam 报告 IDOR、Confluence token 明文、跨租户缓存作用域、Java SDK Bearer 认证、迁移发现碰撞、docgen 生产部署 LLM 禁用 + 下载 URL 破损。

**维护性（P2）：** office 三格式代码收敛、docgen 双/三重复制收敛、限流统一、缓存 eviction、死代码清理、覆盖率门限提高。

**补充说明：** 本报告为只读审查产物，未对任何源码做修改。所有修复建议与测试设计均为实施方案；如需我按优先级实际落地修复与测试，请确认后执行。


---

# 附录 A：二次校验（真实性复核 + 覆盖补审）

**复核日期：** 2026-08-02（第一轮审查之后）
**方式：** 7 个深度复核代理（对 14 个争议 critical/high 重读代码）+ 7 个补审代理（对第一轮未深入覆盖的高价值生产文件组逐文件审查）+ 补审发现的对抗性验证。共 18 个代理，526 次工具调用。

## A.1 争议发现复核裁决

第一轮对 critical/high 的验证中，14 个发现被判为 PARTIAL / REFUTED / 未验证。本轮由独立代理**重读实际代码**给出最终裁决，并修正了部分严重级：

| API-key creation accepts arbitrary scopes/tier -> self-… | **CONFIRMED** | high | 103 | Any authenticated low-priv user can create an API key with scopes=['admin'] (and/or tier='admin'); the scopes are stored verbatim into the api_keys `permissions` column, and deps.get_user_context merges that column into  |
| Guest session identity is client-asserted (format-only … | **PARTIAL** | medium | 332 | Core defect confirmed: the active auth middleware (StreamingAuthMiddleware, wired in main.py:261; the auth.py AuthMiddleware is NOT wired) accepts X-Guest-Session after only a FORMAT check (_is_valid_session_id: UUID v4  |
| RatePolicyResolver.resolve short-circuits all other rat… | **PARTIAL** | low | 60 | The technical claim is TRUE: resolve() builds a service policy via _service_config_policy and, if not None, returns [service_policy] at rate_policy.py:59-60, never calling _load_rules — so global/tenant/user/operation/ap |
| Session->thread mapping is unscoped by tenant/service a… | **PARTIAL** | medium | 1404 | Facts verified true: (1) _session_to_thread_map (langgraph.py:62) is a class-level dict shared by all adapters, keyed only by session_id, written at lines 1361/1404 and read at 1349-1350, with NO eviction/cap/TTL anywher |
| Cross-tenant result cache in shared ToolOrchestrator… | **PARTIAL** | low | 235 | The unscoped cache is real as a latent design flaw, but cross-tenant disclosure is NOT reachable in current code. ToolOrchestrator IS a shared singleton: get_tool_orchestrator() (assistant_service.py:893-901) caches one  |
| Task-planning execution path always fails (invocation_c… | **CONFIRMED** | medium | 971 | CONFIRMED. _execute_single_task raises ValueError when invocation_context is None (tool_orchestrator.py:524-528), and AssistantService._execute_with_planning calls orchestrator.execute_plan(plan, working_memory) with NO  |
| SSE transport builds DocgenService with llm=None, silen… | **CONFIRMED** | medium | 368 | All factual components verified: (1) main_sse at server.py:368 hardcodes `DocgenService(artifact_store=store, llm=None)`; (2) BOTH shipped deployment manifests set MCP_TRANSPORT=sse (docker-compose.yml:22 and k8s/deploym |
| FreshContextVisionCritic never awaits messages.create o… | **PARTIAL** | low | 84 | Both facts confirmed. (a) FreshContextVisionCritic.review() (visual_verifier.py:68) is a SYNC method that calls `resp = client.messages.create(...)` (lines 84-89) with no await; the client comes from client_factory which |
| Zip Slip in skill scripts via zipfile.extractall… | **REFUTED** | low | 54 | The claim is refuted on the core technical fact. CPython's zipfile.ZipFile._extract_member filters path components with `invalid_path_parts = ('', os.path.curdir, os.path.pardir)` and drops any arcname component equal to |
| A: src/api/v1/roles.py (7 routes) has zero tests… | **CONFIRMED** | low | 89 | grep -rn 'api.v1.roles//roles' tests/ returned zero hits. A broader grep for 'roles' found only unrelated uses: auth fixtures in tests/conftest.py:48-101 (roles list param), DB migration column defs (test_agent_studio_mi |
| B: src/api/v1/tenant_policies.py (10 routes) has zero t… | **CONFIRMED** | medium | 67 | grep -rn 'tenant_policies/tool-policies/mcp-configs/audit-log' tests/ returned zero hits. File confirmed with 9 @router routes (not 10 as claimed): list_tool_policies:68, get_tool_policy:80, upsert_tool_policy:97, delete |
| C: src/api/v1/quota.py business logic (forecast/block/u… | **CONFIRMED** | medium | 714 | The only test files importing src/api/v1/quota.py are authz-level: tests/api/test_gateway_capability_matrix.py:15 imports 'from src.api.v1 import quota as quota_routes' and test_gateway_capability_matrix.py:185-202 asser |
| D: knowledge-service routes (knowledge.py) only test re… | **CONFIRMED** | low | 74 | At the route-handler level, knowledge.py non-retrieve routes are untested: create_dataset:74, update_dataset:105, delete_dataset:120, permissions CRUD (141-175), batch_upload_documents:388, create_document_url:517, uploa |
| Migration discovery keyed on 3-digit version prefix sil… | **CONFIRMED** | medium | 82 | Every sub-claim verified in actual code. (1) cli.py:82 compiles regex ^(\d{3})_(.+)\.sql$ and :87 sets version = match.group(1), so version IS the 3-digit prefix. (2) ls of database/migrations confirms duplicate prefixes |

**关键修正：**

1. **API-key 作用域 → admin 提权：升级为 CONFIRMED（原 PARTIAL）**。核实了完整机制：`api_keys` 表无 `scopes` 列但有 `permissions` 列，`APIKeyService._api_key_scope_column()` 将用户提交的 `scopes` 直接写入 `permissions` 列，`deps.get_user_context` 把该列合并进 roles，`api_keys.py:122` 等所有 `"admin" in roles` 判断随即通过。**这是本轮最重要的修正：一个真实的自提权漏洞，升级为必须优先处理的 critical 级问题。**
2. **RatePolicyResolver：REFUTED → PARTIAL（severity low）**。确认是"有意的、有测试覆盖的行为"，但**配置风险真实存在**——设置服务级限额会静默替换（而非叠加）全局/租户/用户/操作限额，是真实的配置 footgun，故从"无缺陷"修正为 low 级设计风险。
3. **Zip Slip：维持 REFUTED**。确认 CPython `zipfile.extractall` 会剥离 `..` 组件，不构成 Zip Slip——原误报排除。
4. **测试缺口（roles / tenant_policies / quota / knowledge routes）：全部 CONFIRMED**。通过实际 grep 确认无任何测试文件引用这些模块。
5. **Guest session / Session→thread / Task-planning / SSE llm=None / Migration discovery**：核心缺陷均确认存在，但严重级按可达性修正（多数降为 medium/low；Task-planning 在 `enable_task_planning` 关闭时不可达）。

## A.2 覆盖补审：新增发现

对第一轮未深入覆盖的 7 组高价值生产文件补审，新增 **87 个发现（4 high / 31 medium / 52 low）**。按组统计：

| 补审文件组 | critical | high | medium | low |
| --- | --- | --- | --- | --- |
| knowledge-service ingestion/processing (chunking, image/pdf/ | 0 | 0 | 5 | 10 |
| assistant-service core/files (document_parser, file_processo | 0 | 1 | 7 | 5 |
| assistant-service runtime memory (core/runtime/memory/* + co | 0 | 0 | 4 | 8 |
| assistant-service runtime context/skills/tools/security/sche | 0 | 1 | 4 | 7 |
| src/api/v1 (gateway routes + schemas + router + eval_export  | 0 | 1 | 4 | 8 |
| knowledge-service core (auth/errors/observability) + fallbac | 0 | 1 | 4 | 4 |
| assistant-service MCP core (connector_mcp.py, oauth.py, resi | 0 | 0 | 3 | 10 |

### A.2.1 新增 High 级发现（已验证）

- **[logic-bug] GOOGLE and GOOGLE_VERTEX default catalog share model IDs; GOOGLE_VERTEX silently overwrites GOOGLE in _models** — `apps/assistant-service/src/assistant_service/core/models/model_registry.py:818` ｜ **验证**: ✅ 确认
  - DEFAULT_MODELS defines identical ids for both providers (gemini-3-flash-preview, gemini-3-pro-preview, gemini-3.1-pro-preview, gemini-3.1-flash-lite-preview). __init__ inserts into self._models keyed by id only, iterating GOOGLE before GOOGLE_VERTEX, so the Vertex entries overwrite the AI-Studio ent

- **[error-handling] Scheduler job leases are never reclaimed: crashed workers permanently strand 'running' jobs** — `apps/assistant-service/src/assistant_service/core/runtime/scheduler/job_runner.py:86` ｜ **验证**: ⚠️ 部分确认
  - claim_due_jobs() only selects rows WHERE status='queued' AND run_at <= NOW(), setting status='running' with a 90-second lease_expires_at. There is no query anywhere (grep confirms) that reclaims status='running' rows whose lease_expires_at < NOW() and re-queues them, and no reaper loop. mark_done()/

- **[security] User-management routes are not tenant-scoped (cross-tenant enumeration/modification)** — `src/api/v1/users.py:212` ｜ **验证**: ✅ 确认
  - `list_users` calls `db.list_users_paginated(status=status, search=search, limit=page_size, offset=offset)` without passing `tenant_id`, even though the storage layer (packages/ai-gateway-core/.../database.py:5915) supports it and filters only when provided. `get_user`/`update_user`/`reset_user_passw

- **[security] Fallback retrieve endpoint has no tenant authorization (cross-tenant IDOR)** — `apps/knowledge-service/src/knowledge_service/api/router.py:74` ｜ **验证**: ✅ 确认
  - _get_dataset runs `SELECT ... FROM datasets WHERE dataset_id = $1 OR name = $1 LIMIT 1` with no tenant_id filter, and retrieve() then vector-searches that dataset's Qdrant collection directly. The full path enforces tenant isolation via require_dataset_access(user, dataset_id, required='viewer') (da


### A.2.2 新增 Medium 级发现（31 项）

- **[security] SSRF in _download_image: arbitrary user-influenced URL fetched with no SSRF guard** — `apps/knowledge-service/src/knowledge_service/services/knowledge/image_processing_queue.py:377`
  - _download_image() fetches task.storage_key directly with a bare httpx.AsyncClient().get() when the key starts with http:// or https://. There is no private/loopback IP rejection, no DNS pinning, and no redirect re-validation. The sibling module ingestion/document_image_extractor.py uses ai_gateway_c

- **[implementation-bug] Dispatcher calls nonexistent knowledge_service._ingest_document_internal** — `apps/knowledge-service/src/knowledge_service/services/knowledge/processing_dispatcher.py:138`
  - _process_text_only() and _process_multimodal() invoke self.knowledge_service._ingest_document_internal(document_id=..., dataset_id=...). A repo-wide grep finds only these two references; no definition exists anywhere (knowledge_service exposes ingestion_service.ingest_document and document_service m

- **[logic-bug] split_pdf zero-copy path returns page_end=0; corrected block is dead code** — `apps/knowledge-service/src/knowledge_service/services/knowledge/pdf_splitter.py:66`
  - When file_size <= max_size_bytes, the early return at lines 66-75 builds SplitResult(page_end=0) (comment 'filled below'), but the block that fills page_end=total_pages (lines 85-96) is unreachable because it is guarded by the same condition that already returned. Any consumer of the unsplit result

- **[logic-bug] section_title is never written, so heading-context embedding enhancement is dead** — `apps/knowledge-service/src/knowledge_service/services/knowledge/structured_document_parser.py:537`
  - _build_hierarchy() (lines 403-427) tracks the current heading by element id and sets element.parent_id and element.section_level, but never sets element.metadata['section_title'] with the heading text. _create_text_chunk() reads element.metadata.get('section_title') (lines 537-538) to prepend 'Secti

- **[logic-bug] CJK text without spaces bypasses token limits in _split_by_tokens** — `apps/knowledge-service/src/knowledge_service/services/knowledge/chunking.py:1271`
  - _split_by_tokens() splits an oversized sentence with words = sentence.split() and the final safety pass _hard_split_by_tokens() also splits on whitespace (t.split()). For CJK text with no whitespace and no 。！？sentence punctuation, a long paragraph is a single 'word'; its token count (tiktoken cl100k

- **[resource-leak] Shared _temp_dir causes same-basename collisions and unbounded disk accumulation on the long-lived FileProcessor** — `apps/assistant-service/src/assistant_service/core/files/file_processor.py:508`
  - _download_from_remote writes to Path(self._temp_dir.name) / Path(file_path).name. The temp dir is a single shared instance attribute; AssistantService creates one long-lived FileProcessor (assistant_service.py:822). Two sessions concurrently downloading different files with the same basename (e.g. r

- **[performance] No file-size guard anywhere in the processing path; whole files are read into memory and base64-tripled** — `apps/assistant-service/src/assistant_service/core/files/file_processor.py:746`
  - _process_image reads the entire file via file_path.read_bytes() and then base64-encodes it (~1.33x additional memory); _process_document parses the whole file; _process_pdf_as_images renders pages to PNG and base64-encodes all of them (bounded only by page count, not bytes). PDFConverter defines TAR

- **[security] DocumentParser._resolve_path trusts absolute paths with no containment check, inconsistent with FileProcessor** — `apps/assistant-service/src/assistant_service/core/files/document_parser.py:104`
  - In _resolve_path, when the input is an absolute path, needs_security_check=False and the path is used as-is (only existence checks). FileProcessor._resolve_path_async explicitly rejects absolute paths from untrusted input, but the strategy code path (GeminiFileStrategy/VisionModelStrategy) calls Doc

- **[resource-leak] PyMuPDF document is not closed if an exception occurs mid-conversion** — `apps/assistant-service/src/assistant_service/core/files/pdf_converter.py:285`
  - fitz.open() creates doc, and doc.close() is called only after the page loop completes (line 285). If any page raises (malformed content, get_pixmap failure, progress-callback error), control jumps to the except handler and doc.close() is never executed, leaking the document handle. The whole convert

- **[config] Invalid access_level in DB rows fails open to PUBLIC; access_level is not enforced on the chat path** — `apps/assistant-service/src/assistant_service/core/models/model_registry.py:861`
  - load_models_from_database maps an unparseable access_level string to ModelAccessLevel.PUBLIC (except ValueError: access_level = ModelAccessLevel.PUBLIC). Combined with the routes/models.py rule where an unknown level is 'permissive' (anything non-admin), a typo'd/absent access level silently grants

- **[logic-bug] VisionModelStrategy default max_pages=None converts every page of a PDF to base64 with no cap (currently dead code)** — `apps/assistant-service/src/assistant_service/core/files/file_strategy.py:200`
  - VisionModelStrategy.__init__ defaults max_pages=None and the factory (get_strategy/_create_strategy) creates it without max_pages; process() then calls converter.convert(max_pages=None), rendering every page to base64 in memory. The FileProcessor.process_files path is safely capped at MAX_PDF_PAGES=

- **[logic-bug] GeminiFileStrategy PROCESSING poll loop has no timeout (dead code, latent hang)** — `apps/assistant-service/src/assistant_service/core/files/file_strategy.py:365`
  - The while uploaded_file.state.name == 'PROCESSING' loop polls every 1s with no deadline and no cap on iterations. If Gemini's file-processing state never transitions to ACTIVE/FAILED, the coroutine spins forever, holding a task and (if the caller awaits it) blocking the request. Also, errors inside

- **[concurrency] Session advisory DB lock and sole pooled connection held across external Qdrant HTTP calls** — `apps/assistant-service/src/assistant_service/core/runtime/memory/indexer.py:502`
  - `_source_database_lock` (lines 152-195) acquires a `pg_advisory_lock` on a single `pool.acquire()` connection and yields it as `database` for the whole `_index_source_locked`/`_replace_source_derivatives` critical section. Inside that section the code calls the network-facing vector store: `ensure_c

- **[performance] Vector score floor at 0.5 lets irrelevant vector matches outrank relevant text hits** — `apps/assistant-service/src/assistant_service/core/runtime/memory/retriever.py:294`
  - `norm_score = max(0.0, min(1.0, (raw_score + 1.0) / 2.0))` maps Qdrant cosine similarity 0 (orthogonal/random) to 0.5. Every vector hit therefore contributes at least `0.65 * 0.5 = 0.325` to the fused `final_score`, while text scores are inverse-rank (`1.0/rank`, retriever.py line 199) with weight 0

- **[security] Prompt-injection/secret threat flags are recorded but never enforced on persistence or retrieval** — `apps/assistant-service/src/assistant_service/core/runtime/memory/lifecycle.py:87`
  - `scan_memory_text` (lifecycle.py 87-100) sets `prompt_injection`/`secret_like` and the result is stored in every `MemoryWriteResult.threat_scan` (source_store.py 510/573/607), but no code path reads the flag to block, redact, or quarantine the write. The flagged text is persisted verbatim (only `<co

- **[logic-bug] `messages[:-0]` negative-slice bug: preserve_recent=0 compresses nothing, and target_tokens is never enforced** — `apps/assistant-service/src/assistant_service/core/memory/compressor.py:202`
  - `messages_to_compress = messages[:-preserve_recent]` — when `preserve_recent == 0`, `[:-0]` is `[:0]` = `[]` and `messages[-0:]` is the whole list, so the function silently keeps everything and returns `summary=""` instead of compressing all messages. Separately, `effective_summary_tokens = max(100,

- **[logic-bug] StreamingWriter never advances the buffer past a matched trigger: repeated identical KB searches and stalled text output** — `apps/assistant-service/src/assistant_service/core/content/streaming_writer.py:338`
  - On a trigger match the code does buffer = buffer[position:] (line 338), which keeps the trigger phrase itself at the start of the buffer. On every subsequent stream delta _find_trigger() matches the same phrase at position 0 again, so the trigger branch re-runs: _extract_verification_query() returns

- **[security] SandboxResolver has no deny-by-default: unknown tools execute in the default sandbox with no approval even under the 'safe' profile** — `apps/assistant-service/src/assistant_service/core/runtime/security/sandbox_resolver.py:73`
  - resolve() classifies only HIGH_RISK_TOOLS and MEDIUM_RISK_TOOLS (denylists) and falls through to SandboxDecision(allowed=True, sandbox='default', requires_approval=False) for every other tool name, regardless of execution_profile. AssistantPolicyEngine.evaluate_tool() (policy_engine.py:105-110) has

- **[security] PIIFilter phone and api_key regexes miss the target market's formats: Chinese mobile numbers are not redacted** — `apps/assistant-service/src/assistant_service/core/runtime/security/pii_filter.py:23`
  - The phone pattern is US-centric 3-3-4 (optional +CC). An 11-digit Chinese mobile such as '13812345678' or '+86 138 1234 5678' does not match (after the optional country code only 10 digits remain for the 3-3-4 structure). The api_key pattern only matches sk_/gw_ prefixed keys, missing ghp_, AKIA, xo

- **[security] Client-controlled execution_profile removes the approval gate for medium-risk tools with no user-tier authorization** — `apps/assistant-service/src/assistant_service/core/gateway/request_router.py:43`
  - route() accepts execution_profile from the request config and only allowlists the string ('safe'/'balanced'/'power') with no user-tier check. policy_engine.evaluate_tool() (line 97) requires approval for MEDIUM_RISK_TOOLS only when profile == 'safe', and the v2 lattice profile layer (execution_gatew

- **[security] Unauthenticated /proxy/{service}/_health probes service existence and leaks infra state** — `src/api/v1/proxy.py:590`
  - In `transparent_proxy_handler`, `if path == "_health": return await proxy_service_health(...)` runs BEFORE `check_service_authorization`, and the separately registered `@router.get("/{service_name}/_health")` (line 981) has no auth dependency at all. `proxy_service_health` calls `proxy.health_check(

- **[config] /kb-tools/* proxy bypasses the per-user rate limiter that /knowledge/* enforces** — `src/api/v1/kb_tools.py:29`
  - The `knowledge.py` route explicitly checks `MultiDimensionRateLimiter` before proxying, but `kb_tools.py` `proxy_kb_tools` forwards to the same KB service with no per-user/tenant rate-limit check (docstring even says "without rate limiting"). Both hit the identical backend, so a caller can trivially

- **[security] Preview chat streams forward client-supplied attachment refs without gateway ownership validation** — `src/api/v1/agent_runtime.py:1305`
  - In `preview_chat_stream` and `version_preview_chat_stream`, `payload.attachments` (client-provided `{artifact_id, filename, mime_type}`) are passed straight into `_runtime_body` and signed into the assistant envelope, never passed through `_resolve_runtime_attachments` — unlike the published/API pat

- **[security] Quiz payload + grading answer keys fetched without tenant scoping at share creation** — `src/api/v1/conversation_shares.py:105`
  - `_collect_quiz_payloads` runs `SELECT ... FROM quizzes WHERE id = $1` and `SELECT ... FROM quiz_questions WHERE quiz_id = $1` with only the UUID, no `tenant_id` filter. `create_share` freezes these into the share snapshot, including `quiz_answer_keys` (correct answers + explanations) stored on `conv

- **[code-quality] get_logger shim bypasses the platform JSON logging and request_id contextvars** — `apps/knowledge-service/src/knowledge_service/core/observability/logging.py:13`
  - get_logger builds a standalone `structlog.wrap_logger(logging.getLogger(name), processors=[add_log_level, add_logger_name, ConsoleRenderer()])`. The processor chain ends in ConsoleRenderer with no merge_contextvars/render_to_log_kwargs, so kwargs are flattened into a single pre-rendered console stri

- **[security] verify_password treats any non-bcrypt-prefixed stored hash as plaintext** — `apps/knowledge-service/src/knowledge_service/core/auth/password.py:22`
  - The fallback `return hmac.compare_digest(plain_password, hashed_password)` directly compares the submitted password to the stored value whenever it does not start with $2a$/$2b$/$2y$. There is no password-hashing function anywhere in the service (no bcrypt.hashpw / generate_password_hash found), so

- **[error-handling] GatewayError.status_code is dead metadata; no central mapping means unhandled subtypes become 500s** — `apps/knowledge-service/src/knowledge_service/core/exceptions.py:6`
  - The exception hierarchy stores status_code (401/403/404/422/429) but nothing reads it: main.py registers no exception handler, and routes manually re-map only PermissionDeniedError->403 and ValidationFailedError->400 (routes/knowledge.py:83-101). NotFoundError/RateLimitError/AuthenticationRequiredEr

- **[api-contract] Fallback router does not preserve the full router's URL shape or response contract** — `apps/knowledge-service/src/knowledge_service/main.py:534`
  - The full router serves routes under /api/v1 (e.g. /api/v1/datasets/{id}/retrieve in routes/knowledge.py), while the fallback api_router is mounted at /api/v1/knowledge with routes /datasets, /{dataset_id}/retrieve, /worker/status. When full-route imports fail, every existing gateway/client call to t

- **[resource-leak] Per-connection semaphore cache grows unboundedly (no eviction)** — `apps/assistant-service/src/assistant_service/core/mcp/runtime.py:290`
  - MCPRuntimeService._connection_semaphore caches one asyncio.Semaphore per `tenant_id:connection_id` key and never evicts. The sibling _connection_breaker path has a `len(...) > 500` eviction (lines 333-339), but _connection_semaphore has none — the dict only grows when a limit changes (entry replaced

- **[error-handling] OAuth access tokens can never be refreshed; expired tokens break the connection while health stays 'healthy'** — `apps/assistant-service/src/assistant_service/core/mcp/oauth.py:428`
  - complete() stores the refresh_token via put_oauth_credential and returns refresh_configured=True, but nothing in the codebase ever consumes the refresh token: there is no grant_type=refresh_token path, no expiry check on credential resolution, and MCPClient treats the token as a static Bearer. When

- **[logic-bug] Tool-level (isError) results are recorded as full success, resetting server health and clearing error history** — `apps/assistant-service/src/assistant_service/core/mcp/runtime.py:774`
  - In invoke()'s success path, `success=not counts_toward_circuit(failure)` evaluates to True when result.is_error is set (failure is MCP_REMOTE_TOOL_ERROR -> APPLICATION, counts_toward_circuit=False), while simultaneously passing error_code='MCP_REMOTE_TOOL_ERROR'. The repository's record_runtime_resu


### A.2.3 新增 Low 级发现（52 项）

- **[code-quality] Duplicate module-level chunk_text(): first (language-aware) definition is shadowed** — `apps/knowledge-service/src/knowledge_service/services/knowledge/chunking.py:2964`
  - Two module-level functions named chunk_text exist: line 318 (chunk_size=1000, chunk_overlap=200, language-aware Arabic separators) and line 2964 (chunk_size=500, overlap=None, mode=..., chunk_overlap alias). In Python the second definition silently replaces the first, so the Arabic-separator and lan

- **[api-contract] L2 section vector payload stores truncated text[:500] while DB stores full text** — `apps/knowledge-service/src/knowledge_service/services/knowledge/hierarchical_indexer.py:521`
  - _index_sections() writes payload['text'] = segment.text[:500] into Qdrant but inserts the full text into segment_rows (line 540). The DB and vector payload therefore disagree. The current hierarchical_retriever._enrich_with_context() reads parent context from the DB (line 461-463), so today's impact

- **[error-handling] describe_image re-raises VLMError without retry; only connection errors are retried** — `apps/knowledge-service/src/knowledge_service/services/knowledge/vlm_service.py:250`
  - Inside the retry loop, `except VLMError: raise` (line 250) bypasses the retry/backoff logic entirely. API-level errors (status >= 400, missing output/choices/text — including transient 429 rate limits) are thrown immediately, so max_retries effectively applies only to transport exceptions.

- **[code-quality] describe_images_batch indexes failure-message placeholders as content** — `apps/knowledge-service/src/knowledge_service/services/knowledge/vlm_service.py:333`
  - For failed images, describe_images_batch returns ImageDescription(description=f'[图片描述生成失败: {result}]') which is then embedded and stored as searchable segment text. Error text becomes part of the index and can surface in retrieval results.

- **[concurrency] base_url override mutates module-global dashscope.base_http_api_url** — `apps/knowledge-service/src/knowledge_service/services/knowledge/vlm_service.py:116`
  - When base_url is provided, DashScopeVLMService sets dashscope.base_http_api_url = base_url, a module-level global. Creating multiple service instances (or a shared process serving multiple tenants) with different base_urls causes cross-instance interference — the last constructed instance redirects

- **[code-quality] _process_simple ignores content_bytes/filename/mime_type and re-ingests from DB** — `apps/knowledge-service/src/knowledge_service/services/knowledge/enhanced_ingestion.py:296`
  - _process_simple(dataset_id, document_id, content_bytes, filename, mime_type) never uses the last three arguments; it calls self.kb_service.ingest_document(dataset_id, document_id), which re-processes whatever is already stored in the database. This matches the (dataset_id, document_id) signature (ve

- **[performance] PDFExtractor re-opens the full PDF per page in parallel threads, multiplying memory** — `apps/knowledge-service/src/knowledge_service/services/knowledge/ingestion/document_image_extractor.py:477`
  - _extract_page_images_parallel() opens a fresh fitz document from the full content bytes for every page (fitz.open(stream=content)), while _extract_sync_parallel() also holds the main doc open. With up to min(4, cpu_count) worker threads, peak memory is roughly (threads+1) x PDF working set, which fo

- **[implementation-bug] doc.page_count accessed after doc.close(); doc.close() may run twice** — `apps/knowledge-service/src/knowledge_service/services/knowledge/pdf_image_processor.py:213`
  - In process_pdf_bytes(), doc.close() is called at line 201, then line 213 builds PDFExtractionResult with page_count=doc.page_count if hasattr(doc,'page_count'). Accessing a closed fitz document can raise (caught by the outer except at line 217, which then calls doc.close() a second time). The double

- **[performance] SiliconFlow OCR retries are stacked: backend loop x service _call_with_retry** — `apps/knowledge-service/src/knowledge_service/services/knowledge/vlm_ocr_service.py:199`
  - _SiliconFlowOCRBackend.call() has its own retry loop (max_retries from service), and VLMOCRService._call_with_retry() wraps every backend call with another max_retries loop. With defaults this yields up to 2x3=6 attempts with cumulative backoff for a persistently failing key, tripling latency and co

- **[error-handling] flush_batch does not validate vector count or None vectors against batch_meta** — `apps/knowledge-service/src/knowledge_service/services/knowledge/vision_pdf_processor.py:120`
  - flush_batch() zips embedder output with batch_meta by index (for idx, vector in enumerate(vectors): meta = batch_meta[idx]) then clears both lists. If embed_images returns fewer vectors than images, the surplus batch_meta entries are silently dropped (segments never created, no error logged); if a v

- **[error-handling] Document parse failures are silently swallowed: model gets no content and no user-visible error** — `apps/assistant-service/src/assistant_service/core/files/file_processor.py:850`
  - _process_document catches DocumentParseError and returns ('', False, metadata) with only metadata['parse_error'] set. In process_files, an empty text result adds nothing to text_parts and no error text, so the model receives zero context about the failed file and cannot tell the user (contrast with

- **[logic-bug] base64.b64decode without validate=True silently corrupts malformed artifact payloads** — `apps/assistant-service/src/assistant_service/core/artifacts.py:112`
  - base64.b64decode(content_base64) is called with default validate=False, so non-alphabet characters are silently discarded and no exception is raised for most malformed inputs. The surrounding try/except only catches real decoding errors; garbage input is decoded into corrupted bytes and then stored

- **[performance] Processing cache key ignores content; 8-hex-char file ids make stale hits plausible, and large base64 blobs are cached 24h** — `apps/assistant-service/src/assistant_service/core/files/file_processor.py:283`
  - _get_cache_key uses only api_path + vision-mode ('file_proc:{api_path}:{mode}'), no content hash or mtime (the comment acknowledges this). File ids are str(uuid4())[:8] (32 bits of entropy), so across a busy tenant the birthday bound is ~65k uploads for a ~50% id collision; a re-upload colliding on

- **[redundancy] preprocess_file duplicates process_files logic with a divergent subset of behavior** — `apps/assistant-service/src/assistant_service/core/files/file_processor.py:343`
  - preprocess_file re-implements the image/PDF/document branches of process_files (including separate fallback and serialization code) with slightly different semantics (e.g. it mutates metadata with fallback fields inline, and it writes a JSON-cached result that process_files must then reverse-map). T

- **[test-gap] No test coverage for file_processor.py, pdf_converter.py, file_strategy.py, artifacts.py, or the model-registry collision** — `apps/assistant-service/src/assistant_service/core/files/file_processor.py:1`
  - Repository-wide grep found only tests/unit/test_document_parser.py covering this file group; file_processor, pdf_converter, file_strategy, artifacts, and model_registry (including the default-catalog collision, the path-containment logic, the temp-dir lifecycle, and the strategy selection) have no d

- **[concurrency] Advisory-lock unlock delegated to a spawned task can leak the session lock under repeated cancellation** — `apps/assistant-service/src/assistant_service/core/runtime/memory/indexer.py:184`
  - The `finally` block creates `unlock_task = asyncio.create_task(connection.execute("SELECT pg_advisory_unlock(...)"))` then `await asyncio.shield(unlock_task)`. Under a single cancellation shield holds, but if the task is cancelled again while awaiting the shield, `except asyncio.CancelledError: awai

- **[security] DailyMemoryReflector captures raw user sentences as long-term facts with no PII redaction** — `apps/assistant-service/src/assistant_service/core/runtime/memory/reflector.py:46`
  - `build_reflection` copies user message content (up to 200 chars each) into `facts` when it contains markers like "prefer/喜欢/习惯/always/usually", and embeds the raw sentences into the summary lines. Unlike `sync_turn_to_memory` (which runs PII redaction before append), this path stores the user's verb

- **[resource-leak] Lock files under .locks/ are created but never removed** — `apps/assistant-service/src/assistant_service/core/runtime/memory/source_store.py:387`
  - `_exclusive_path_lock` opens `lock_path = lock_dir / f"{sha256(path)}.lock"` with O_CREAT and flocks it, but nothing ever unlinks the lock file. Every unique source path ever written/read adds a permanent `.lock` file to its parent's `.locks/` directory.

- **[logic-bug] Fact dedup `_contains_entry` uses substring matching and silently drops legitimate distinct facts** — `apps/assistant-service/src/assistant_service/core/runtime/memory/source_store.py:462`
  - `_contains_entry` normalizes lines and checks `normalized_entry in normalized_existing`. A short fact such as "prefer markdown" or "usually coffee" is considered a duplicate if the same string appears anywhere as a substring of an existing line (e.g. an earlier sentence "I prefer markdown for docs")

- **[logic-bug] Language/verbosity/name extraction has coverage and case-sensitivity gaps** — `apps/assistant-service/src/assistant_service/core/memory/preference_extractor.py:66`
  - The ja language patterns include "日本語/日语/japanese" but not the very common "日文", so "请用日文" yields no language preference. Verbosity English keywords ("more detail", "comprehensive", "in detail", "brief", "short", "concise") are tested with case-sensitive `kw in text` on the raw text rather than `tex

- **[config] memory_policy_enabled treats any non-'off' value as enabled** — `apps/assistant-service/src/assistant_service/core/runtime/memory/lifecycle.py:135`
  - `mode = str(memory_mode or '').strip().lower(); return mode != "off" and profile != "off"`. Unknown, misspelled, or garbage values ("on", "true", "enabled", "full") all evaluate as enabled. The docstring warns callers must never let a model-selected argument re-enable memory, but the function itself

- **[performance] inspect() runs 2x full SQL manifests plus full Qdrant point inventories per principal** — `apps/assistant-service/src/assistant_service/core/runtime/memory/governance_cleanup.py:332`
  - For every principal (up to `_MAX_PRINCIPALS` = 10,000) `inspect` calls `_source_manifests(cutoff=None)` and again `_source_manifests(cutoff=cutoff)` (each a full scan of `assistant_memory_sources` + chunk subquery + `list_scoped_source_records`), then `_vector_manifests` which scrolls every matching

- **[redundancy] PRESERVE_PATTERNS 'tables' and 'json' are dead configuration** — `apps/assistant-service/src/assistant_service/core/memory/compressor.py:44`
  - `PRESERVE_PATTERNS` defines patterns for urls, code_blocks, tables, and json, but `compress()` only ever calls `_extract_all(..., 'urls')` and `_extract_all(..., 'code_blocks')` (lines 206-207). The tables/json patterns and the 'json'/'tables' branches in `_extract_all` are unreachable in practice.

- **[error-handling] LLM task decomposition keeps hallucinated/unavailable tool names when no alternative exists** — `apps/assistant-service/src/assistant_service/core/tasks/task_planner.py:1357`
  - _parse_llm_response() validates each tool against available_tools and substitutes an alternative when found, but when no same-type alternative exists it silently keeps the LLM-supplied tool name (e.g., a hallucinated 'execute_python_code' when only kb_search is available). The task is still added to

- **[logic-bug] Duplicate task IDs produce a spurious CircularDependencyError instead of a clean validation error** — `apps/assistant-service/src/assistant_service/core/tasks/task_planner.py:1412`
  - analyze_dependencies() builds task_ids and task_map keyed by id; duplicate ids collapse, so the Kahn loop can never reach len(processed) == len(tasks) and falls into the cycle branch, raising CircularDependencyError with an empty cycle message. validate_plan() checks duplicates, but create_plan()/an

- **[redundancy] HIGH_RISK branch returns the same decision for safe and non-safe profiles (dead code)** — `apps/assistant-service/src/assistant_service/core/runtime/security/sandbox_resolver.py:51`
  - In resolve(), for HIGH_RISK_TOOLS the 'if profile == "safe"' branch and the trailing return are byte-for-byte identical (allowed=True, sandbox='docker_restricted', requires_approval=True), differing only in the reason string. The profile check has no behavioral effect.

- **[config] Unknown lane names silently fall back to a concurrency limit of 1** — `apps/assistant-service/src/assistant_service/core/runtime/tools/lane_scheduler.py:27`
  - get_limit() returns self._lane_limits.get(lane, 1) and _get_semaphore() lazily creates a Semaphore for any lane string. A misspelled or unexpected lane is silently serialized to limit 1 with no warning, and every distinct lane string allocates its own unbounded semaphore entry.

- **[resource-leak] Sessions with a non-empty active_tasks set are never expired or evicted, and there is no task-context reaper** — `packages/ai-gateway-core/src/ai_gateway_core/tasks/task_manager.py:408`
  - _get_or_create_session() only reaps an expired session when active_tasks and active_contexts are both empty; _cleanup_expired() and _evict_oldest() apply the same guard. If register_task() succeeded but complete_task() is skipped (exception between registration and the finally, cancellation inside t

- **[code-quality] tokens_by_category sums attribution-only overlays, so summing categories does not equal total_tokens** — `apps/assistant-service/src/assistant_service/core/runtime/context/cost_breakdown.py:203`
  - total_tokens is the sum of transport contributors only (attribution_only excluded), but tokens_by_category adds every contributor including attribution-only source overlays (injected_files, skills, memory, summaries). A consumer summing tokens_by_category will over-report versus total_tokens, and th

- **[config] memory_profile/memory_mode coherence is one-directional** — `apps/assistant-service/src/assistant_service/core/gateway/request_router.py:68`
  - route() forces memory_profile='off' when memory_mode='off' (line 68-69), but the reverse is not enforced: a client can request memory_mode='strict' with memory_profile='off', yielding a routed request that promises aggressive memory usage while disabling memory profiling, and memory_mode='auto' (the

- **[security] Anonymous quiz-attempt identity is header-spoofable, letting one actor consume another viewer's single attempt** — `src/api/v1/conversation_shares.py:354`
  - `_resolve_anon_id` prefers the `ag_anon_id` cookie, then the client-supplied `X-Anon-Id` header, then IP. `submit_shared_quiz` keys the single-attempt cache on `(share_code, anon_id, quiz_id)`. An attacker can set `X-Anon-Id` to a victim's anon id (or cookie) and submit a wrong answer first, permane

- **[code-quality] Dead code after share-code collision raise; unguarded json.loads can 500** — `src/api/v1/conversation_shares.py:251`
  - In `create_share`, the `for ... else` block raises HTTPException(409) then has an unreachable `share_code = _generate_share_code()` after it (lines 251-260). Also `history = session["history"] if isinstance(..., (list, dict)) else json.loads(session["history"])` will raise an unhandled `json.JSONDec

- **[error-handling] await inside `except BaseException` while handling GeneratorExit (client disconnect)** — `src/api/v1/agent_runtime.py:1038`
  - In `_record_idempotent_stream.recorded()`, `except BaseException: await repository.fail_runtime_idempotency(...)` catches GeneratorExit. Awaiting inside an async generator's GeneratorExit handler raises RuntimeError("async generator ignored GeneratorExit"), which replaces the intended fail-state wri

- **[api-contract] List endpoints report page length as `total` instead of the real result count** — `src/api/v1/conversations.py:225`
  - `list_conversations` returns `"total": len(threads)` and `get_messages` returns `"total": len(messages)` — i.e., the current page size, not the underlying total. Any client implementing pagination from these fields will compute wrong page counts, and the response model advertises a total it does not

- **[error-handling] Broad `except Exception` returns 500 with raw error string (same in providers.py)** — `src/api/v1/models.py:132`
  - `create_model` wraps the whole write in `try/except Exception` and raises `HTTPException(500, detail=str(e))`; `create_provider` / `create_provider_from_template` do the same. Raw exception text (SQL text, service internals) is returned to the client, and all non-duplicate-key failures collapse into

- **[redundancy] Large dead code in disabled direct-upload endpoints** — `src/api/v1/presign.py:39`
  - All three `/presign` endpoints return 501 (direct upload disabled), yet the module retains the full unused implementation: `_upload_sessions` dict, `_cleanup_expired_sessions`, `MAX_UPLOAD_SESSIONS`, `_validate_document_access` (lines 39-167). None of it is reachable. It is misleading and rots (e.g.

- **[code-quality] Deprecated pydantic v2 APIs (`UserResponse.__fields__`, `body.dict()`)** — `src/api/v1/users.py:179`
  - `_build_user_response_payload` uses `UserResponse.__fields__` and `UserResponse(**response_data)`; `update_user` uses `body.dict(exclude_unset=True, ...)` and `ProfileUpdate`/`UserUpdate` use legacy `@validator`. Pydantic v2 deprecates `__fields__` and `.dict()` (removal in v3), so these emit deprec

- **[security] create_session accepts an arbitrary service_id without service-access validation** — `src/api/v1/sessions.py:109`
  - `POST /sessions` takes `body.service_id` from the client and passes it directly to `session_manager.create` with no check that the user is permitted to use that service. Other surfaces (e.g., the transparent proxy and service-access resolver) enforce allowed/denied service scopes; this route does no

- **[error-handling] Fallback retrieve silently returns 200 empty results when embedding or vector search fails** — `apps/knowledge-service/src/knowledge_service/api/router.py:254`
  - If api_key resolves to empty or the provider call fails, _get_query_embedding returns None and retrieve returns {'results': []} with HTTP 200; likewise, when request.app.state.qdrant is None (init failed) the search block is skipped entirely. No error is logged or surfaced, so an empty response is i

- **[performance] _dataset_cache grows unbounded and mixes name/tenant lookups** — `apps/knowledge-service/src/knowledge_service/api/router.py:64`
  - _dataset_cache never evicts entries (reads only check the 60s TTL validity; nothing removes old keys), so every distinct dataset_id/name accumulates for the process lifetime. Because _get_dataset also matches by `name` and the cache key is only the raw string, a name collision can serve one tenant's

- **[logic-bug] worker_status endpoint is a hardcoded stub even when the real worker is running** — `apps/knowledge-service/src/knowledge_service/api/router.py:300`
  - worker_status always returns WorkerStatus(running=False, queued_tasks=0, active_tasks=0), while the full routes report real queue state from app.state.knowledge_worker. In fallback mode this endpoint lies to ops/monitoring about worker health.

- **[config] /health/ready reports ready even when KnowledgeService/worker initialization fails** — `apps/knowledge-service/src/knowledge_service/main.py:344`
  - app.state._ready = True is set unconditionally after the try block that initializes KnowledgeService/worker; a failure is only logged as knowledge_service_init_partial. /health/ready (main.py:504-507) keys readiness only off _ready, db, and qdrant, so readiness can be 'ready' while knowledge_service

- **[security] initialize() mutates a shared http_client's default Authorization/Origin headers (credential mixing)** — `apps/assistant-service/src/assistant_service/core/mcp/client.py:421`
  - When a caller injects http_client, initialize() calls self._http.headers.update(headers) with the server-specific Authorization (Bearer api_key) and Origin headers. _jsonrpc/_notify only set MCP-Protocol-Version and Mcp-Session-Id per request, so Authorization always comes from the client default he

- **[resource-leak] MCPManager.initialize_all leaks the AsyncClient when list_tools() raises** — `apps/assistant-service/src/assistant_service/core/mcp/manager.py:55`
  - In _init_one, `client = MCPClient(config); await client.initialize(); tools = await client.list_tools(); self._clients[config.name] = client`. If initialize() or list_tools() raises, the client is never stored and never closed; the httpx.AsyncClient it created internally (owned) is leaked until GC.

- **[logic-bug] operation_started set True before the request is actually dispatched** — `apps/assistant-service/src/assistant_service/core/mcp/client.py:545`
  - call_tool sets operation_started = True before entering the retry loop, i.e. before _jsonrpc has even resolved DNS or opened a connection. A pre-dispatch transport failure (MCP_DNS_UNAVAILABLE, connection-refused MCP_UPSTREAM_UNAVAILABLE) on a side-effecting write is then classified by decide_mcp_fa

- **[concurrency] _circuit_for eviction can discard a breaker with an in-flight lease, losing failure history** — `apps/assistant-service/src/assistant_service/core/mcp/client.py:197`
  - When self._circuits exceeds 500 entries, the least-recently-touched breaker is popped. A concurrently in-flight request holding a lease from the evicted breaker will later record_success/record_failure against that detached breaker object, while the freshly-created replacement breaker starts back at

- **[error-handling] int(getattr(context, 'max_retries', 0) or 0) can raise, masked as a transport error** — `apps/assistant-service/src/assistant_service/core/mcp/runtime.py:370`
  - _invocation_policy coerces context.max_retries with int(); if a caller provides a non-numeric value (e.g. a string), ValueError/TypeError propagates. In invoke() this is thrown after authorization, so it is caught by the broad `except Exception: error_code = 'MCP_UPSTREAM_UNAVAILABLE'` (line 841), m

- **[config] local_exception for allow_private_network only matches .local/.internal hostnames, not literal private IPs** — `apps/assistant-service/src/assistant_service/core/mcp/client.py:274`
  - _validate_url computes local_exception = platform_managed and ((allow_localhost and is_local_name) or (allow_private_network and internal_name)). A platform-managed config with allow_private_network=True pointing at http://192.168.1.5 is rejected as MCP_TLS_REQUIRED (scheme != https and no exception

- **[implementation-bug] SSE response parser takes the last data line; a trailing '[DONE]' marker fails the call** — `apps/assistant-service/src/assistant_service/core/mcp/client.py:741`
  - _response_payload for text/event-stream collects all `data:` lines and json.loads only data_lines[-1]. MCP/SSE servers that append a terminal `[DONE]` data frame (common SSE convention) produce a JSONDecodeError -> MCP_RESPONSE_INVALID, discarding a fully valid response. Comment-only/empty trailing

- **[security] Connector deactivation does not deregister tools/credentials; static predicate keeps Confluence visible to every tenant (canonical impl re-exported here)** — `apps/assistant-service/src/assistant_service/core/mcp/connector_mcp.py:10`
  - connector_mcp.py re-exports ai_gateway_core.connectors.ConnectorMCPService. In that canonical implementation, start_confluence() calls register_confluence_tools(domain, email, api_token) with no database, which builds a static_client whose ConnectorRegistry predicate returns True whenever static_cli

- **[implementation-bug] Non-dict tool content items are silently discarded from results** — `apps/assistant-service/src/assistant_service/core/mcp/client.py:683`
  - call_tool returns `content=[item for item in content if isinstance(item, dict)]`, dropping any non-dict content element (e.g. a bare string a non-conformant server emits) without error. The tool then completes 'successfully' with empty content, hiding the loss from the model/operator.

- **[security] redirect_uri is only SSRF-validated, never bound to a platform-registered callback** — `apps/assistant-service/src/assistant_service/core/mcp/oauth.py:297`
  - begin() accepts a caller-supplied redirect_uri and only checks it is a valid, TLS, non-private endpoint (_validate_endpoint). It is not compared against a per-deployment allowlist of registered OAuth callbacks. The user's browser is redirected to authorization_endpoint with that redirect_uri after c


## A.3 覆盖结论

- **文件级覆盖**：第一轮 16 个代理实际审查了全部微服务模块；补审补齐了 7 组此前未深入覆盖的文件（knowledge-service 摄取管线、assistant files/runtime-memory/runtime-context/MCP、gateway 未覆盖路由与 schemas、knowledge core）。
- **新增重要发现**：4 个新的 high 级问题——`users.py` 用户管理跨租户枚举（CONFIRMED）、knowledge fallback retrieve 跨租户 IDOR（CONFIRMED）、model_registry GOOGLE/GOOGLE_VERTEX 模型 ID 冲突覆盖（CONFIRMED）、scheduler job lease 永不回收（PARTIAL）。
- **真实性问题**：原 281 个发现经两轮验证，误报率低（仅 2 个 REFUTED：RatePolicyResolver 语义、Zip Slip）。
- **总覆盖**：两轮合计 368 个发现，覆盖全部微服务模块与绝大部分生产源文件；残留未深入审查的多为 `__init__.py`、三格式重复的 office 脚本（docx 那份已审查并标注冗余）、以及工具类脚本（scripts/*，属低风险）。

**最终有效发现统计（合并两轮）：**

| 级别 | 数量 | 说明 |
| --- | --- | --- |
| 🔴 Critical | 1 | API-key scope→admin 自提权（已确认） |
| 🟠 High | ~34 | 含补审新增 4 项（users 跨租户枚举、fallback IDOR、model 冲突、scheduler lease） |
| 🟡 Medium | ~155 | 含补审新增 31 项 |
| ⚪ Low | ~160 | 含补审新增 52 项 |
| 误报排除 | 2 | RatePolicyResolver 语义（有意行为）、Zip Slip（CPython 已防护） |
