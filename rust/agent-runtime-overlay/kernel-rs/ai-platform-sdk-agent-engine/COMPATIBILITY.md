# Native framework execution boundary

This binary is a Rust implementation of the platform's private SDK worker protocol.
It does **not** embed or claim to be OpenAI Agents SDK or Claude Agent SDK. The
framework identifiers remain wire-compatible routing identifiers. Providers are
called directly using OpenAI Responses and Anthropic Messages HTTPS APIs.

## Contract

- One stdin envelope: compact JSON `{ "request": ..., "provider_key": ... }`,
  final LF and EOF, maximum 512 KiB. Provider keys are neither command arguments
  nor logs. Errors expose only a stable `sdk_execution_failed` code.
- NDJSON `delta`, `completed`, `error`, `approval_required`, `approval_result`, and
  `tool_result` events. Each line is at most 1 MiB. Parent must enforce aggregate
  output limits and hold session ownership until process-group cleanup completes.
- `HOME` is the parent-owned session directory. `SDK_AGENT_GATEWAY_URL` is an
  operator-only HTTPS origin (HTTP allowed solely for loopback). Neither provider
  nor Gateway requests follow redirects, inherit proxies, or retry uncertainty.
- The parent must supervise cancellation, SIGTERM/SIGKILL, disconnects, durable
  pending/receipt publication, retention and tombstones, and ten-second authority
  checks while a run is active. Engine checks admission before each provider turn
  and before successful state publication.

## Implemented

Strict launch types, allowed framework routing, bounded identities and parameters,
Python-compatible ASCII-escaped scope hashing, selected dataset and governed tool
validation; native streamed provider loops; provider-bound call IDs; sequential
platform tool calls and stable approval identity polling; unknown-side-effect
termination; pure arithmetic/time tools; selected-dataset knowledge retrieval;
raw-evidence usage aggregation with unknown values preserved; atomic/fsynced
native conversation state; native state framework fencing.

All model-proposed platform actions go through Gateway authority. No shell, file,
network or credential tool is granted to the model. No Python interpreter or SDK
subprocess is launched by this binary.

## Explicit compatibility limits

- OpenAI 0.23.1 `SQLiteSession` history is imported read-only from `openai.sqlite`,
  scoped by the existing session hash and ordered by message ID. These stored
  items are already Responses input objects. The source database is retained;
  native history is published only after a successful continued run. Imports
  reject malformed rows and are bounded to 10,000 items / 8 MiB, rather than
  silently discarding context. Schema evidence: [official tagged source](https://raw.githubusercontent.com/openai/openai-agents-python/v0.23.1/src/agents/memory/sqlite_session.py).
- Opaque Claude resume IDs in `claude-session.json` cannot be reconstructed through
  the Messages API. Such existing sessions fail closed and require a separately
  verified export/migration; conversations are never silently reset.
- Anthropic Messages requires a completion-token ceiling; this implementation
  uses 16,384. Claude Code/Agent SDK orchestration, hidden prompts and upstream
  resume semantics are not reproduced or represented as equivalent SDK behavior.
- Provider usage is aggregated from raw response observations. Direct APIs do not
  provide the Claude SDK's `total_cost_usd`, so cost remains unavailable. Missing
  cache counters do not become measured zeroes.
- Arithmetic supports decimal/scientific and radix numeric literals, digit
  separators, parentheses and `+ - * / %` with Python sign-of-divisor modulo.
  Numeric operations use f64 within the original 1e15 bound.
- SSE parsing handles the providers' single-line JSON data events. General
  multiline SSE data fields are not supported.

## Verification

Unit/loopback tests cover scope escaping, identity rejection, arithmetic fencing,
usage unknowns, persisted history fencing, Responses streaming terminal handling,
truncated streams and Anthropic streamed tool arguments. Compilation and execution
are performed by the coordinating build lane, not assumed from source presence.
