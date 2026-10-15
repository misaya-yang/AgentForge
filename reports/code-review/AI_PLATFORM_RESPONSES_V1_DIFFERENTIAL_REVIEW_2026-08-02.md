# OpenAI Responses v1 Differential Review

## Executive Summary

| Severity | Found | Unresolved |
| --- | ---: | ---: |
| Critical | 0 | 0 |
| High | 4 | 0 |
| Medium | 1 | 0 |
| Low | 0 | 0 |

**Overall Risk:** Low after remediation
**Recommendation:** Approve the reviewed adapter boundary. Keep the wire protocol opt-in and the default Qwen path on `chat_completions`.

Key metrics:

- Seven adapter, registry, environment, Compose, initializer, and test files reviewed against the current worktree and `HEAD`.
- Five adversarial defects reproduced before repair; all five now have regression tests.
- High blast radius: `ModelRegistry.chat` and `ModelRegistry.chat_stream` are shared by every assistant turn using the configured provider.
- Real Singapore Qwen evidence: the same native-search canary changed from `unsupported_output_item` with no text to one terminal `stop` result with text after the fix.

## What Changed

**Baseline:** current `HEAD` (`46ac600` is the latest commit touching the registry) versus the uncommitted multi-agent worktree. The Responses adapter and its focused test file are new, so there is no earlier per-line Git history for them.

| File | Review focus | Risk | Blast radius |
| --- | --- | --- | --- |
| `apps/assistant-service/src/assistant_service/core/models/responses_api.py` | Request conversion and strict SSE reducer | High | All Responses turns |
| `apps/assistant-service/src/assistant_service/core/models/model_registry.py` | Protocol selection and endpoint resolution | High | All configured model calls |
| `.env.example` | Safe opt-in defaults | Medium | New installations |
| `docker-compose.yml` | Runtime variable ownership | High | Container quickstart |
| `scripts/new/init-env.sh` | Environment propagation | High | Generated local environments |
| `tests/services/assistant/test_responses_api.py` | Malformed-event mutations and provider projections | High | Regression gate |
| `tests/scripts/test_open_source_container_distribution.py` | Compose and initializer ownership gate | Medium | Release packaging |

The review did not alter the gateway API shape and did not add a public `/v1/responses` ingress route.

## Critical Findings

### High: Native web search was requested but every real search result failed parsing

**File:** `apps/assistant-service/src/assistant_service/core/models/responses_api.py:340`
**Blast Radius:** Every DashScope Responses turn with native search
**Test Coverage:** Yes

The request builder emitted `tools: [{"type":"web_search"}]`, while the reducer accepted only `message`, `reasoning`, and `function_call` output items. Alibaba and OpenAI both define `web_search_call` as a server-side output item with dedicated `in_progress`, `searching`, and `completed` events. A real Singapore Qwen canary reproduced `unsupported_output_item` before any answer text was accepted.

The repair models this as a server-side tool lifecycle, never as a client `tool_call`. It validates item/index identity, strict status transitions, terminal `action.query`/`queries` and HTTP(S) source shape, then retains only a SHA-256 fingerprint for final-output rebinding. Query and source values are not projected into runtime deltas.

Mutation coverage includes unsupported item type, status regression, terminal status at `added`, malformed query values, unsafe URL schemes, and changed terminal sources.

### High: `response.completed.output` could be absent without terminal rebinding

**File:** `apps/assistant-service/src/assistant_service/core/models/responses_api.py:510`
**Blast Radius:** Every streaming Responses turn
**Test Coverage:** Yes

The previous reducer returned early when `response.completed.output` was missing. That allowed a terminal success without binding the provider's full final output back to every streamed item. The terminal response must now contain a list whose length, order, item identity, text, function arguments, and server-tool fingerprint match the completed stream.

### High: Standard `/v1` or full endpoint base URLs produced duplicate paths

**File:** `apps/assistant-service/src/assistant_service/core/models/model_registry.py:1017`
**Blast Radius:** OpenAI or compatible providers configured with SDK-style base URLs
**Test Coverage:** Yes

Appending `/v1/responses` to a base already ending in `/v1` produced `/v1/v1/responses`; a full endpoint produced `/v1/responses/v1/responses`. Endpoint selection now handles host/prefix bases, SDK-style `/v1` bases, and complete `/v1/responses` endpoints without changing the default Chat Completions endpoint.

### High: Compose opt-in variables did not reach the actual execution service

**Files:** `docker-compose.yml:548`, `scripts/new/init-env.sh:189`
**Blast Radius:** Container quickstart and generated local environments
**Test Coverage:** Yes

`DASHSCOPE_CHAT_WIRE_PROTOCOL`, `OPENAI_BASE_URL`, and `OPENAI_WIRE_PROTOCOL` were present for the gateway, but not all were passed to `assistant-service`, which owns `ModelRegistry`. The initializer also did not preserve shell-provided values for those settings. Both paths are now wired, with `chat_completions` retained as the explicit default.

## Test Coverage Analysis

All reviewed high-risk branches have direct positive and negative coverage:

- native server-tool lifecycle and no client-tool projection;
- response/item/index identity and final-output rebinding;
- strict usage and function-argument reconciliation;
- continuous sequence numbers starting at zero;
- terminal completeness and duplicate/post-terminal rejection;
- cancellation propagation and prompt/key/provider-body error redaction;
- OpenAI/DashScope endpoint divergence and Qwen default Chat compatibility;
- Compose and initializer ownership of opt-in variables.

Verification receipts:

- Focused Responses suite: `63 passed`.
- Responses plus model registry, provider boundaries, model config, and container distribution: `182 passed`.
- Full Assistant suite: `1759 passed, 1 skipped` (PostgreSQL DSN not configured).
- Real Singapore Qwen native-search canary: completed with text, one `stop` terminal, no client tool call.
- Ruff: passed on all reviewed Python files.
- Adapter mypy: `Success: no issues found in 1 source file`.
- Shell syntax, Compose YAML parse, and `git diff --check`: passed.

The whole `model_registry.py` mypy gate still reports six pre-existing errors at lines 314, 336, 1180, 1188, 1397, and 1422. None is introduced by the Responses endpoint or reducer changes.

## Blast Radius Analysis

| Function or boundary | Consumers | Risk after fix | Priority |
| --- | --- | --- | --- |
| `iter_responses_stream` | `ModelRegistry.chat_stream` and AgentLoop | Low | P0 regression gate |
| `parse_responses_response` | `ModelRegistry.chat` | Low | P0 regression gate |
| `_responses_endpoint` | Streaming and non-streaming provider calls | Low | P0 regression gate |
| provider wire environment | Gateway and assistant containers | Low | P0 packaging gate |

No database schema, public API contract, default provider, or client-tool execution contract changed.

## Historical Context

The adapter is a new untracked worktree file, so Git blame cannot establish older intent. The closest registry baseline is commit `46ac600` (`feat: optimize assistant runtime`). Existing Qwen Chat Completions behavior is protected by default-protocol and request-shape tests and remains unchanged unless an operator explicitly selects `responses_v1`.

## Recommendations

### Immediate

- [x] Repair `web_search_call` as a server-side lifecycle.
- [x] Require and reconcile `response.completed.output`.
- [x] Normalize Responses endpoints.
- [x] Wire opt-in settings to the assistant execution container and initializer.
- [x] Preserve Chat Completions as the default.

### Before Production

- [x] Run the real Qwen native-search canary against the Singapore endpoint.
- [ ] Run a real OpenAI Responses canary when an approved OpenAI credential and model are available.
- [ ] Recreate or hot-update the assistant container only through the repository's ownership-checked workflow.

### Technical Debt

- [ ] Resolve the six existing `model_registry.py` mypy errors in a separately scoped change.

## Analysis Methodology

**Strategy:** Focused differential review of a new provider boundary.

Techniques applied:

- worktree-to-HEAD diff and targeted Git history review;
- one-hop caller and runtime-configuration tracing;
- official Alibaba and OpenAI event-contract comparison;
- malformed-event and endpoint mutation tests with observed red-before/green-after results;
- real-provider canary verification performed by the root task without exposing credentials;
- focused, provider-registry, packaging, and full Assistant regression gates.

Limitations:

- This reviewer did not read or write a real API key and did not call a provider directly.
- No OpenAI live canary was run.
- Containers were not restarted or rebuilt during this review.

**Confidence:** High for the reviewed adapter and Qwen Responses path; medium for live OpenAI behavior until a provider canary is run.

## Appendices

Primary contracts consulted:

- [Alibaba Cloud Model Studio: OpenAI-compatible Responses API](https://help.aliyun.com/en/model-studio/qwen-api-via-openai-responses).
- [OpenAI Responses streaming reference](https://platform.openai.com/docs/api-reference/responses-streaming/response/refusal/delta?lang=curl) for `web_search_call` status events.
