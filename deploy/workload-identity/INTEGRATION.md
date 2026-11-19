# Root integration handoff — U-PE12

Source modules are complete and exported; this document lists the remaining
process/deployment wiring owned by the root integration. No stack was started.
No Runtime overlay/upstream harness source or shared manifest/lock was modified.

## Dependencies and build

Production/test code reuses workspace rustls 0.23.46, reqwest 0.13.5, tokio,
axum, sha2 and uuid. Root must add **`tokio-rustls.workspace = true` to
platform-service-primitives dependencies** for the shared Gateway/Knowledge
listener; the pinned dependency already exists in this workspace. No other
manifest addition or dependency download is needed. Root owns lock reconciliation. The new
`platform-service-primitives/src/bin/workload-credentials.rs` is an auto-discovered
native binary. Package it explicitly if deployment images need the issuer/import
adapter; do not add OpenSSL or issuer CA keys to application images merely to
serve TLS. Unit certificate fixtures require the existing test OpenSSL tool.

Root's serialized checks:

```
# Through scripts/new/native-container.sh, using its shared cache:
cargo test --locked -p ai-platform-service-primitives --lib workload_identity
cargo test --locked -p ai-platform-auth --lib workload
cargo test --locked -p ai-platform-gateway --lib workload_tls
cargo check --locked -p ai-platform-service-primitives --bin workload-credentials
```

This list is a handoff, not executed evidence. Tests perform real rustls TLS
handshakes in memory with ephemeral OpenSSL CA/leaf keys; no service stack is
started. Certificate tests exercise chain, URI, optional DNS, EKU, expiry,
anonymous/dual behavior, unrelated CA, key mismatch, rotation and private-file
rejection. They also check standards-compliant URI-only SVIDs.

## Gateway process/listener mount

1. Read `_FILE` secrets at the configuration owner with
   `workload_identity::secrets::environment_secret`. Reject inline conflicts.
   Construct `WorkloadAuth::from_environment(env, exact_peer_rules)` once for the
   selected transition mode. Required rejects any retained shared-token setting.
2. `Configuration::resolve_from_environment(env).await` resolves an explicit
   service alias to one private bind address; `Configuration::new` also accepts
   an already-resolved private address. Never bind 0.0.0.0 or a public address.
3. Build an explicit `Routes` inventory from actual internal business routes,
   including templates if needed. The facade accepts only `/internal/` routes,
   canonical paths and listed methods; public APIs/health/UI are excluded.
4. Preserve the assembled router's business handlers and layers. Mount:

   ```rust,ignore
   let facade = workload_tls::restrict(assembled_internal_router, gate, routes);
   let listener = workload_tls::WorkloadListener::bind(configuration.address, configuration.credentials).await?;
   axum::serve(listener,
       facade.into_make_service_with_connect_info::<workload_tls::WorkloadConnection>())
       .with_graceful_shutdown(shutdown).await?;
   ```

   Missing typed connect-info fails closed in required mode. The new connection
   layer removes a pre-existing peer extension, installs the TLS-derived peer,
   and then the admission gate installs `WorkloadAuthentication`.
5. Replace **only the old shared transport-bearer predicate** in each scoped
   internal handler with `WorkloadAuthentication::require(request.extensions())`.
   Its proof is emitted exclusively by the gate, never a header. Keep the
   existing actor-signature, tenant, capability, lease, nonce and domain checks
   exactly where they run. Do not synthesize a shared Authorization header to
   satisfy an old guard. Do not infer tenant/user from a workload's service name.
6. Mount the same gate on any remaining listener's copies of these internal
   routes. In required mode a cleartext request has no TLS peer and is denied
   even with the old valid bearer. Merely adding the companion TLS listener
   while leaving original bearer guards exposed does not retire the bearer.

Known existing seam examples: `migration_host/catalog/http.rs` transport check
for `/internal/v2/agent-capabilities/catalog`; capability/knowledge bridge
handlers and model-plane callbacks also have their own transport token guards.
Use the owning route inventory, rather than assuming these examples are complete.

## Knowledge and outbound clients

The listener is shared as
`workload_identity::listener::{WorkloadListener,WorkloadConnection,install_peer}`;
Gateway re-exports its listener/connect-info types. Knowledge can directly mount
that shared listener and `install_peer` before the same auth gate, without a
Gateway dependency or a duplicate TLS implementation. The identity/auth primitives
are service-neutral. Knowledge process/bootstrap remains root-owned and was not
edited by this implementation.

Replace only the internal transport client with `WorkloadHttp`, supplying
`ClientCredentials`, exact HTTPS origin -> expected SPIFFE pairs and bounded
connect/total durations. Its `send` method retains signed business headers/body
but never exposes its credential-bearing reqwest Client. Keep public web fetch,
provider calls, user authorization and existing request signing on their current
separate clients. No certificate state belongs in a process-wide default client.

Native file layout is `current/{ca.pem,cert.pem,key.pem}`. Runtime's existing
outbound overlay uses `CA_FILE` and combined `IDENTITY_FILE`; the offline tool
also writes `identity.pem` for that compatibility seam. Runtime source/lock,
rotation and its required-mode outbound rollout remain execution-owner work.
Do not change its open-source harness to perform this integration.

## Deployment and acceptance boundary

Root owns base Compose/Helm aliases, read-only per-workload credential mounts,
private listener lifecycle, Kubernetes SVID exporter/rotator and complete mesh
policy. The examples in this directory are not auto-applied or production-ready
chart overrides. Keep the existing project/volumes, no public internal ports and
no CA-private mount. File secret migration precedes dual acceptance.

Required acceptance must include a real signed tenant operation over mTLS,
wrong identity/CA/EKU/expired-certificate rejection, forged-header denial,
old-bearer denial on **all** cleartext/TLS replicas, file rotation and connection
drain, plus unchanged public-provider transport. Until root executes and records
those checks, this is implemented source, not measured rollout/retirement.

## Connected reference deployment source

The subsequent deployment package now connects this module through the complete
Compose/Helm paths in [DEPLOYMENT.md](DEPLOYMENT.md), including the real native
projected-SVID importer/rotation sidecar. Its offline render receipts are separate
from this source handoff; no paired-service runtime/retirement acceptance is
inferred. Root still owns serialized native builds and final protocol integration.
