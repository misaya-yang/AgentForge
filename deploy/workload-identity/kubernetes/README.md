# SPIFFE / mesh contract

Use a trust domain owned by the cluster, with distinct Kubernetes ServiceAccounts
for Gateway, Knowledge, Runtime and capability Worker. Example admitted identities
are `spiffe://agentforge.local/ns/agentforge/sa/gateway` and corresponding service
names. Registration binds namespace + ServiceAccount selectors, never pod labels
provided by a caller. Configure exact inbound identity allowlists and exact
outbound HTTPS-origin -> SPIFFE mappings; no wildcard trust-domain policy.

The native client accepts standards-compliant URI-only X.509 SVIDs: WebPKI still
checks the complete trust chain, certificate validity, serverAuth EKU, handshake
signature and possession; the expected SPIFFE URI replaces DNS as the authenticated
service name. The optional stricter DNS profile adds DNS SAN verification. Do not
substitute `danger_accept_invalid_certs`, forwarded identity headers, a public CA
store, or a mesh `PERMISSIVE` policy for either profile.

A trusted SPIRE Workload API consumer or equivalent admitted issuer exports
`cert.pem`, `key.pem`, `ca.pem` in a dedicated projected source volume. The
standard SPIFFE CSI driver exposes the Workload API socket; it does not itself
materialize these PEM files. That exporter remains an issuer integration, not a
claim made by this file adapter. Set source key mode
0600. A trusted rotator invokes the native adapter on each update:

```sh
workload-credentials import-svid --source-root /run/spiffe/projected --out /run/agentforge/workload --generation generation-002 --spiffe-id spiffe://agentforge.local/ns/agentforge/sa/gateway
```

`import-svid` pins a Kubernetes `..data` projection once and resolves source
symlinks only within that root, checks file permissions, copies into a new private generation, checks the leaf
URI and certificate/key/chain, and atomically activates it. The rotator must
publish a coherent source generation: replace the projected directory atomically
or use an issuer snapshot. A multi-file update without that guarantee is rejected
if mismatched, but cannot provide a consistent trust-bundle snapshot by itself.
Native application mounts are read-only; the rotator owns a separate writable
emptyDir and shares only the prepared credential root. No pod receives an issuer
CA private key. Non-root application UIDs must own their 0600 keys.

For a service mesh, use end-to-end TLS passthrough to the native listener, or use
an explicitly authenticated local UDS transport adapter in a separately reviewed
integration. This module has no header-based proxy identity mode. Istio STRICT
PeerAuthentication/AuthorizationPolicy and NetworkPolicy should independently
restrict the service's internal TLS port to admitted workloads. Do not expose
that port through a public Ingress/LoadBalancer, mount credentials in frontend or
provider-fetch containers, or allow cleartext bypass to internal handlers.

Use overlapping trust bundles and short SVID lifetimes. Rotate all replicas,
observe peer identities and drain old connections before removing old roots.
`required` must reject the old bearer even when it remains valid; remove its
secret reference/mount after the measured transition. NetworkPolicy and mesh
configuration are defense in depth and do not prove that native required-mode
admission is mounted on every internal route.
