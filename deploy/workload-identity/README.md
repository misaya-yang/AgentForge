# Native workload identity

The owning source modules are `platform-service-primitives::workload_identity`,
`platform-auth::workload`, and `platform-gateway::workload_tls`. They provide a
TLS 1.3 listener, private file secrets, dedicated internal HTTP transport, exact
SPIFFE admission and an explicit shared-bearer migration gate. Process/router,
Knowledge and deployment integration is owned by the root release change; these
modules and examples alone do not establish an active rollout or retirement.

The chain is verified by rustls/WebPKI, including validity, EKU, certificate and
handshake signatures. The peer must have exactly one URI SAN and that URI must
match the configured service identity. CN, DNS aliases and HTTP identity headers
cannot grant workload identity. The internal client authenticates the expected
SPIFFE URI, supporting standard URI-only SVIDs; `load_with_dns` additionally
requires DNS SAN matching when a deployment selects that profile. An exact HTTPS
origin-to-SPIFFE map controls every outbound request. The credential-bearing
reqwest client is private to this transport; public web/provider clients never
receive its config, keys or certificate. Proxies, redirects and retries are off.

This is workload admission only. Existing actor signatures, tenant binding,
capability/lease validation and domain authorization must run after admission.
No generated Authorization or tenant header bridges mTLS to a legacy guard.

## Files and rotation

Each workload mounts only its own credential root, read-only:

```
/run/agentforge/workload/
  generation-001/{ca.pem,cert.pem,key.pem,identity.pem}
  current -> generation-001
```

`key.pem` and the Runtime-compatible combined `identity.pem` are private mode
0600; roots/generations must not be group/other writable. Certificate/trust files
must be regular files, bounded and not group/other writable. Native readers
resolve `current` once per handshake/request, reject an escaping generation or
file symlink, check opened-file identity, and refuse mismatched keys/identities.
Secrets have no Debug/Serialize implementation or plaintext environment fallback.
`environment_secret(env, "NAME")` reads `NAME_FILE`, rejects a conflicting inline
`NAME`, and removes only one terminal newline. Config owners must use this for
sensitive values and keep returned bytes out of diagnostics.

The new `workload-credentials` native binary performs offline provisioning using
an already installed OpenSSL. It never starts a stack or downloads software.
It creates a private local CA, issues a short-lived leaf generation, validates
certificate/key/bundle before activation and switches `current` atomically. Its
`import-svid` adapter copies CSI/projected SVID files into an owned private
native generation and verifies the configured URI before activating. CA private
keys remain on the issuer host and are never mounted in an application container.
See [Compose](compose/README.md) and [Kubernetes](kubernetes/README.md).

Native listeners reload before each new handshake. Invalid rotations reject new
connections instead of retaining an old configuration. TLS session tickets and
resumption are disabled. Internal HTTP calls reload before each request and do
not retain idle pooled connections. Already admitted in-flight connections and
streams are not retroactively revoked: drain them within the service's bounded
request/stream lifetime during trust retirement. A distributed rollout needs an
overlap interval; a file switch on one instance is not a cluster-wide barrier.
Trust roots have no implicit public/system CA fallback. This profile uses short
certificate lifetimes and trust-bundle retirement, not an OCSP/CRL service.
Issuing a new leaf does not revoke an older still-valid leaf under an admitted CA;
its acceptance ends at expiry or removal of that issuing trust root.

## Explicit migration states

| Mode | No certificate | Valid admitted certificate | Invalid/unadmitted certificate | Shared token configuration |
| --- | --- | --- | --- | --- |
| disabled | Exact legacy bearer required by gate | Rejected by gate | Rejected | Private `_FILE` required |
| dual | Exact legacy bearer required | mTLS accepted | TLS failure; no bearer downgrade | Private `_FILE` required |
| required | Rejected | mTLS accepted | TLS failure | Nonempty inline/token `_FILE` forbidden |

First migrate file secrets and deploy separate internal TLS endpoints with
`dual`; migrate each caller to its own SVID and exact endpoint/peer mapping.
Measure that every caller uses mTLS and that the old cleartext/bearer listener is
blocked for these internal routes. Then set `required` on every instance, remove
the shared token setting/mount, and rotate/delete the old bearer through its
owning secret authority. Keep business-signing keys: retiring a shared transport
bearer does not retire signed actor/tenant/capability proofs. Until those steps
are actually executed and measured, the shared bearer is retained, not retired.

Rollback from dual may use the previous explicitly admitted generation and
still-valid file bearer. Rolling required back to dual is a security-policy
change needing an explicit release decision; malformed credentials never trigger
it automatically. An old CA is accepted only while present in an explicit bundle.
