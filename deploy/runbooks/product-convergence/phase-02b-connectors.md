# PC-02B — Connectors productization (track B, worktree)

## Contract

B1. Catalog management: new `src/api/v1/connector_admin.py` (GET/POST/PUT/DELETE/toggle on
    `connector_configs`; DELETE refused while `user_connectors` rows exist; permission
    `console:settings:view`; RedactedValidationRoute pattern for client_secret) + new
    `src/api/schemas/connectors.py` (provider definition: display_name, icon, mode
    live|ingest|both, auth/oauth, mcp_tools info) + migration `084_connector_modes.sql`
    (`ALTER TABLE connector_configs ADD COLUMN mode VARCHAR(16) NOT NULL DEFAULT 'live'`;
    backfill 'both' where supports_sync). UI: new `web/src/pages/settings/ConnectorsSettings.tsx`
    at `/settings/connectors` + card link in Settings.tsx (no new tab inside Settings.tsx —
    809-line budget).
B2. Agent binding: connector capability (CapabilityType already includes "connector",
    src/api/schemas/agents.py:19) → gateway agent_runtime validates: resource_id must be an
    enabled connector_configs row AND caller has a connected user_connectors row, else deny.
    Assistant restricts tool selection to the bound connector set. Contract: extend
    `deploy/runbooks/agent-studio-prd/architecture-contract.md` with connector validation/deny
    semantics + add AS-0x assertion to the regression script.
B3. Naming: `git mv src/connectors src/transports`; `ConnectorType → TransportType`
    (ai-gateway-core enums); update ~14 importer files (dispatcher, adapters, models/service.py,
    registry, tests). Keep `connector_type` field name (no schema churn).
B4. `/settings/connectors` is the canonical surface; in-chat ConnectorsPanel stays as quick
    access. No new top-level nav item.
B5. Docs: `docs/design/connectors.md` (live/ingest semantics, token custody, MCP tool model) +
    `docs/connectors/ingest-mode.md` (ingest reuses KB upload endpoints; not implemented).

## Gate

```bash
uv run --all-packages --extra test pytest -q --no-cov tests/services/test_registry.py tests/adapters tests/api/test_connector_admin.py
uv run --all-packages --extra dev ruff check src/ apps/ packages/
pnpm -C web type-check && pnpm -C web lint && pnpm -C web build
make verify-agent-studio        # live-stack items reported explicitly if they cannot run
make migrate-status             # needs running DB; report explicitly if unavailable
```

## Evidence (fill on verify)

- [ ] pytest + ruff + pnpm outputs
- [ ] verify-agent-studio output (or explicit not-run statement)
- [ ] migrate-status output (or explicit not-run statement)
- [ ] `rg -n 'src\.connectors|from src\.connectors' src/` → zero
