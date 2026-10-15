# ai-gateway-core

Shared primitives consumed by Gateway and Knowledge Python services.

Scope is deliberately narrow:

- `logging/` — structured JSON logging with contextvar-based request context
- `exceptions/` — `GatewayError` hierarchy
- `enums/` — pure enum types including the `StreamEventType` SSE contract
- `auth/`, `persistence/`, `session/`, `metrics/`, `storage/`, `knowledge/` — `typing.Protocol` contracts only; concrete implementations live in each service

Rules:

- No LLM SDKs (openai, anthropic, google-genai, tavily, …) belong here.
- No FastAPI, httpx, SQLAlchemy, asyncpg instantiation belongs here (Protocols are fine).
- No runtime dependencies unless absolutely necessary — this package must stay lean.

See `plans/Assistant-Service-True-Isolation-Plan.md` for the migration context.
