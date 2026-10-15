# Optional official SDK worker: candidate packaging and rollout gates

Status: source candidate only, 2026-10-12. No image was built or deployed. This does
not claim production readiness or adoption of the proposed
[ADR-012](../architecture/ADR-012-optional-official-sdk-execution.md).

## Who configures what

End users register an explicit OpenAI Agents or Claude Agent SDK service, supplying
a write-only provider API key, an entitled model and instructions. Worker URL/token,
provider base URLs, permissions and filesystem paths are never user-editable fields.
The native Gateway strips keys from all detail projections, including administrators.
Existing core Assistant/Codex deployment and Qwen readiness remain unchanged.

Operators supply `SDK_AGENT_WORKER_IMAGE` (tested immutable tag or digest),
`SDK_AGENT_WORKER_TOKEN` (32–4096 printable non-whitespace ASCII characters from a
secret store) and the Gateway route. Never print `.env`, render interpolated Compose
configuration into logs, put secrets in build arguments, or reuse provider keys as
worker tokens. This overlay injects the shared service token at runtime only. Direct
worker deployments alternatively support `SDK_AGENT_WORKER_TOKEN_FILE`; supplying
both sources fails closed. The native Gateway currently consumes the token env var,
so its supervisor must perform any secret-file-to-env injection without logging.

## Optional topology

`docker-compose.sdk-agent-worker.yml` is an opt-in overlay, with profile `sdk-agents`.
It does not add a build to the normal quickstart. It must be combined with an already
verified SDK-capable **native** Gateway deployment: the default legacy Python
Gateway image cannot dispatch these Rust adapters. This overlay does not fabricate
a replacement native Gateway image or silently rewrite its database/startup config.

The worker shares `gateway` network namespace, binds `127.0.0.1:8096`, and publishes
no ports. The Gateway uses `SDK_AGENT_WORKER_URL=http://127.0.0.1:8096`; this matches
the adapter's explicit loopback-only HTTP exception. No cross-container plaintext
credentials traverse a bridge. For the optional Knowledge bridge, worker-only
`SDK_AGENT_GATEWAY_URL` defaults to `http://127.0.0.1:8080`, matching the native
Gateway default in `migration_host/settings.rs`. If the native Gateway port changes,
operators must set this origin to its verified loopback listener. Separate/direct
worker deployments must configure their verified private HTTPS Gateway callback
origin. This is never a user-editable field, inherited user token or public API URL;
run-bound read grants are used by the KB callback. Recreating the Gateway network namespace requires
recreating its sidecar afterward; this lifecycle has not been tested here. The
worker must never be placed behind frontend/nginx or public service routing.

The shared namespace is not a provider-only egress policy: it permits access to
Gateway-local sockets and the Gateway's networks. Containers also share a kernel;
this is not a hard multi-tenant sandbox. For separate-node/isolation deployment,
provide private TLS/mTLS termination with a Gateway-trusted certificate and network
policy limiting ingress to Gateway plus provider/authorized callback egress. That
infrastructure is not included. Do not weaken the HTTPS URL validator to make a
bridge-host HTTP address work. Health failure degrades SDK services only; it must
not become a mandatory readiness dependency for the core Assistant.

## Image/source shape and checks

The Docker build context is only `apps/sdk-agent-worker`; `.dockerignore` admits
only package metadata and Python source, excluding env files, state, caches and
credentials. The final image contains the installed worker/SDKs, not uv or build
sources; it runs as UID/GID 10001, read-only root, no capabilities/host mounts,
bounded memory/CPU/processes and private temporary storage. `init: true` reaps SDK
children. Built-in shell/file tools remain disabled by the SDK contract.

`docker-compose.sdk-agent-worker.build.yml` is a separate explicit source override.
The worker owns `apps/sdk-agent-worker/uv.lock`, independent of the Gateway
workspace. It was originally resolved from official PyPI with uv 0.8.22; the
unchanged lock is now verified with uv 0.13.0. Exact SDK pins remain
in `pyproject.toml`; the lock also fixes transitive versions and registry artifact
hashes. Its required environments include Python 3.12 on Linux amd64 and arm64.
The separate `build` dependency group pins Hatchling and locks its dependencies;
these build tools are not copied into the runtime image.

The Dockerfile rejects stale metadata using an offline lock check, then uses frozen,
wheel-only sync for both environments. It builds only the local worker wheel with
the locked backend and network access disabled, and installs that wheel without
resolving dependencies. `.dockerignore` explicitly includes the lock. This makes
Python dependency installation repeatable; **the Python base and uv image tags
still need digest pins before release**. No multi-architecture container image,
container runtime behavior or provider execution is established by a dependency
lock or cross-platform wheel check.

To refresh dependencies intentionally, use uv 0.13.0 and Python 3.12, run
`uv lock --project apps/sdk-agent-worker --python 3.12 --default-index https://pypi.org/simple`,
review the metadata/lock changes, and rerun both target-platform wheel checks.
`uv lock --project apps/sdk-agent-worker --python 3.12 --check --offline --no-cache`
is the no-network metadata-drift check. With an isolated external environment path,
`uv sync --project apps/sdk-agent-worker --python 3.12 --frozen --all-groups --no-install-project --no-build --python-platform TARGET --dry-run --offline`
checks wheel availability; replace `TARGET` with `x86_64-manylinux_2_36` and
`aarch64-manylinux_2_36` for the Bookworm base. A dry run is not an installation or
an executable-binary acceptance test. Never refresh this lock implicitly during a
release build; install errors or missing wheels must fail closed.

Before any build/up/stop, run the repository ownership and runtime preflight from
`docs/harness/runtime-and-secrets.md`, including `assert_compose_owner` from
`scripts/new/common.sh`. Additionally check the Compose labels of every existing
`sdk-agent-worker` container: `com.docker.compose.project.working_dir` must equal
this checkout and its project must match the selected deployment. The shared helper
currently does not enumerate this optional service. Abort on mismatch. Inspect
existing Docker/Compose status and health before selecting an operation. No new
startup script is supplied that might bypass these requirements. Use serialized,
resource-guarded candidate builds only after explicit authorization.

## Offline official-result regression

The optional worker owns `apps/sdk-agent-worker/tests/test_official_results.py`.
Run it explicitly with Python 3.12 and the worker's frozen dependencies already
installed (from the repository root):

```bash
uv run --project apps/sdk-agent-worker --frozen --no-sync python -m unittest discover -s apps/sdk-agent-worker/tests -p test_official_results.py -v
```

Use `UV_PROJECT_ENVIRONMENT` when the prepared worker environment is external.
This command does not install dependencies; missing SDKs fail collection rather
than skip. The Gateway workspace's default `tests/` collection does not include
this worker-only suite. Expect five passing tests and zero skips. They cover JSON
text preservation, OpenAI result error propagation, Claude empty success, and
Claude error terminals refusing completion/resume-map creation. Official result
objects/parsers and worker conversion execute; Runner and ClaudeSDKClient are
result-boundary doubles. This is neither actual SDK-loop acceptance nor native
Gateway persistence/provider evidence. Public output configuration remains text-only.

## Private durable state and health

The named `sdk-agent-state` volume mounts at `/var/lib/sdk-agent`. Its image-initialized
root is owned by 10001:10001 with mode 0700. Startup and health fail closed for a
missing, symlinked, wrong-owner, non-private or non-writable root. Existing volumes
with different ownership need an operator-reviewed migration; the entrypoint never
recursively changes permissions. Umask 0077 protects new SQLite/transcript/receipt
files. Storage encryption, backup access control and volume quotas are operator
obligations; Docker named volumes do not provide these automatically.

Exactly one worker process owns this volume. No autoscaling/multi-writer claims.
The private health probe authenticates against `/health/ready`, bypasses proxies,
prints no token, and checks package versions/state availability without a paid API
call. CLI binary execution, model entitlement, provider availability and actual
multi-turn continuation require separate acceptance tests. Access logs are disabled;
never add request/header-body logging to this endpoint or a TLS proxy.

## Execution deadline and failure ownership

The per-run execution deadline starts before child creation and includes launch
payload write/drain, event reads, child exit and the final authority recheck. The
budget is 300 seconds for pure/Knowledge runs and 1800 seconds for governed runs;
stdin backpressure must not bypass it. Expiry emits the existing execution-failed
response, retains the unknown `pending.json` marker and rejects replay until the
session is reconciled or explicitly deleted. No automatic provider retry is safe.

A lifespan-owned supervisor registers the session lock before child creation and
keeps strong references to execution, spawn, watchers and cleanup independently of
the ASGI response. Final registration rechecks shutdown/readiness after authority
admission awaits. An unstarted response may release its lock only because no spawn
was attempted. Active responses use a bounded sixteen-event queue; disconnect or
completion detaches the consumer without stranding a blocked producer.

Cleanup has a five-second monotonic **observation** budget, including the existing
three-second TERM grace before KILL. One owner sends the signals before joining
cancelled watchers. A request may report execution failure when observation expires,
while the unresolved owner, session lock and execution capacity remain retained.
Readiness becomes false and new admissions fail closed; cleanup cannot silently
return capacity. If all known local work later finishes, the owner can release its
lock while retaining an unknown execution's pending marker. Signal errors, cancelled
owner tasks and ambiguous launch failures stay fenced for operator reconciliation.
An unresolved spawn is not cancelled to manufacture certainty: its eventual handle
is observed and stopped. A FileNotFoundError naming the fixed interpreter is the
narrow no-SDK-execution exception. Other unknown launch outcomes remain refused.

Before spawn, `.cleanup/<scope>.json` is persisted and fsynced outside the deletable
session directory. It precedes receipt replay, deletion and retention, including
following process exit/restart. A successful local cleanup protocol writes the
private `.cleanup-complete` proof and then removes/fsyncs the active refusal marker.
The active marker always overrides the proof. Legacy pending runs without valid
cleanup proof cannot be deleted or expired automatically. No new reconciliation or
marker-clearing endpoint is provided. The offline operator procedure below requires
verified old-worker drain/containment. Do not clear these
markers just to retry a provider execution.

Success is committed only after the known leader wait, required cleanup protocol
and final authority recheck. A terminal write/unlink/fsync failure retains refusal,
even if a result file is visible. Once receipt and pending-removal fsync have
successfully completed, a later cleanup-metadata/release failure preserves that
known completed result while keeping the worker unhealthy and its live lock held.
This avoids reporting a failed execution whose committed receipt can safely replay.
No late cleanup may promote an already failed response into success.

Lifespan closes admission and observes owners, their independently retained tasks
and the retention coroutine together under one five-second shutdown budget. Expiry
reports `sdk_cleanup_unresolved`; it does not claim a clean service shutdown or
cancel/drop the remaining owners. The current Uvicorn ten-second graceful period
and optional container fifteen-second stop grace are unchanged. They provide no
verified scheduling/fsync margin. Blocking filesystem I/O, a stuck executor thread,
a stalled event loop or forced process exit can exceed or bypass asyncio observation;
this is not a bounded interpreter-exit or total HTTP wall-clock promise. Process
exit necessarily drops OS locks, while durable refusal remains across restart.

The local cleanup protocol observes only known tasks/handles and signal outcomes.
TERM/KILL delivery and leader exit do not prove that every descendant or an accepted
remote provider request terminated. Real process-tree/container containment remains
a separate rollout gate. No listener, process observer or provider was used to
establish the source-level ownership contract.

Run admission checks the incoming ASGI chunk against remaining body capacity before
copying it into its 393216-byte staging buffer. This caps logical staged body length,
not the ASGI server's already-received chunk, Python allocator overhead or total RSS.

`tests/unit/test_sdk_worker_lifecycle.py` executes the real ASGI routes, filesystem
locks and receipts with subprocess/signal and Gateway-response doubles. It covers
startup/drain/output/exit timeouts, cancellation/disconnect cleanup, unknown-pending
fences, request/tenant isolation, authority revocation and bounded body staging.
Run it and the shutdown suite directly with the optional Worker's frozen Python
environment: `<worker-venv>/bin/python -I -B tests/unit/test_sdk_worker_lifecycle.py -v`
and `<worker-venv>/bin/python -I -B tests/unit/test_sdk_worker_shutdown.py -v`.
The latter covers repeated raw cancellation, actual Uvicorn shutdown without a
listener, delayed spawn outcomes, durable restart refusal, terminal fsync failures,
resistant watchers, combined lifespan drain and an actual blocked retention thread.
It uses neither a listener nor provider credentials. Shortened test deadlines and
doubled process boundaries are unit evidence, not real-process, provider, Gateway
or deployment acceptance. The existing engine SDK tests remain a separate gate.

## Operator reconciliation of cleanup refusal

This is an **offline, explicitly approved operator procedure**, not an automatic
startup repair or a user-facing recovery endpoint. A failed health check alone
never authorizes marker removal. Do not restart/retry an SDK execution as a probe.
No state is deleted or reconciled by the source tests described above.

1. **Freeze admission and establish authority.** Disable new SDK dispatch at the
   Gateway using the deployment's approved change process. Identify the exact
   tenant/user/service/framework/session tuple from the Gateway-owned scope index
   and deletion outbox, and verify its digest using the same canonical scope
   function. A marker's hash alone cannot identify its owner. Record the authorized
   operator, affected scope, intended transition and approval. Ordinary cleanup
   review does not authorize permanent session retirement or content deletion.
2. **Prove containment has stopped.** Stop the old worker through its already
   approved supervisor/container control. Obtain authoritative evidence that the
   entire old worker containment, including its child processes, has terminated
   and that no other worker owns or mounts this state volume for execution. Record
   the containment identity, image/source identity, stop outcome and observation
   time. A sent KILL, exited Python leader, absent/reused PID, false readiness or
   elapsed grace is insufficient. For a bare-process deployment without a trusted
   way to establish descendant containment, stop here and leave refusal intact.
   Remote provider outcomes remain independent and may still be unknown.
3. **Preserve and inspect, without changing state.** While the worker remains
   stopped, retain an access-controlled backup/snapshot according to the existing
   storage policy. Inventory the exact scoped active marker, `.cleanup-complete`,
   pending marker, request receipts and permanent tombstone, with file hashes and
   the relevant failure evidence. Keep transcripts/results private; do not include
   provider keys or tokens in a ticket. Reject malformed/symlinked files or an
   untrusted/foreign volume; permission repairs require their own approved change.
4. **Classify the durable state before approving any mutation.** The active marker
   takes precedence over every other file. Use these rules:
   - An existing permanent tombstone always stays permanent. Do not resurrect the
     session or make its receipts available again.
   - A valid pending record means execution remains unknown, even when a receipt
     also exists. Preserve the original request identity/fingerprint and every
     receipt. No replay or new SDK turn is permitted for this session.
   - With no pending record, a valid current-format cleanup proof and a matching,
     intact committed receipt can support a known completed result. Verify that
     the proof was produced by this implementation and the receipt's request hash,
     fingerprint and result agree with trusted request/incident evidence. Do not
     infer success merely because a file named `run-*.json` exists.
   - A receipt without pending or valid cleanup proof is an ambiguous terminal
     write, not a confirmed commit. Keep it quarantined. If an approved backup has
     the exact original pending record, an explicitly reviewed offline repair may
     restore that record unchanged so the run remains unknown and non-replayable.
     Never invent a request ID/fingerprint or manufacture a completed receipt.
   - Missing/inconsistent identities, malformed records, an absent session directory
     with an active marker, or insufficient containment evidence require continued
     refusal. A new session ID does not by itself reconcile the old state.
5. **Apply only the approved, scope-specific transition while offline.** Before
   changing either proof or refusal, evaluate the scope's activity time against the
   configured retention policy and inspect existing tombstones/deletion-outbox
   intent. Restoring cleanup eligibility can permanently expire or delete content
   as soon as the worker restarts, including for a legacy pending scope. Approval
   for this exact scope must explicitly cover that retention/deletion consequence;
   cleanup-only approval is insufficient. If it is not authorized, leave refusal
   intact. Do not touch activity timestamps, change TTL or remove deletion intent
   to avoid this decision. Hold exclusive volume ownership plus the same admission/
   session locks used by the worker. The reviewed maintenance change must use the
   existing durable protocol:
   write/fsync the local-cleanup proof, then remove/fsync the active cleanup marker
   (the `complete_cleanup` ordering). Never just unlink the marker. This transition
   is allowed only after step 2 and when either the preserved pending record still
   fences unknown execution, a verified committed result satisfies step 4, or an
   already-authorized permanent tombstone prevents all reuse. Preserve pending,
   receipt and tombstone contents exactly; do not convert unknown execution into
   success or run the SDK again. The same containment evidence permits an approved
   legacy-pending scope to receive local-cleanup proof while keeping its pending
   record, subject to the retention/deletion authorization above; an existing
   approved deletion request or expiry can then be delivered immediately.
6. **If uncertainty cannot be repaired, do not clear it for availability.** Keep
   the worker/scope quarantined. The alternative is a separately approved permanent
   retirement of that exact owned session. Retirement must first persist/fsync the
   existing permanent scoped tombstone before any cleanup-marker transition. It
   permanently denies reuse and can allow existing deletion/retention processing
   to erase local content, so obtain explicit deletion authorization first. Do not
   move state to another worker/volume, remove tombstones or delete content as an
   implicit part of this procedure. Missing/malformed state that cannot follow the
   existing durable protocol remains blocked for a separately reviewed repair.
7. **Read back and restart conservatively.** Verify the approved files and directory
   fsync outcomes, retain before/after hashes and stop evidence, and release locks
   only when maintenance is complete. Start only a compatible worker that enforces
   this refusal format. Keep Gateway SDK admission disabled until private readiness
   and the read-only state inspection agree. Unknown pending scopes must still
   refuse runs; retired scopes must remain permanently denied. Do not test those
   properties by making a paid execution. Re-enable admission only under the
   deployment's approved change and monitor any already-authorized deletion outbox.

Any write, fsync, identity or validation error stops this procedure with admission
still disabled. Record the uncertainty instead of repeating marker removal blindly.
There is deliberately no unauthenticated or automatic implementation of these
maintenance transitions. Rolling back to an older worker that ignores cleanup
markers is unsafe while any refusal remains; keep SDK admission disabled until an
operator has reconciled every affected scope or a compatible worker is restored.

## Finite retention and scoped deletion

`SDK_AGENT_RETENTION_SECONDS` is required, from 3600 through 7776000 seconds
(one hour through 90 days); there is deliberately no infinite/default retention.
The server starts a periodic sweeper, scanning at most 100 root entries every
30 seconds and resuming its directory scan on the next tick. Large stores take
multiple ticks, so this is a bounded-work eventual sweep, not a hard wall-clock
purge SLA. Monitor sweep failures (`sdk_retention_sweep_failed`), disk growth and
backlog; enforce a deployment storage quota and size the policy accordingly.

Committed/admitted run activity determines expiry. Failed/pending/replayed requests
do not extend TTL. The worker holds a global admission/removal gate while acquiring
a per-session lock; deletion takes the same locks. Active sessions are skipped by
the sweep and explicit deletion returns 409 until the child exits and releases its
lock. There is no race in which a new run recreates a directory during removal.

The operator-private, bearer-authenticated `DELETE /v1/sessions` accepts exactly
`tenant_id`, `user_id`, `service_id`, `framework`, `session_id` in its JSON body.
These are canonical Gateway-owned identities, never an SDK resume ID/path. Identity
order/digest matches `Launch.scope`. Success is idempotent; invalid bodies return400,
active/gated scopes 409 and incomplete filesystem deletion 503. As with run launch,
only a verified Gateway caller may possess the operator token. Do not expose this
route directly to users or let a user's request choose another tenant's identity.

Deletion/expiry first fsyncs a small hashed tombstone, then removes the entire private
session directory including SQLite WAL/SHM, Claude transcripts/resume maps, pending
markers and replay receipts. Tombstoned scopes return410 on admission permanently;
after expiry the product must create a fresh platform session. Unknown runs with observed local cleanup receive
`deleted_unknown` or `expired_unknown`; active/uncertain cleanup and legacy pending
runs without valid proof remain refused. The unknown tombstone records uncertainty without content and
prevents unsafe replay. Deletion does not imply provider execution was cancelled.
If removal fails after tombstone, retries finish removal without reopening admission.
Tombstones contain only reason/time under a one-way scoped digest; they remain until
a separately reviewed identity-decommission procedure, not the conversation TTL.

## Remaining deletion authority and rollout gates

The worker lifecycle and Gateway-owned public session deletion index/outbox are
implemented together. Gateway deletion atomically deletes the owned canonical
session (retaining a tombstone only for existing Runtime FK references), blocks SDK admission and records scoped jobs. A supervised retry loop
calls the private deletion route; only explicit acknowledgement completes a job.
Public success means logical deletion plus durable intent, not immediate physical
erasure while the worker is busy/offline. See [durable cleanup](../architecture/sdk-session-cleanup.md)
for failure ordering, bounded operator diagnostics and epoch-14 verification.

Service/user/tenant cascade enumeration is not implemented by this unit. Existing
scope evidence survives service adapter changes or deletion, so subsequent owned
public session deletion can still find its worker state. General account/cascade
retention is a separate rollout requirement; hashes in worker storage cannot
reconstruct tenant/session identities. End-to-end public deletion and restart retry
still require database/runtime acceptance evidence.

Backup/snapshot encryption, access restrictions and finite expiry are operator duties;
local deletion cannot erase a provider's retained data or independent backups. Tests
must prove public-delete propagation, offline-worker retry, cascade coverage and backup
policy in addition to the source lifecycle unit tests. No production session, volume,
provider credential or external service was accessed or deleted during staging.

## Governed capability rollout

The [paired SDK authority](../architecture/sdk-capability-authority.md) requires
canonical epoch 15 after epoch 14, native admission, the matching Runtime endpoint,
and matching production capability-worker SDK validation. Never enable selected
capability_ids against only one side of that change. Existing pure/Knowledge-only
profiles do not require the new authority. No epoch15 verification fingerprint is
fabricated: isolated canonical measurement and role checks remain mandatory.

Nonempty capability_ids requires streaming; ingress must permit a 30-minute run
and idle periods while an approval waits. The worker uses a 30-minute hard deadline
for governed runs, 10-minute original approvals and finite owner leases. It checks
Gateway authority while the child runs and stops its local process on revocation.
External calls already accepted may still finish. Preserve the original journals
and reconcile unknown writes; do not delete a pending marker just to retry.
