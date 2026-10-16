# Native Rust execution for SDK-labelled agent services

Status: the native Rust Gateway adapter, private worker and provider engine are
present. A composed loopback fixture has passed once. Real provider and
Gateway/worker restart acceptance remain pending.

The public adapter identifiers below are retained service and protocol values.
They select a native Rust execution path; they do not establish implementation
of, or compatibility with, either vendor's official Python Agent SDK.

## Public service contract

Clients create these services through the create-only
`POST /api/v1/service-registrations` route. Registration does not fall back to
the legacy service upsert route. The service is an HTTP connector with sessions
enabled, retries disabled, and text sync/stream modes. Its metadata contains:

- `adapter_type`: `openai_agents` or `claude_agent_sdk`
- `sdk_agent`: `model`, `instructions`, and `max_turns` (1–32)

The provider credential is submitted as
`connector_config.sdk_provider_api_key`. The Gateway validates and encrypts it
with the host-injected provider encryption key in a service-ID-bound envelope.
The public service projection returns only
`metadata.sdk_provider_key_configured`; it omits plaintext and ciphertext.
Clients cannot submit ciphertext, and an update that omits a replacement key
preserves the existing encrypted credential.

The user-facing service identity and session are supplied by authenticated
Gateway dispatch. The adapter rejects non-empty public parameters and builds the
private request from the admitted tenant, user, service, request and session
identity. Neither request parameters nor service metadata select a worker or
provider destination.

## Native request path

The Rust Gateway adapter reads its private worker origin and bearer token from
operator configuration. It decrypts the service-bound provider key only after
loading the service, marks the outbound secret headers sensitive, and sends
them to that operator-configured origin without redirects or retries. The
worker origin is not a public service field.

The private worker accepts `POST /v1/runs` under bearer authentication. Its
`ai-platform/sdk-agent/v1` request contains the framework identifier, scoped
identity, request and session IDs, agent settings, text input and stream flag,
with optional Gateway-issued read and capability bridges. Unknown fields are
rejected. Provider endpoint overrides are not part of this request. The worker
accepts provider origins only from operator-owned `OPENAI_BASE_URL` and
`ANTHROPIC_BASE_URL` configuration, restricted to HTTPS or loopback
HTTP and validated against credentials, query, fragment and ambiguous paths.

For each run, the worker starts the Rust
`ai-platform-sdk-agent-engine` in a separate process with a scrubbed environment.
It passes the provider key in a bounded stdin frame, not in argv or the
environment, and does not capture provider stderr. The engine returns bounded
NDJSON events; the worker exposes completed results or SSE deltas and terminal
events through the private protocol.

| Public identifier | Native provider transport |
| --- | --- |
| `openai_agents` | OpenAI Responses API, `/v1/responses` |
| `claude_agent_sdk` | Anthropic Messages API, `/v1/messages` |

These are direct provider API loops in Rust. The engine does not import or launch
an official Python SDK. SDK-specific orchestration, event semantics, tools,
session behavior, costs and full compatibility are not implied by the retained
identifiers.

## Gateway authority and tool boundary

The Gateway admits each authenticated session and supplies scoped grants. The
worker and engine re-check Gateway authority before provider turns, governed
actions and successful history publication. The model receives only tools
derived from the service's selected configuration: bounded arithmetic and time,
optionally selected-dataset knowledge retrieval, and the Gateway capability
tools admitted for that run. Capability actions use the existing approval and
execution authority; a side effect with unknown outcome terminates the run.
There is no shell, local file, arbitrary network, credential or user-selected
provider-routing tool.

Knowledge access uses the Gateway's scoped read bridge. This page does not
describe a Python Knowledge-service or Python Gateway hybrid execution path.
A configured read grant is not permission to broaden the selected dataset set.

## Credentials and private deployment boundary

The Gateway reads `SDK_AGENT_WORKER_URL` and `SDK_AGENT_WORKER_TOKEN` from
operator configuration. The worker requires the same bearer token, an absolute
private `SDK_AGENT_STATE_ROOT`, and finite `SDK_AGENT_RETENTION_SECONDS`.
The engine receives the operator-owned `SDK_AGENT_GATEWAY_URL` for scoped
authority checks.
The worker binds its service listener to loopback. Deployment policy must keep
the private endpoint reachable only by the Gateway and provide protected
persistent storage for worker state.

The provider key travels from the Gateway to the worker in sensitive
`X-SDK-Provider-Key`, then from the worker to the engine over stdin. It is not
a public metadata value, request parameter, command-line argument or
environment variable. Provider endpoint selection is operator-owned; the caller
cannot supply a base URL. Health readiness checks the worker protocol, engine
file and state/retention health; it does not prove provider access.

## Session persistence and uncertain outcomes

Worker state is scoped by a hash of tenant, user, service, framework and session.
A non-blocking file lock serializes a session. The worker writes a pending marker
before starting execution, then atomically publishes a request receipt only
after a valid terminal result, child cleanup, authority revalidation and durable
state commit. Exact receipt replay returns the saved result without another
provider call. Reusing a request ID with changed input is rejected.

Cancellation, timeout, process failure, or a commit/cleanup uncertainty never
authorizes automatic replay. The supervisor owns child and process-group cleanup
independently of the response task. If cleanup cannot be established, the
session lock and durable cleanup marker remain, readiness fails closed, and
later turns require reconciliation. A pending request without a committed
receipt returns `sdk_session_reconciliation_required`. Deletion and finite
retention use scoped locks and durable tombstones; they do not claim that a
remote provider stopped an already accepted request.

For history, the engine stores native conversation state separately and fences
it by framework. It can import compatible OpenAI `agent_messages` records from
`openai.sqlite` read-only, scoped and bounded; it does not modify the source
database. Opaque Claude session identifiers in `claude-session.json` cannot be
translated to Messages API history, so those sessions fail closed and require
separate migration rather than silently starting over.

## Usage and accounting

The engine aggregates raw usage returned by provider API responses. Missing or
partial token and cache counters remain unavailable; the worker does not
invent zeroes. The Gateway preserves usage observations through its existing
audit and accounting paths. Provider-reported cost is not an additional charge
or an override of platform pricing. Real-provider usage and cache-pricing
reconciliation remain acceptance work.

## Verification and remaining acceptance

The most recent serialized `native_stack` composed test passed once in 7.98
seconds. It runs the real Rust worker and engine executables against a loopback
Responses fixture and stubbed Gateway authority endpoints. The fixture covers
the configured provider path, non-stream and streaming responses, receipt replay,
history continuation, refusals, allow/deny capability flow, rejection of
request-controlled provider destinations, cancellation and reconciliation
fencing. Its synthetic key and loopback provider are fixture inputs; this is not
a live provider or production Gateway test.

Real provider calls, live Gateway/database acceptance, and Gateway/worker
restart with production persistent storage have not been established by that
fixture. Those release gates remain open. See the engine's
[native compatibility limits](../../rust/agent-runtime-overlay/kernel-rs/ai-platform-sdk-agent-engine/COMPATIBILITY.md)
and the separately maintained
[worker deployment guide](../deployment/sdk-agent-worker.md) for their scoped
contracts.
