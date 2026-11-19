# Tenant guardrails v1

This pipeline reads the existing active tenant's `metadata.guardrails_v1`, never
request metadata. Absent policy preserves existing behavior. An explicit policy
must validate the exact schema/version, all three stages, bounded regex programs,
nonzero thresholds, directions and size/time bounds; malformed policy fails closed.
There is no new database schema, provider call, credential or billing authority.

GET/PUT `/api/v1/guardrails/policy` require current same-tenant admin or
`guardrails:manage`. PUT takes `{policy,expected_pin}` and changes the current tenant
metadata under a row lock and JCS-pin CAS, together with an existing audit-log entry.
A missing previous policy uses null expected_pin. The pure producer is
`guardrails/contract.rs::extend`. Bootstrap mounts the actual router with the same
AdmissionService and pool under the existing BFF/CSRF middleware.

Policies use schema `agentforge.guardrails/v1`, an operator version, enabled flag,
`budget_ms` (5–500), max_input_bytes (1 KiB–2 MiB), max_output_bytes (1 KiB–4 MiB),
and exactly three stage definitions: injection, sensitive_data, content_safety.
Each declares enforce/observe, input/output directions, score threshold, rules and
detectors. Rule fields are id, all regex signals, any regex signals, min_any and
score. All/all-required and minimum-any evidence must occur in the same fragment;
a single rule contributes once. Configuration is explicit and domain-specific.
Injection/content rules are local signal policies, not a universal semantic
classifier or proof that arbitrary adversarial content is safe. Policy owners must
select suitable coverage and validate their own domain corpus before enforcement.
There is no hidden single-word blacklist or external classifier success fallback.

Sensitive detectors include email structure, US SSN area/group validation,
payment-card length plus Luhn validation, private-key envelopes, AWS access-key
shape, and credential assignment with entropy validation. Explicit JSON secret
fields also trigger, including embedded tool argument JSON. Detectors and patterns
are opt-in within the explicit sensitive stage. Observe records evidence without
blocking; enforce rejects triggered stage thresholds. Unsupported/invalid output,
CPU queue/deadline exhaustion and unavailable audit authority fail closed.

## Actual paths

Public Chat Completions, Embeddings and Anthropic Messages check input before any
provider dispatch. Selected model authorization, routing, admission and frozen
price receipts retain their existing authorities. On completed provider output,
guardrails run before release; real usage still commits even when content is
withheld. Native usage metadata carries input/output reports, including policy pin
and latency, without raw content.

Existing `/v1/responses` and `/api/v1/responses` authenticate, authorize the requested
model, then inspect the original input before accepting a turn. Output screening
also covers idempotency replay. Accepted-turn observation failures and rejections
retain the original run/session and digest via the existing fenced response-cache
path; they never abort an accepted claim to permit another execution. Runtime
usage continues through the original model plane, not the compatibility observer.

The private model plane checks the actual prepared provider body only after lease,
tenant/scope and provider validation. Configured output protection holds projected
frames before the Runtime can observe text or invoke a tool. Once a genuine native
or chat terminal provides actual usage, the original versioned/legacy completion
function runs even if guardrails or its audit reject output. Unknown/incomplete
provider execution keeps the original unknown/recovery behavior. No completion
receipt format, old five-argument function or accounting formula changes.

Large existing handlers were moved, preserving their control flow, to
`migration_compat/responses_guarded.rs` and `migration_agent/model_guarded_stream.rs`;
the original files now delegate to these children and shrink in size.

## Audit and measured overhead

Existing `gateway.audit_logs` stores policy pin/version, stage/rule IDs, safe
structural locations, scores, decision/reason and elapsed microseconds. It never
stores matched values, prompt/output text or arbitrary JSON property names.
GET `/api/v1/guardrails/decisions/{request_id}` requires the same tenant and original
actor or current guardrails manager. Public Responses exposes
x-guardrail-request-id; direct LLM decisions use its gateway request ID, and private
model decisions use the authorized model call ID.

`budget_ms` bounds policy compilation, CPU-lane queueing and local classification;
config fetch time is included in input classification's elapsed_us. Audit writes
have a separate 250 ms confirmation bound; actual audit_us is recorded in native
usage metadata. The audit row's own audit_us is null because a write cannot record
its future completion time. Failure to confirm audit never becomes allow.

Any configured output stage, including observe, requires complete text to classify
cross-frame secrets. It therefore opts into full-response holdback; successful SSE
frames remain byte-for-byte intact but first-byte delivery waits for completion.
Report holdback_us measures this delay separately from classifier CPU. Existing
request/lease timeouts bound provider/Runtime waiting. Default absent policy keeps
immediate streaming and does not poll/buffer the original response body.

Compilation/evaluation uses four shared bounded blocking slots. Public/direct
output buffering uses four slots and at most 4 MiB per response; nested private
model holdback has a separate four-slot bound to avoid sharing its caller's lane.
Decoded fragments and joined deltas make bounded additional copies. Over-size
output is withheld and audited; if execution has no confirmed terminal, its usage
remains unknown instead of inventing free completion.

Source regressions cover combined injection evidence, valid/invalid PII formats,
secret/key/embedded-JSON handling, domain content rules and observe mode, invalid
policies, deadline refusal, safe audit locations, split SSE/choice isolation,
blocked sync/stream output with real observed usage still settled, default immediate
streaming, private model holdback and accepted Responses idempotency fencing.
No Cargo/DB/Docker/provider execution ran in this worker; primary owns those gates.
