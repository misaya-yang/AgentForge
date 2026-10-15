# ADR-005: Tenant-configurable model capability profiles

**Status:** Accepted

**Date:** 2026-08-19

**Scope:** Gateway model control plane, Assistant provider requests, and console model settings

## Context

The model table was configurable, but reasoning, prompt caching, and native
search were still selected by model-name checks inside Assistant. That split
the source of truth: the console could change a model row while the runtime
continued to infer capabilities from code. It also made custom compatible
providers either silently lose settings or receive unsupported fields.

OpenCode model variants, OpenClaw thinking profiles, and provider model
catalogs all converge on one useful boundary: a model advertises its actual
options and a protocol adapter serializes those options. The Agent loop should
not classify user text or know provider field names.

## Decision

1. `llm_models` owns a versioned capability profile split into provider catalog
   facts and tenant overrides. Overrides win and provider sync never erases
   them.
2. `ai-gateway-core.models.capabilities` is the shared schema, merge, validation,
   option resolution, and declarative catalog boundary.
3. Assistant capability adapters own provider wire paths. Model identifiers and
   reasoning budgets are data; production request code may not branch on model
   names.
4. The console renders the selected model's declared reasoning options. `auto`
   means the profile default, not prompt classification.
5. Unsupported or conflicting settings fail before the provider HTTP call.
   Tool authorization, approval, and tool-call/result pairing remain runtime
   invariants and are not configurable.
6. Each run captures an immutable profile revision. Gateway publishes exact
   invalidations after configuration commits; a bounded TTL is the degraded
   fallback.

## Compatibility

- `thinking_level` remains accepted as a legacy alias; new clients send
  `reasoning_option`.
- `supports_tools` and `supports_vision` remain projected compatibility fields.
- Existing custom models without a profile receive a fail-closed safe profile.
- The public Assistant model response only exposes effective capabilities;
  catalog and override layers remain on the administrator API.

## Consequences

Adding a new provider wire protocol requires a typed adapter and golden request
fixtures. Adding or updating a model normally changes declarative catalog data
or a tenant override, not Agent-loop code. OpenAPI, migration, Assistant runtime,
and browser settings tests are required together because this is an end-to-end
control-plane contract.
