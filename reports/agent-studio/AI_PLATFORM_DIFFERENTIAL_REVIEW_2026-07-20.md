# AI Platform / Agent Studio Differential Security Review

**Review date:** 2026-07-20
**Baseline:** `945eb2225d644093802bf5f9d75ca4d9dbad6a8d` (`main`, equal to `origin/main` at review start)
**Candidate:** local Agent Studio AS-00 through AS-09 working tree
**Strategy:** SURGICAL (large change set: 227 tracked files plus 111 untracked files)
**Outcome:** **APPROVE for merge; ready-but-not-deployed**
**Blocking findings:** 0

## Executive Summary

The change introduces the complete Agent Studio lifecycle: tenant-scoped Agent
CRUD and ACLs, immutable Draft/Version material, signed runtime resolution,
MCP/Connector/Skill/Knowledge bindings, Preview, Eval, atomic publish and
rollback, Hosted, origin-bound Embed, scoped Runtime API tokens, observability,
governance, retention and deletion. It also preserves the existing Assistant
composition path and supplies a multi-architecture open-source container
quickstart.

No Critical, High or blocker-level Medium security regression was found. The
highest-risk paths were read in depth and adversarially traced. Tenant identity
is resolved server-side; mutable browser fields cannot select a tenant, model,
prompt, capability or published Version. Runtime material is signed with a
closed schema and request/snapshot hashes, MCP destinations fail closed and are
DNS-pinned, releases lock and revalidate mutable dependencies, and public
channels use exact publication/origin/scoped-token bindings.

The accepted terminal aggregate is the governing test receipt:
`make verify-agent-studio` passed 39/39 required gates with zero skips on stable
source SHA-256
`2ffa4684a3055b123b51b779eef9321e0821c1940c2942c10b34a2c054f14115`.
The run includes PostgreSQL concurrency/constraint tests, authenticated runtime
isolation, browser accessibility and interaction suites, built-image header
smoke, security tests and existing Assistant regression. See
`reports/agent-studio/agent-studio-regression-v1-result.json:1` and
`reports/agent-studio/as-09-whole-demand-matrix.md:1`.

## Review Scope and Coverage

### Change inventory

| Surface | Changed paths observed | Review depth |
| --- | ---: | --- |
| Assistant service | 132 | Deep on Agent runtime, MCP, capability policy and compatibility roots; surface scan elsewhere |
| Core packages | 18 | Deep on Agent runtime signer, repository, resource resolver and MCP repository |
| Gateway API | 18 | Deep on Agent management, Runtime, public channels, MCP and schemas |
| Web | 30 | Deep on Hosted/Embed messaging and token handling; browser/build evidence for Agent pages |
| Database | 11 migrations | Deep on tenant keys, immutable history, publish/runtime constraints and governance |
| Tests | 39 | Mapped to the immutable 39-gate manifest and security attack surfaces |
| Deployment/scripts | 39 | Container ownership, secret inputs, multi-architecture images and built header behavior |
| Runbooks/reports | documentation/evidence | Consistency and evidence-boundary scan |

The numerical inventory describes the complete dirty working tree, including
pre-existing Assistant/doc-generation and runbook edits. The eventual commits
must exclude machine-local state and unrelated inherited edits; this review
does not reinterpret unrelated edits as Agent Studio implementation.

### Baseline and history

The baseline branch was synchronized (`HEAD == origin/main`). Git history was
consulted for removed security-sensitive behavior. Commit `b4db501` introduced
the previous MCP SSRF hardening and tenant-tool policy baseline. The candidate
does not remove that invariant: it replaces a single-record, DNS-failure-
allowing check with fail-closed multi-record resolution, IP-literal connection
pinning, TLS SNI/Host preservation and redirect refusal in
`apps/assistant-service/src/assistant_service/core/mcp/client.py:145`.

The legacy Assistant tenant policy still keeps its compatibility semantics,
while the new Agent path adds a separate no-expansion, database-required
resource decision in
`apps/assistant-service/src/assistant_service/core/tools/tenant_tool_policy.py:40`.
This avoids changing the existing Assistant contract while making Agent runs
fail closed.

### Risk classification

- **High:** authentication/authorization, tenant boundaries, HMAC/runtime
  envelope, API tokens, MCP/OAuth egress, public Embed, release/rollback and
  deletion.
- **Medium:** Agent lifecycle state changes, Eval gates, observability,
  governance, container/runtime configuration and new public APIs.
- **Low:** styling, translations, deterministic fixtures, reports and most
  presentation-only changes.

## Security Invariants

1. A request cannot choose or widen its tenant, caller principal, Agent,
   published Version, model, prompt, capabilities or knowledge bindings.
2. A Draft is mutable only under optimistic revision and tenant ACL checks; a
   Version and its normalized bindings are immutable after sealing.
3. Publish and rollback are owner-only, idempotent and atomic; mutable release
   inputs are revalidated under lock immediately before pointer movement.
4. Existing sessions remain pinned to their resolved Version after a rollback;
   new sessions follow the current Publication pointer.
5. Runtime Envelope verification binds request body, Snapshot, tenant, caller,
   session, Publication/channel, Version/revision and spec hash; replay is
   consumed atomically and fails closed when its store is unavailable.
6. Tenant MCP destinations require HTTPS, safe DNS results and connection
   pinning; redirects, DNS-set changes, private/link-local/reserved targets,
   oversized responses, session confusion and OAuth audience elevation fail
   closed.
7. Raw Runtime API tokens are returned only on issue/rotation; only SHA-256
   hashes are persisted, and owner/scope/expiry/revocation checks precede use.
8. Hosted/Embed exposure follows the Publication's auth mode. Embed documents
   require an exact allowed parent origin, use exact `frame-ancestors`, and
   exchange protocol messages only with the expected source and origin.
9. Agent specs, audit payloads and API responses recursively reject or redact
   credential-like fields.
10. Existing Assistant, Knowledge, Eval and Share routes remain mounted when
    Agent Studio is disabled.

## Critical Path Analysis

### Runtime Envelope and execution

`AgentRuntimeSigner` validates a closed Snapshot, canonicalizes JSON with NaN
rejection, hashes the body and Snapshot, signs with HMAC-SHA256, enforces
issuer/identity/time bounds and consumes a nonce via an atomic replay-store
contract (`packages/ai-gateway-core/src/ai_gateway_core/agents/runtime.py:289`).
The Gateway builds the Snapshot only from repository resolution and current
resource authorization (`src/api/v1/agent_runtime.py:504`), then signs the exact
Assistant request (`src/api/v1/agent_runtime.py:1174`). The Assistant independently
verifies the envelope and switches to Redis replay storage when configured for
multi-worker operation
(`apps/assistant-service/src/assistant_service/api/routes/chat.py:249`).

Blast radius: `AgentRuntimeSigner` appears in 6 Python files, `_build_snapshot`
in 7, and `_proxy_runtime_stream` in 2. The terminal manifest covers envelope
unit tests, resolver isolation, trace/session behavior, Assistant runtime gates
and credentialed isolation.

### Tenant ACL and persistence

All management endpoints require an authenticated non-public actor before
repository calls (`src/api/v1/agents.py:159`). The repository's common lookup
joins membership under both tenant and Agent identity and collapses denial into
not-found (`packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_repository.py:560`).
Migrations use tenant-qualified primary/candidate keys and composite foreign
keys, including Agent ownership, Drafts, Versions, Publications, publish events,
tokens, MCP resources and knowledge bindings
(`database/migrations/071_agent_studio_domain.sql:29`).

The database role does not add PostgreSQL RLS in this change; isolation relies
on application predicates plus composite constraints. That is acceptable for
the current architecture and is extensively tested, but RLS remains a useful
production defense-in-depth item.

### Publish, rollback and idempotency

Publish hashes the request identity, takes a transaction-scoped idempotency
lock, locks the Agent/Draft/Publication, requires a passed non-stale evaluation,
matches release fingerprints and revalidates model, Dataset and resource
authority before atomically inserting the immutable Version/event and moving
the Publication pointer
(`packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_repository.py:3184`).
Rollback applies the same ownership, locking and idempotency discipline, and
permits only a non-revoked Version that actually belongs to that Publication's
history (`agent_repository.py:3756`).

Blast radius: the publish and rollback repository operations each appear in 4
Python files. The accepted AS-06 gate exercised 35 publish/API/PostgreSQL tests,
including races, stale evidence, conflicting idempotency keys, invalid rollback
targets and pinned sessions.

### MCP, Connector and OAuth egress

Tenant MCP registry writes resolve destinations before persistence
(`src/api/v1/mcp.py:181`). Runtime repeats validation on every request, validates
every DNS answer, connects to one validated IP literal while preserving Host
and TLS SNI, rejects DNS-set changes and redirects, pins the MCP session, bounds
response bytes and exposes only stable redacted errors
(`apps/assistant-service/src/assistant_service/core/mcp/client.py:145`). OAuth
metadata/token calls use the same egress boundary, PKCE S256, one-time state,
issuer-origin equality, exact audience/resource checks, non-expanding scopes and
opaque secret references (`apps/assistant-service/src/assistant_service/core/mcp/oauth.py:158`).

Blast radius: `MCPClient` appears in 12 Python files and the persistence
destination validator in one API module. AS-03 executed registry/runtime,
security, lint and existing Assistant regression gates with zero skips.

### Hosted, Embed and Runtime API

The public resolver looks up by stable public ID and obtains the real tenant
only after access is accepted
(`packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_repository.py:4431`).
Embed authorization is a short-lived HMAC claim bound to public ID, exact parent
origin and a non-PII abuse subject. The document escapes dynamic HTML, sets
`default-src 'none'`, exact `frame-ancestors`, `no-store` and restrictive
permissions (`src/api/v1/agent_public.py:55`). Browser code uses `textContent`,
checks both message source and origin, and never broadcasts with `*`
(`web/public/agent-embed.js:1`, `web/public/agent-widget.js:1`). API token
resolution hashes the presented token and checks its exact Publication, channel,
scope, expiry and revocation under row lock (`agent_repository.py:4516`).

AS-07 covered Runtime API security, config and built-image headers, Hosted/Embed
browser behavior and the complete existing-route browser suite.

### Governance and deletion

Governance limits are loaded from tenant/Agent policy, intersected with the
Publication's narrower policy, and enforced atomically across principal, IP and
Publication buckets. Missing governance or Redis state fails closed
(`src/api/v1/agent_runtime.py:700`). Deletion is scoped by tenant and Agent,
honors legal hold, deletes mutable runtime/user data in a transaction, revokes
tokens/grants and keeps immutable release/audit material. Table names used by
the deletion loop are fixed internal constants, not request-derived identifiers
(`agent_repository.py:5960`).

## Adversarial Scenarios

| Scenario | Expected control | Result |
| --- | --- | --- |
| Forge tenant/model/tool fields in a browser chat request | closed request schema plus server-built signed Snapshot | blocked; covered by envelope/forgery tests |
| Replay or mutate a signed Agent request | body/Snapshot hashes, HMAC, TTL and atomic nonce store | blocked; replay-store failures also deny |
| Use DNS rebinding or a mixed public/private DNS answer for MCP SSRF | all-address validation, re-resolution equality and IP pin | blocked |
| Redirect MCP/OAuth credentials to another host | redirects disabled; Host/SNI and issuer/audience pinned | blocked |
| Publish with a stale Eval or swap a dependency after Eval | release identity comparison and in-transaction resource/model revalidation | blocked |
| Reuse an idempotency key for another publish/rollback/body | identity/request hashes under lock | conflict, no second execution |
| Roll back to another Agent's or unhistorical Version | tenant/Agent composite predicates and channel-history check | blocked |
| Frame Embed from an unapproved parent or spoof `postMessage` | exact origin policy, CSP and source/origin checks | blocked |
| Exfiltrate a Runtime token through list APIs or stored spec | one-time raw return, hash-only storage, closed/redacted schemas | blocked |
| Delete another tenant's runtime data or bypass legal hold | tenant-qualified queries, owner gate and terminal deletion guard | blocked |

## Findings

### Blocking findings

None.

### Non-blocking hardening observations

1. **Production replay backend and key separation.** The quickstart correctly
   defaults to a single-instance in-memory replay store and documents Redis for
   multi-worker/multi-replica operation (`.env.example:42`). Production should
   select Redis and may use a separately rotated Agent/Embed signing purpose
   instead of relying on the internal shared-secret fallback. This is already a
   deployment-readiness item, not a local implementation defect.
2. **Database defense in depth.** Tenant-qualified predicates and composite
   foreign keys are strong, but PostgreSQL RLS is not enabled for the new
   tables. Consider an additive RLS rollout after measuring connection-pool and
   migration implications.
3. **Complexity concentration.** `agent_repository.py` is 6,881 lines,
   `agents.py` 1,922 and `agent_runtime.py` 1,695. Their common gates and
   transaction boundaries are coherent, but future maintenance risk will rise
   unless they are split by lifecycle, release, delivery and governance domain.
4. **Production evidence boundary.** Provider-quality smoke, production Secret
   Store/OAuth/egress configuration, monitoring observation and rollout
   authorization remain deliberately unexecuted. They block deployment, not
   merge or the local Agent Studio completion claim.

## Test and Evidence Assessment

The accepted run executed every entry in
`tests/fixtures/agent-studio/regression_manifest.json:1` from one stable source
snapshot:

- required/executed/passed/failed: **39/39/39/0**;
- non-zero skipped summaries: **0**;
- Agent Studio browser suite: 25/25;
- full open-source existing-route browser suite: 41/41;
- publish/Eval browser suite: 10/10;
- Hosted/Embed browser suite: 8/8;
- analytics browser suite: 5/5;
- publish/API/PostgreSQL suite: 35/35;
- operations/governance suite: 24/24;
- Assistant compatibility groups: 33/77/10/98 plus golden and isolation 6/6.

The manifest/result/log hashes were independently checked during AS-09. The
legacy Harness `--strict` option is unsupported by the installed v2 validator
and is not represented as passing. The supported structure/claim check passed,
but product completion rests on the actual 39-gate run and independent Critic.

## Recommendation

Merge the scoped Agent Studio and open-source distribution changes. Do not
stage `.claude/settings.json`, ignored local Harness documents, or unrelated
inherited doc-generation/runbook edits. Keep the release status
`ready-but-not-deployed` until provider, production security configuration,
monitoring and rollout authorization are supplied.

## Methodology and Limitations

The review followed differential-review plus audit-context-building: baseline
and history inspection, risk triage, critical code/data-flow reading, removed
validation analysis, test mapping, quantitative blast-radius search and
concrete attacker scenarios. All changed paths received a name/diff/pattern
surface scan; high-risk Agent Studio paths received direct code inspection.

This was not a line-by-line audit of every one of the 338 dirty paths. The
SURGICAL strategy intentionally relied on the immutable aggregate for lower-risk
UI, documentation and broad Assistant regression, while reading auth, crypto,
external calls, persistence and public delivery deeply. No claim is made about
production provider quality, organization incident-response practice or a
deployed environment.
