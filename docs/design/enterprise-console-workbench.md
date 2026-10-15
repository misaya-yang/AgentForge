# Enterprise console workbench

The authenticated console has a permission-aware entry surface at `/workbench`.
The existing Carbon Indigo tokens, compact 187px navigation rail, Ant Design
controls and Radix dialogs remain the visual foundation. There are no additional
runtime dependencies and no new service contracts.

## Navigation and access

`web/src/config/consoleNavigation.ts` is the single navigation definition used by
the sidebar, quick navigator and workbench. Existing route permission checks remain
in place; navigation filtering is discoverability, not server authorization.
Agent Studio remains feature-gated. A model-tester-only account sees only the
playground and is redirected there if it opens the workbench.

The root route opens the workbench. Existing deep links, including `/dashboard`,
remain unchanged. Ctrl/Cmd+K opens searchable navigation. Arrow keys and Enter
select a page, Escape closes the Radix focus-trapped dialog, and focus returns to
the triggering control. A skip link targets the main content region.

## Live data

The usage section calls the existing `getUsageSummary` client for a selected
7- or 30-calendar-day inclusive date range, only with `console:usage:view`.
The query key contains the account ID and date range. The Gateway owns tenant
and user scoping. No tenant-wide scope is inferred by the frontend. Success rate
uses the API's percentage units; missing rates and failed initial loads show a
dash, never a fabricated zero. Failed refreshes explicitly identify cached data.
Collection status and the API's last-ingestion timestamp remain visible.

Model configuration reuses the permission-gated `useSetupState` query. Configured
means a provider is configured, not that all services are healthy. Loading,
missing configuration, errors and retry actions are independent of usage loading.

Tool cards and directory links open existing implemented routes. There are no
placeholder actions, synthetic activity, fake teams or fabricated health badges.
The workspace label uses the authenticated account's department when present.
All new copy is provided in English and Simplified Chinese.

## Responsive and accessibility contract

The hero, usage grid, tool grid and side panels reflow at 1200px, 767px and 420px.
Numeric values use tabular figures; focus outlines, reduced-motion handling,
accessible search labels and status announcements are explicit. Dark mode uses
existing semantic tokens rather than a separate hard-coded palette.

## Verification

Run the repository's direct app/node TypeScript checks, frontend lint and build,
then the open-source navigation/route smoke suite. Browser acceptance should cover
light/dark themes; 375px, 768px and desktop widths; an administrator and a
model-tester-only account; Ctrl/Cmd+K, Arrow keys, Enter and Escape; empty, loading,
failed, stale and populated API responses; and period changes plus refresh.
This document describes implementation intent, not evidence of executed tests.

## Model-plane and connector management

The Services surface uses URL-backed tabs (`?tab=services|providers|models`),
combined text/enabled-state filters, and an explicit first-provider action.
Summary cards count registered definitions, enabled providers/models and
credential-ready enabled providers. None of these counts claim connection health.
Catalog failures are distinct from empty results and support retries; cached
responses are labeled when refresh fails. Existing provider/model creation,
editing, discovery, deletion and connection-test endpoints remain unchanged.

The connector catalog links back to model services and includes searchable,
status-filtered definitions with truthful catalog/search-support counts. A notice
distinguishes enabling a definition from connecting a user's account. Existing
administrator edit permission checks remain. Initial failures, stale responses,
empty catalogs and filtered-empty states are distinct. Pending toggles/deletions
are guarded against repeat submissions. Editor fields have explicit labels,
required nonblank identifiers/names and native URL validation. Secrets remain
write-only; local form values are cleared after save or dismissal. An in-flight
save cannot be dismissed to race a second editor instance. All changes call the
existing API clients and retain the connected-users deletion guard.

### Global connector templates and tenant overrides

Definitions with an empty `tenant_id` are global templates. Their enable switch is
read-only and they have no edit/delete actions. Tenant administrators can create
an override through the existing POST catalog endpoint using the same provider ID.
The editor copies public configuration, redirect URI, icon, tool declarations and
extra configuration; it never copies a secret. Overrides start disabled and the
editor explains that tenant OAuth credentials must be configured before enabling.
User connector lookups and availability lists select the tenant override before
filtering enabled state, so a disabled tenant definition shadows an enabled
global template rather than falling back to it. This matches connector principal
authorization. Existing tenant definitions retain their edit/enable/delete workflow. Deleting a
tenant definition may reveal a global fallback; the deletion confirmation explains
this and the backend connected-user guard remains authoritative.

Catalog queries are account-scoped and discarded once inactive. The workspace
remounts when account or authorization changes so an open editor is not reused for
another account. Initial failure, stale data, retry and empty-state behavior remain.
This workflow configures connector definitions, not plugin installation or user
account authorization. Browser and API acceptance are pending integration.

## Read-only plugin registry

`/settings/plugins` is discoverable from Settings and uses the same console
settings-view route gate. Its backend `GET /api/v1/assistant/plugins` uses JWT
caller authentication and returns `ai-platform-plugin-registry/v1`. The query
key includes the current account ID. All fields are typed in `api/plugins.ts`.
The UI reports visible plugin IDs, exact version labels, catalog revision,
policy availability, required/declared permissions, capability schema hashes,
risk and read-only metadata. Search matches plugin/version/capability text;
availability filters versions without inferring semantic-version ordering.

This is a catalog projection, not an installation manager. Permission-denied
capabilities are omitted by the backend; policy-blocked visible capabilities
remain visible. No mutation endpoint is invented, and no install, enable,
disable or upgrade action is shown. Existing runtime policy remains authoritative.
An empty plugin list is valid, including when bundled tools have no plugin
attribution. Loading, failed catalog/policy retrieval, stale cached responses,
empty registry and filtered-empty states are distinct. Browser and API acceptance
still need to run after the frontend/backend slices are integrated.

## Tenant governance

Settings links administrators to `/settings/governance`. The existing
`console:settings:view` route gate is retained. Before any tenant-policy request,
the page additionally requires exact role `admin` or case-insensitive tier
`admin`, without trimming, matching native `tenant_policies_api`. The API remains
the authority; `platform_admin` alone does not grant access to these cross-tenant
routes. Account/role/tier changes remount the editor and queries. Governance query
caches are account-scoped and removed when inactive.

The tool-policy list and editor use the existing GET/PUT administration routes.
The review step identifies the tenant and every overwritten field. JSON list
inputs preserve the distinction between null, an empty list, and string arrays;
rate limits are bounded to 1–10,000. Backend tenant validation remains authoritative.
The server uses upsert and does not offer optimistic version checks. There is no
new delete operation, schema, tenant-membership API or runtime enforcement claim.
After a successful write response, an exact GET verifies the persisted editable
fields before success is shown. Pending submissions block duplicate actions and
dismissal. A write/invalidation/readback failure may follow a committed write;
the UI explains this, refreshes the list, and never automatically retries a mutation.

The second tab uses the existing tool-execution audit GET endpoint with exact
tenant/user filters, case-sensitive tool-name substring and 25-row offset pages.
Changing filters resets pagination. It displays only recorded metadata, outcome,
latency and correlation ID, not raw input summaries or error text. Configuration changes are shown separately in the third tab described below. Audit rows and totals are separate reads, so the UI
explains pagination drift. Empty results do not claim complete audit collection,
retention or policy enforcement. Initial failure and stale refresh remain distinct.

The third tab reads only persisted `config_changed` metadata from `audit_logs`
through `GET /api/v1/admin/tenant-policies/config-audit-log`. It shares the exact
cross-tenant role/tier administrator check above. The optional tenant, user,
resource type, resource ID and action filters are exact matches. The tenant field
is labeled **actor tenant**: the existing writer stores the actor's tenant and
cannot establish affected-resource ownership. The explicit projection excludes
configuration snapshots, arbitrary summaries, IP/user-agent data and error text.
IDs are strings to preserve BIGSERIAL precision. No schema or privilege changes
are introduced; an unavailable reader returns an error, not an empty history.

Pages contain at most 100 rows (the UI requests 25), sorted by creation time and
ID descending, with one extra row used to report `has_more`. No total is invented
or counted. Offsets are limited to 10,000; the UI asks for narrower filters at the
bound. Concurrent inserts/deletions may shift pages. The reader does not imply
complete collection, retention guarantees or transactional auditing of writes.
In particular, current tenant-policy writes do not emit configuration events.
This is a read-only metadata viewer; before/after snapshots are not exposed.
Account/authorization changes remount it and its account-scoped cache is removed
when inactive. Initial errors, stale refreshes and empty histories are distinct.

The configuration-audit slice requires native compile/authorization/query checks,
frontend type/lint checks and live persistence/browser acceptance in the later
consolidated gate; those checks were not executed when this source was staged.

Focused helper tests cover authorization equivalence, null/list round trips,
validation and readback mismatch detection. Externally staged browser scenarios
author mocked coverage for unauthorized access without data requests, review/save,
readback failure, audit filtering/pagination and loading failure. Mocked checks do
not verify live database persistence, backend authorization or runtime policy
application. Full type/lint/browser/live regression remains a consolidated gate.


## Agent SDK registration and Playground

The Services registration dialog offers LangGraph, Dify, Agent SDK, and advanced
YAML tabs. Agent SDK selects the official OpenAI Agents SDK (`openai_agents`) or
Anthropic Claude Agent SDK (`claude_agent_sdk`). The public service registration
uses `service_type: custom`, `connector_type: http`, `metadata.sdk_agent` with
`model`, `instructions`, and integer `max_turns` (1–32), and write-only
`connector_config.sdk_provider_api_key`. Worker origin and worker authentication
are operator-side settings, never form fields or browser-supplied headers.

The form does not choose an unverified model for the user. Provider API keys live
only in transient component/request memory; there is no browser storage, console
logging or raw transport-error rendering. Submission clears the password field.
Closing or changing SDK discards its key; switching SDK also clears the model.
While registering, duplicate submission, tab switching and dialog dismissal are
blocked. An uncertain result asks the user to inspect the service list before
retrying, rather than automatically replaying registration. Gateway redaction and
secure persistence remain server responsibilities, not a browser guarantee.

Dify registration also blocks duplicate submission, tab switching and dialog
dismissal while its request is pending. Its fields belong to the current mounted
dialog and authenticated credential/authority scope: closing the dialog or
changing that scope discards the draft and app key, and a late response cannot
close a newer dialog or overwrite its error state. The password field clears at
dispatch; an ordinary failed request shows a sanitized error and retains
nonsecret fields for an explicit retry with a newly entered key. This client
ownership fence does not cancel or roll back a dispatched write. Dify continues
to use the legacy `POST /api/v1/services` upsert; it is not an atomic create-only
operation and may replace configuration for an existing service ID. Custom Dify
inputs are available in Playground as a JSON-object editor for `dify_chat` services,
and through the existing API `parameters.inputs` field. The editor preserves JSON
types, rejects malformed/non-object JSON and nonfinite numbers before dispatch,
and keeps field names (including prototype-shaped keys) as data inside `inputs`;
they are never merged into request or application configuration. There is no new
browser size cap; existing server/provider request limits still apply. The UI does
not discover or invent the app's field schema. It targets the existing chat-messages
contract, not standalone workflow execution or unsupported output modes.

Input values are sent to the configured Dify service, as the editor explicitly
warns. Drafts remain in transient component memory, clear on service/session,
session-persistence or credential/authority changes, and are excluded from session
metadata, message history, request snapshots and telemetry. Invalid drafts disable
the composer without clearing its prompt; the editor is locked during a run. Each
request takes a value snapshot before asynchronous session preparation, retaining
the independently loaded `conversation_id`. Explicit regeneration uses the current
editor values. No automatic replay is introduced. Legacy Python `dify` remains
its existing GenericRESTAdapter mapping and does not expose this native-only UI;
registration upsert and native/Python backend behavior are unchanged.

Registered SDK agents use the existing public session ID and normalized stream
contract in Playground. Backend-owned history avoids browser-owned SDK resume
identifiers. Dify conversation binding and LangGraph proxy handling are preserved.
SDK runs disable automatic stream-to-sync fallback: an interrupted stream must not
silently replay a potentially side-effecting agent. Empty responses become visible
errors; the user may review history before explicitly trying again. Registration
is configuration success, not proof of worker/model/key health or live SDK/tool
execution. Available governed tools remain subject to server-side authorization.

Acceptance still requires frontend type/lint/i18n/build checks and real browser
coverage of both SDK choices, required fields, rapid repeated submission, busy
Escape/outside click/tab switching, reopening after dismissal, uncertain save,
stream interruption without a second invoke, resumed sessions, and Dify regression.
Source-only staging is not live acceptance or official SDK runtime verification.


### Optional Knowledge tools for SDK agents

The SDK registration form optionally reads the existing authenticated, paginated
`GET /api/v1/knowledge/datasets` through `api/knowledge.listDatasets`. Loading is
explicit and requires the console's `knowledge:dataset:view` permission. The picker
only offers returned UUID datasets with `my_permission` viewer/editor/owner; it
never accepts free-text IDs or tenant IDs. Selection defaults to none, is bounded
to 16, and is validated against the currently loaded visible catalog before save.
Only a nonempty selection adds `metadata.sdk_tools.knowledge_dataset_ids` to the
existing registration payload. No Knowledge token, worker bearer, or endpoint is
accepted from the browser. Basic key/model creation has no dataset-read prerequisite.

Refreshing clears selections before reading; failed reads never preserve a stale
binding. Unmount invalidates late results. Account/permission/role/tier changes
remount the SDK form, discarding provider keys and dataset selections. Loading,
error, empty and no-view-permission states remain distinct. Failure to list Knowledge
does not block registration without Knowledge tools. Permission display is not
authorization: the Gateway/Knowledge service must recheck the invocation caller
and dataset access on every retrieval. Explicit dataset selection does not grant
other service users access to the administrator's private datasets.

Browser acceptance must cover both SDKs, zero/one/16 selections, a denied or
revoked dataset, catalog refresh/error, account changes during reads, and request
payload inspection for absence of tenant overrides, endpoint overrides or tokens.
External pure/helper checks do not prove live ACL enforcement or retrieval.


### Governed platform tools for SDK agents

SDK registration also offers an explicit, authenticated platform-tool picker using
`listAgentTools` / `GET /api/v1/assistant/tools`. It preserves the existing service
form's account/authorization remount, transient provider-key clearing, and scoped
Knowledge dataset selection. Neither catalog refresh nor a failed read retains
prior selections. Up to 16 unique IDs are selected, never free-text IDs, schemas,
effect overrides, provider credentials, or a worker endpoint. Only selected IDs
are added as `metadata.sdk_tools.capability_ids`; the Knowledge list coexists.
The selector excludes attachment/context/memory/discovery/subagent tools and
remote MCP/skill entries, as well as read capabilities whose policy is not `never`.
Missing or inconsistent policy metadata is not selectable. Current server-side
catalog and caller authorization remain authoritative for every invocation.

A capability-enabled definition advertises `stream` only. The Gateway rejects
sync requests with `sdk_capabilities_require_stream`; Playground retains the
native SDK no-fallback/no-automatic-replay behavior. Structured `StreamChunk`
events use `sdk_tools_profile=governed_capabilities_v1` with `sdk_lifecycle`.
The reducer separately tracks approval and execution status by run/tool-call ID,
rejects stale or malformed events, preserves a whitelisted optional durable
`approval_status` (rejected/expired/approved/consumed/cancelled), and never treats an `approval_result` as a
successful execution. Empty lifecycle content does not become assistant text or
raw arguments. `tool_result` alone establishes the reported execution outcome.

The Playground renders required approvals even when ordinary tool details are
hidden. It reuses `ToolApprovalCard` and the authenticated v1 approval GET/POST
endpoints without inventing a Runtime thread ID. It fetches the exact preview,
shows effect, target, sanitized parameters, scope and expiry, then refreshes and
compares the argument hash immediately before deciding. Missing, changed,
unsupported, or expired previews fail closed; a pending unsupported request can
still be rejected. A single in-flight decision is allowed, and a recorded decision
cannot be sent again by repeated clicks. Approval resolution is distinct from a
tool result, including failed/cancelled/timeout/unknown-side-effect outcomes.

Account/authorization changes suppress old-account cards and discard late preview
responses. A stopped, disconnected, or completed stream never offers stale
approval actions; this UI does not reconnect or automatically rerun SDK writes.
The existing Runtime approval transport remains the default card behavior.

Verification for this change is staged outside the checkout: pure TypeScript
selection/configuration/lifecycle tests and source-contract checks. Full frontend
checks and real authenticated browser/provider approval flows must be reported
separately; offline fixture tests are not live execution evidence.

### Tenant MCP policies

The governance MCP tab uses the existing administrator-only `mcp-configs` list,
exact-get and upsert routes. It manages tenant server allowlists and connection
limits (1–100), with review before saving. Arrays, including an empty allowlist,
are preserved exactly; null and non-string entries are rejected. The server
retains authority over Unicode tenant validation and runtime policy behavior.
Existing server overrides are normalized from either JSONB text or an object and
preserved without displaying them. Malformed overrides fail closed; they are
never silently cleared. New policies start with no overrides. This surface does
not edit credentials/endpoints, delete policies, or claim live MCP connectivity.

Before PUT, an exact GET must still match the loaded baseline; creation requires
a verified 404 rather than a failed request. A successful PUT is followed by an
exact GET and semantic comparison of every writable field, including hidden
preserved overrides. Empty override objects normalize to null as in native SQL.
This is best-effort conflict detection, not atomic compare-and-swap. Concurrent
changes between GET and PUT remain possible with the existing backend contract.
After an uncertain result, the editor blocks resubmission and asks the operator
to close, refresh, and review the current values. No mutation is retried.

The tab shares the governance administrator gate and identity remount. Query keys
are account-scoped, inactive caches expire immediately, and late list reads are
cancellable. A token/account change aborts pending API requests and fences each
preflight/write/readback stage; cancellation cannot undo an already committed
write. Pending saves prevent dismissal and repeat submission. Initial
failure, stale cached data, empty lists and search misses remain distinct. Raw
transport errors and server overrides are not rendered. Bilingual text is
provided. Focused tests cover validation, JSONB normalization, override retention,
conflict/readback failures and uncertain writes; authenticated browser and live
persistence/runtime enforcement require separate acceptance.


### SDK run inspection and cancellation in Playground

Playground's existing assistant message now retains a genuine SDK run reference
from Gateway `assistant_run_id`/`sdk_framework` metadata, governed lifecycle
metadata, or server-owned `stats.sdk_usage` history. The message's compact SDK run
control uses the existing owner-authenticated `GET /api/v1/assistant/runs/{id}` and
`POST /api/v1/assistant/tasks/{id}/cancel`. Generic task IDs and Codex Thread/Turn
IDs are never fallback identities; no new task store, run browser or Runtime UI
contract is introduced. Legacy history without a genuine ID has no run control.

Inspection is explicit, avoiding hundreds of automatic history requests. An opened
active run polls every two seconds; terminal or unavailable reads require explicit
refresh. Responses must match run ID, SDK engine and known session before they
can enable cancellation. GET is repeated immediately before the single POST so
completion/authority expiry removes stale cancellation affordances. Failed or
interrupted cancellation is reconciled with GET only, never automatic POST replay.
Read/cancel requests have ten-second client deadlines, and unmount aborts them.
Owner, authorization-scope, token and view changes fence late callbacks; repeated
clicks share a synchronous in-flight guard. These UI fences are not authorization:
Gateway still enforces authenticated tenant/user ownership on every read/mutation.

The control distinguishes active, completed, failed, locally cancelled, expired
authority and unverified status. Transport interruption does not establish the
durable run outcome. SDK token statistics have no text-based estimate fallback;
missing/partial/invalid measurements stay unavailable, and measured zero remains
zero. Provider cost can be observed independently of token availability. The UI
explicitly distinguishes observations from settled platform charges/quota, and
local cancellation from unconfirmed provider termination or cost. Existing
non-SDK estimates, Dify/LangGraph behavior and managed Runtime UI are preserved.

A metadata-only Gateway admission chunk exposes the server's genuine ID before
first provider text; older adapters only expose it when their first SDK chunk
arrives. Before an ID arrives, only the existing transport-stop control is
available. This UI never manufactures an ID or replays an interrupted SDK invoke.

Focused Node tests cover metadata/history identity, invalid/partial/zero usage,
status binding, expiry, single-attempt cancellation, unknown outcomes, account
changes, repeated clicks, cleanup and existing route wiring. Frontend aggregate
type/lint/i18n/build and authenticated browser/provider tests remain distinct
release gates; these source/Node checks do not prove live authorization, provider
cancellation, billing or end-to-end acceptance.

SDK admission-only identity does not start first-token timing. Its validated
framework/run pair is retained independently of usage for session history, even
when an interrupted or successfully completed stream has no text. History stores
`sdk_stream_status` as a local observation (`completed`, `failed`, `interrupted`),
never as the durable run/provider outcome. The existing owner-bound run GET remains
authoritative. Missing terminal markers still fail without automatic replay;
a genuine SDK run with a successful empty terminal remains inspectable. No token
counts, provider outcome or charges are fabricated for identity-only history.

The official SDK adapter emits only the unstreamed suffix in FINAL content.
Gateway includes that suffix exactly once in SDK session history, including an
answer delivered entirely in FINAL, before its usual frontend event rewrite.
Generic non-SDK FINAL accumulation behavior remains unchanged.

### Playground completed responses without text

The shared message renderer shows a localized status for raw empty/whitespace
assistant responses only when the row has an explicit local completion observation,
its status is `completed`, and it is no longer streaming or thinking. Live rows
receive that observation at the existing confirmed-terminal final flush; restored
rows require an exact `completed` SDK or Gateway stream-status marker under the
existing SDK identity precedence. Legacy history defaults remain unchanged and do
not establish this observation. Missing, failed, interrupted and unknown markers
cannot enable the notice. This is not a durable provider or side-effect outcome.

Tool activity, SDK tools, timeline steps and artifacts suppress the extra notice,
even when their display controls are hidden. Process-only filtered output is
outside this raw-empty correction. The notice is separate UI feedback and is not
generated text, copied, shared, persisted, or used to fabricate tool results or
usage. Failed, cancelled, idle, streaming and absent-status rows keep their existing
presentation, including partial text and actions. This bounded correction does
not establish full AS-02, authenticated browser, provider execution or live/history
persistence acceptance.
