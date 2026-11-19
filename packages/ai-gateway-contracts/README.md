# ai-gateway-contracts

I/O-free cross-service protocol contracts for the AI Gateway platform
(PRD §ARC-04). Everything shared between the Gateway (`src/`), the Knowledge
service (`apps/knowledge-service/`) and the Rust capability worker that is a
*pure protocol* lives here.

## Allowed content (mechanically enforced)

- Pydantic/dataclass protocol models;
- version/schema constants;
- pure validation and signed-payload specifications (HMAC canonicalization,
  replay-store protocol + in-memory implementation);
- I/O-free serialization helpers.

## Forbidden

Database drivers, Redis, HTTP clients, FastAPI/Starlette, provider SDKs and
service configuration. `scripts/core_boundary/check_core_boundary.py` fails
the build on any forbidden import and on content outside the allowlist.

## Current modules (ARC-04 first batch)

| Module | Provenance | Wire contract |
| --- | --- | --- |
| `agent_launch` | ARC-02 Gateway launch convergence | `resolved-agent-launch/v1` |
| `capability_proof` | `ai_gateway_core.auth.capability_proof` | `ai-platform-capability-proof/v1` (co-owned by Rust worker) |
| `agent_runtime` | `ai_gateway_core.agents.runtime` | `agent-runtime/v1`, `agent-runtime-envelope/v1` |
| `agent_runtime_lease` | `ai_gateway_core.agents.runtime_lease` | `agent-runtime-model-lease/v1` |
| `event_envelope` | `ai_gateway_core.events.envelope` | `usage.recorded.v1` |
| `event_errors` | `ai_gateway_core.events.errors` | event-bus error taxonomy |
| `replay` | `ai_gateway_core.auth.gateway_secret` | seen-id replay protection protocol |

The original `ai_gateway_core` modules remain as thin compatibility shims;
each shim lists its consumers and removal conditions.
