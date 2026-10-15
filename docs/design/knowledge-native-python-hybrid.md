# Native Gateway with retained Python Knowledge

## Supported implementation contract

This explicit hybrid profile makes the existing saved-binding Confluence manual
workflow reachable through native Gateway. It retains **one Python Knowledge
owner for the entire Knowledge API and its persistence/ingestion lifecycle**.
It does not split Sources reads from sync writes between implementations, mount
Python business routes inside Gateway, or require a full Rust Knowledge cutover.
This is a source/configuration interoperability contract, not an activated or
live-accepted deployment. [Plan 03](../plans/product-feature-completion-2026-09-23/03-knowledge.md)
owns the product workflow and its limitations.

Topology:

- Web calls native Gateway's `/api/v1/knowledge/…` (or `/v1/knowledge/…`).
- Native Gateway signs and forwards to retained Python Knowledge at
  `/api/v1/knowledge/…`. Both public aliases produce that same downstream prefix.
- Python Knowledge resolves current actors back through native Gateway's signed
  `POST /internal/v2/agent-capabilities/knowledge-actor` before governed effects.
- Python Knowledge owns source binding, durable sync receipt, document detach,
  ingestion enqueue, dataset ACL and object/vector ownership. Gateway owns current
  users/roles. Neither imports the other's implementation or writes its tables.

## Explicit configuration mapping

Native `gateway --config-stdin` consumes one complete, already-resolved
`GatewayDocument` JSON. Its `environment` object is an explicit configuration map;
setting only the native process's ambient environment does **not** configure it.
Keep the existing reviewed document's other settings and credentials. These are
fields to set in that document, not a standalone runnable document:

| Native Gateway document field | Required relationship |
| --- | --- |
| `environment.KB_SERVICE_URL` | Origin of the retained Python Knowledge API/all service, with root path and no credentials/query/fragment; e.g. `https://knowledge.example.invalid` |
| `native.allow_private_knowledge_http` | `false` with HTTPS; explicitly `true` only for approved private-network HTTP |
| `environment.AI_PLATFORM_INTERNAL_TOKEN` | Shared internal signing primary secret, supplied securely; at least 16 characters |
| `environment.INTERNAL_AUTH_KEYS` | Same key-id/secret mapping accepted by Python; when using rotation, configure matching keyrings on both sides |
| `environment.INTERNAL_AUTH_ACTIVE_KEY_ID` | Explicit active id present in that keyring; avoid relying on fallback ordering |
| `native.confluence_registration` | `disabled` for this existing-binding profile; native canonical registration remains unavailable and `required` fails startup |

`KB_SERVICE_URL` is projected into `runtime.knowledge_origin` and used by the
public Knowledge proxy and existing Knowledge authorization/read clients. There
is no second origin setting for the manual routes. Keep every other configured
Knowledge consumer, including the Capability Worker, on this same Knowledge
owner; changing only a public path is insufficient for a coherent profile.

Retained Python Knowledge reads ordinary service environment/settings:

| Python setting/environment | Required relationship |
| --- | --- |
| `GATEWAY_URL` | Private reachable origin of the selected native Gateway for current-actor lookup; the default Compose name is only appropriate if it resolves to that Gateway |
| `AI_PLATFORM_INTERNAL_TOKEN` | Same primary secret as native document; never supply it through a public request |
| `INTERNAL_AUTH_VERSION` | `v2` |
| `INTERNAL_AUTH_KEYS`, `INTERNAL_AUTH_ACTIVE_KEY_ID` | Matching configured keyring and explicit active id as above; the actor signer also consumes these settings |
| `INTERNAL_COMM_STATE_BACKEND`, `INTERNAL_COMM_REDIS_URL` | Production replay protection uses `redis` and its approved URL; memory replay is limited to controlled local/test evidence |
| `KNOWLEDGE_RUNTIME_ROLE` | `all`, or `api` with a separately configured compatible Python ingestion worker |
| `KNOWLEDGE_APP__ALLOW_ANONYMOUS` | `false` |

Preserve the reviewed Python Knowledge database, storage, vector, model/embedding,
provider and internal-worker configuration. Require its supported schema window
(currently epochs 5–17), scoped roles and canonical migration ownership before
starting it. Native Gateway still needs its own compatible schema/profile gate;
this document does not establish native process-wide schema admission. Do not
silently roll back a migrated database to the retained Python Gateway.

A saved active Confluence connection and owned binding must already exist in that
Knowledge deployment. The actor must be active, own the connection/binding, and
have dataset editor access. No new connection, credential, binding registration,
provider activation, scheduler, deployment or migration is performed by selecting
this contract. Python startup registers the manual coordinator in API/all; its
accepted reconciliation is process-owned with durable task identity. Crashed
pending tasks remain blocked for operator reconciliation rather than auto-replay.

## Public workflow and capability truth

1. `GET /api/v1/knowledge/{dataset}/sources` returns the Python projection through
   the proxy unchanged. `manual_sync_available` is per binding and requires the
   current source connection/ownership/editor prerequisites. A saved binding is
   not proof of a scheduler or of successful provider connectivity.
2. An explicit `POST /api/v1/knowledge/{dataset}/sources/confluence/{binding}/sync`
   sends only a fresh UUID `request_id`. The response `202` contains that exact
   durable task receipt. The native proxy classifies this as a write operation;
   key-based callers need `knowledge:write`, not merely `knowledge:read`.
3. Exact-task `GET …/sync/{request_id}` observes the accepted task, including
   failed partial progress. Sources/readback are read operations. The native
   proxy streams status/body and does not invent success, rename receipt fields
   or infer rejection from a downstream read `404`.
4. Existing Web uncertainty/recovery rules continue: no automatic POST after
   refresh, timeout or lost receipt; only explicit exact-task readback. Native
   transport retries only a connect failure with a fresh signature. Timeout or
   possibly committed response failure is not a mutation replay.

With native **Knowledge** as origin, these manual handlers/coordinator remain
absent and `confluence_sources_enabled` remains false. This profile does not
change native Knowledge capability reporting or imply full-native Confluence
support.

## Controlled interoperability verification

`knowledge_proxy/forwarding.rs` builds the actual reqwest request in a private
helper used by production transport. The native `hybrid_contract_tests` exercises
Sources, POST and exact-task GET construction, spoofed-identity stripping,
custom-key removal, body/path/identity-bound signing and replay rejection. It also
accepts the actual request captured from Python `resolve_runtime_actor` with the
native reverse-direction verifier, and rejects replay, changed body and wrong
signer direction. No request is sent by the native test.

The checked synthetic fixture in `tests/fixtures/knowledge_hybrid_manual_sync.json`
contains exact emitted native requests and the Python actor request. The Python
contract suite feeds those native bytes/headers into the real signed ASGI
middleware and public Sources/manual-sync handlers. It exercises the existing
manual coordinator/source lifecycle with controlled persistence, provider and
current-actor collaborators, including capability gating, admission, terminal
receipt, shared-source retention and tamper/replay rejection. Python tests pin the clock
to each native producer timestamp; the frozen Python actor fixture is verified
with an explicitly widened, test-only native skew allowance. Production
clock/skew/replay settings are unchanged.

Run via the repository's approved isolated Rust verification environment:

```sh
cargo test --locked --offline -p ai-platform-gateway --lib hybrid_contract_tests::
uv run --all-packages --extra test pytest -q --no-cov tests/services/knowledge/test_native_gateway_manual_sync_contract.py
```

For refreshed native evidence, set `KNOWLEDGE_HYBRID_CONTRACT_OUTPUT` only in that
test process to an external output file. It exports requests constructed and
verified during the test. Preserve source hashes and distinguish fixture emission
from real runtime acceptance. The JSON contains only explicitly synthetic keys
and example.invalid origins; it is never runtime configuration.

These checks do **not** exercise sockets, native public JWT/API-key admission,
actual Gateway actor SQL, Redis, PostgreSQL, startup, providers, worker restart,
Web browser interaction or deployment. Source mounting/configuration is inspected
separately. Actual selected binaries/images, database roles/schema, both network
hops, actor revocation, provider execution and operator crash recovery remain
release acceptance gates. Existing test totals or a config-only projection are
not substitutes for those gates.

## Existing-connection source setup extension (2026-10-13)

A saved binding is no longer required to begin the bounded Web setup path when
an active, owned Knowledge Confluence connection already exists. The Python
owner exposes safe paginated connection selection, exact space/homepage preview,
explicit idempotent manual binding creation and exact binding receipt readback.
The existing native Gateway signed wildcard and read/write classification carry
these endpoints without new native source ownership. See the owning
[Knowledge plan](../plans/product-feature-completion-2026-09-23/03-knowledge.md#existing-saved-connection-to-manual-source-binding-2026-10-13).

The selected import scope is the verified homepage and its descendants through
10 levels, not every page in the space. Binding creation starts no sync, scheduler
or provider mutation. The existing explicit manual sync remains a separate action.
The saved active connection is still a prerequisite: no credential registration,
OAuth-to-Knowledge credential copy, native canonical registration, deployment or
migration is performed. Live profile/provider/database/browser acceptance remains
open; offline production-path evidence does not activate this profile.

### Secure Confluence Cloud connection setup

The retained Python Knowledge owner now also exposes the explicit create/verify
and metadata-readback flow described in [plan 03](../plans/product-feature-completion-2026-09-23/03-knowledge.md#secure-cloud-connection-onboarding-2026-10-13).
Native Gateway continues its signed wildcard forwarding; no native credential
store or alternate sync owner is introduced. The operator must separately supply
the existing dedicated `GATEWAY_ENCRYPTION_KEY` to Knowledge API and worker roles.
A missing key disables new setup with an actionable capability reason. New
credentials use an authenticated owner/origin-bound envelope; historical
plaintext rows are neither migrated nor deleted. Connection setup is Cloud-only,
manual and explicit, followed by the existing verify-space/bind/sync actions.
Source tests and synthetic transport do not establish a live hybrid acceptance.


### Reversible saved connection controls (2026-10-13)

The Sources page exposes owner-scoped metadata management independently of the
credential-encryption key and current index writability. Current dataset editor
rights, current authenticated actor, exact connection owner and tenant remain
required for changes; canonical grants are rechecked in the held transaction.
Pause is an application admission control, not provider-token revocation. It
applies to every binding using the connection. Resume never schedules or starts
work and does not imply index readiness. Credentials (both versioned encrypted
and legacy plaintext), binding configuration and task records stay unchanged.

GET `/knowledge/{dataset}/sources/confluence/connections/manage` includes active
and disabled owned connections, with `offset`/`next_offset` pagination. GET
`/knowledge/{dataset}/sources/confluence/connections/{id}/control` returns current
metadata plus opaque `revision`, the same-tenant connection's total binding count
and paginated impact. Binding names/IDs/datasets/status are disclosed only for
owned bindings in currently visible same-tenant datasets; inaccessible bindings
have only a safe per-page hidden count. Impact is a point-in-time preview; changes
apply to all bindings, including bindings added subsequently. Legacy string
connection IDs are supported. Other legacy statuses are visible but not mutable.

PATCH of the latter path accepts only `expected_revision`, `expected_status`
and desired `status` (`active` or `disabled`). A short transaction locks connection
then dataset (matching binding admission order) and compares the exact timestamp serialization and status. It
updates only status and the existing updated_at execution fence. The schema's
BEFORE UPDATE trigger assigns transaction NOW(), so the SQL also requires
`transaction_timestamp() > updated_at`; a non-advancing clock/older transaction
fails without a write, rather than reviving an old execution generation. A stale
CAS returns 409. Generic response idempotency caching is bypassed for this exact
PATCH path, preserving current authority checks. Existing Python Gateway and
native-Gateway/Python-Knowledge hybrid wildcard forwarding retain PATCH support;
this is not a new full-native Knowledge handler.

Pause blocks new sync admission and pre-pause runs at their next current-authority
or publication check. A subsequent resume advances the revision again, so it
cannot revive an already-admitted pre-pause task. This is not instantaneous
cancellation or a globally linearized I/O barrier: an already-admitted provider
request or current publication step may finish; the UI may show processing
briefly, and partial effects remain. Published/indexed content is not removed.

Web requires an impact review and explicit all-bindings confirmation, stores only
non-secret CAS metadata before PATCH, and fences repeat clicks. Any uncertain or
rejected mutation response requires metadata GET readback, never automatic PATCH
retry. GET404 does not prove failure. Readback describes the current state, not
which historical request committed; the user explicitly adopts it before making
another decision. Account/dataset recovery ownership prevents stale controls
from overwriting another view's pending change. Capability changes invalidate
in-flight reads and impact confirmation. Backend authorization is authoritative.

Controlled tests cover exact/legacy ownership, tenant/current editor checks,
key/index-independent pause, hidden impact and pagination, encrypted/plaintext
credential preservation, stale/unchanged revision refusal, cache bypass, lost
acknowledgment readback, active-task fencing and publication rechecks after pause
and fast resume. Production component/API tests cover confirmation, recovery,
no blind retries and stale capability/read fencing. These use synthetic I/O and
persistence; real PostgreSQL trigger/lock/concurrency, browser, live provider and
deployment acceptance are not established. No schema, scheduler, credential
rotation/revocation, automatic resync or delete behavior is added.

## Source-owned wildcard contract composition

The existing `make snapshot-native-openapi` path now composes the two native
Knowledge wildcard aliases with the retained Python owner's actual mounted
FastAPI/Pydantic declarations. The small Rust producer shares prefix, method,
read/write and special-route helpers with the production proxy. Its compiled
source hashes bind forwarding, account confirmation, current-identity handling
and the actual host mounts. Python API construction and the offline exporter use
one required-router registry; export imports routers without constructing the
service app or entering its lifespan. Fresh hash-seed processes must agree.

The published contract/profile remain unchanged. Only the existing five explicit
methods on each wildcard alias are added to the native candidate. Concrete
Knowledge targets and namespaced owner components are supporting provenance and
constraints, not new published operations. Internal owner routes are excluded.
This is specifically the native Gateway + retained Python Knowledge profile;
pointing the Gateway at native Knowledge does not inherit these Python contracts.

The same candidate is consumed by `make verify-native-openapi-contract`. Its
source-composition check rejects stale compiled proxy sources, changed aliases or
upstream prefix, lost method/transform declarations, stale owner sources and
modified composed schemas. The ordinary full comparison still reports actual
security/status/body differences against publication and still fails for missing
common operations. Generating this partial candidate cannot satisfy the full
native gate or claim deployed release identity.

Important client boundaries remain visible:

- Gateway authentication has guest/configured-static-key/current-account branches;
  per-operation Python actor and object ACL checks remain authoritative. A wildcard
  auth alternative is not permission to every concrete route.
- DELETE datasets/{dataset_id} verifies/removes the account password and signs a
  short-lived confirmation. GET datasets/{dataset_id}/impact remains Gateway-owned
  after a signed owner precheck. Neither is mislabeled as an unchanged Python call.
- Status/body bytes and content types are streamed. Native body/admission limits,
  timeout/connect errors, no-store, hop-header filtering and SSE handling still
  apply. Only a connect failure retries; a timeout or lost accepted response does
  not authorize replaying a mutation. Keep exact task/binding/connection readback
  and the existing user-visible uncertain-outcome recovery instructions.
- Owner responses without declared schemas stay opaque. Sanitized connection
  handlers that parse Request.json manually keep an explicit undeclared-body gap.
  Custom validators, streaming payloads, dynamic rows and exhaustive errors are
  not manufactured from sample data or from the SDK publication.

The exporter creates only external schema/evidence files. Controlled declaration,
source-mutation and exact native helper/signature tests are separate from actual
Gateway admission, selected process images, service/network hops, Redis/SQL,
provider execution, restart and browser acceptance. Do not relabel unrun checks
or estimated coverage as measured results.

Offline export also needs a verified cl100k_base tokenizer cache because the
retained owner's chunking module constructs that encoder while importing routers.
Pass `KNOWLEDGE_OPENAPI_TOKENIZER_CACHE=/external/verified/cache` to the existing
snapshot Make target (or `--knowledge-tokenizer-cache` to its script). The exporter
checks the asset against the installed tokenizer's source-declared hash before
importing routes and cannot download missing data. Asset preparation is separate,
explicit, and recorded alongside the dependency/source identities; missing or
corrupt data fails closed. Fresh owner processes skip site initialization and
terminate on attempted networking/process launch rather than hiding an import
fallback. These are offline evidence controls, not a claim about live process
network isolation.

Owner route discovery is bound to the locked FastAPI version and the reviewed
routing/OpenAPI source hashes. Its private include-context iterator is used only
for ordered provenance; public get_openapi receives the untouched router tree.
Nested prefixes, inherited dependencies/responses and include-in-schema flags
therefore retain framework behavior. Only selected public operations and their
transitive schema references are composed; private-only models are removed.
A framework revision, duplicate public operation, missing reference, unsupported
component kind or discovery/schema mismatch fails closed and requires review.

Controlled verification on 2026-10-13 measured 86 public owner paths, 101 operations
and 59 reachable schemas from the six actual mounts; five internal operations were
excluded. The 150 selected Python contract tests and changed-file Ruff check passed
with terminal offline import guards. Newly compiled, source-bound native producer
and Gateway test binaries passed the two producer tests and four socket-free
routing/signature tests. The actual combined wrapper verified fresh deterministic
native/owner exports and measured 35 native paths/53 operations: 43 bounded native
operations plus 10 opaque wildcard operations. Owner metadata preserves 97 opaque
success declarations and 18 undeclared request-body operations. It does not add 101
published operations or manufacture dynamic schemas.

The full native comparison remains FAIL 449, with 399 missing published operations.
Compared with the prior FAIL 431, two missing-wildcard-path diagnostics disappear;
their ten methods now expose ten authentication-contract differences and ten
absent guaranteed 422 detail fields. No unrelated diagnostic changes. Preserve
those limitations rather than claim parity or impose a false universal error
shape. These compiled/controlled checks do not establish a deployed process,
service transport, live SQL/Redis/provider execution or production authorization.


## Citation current-source read validation

The current `/documents/{document_id}/source` reader loads its response row before
checking document active-state authority, then rechecks current dataset viewer
permission and content generation. This rejects disable/archive/lifecycle-marker
changes observed during the row fetch, including deactivation before its revision
bump, and ACL or generation changes observed before return. Native forwarding
and the Web legacy-citation reader retain this Python-owned serving authority.

This is optimistic read validation, not atomic revocation after the last authority
read; already returned content cannot be recalled. Current-source reads do not
establish immutable historical identity. Versioned citations still use their
separate hash-bound source endpoint. Controlled await-boundary tests do not prove
live database isolation, browser behavior, connector detach or deployment.
