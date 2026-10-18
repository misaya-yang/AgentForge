# Compose provisioning and mount contract

These are integration examples for the existing owned Compose project. Do not
create a new project, publish an internal port, or run `up` from this directory.
Root must wire the callable modules before applying the overlay fragment.

With the native `workload-credentials` binary available and an external private
location selected by the operator:

```sh
workload-credentials init-ca --out /srv/agentforge-issuer
workload-credentials issue --ca /srv/agentforge-issuer --out /srv/agentforge-workloads/gateway --generation generation-001 --spiffe-id spiffe://agentforge.local/ns/platform/sa/gateway --dns gateway-workload
workload-credentials activate --out /srv/agentforge-workloads/gateway --generation generation-001
```

Issue distinct roots for `knowledge`, `runtime` and `capability-worker` with their
own exact `sa` name and DNS alias. Here "roots" means credential directories;
the CA may be shared by the local deployment. Mount each service's directory
only in that service and put the alias on its private app network. Align private
key ownership with the service UID (the existing images use a non-root UID).
Do not chmod keys group/world readable to make a mount work. Never mount the CA
private directory into any container or put keys in Compose interpolation.

The example deliberately starts the server migration window at `dual`; the
legacy shared token is separately mounted using `AI_PLATFORM_INTERNAL_TOKEN_FILE`.
It is not retired. Callers may select required outbound mTLS while servers remain
dual. The Runtime owner must retain its controlled-source workflow and map the
existing overlay's CA/combined-identity paths; native endpoints use the separate
credential-generation API. Native rotations are implemented here; Runtime
rotation behavior remains owned by the Runtime source release.

For root rotation, publish a bundle containing old and new CA certificates to
all peers first. `issue --trust-bundle /absolute/overlap-ca.pem` copies an explicit
bundle into the next generation. Issue/activate new leaves, measure every peer,
then activate bundles without the old CA and drain old connections. Issuing does
not activate automatically. Never overwrite an existing generation or retain
private keys in source, reports, image layers, environment values or CI artifacts.
