# Scoped Knowledge reads from official SDK agents

Status: source proposal with offline tests; not live/provider-verified. This builds
on the optional official SDK adapters and does not implement or bypass Codex write
approvals. The Gateway remains Rust and Knowledge remains its own signed HTTP/data
authority. No new database or long-lived service is created by this read broker.

## Explicit selection

`metadata.sdk_tools` is optional. Its read field is `knowledge_dataset_ids`: up to
16 unique UUID strings, selected explicitly from the user's visible datasets. Missing
or empty means the two pure tools only, plus tool-less durable lifecycle admission. Registration validates the shape before save.
The list is a maximum resource scope, not permission by itself; Knowledge still
requires current viewer access for the authenticated user. The returned stream
profile is `scoped_knowledge_read_v1` when a selected list is active.

## Actual trust path

1. Existing public invoke/stream authenticates its JWT and obtains `Principal`.
   It issues an in-memory opaque provenance lease for every SDK profile for that user, tenant, service and
   request ID. Body `context`, user/tenant overrides, or a worker-supplied identity
   cannot issue a lease. API-key/userless task paths cannot create one implicitly.
2. The normal service authorization/preflight and session ownership checks run.
   Only then can the SDK adapter activate the lease and pin the resolved session.
   A lease is owned by that invocation's RAII guard or its streaming HTTP body.
   Completion/disconnection drops it. TTL is 330 seconds for read-only profiles, with a 1,024-entry cap;
   repeated active request IDs are rejected rather than overwrite authority.
3. The worker receives only the random 256-bit grant and explicit dataset IDs.
   Empty dataset selection never creates a Knowledge read_bridge; its run-lifecycle
   grant cannot admit or retrieve any dataset.
   The original JWT remains inside Gateway memory, is never serialized to worker,
   persisted, used as a model prompt, or forwarded to the Knowledge service.
4. The worker calls only operator `SDK_AGENT_GATEWAY_URL` and the fixed private
   `/internal/v1/sdk-tools/knowledge/admit` or `/retrieve` paths. HTTPS is required
   except literal loopback/localhost development. Redirects/proxies are disabled;
   no provider key is included in either call.
5. On each call Gateway reads the grant, checks expiry/active invocation and
   resource binding, reloads current service selection/status, and reauthenticates
   the original JWT using the existing KnowledgeProxy identity resolver. The
   current Gateway session ownership/expiry, service-role authorization and AgentInvoke
   capability are also rechecked on each call. The resulting genuine current ContentIdentity is forwarded through the existing
   signed Knowledge boundary, which yields the normal UserContext at Knowledge.
   No roles are invented from request strings.
6. Retrieval uses the existing Knowledge `retrieve` use case, including viewer
   access, active generation and published source identity checks. Authority is
   checked again before returning content. Expired/revoked grants fail closed.

Every retrieval takes exactly `{dataset_id, query, top_k}`. Unknown fields are
rejected. There is no generic HTTP/path override, actor override, SQL, filesystem,
write operation, arbitrary remote MCP or raw credential delegation. Query length
is at most 4 KiB, `top_k` 1–5, returned JSON at most 1 MiB, and broker timeout 30
seconds. Grants allow a bounded 32 admissions (preflight/rechecks count toward it).

## Historical context and replay

Before loading SDK context or returning a cached receipt, worker admission checks
current viewer access to every selected dataset using the signed dataset GET path.
The child repeats admission immediately before invoking the SDK. A successful
retrieval from an earlier turn is not enough to authorize a later turn after ACL
revocation.

The dataset list is immutable for a session via `sdk-profile.json`. A changed list
requires a new session, rather than mixing older retrieved context with a smaller
current allowlist. Old pure-only sessions can adopt an empty profile; existing
pre-profile context cannot silently adopt a Knowledge profile. This is scoped
worker state subject to the same durable deletion/retention path as SDK transcripts.

Receipt fingerprints exclude only the fresh ephemeral grant, while retaining
selected dataset IDs and all other semantic input. No grant or original JWT is
written into pending markers or receipts. Cached results require fresh current
access admission too. A crashed/unknown SDK run remains subject to the worker's
reconciliation barrier; these ephemeral read leases do not claim durable write
recovery or cross-Gateway-restart continuation.

## Integration coordination

This package changes native adapter registration and invocation HTTP lifetime,
foundation grant/config modules, existing KnowledgeProxy and graph assembly, plus
SDK worker contract/engines/child/Knowledge tool. Worker `server.py` is owned by the
packaging/lifecycle followup, which must integrate all three narrow changes:

- Copy only operator `SDK_AGENT_GATEWAY_URL` into the scrubbed child environment
- Exclude only `read_bridge.grant` from receipt fingerprint
- Under session lock, run `ensure_profile` and awaited `ensure_access` before
  receipt replay/pending creation, with redacted failure and guaranteed lock release

The durable session-cleanup followup adds `session_pool` to NativeSdkFactory and
re-registers it; that registration must also retain `read_grants:
registry.sdk_read_grants()` and `encryption_key: registry.sdk_agent_key_handle()`.
These changes are separate patches to one existing factory, not separate adapters.

## Verification

Eight Python offline tests exercise opt-in tool schema, selected-dataset checks,
operator origin constraints, credential minimization, hidden grant repr, immutable
profile and safe legacy handling. A mocked HTTP opener is used; no network request
was made. Four Rust external grant tests are staged for root's controlled native
verification: identity binding, absent/repeated grants, activation/drop revocation,
and dataset-config validation. They were not compiled by this worker.

Required remaining gates: native compilation, actual JWT revocation and Knowledge
ACL/generation hosted scenarios, invocation/body-drop cancellation, receipt replay
after ACL revoke, provider SDK tool execution, session cleanup/retention and worker
image/lock verification. Existing source checks and SDK test doubles do not prove
these live behaviors. Enterprise writes remain outside this permission surface.

## Governed-profile composition

The separate [SDK capability authority](sdk-capability-authority.md) adds optional
capability_ids to the same selection object. For those profiles the in-memory
bearer has at most 1,830 seconds of provenance lifetime; durable SDK run ownership
and revocation remain mandatory, and only Rust Runtime signs Worker leases. This
does not promote the read bearer into write authority. KB ACL/current-service checks,
resource limits and read-profile isolation remain unchanged.
