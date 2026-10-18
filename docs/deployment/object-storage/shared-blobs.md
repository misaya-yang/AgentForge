# Shared blobs and verified local-to-S3 migration

`AI_PLATFORM_DEPLOYMENT_MODE=multi-instance` selects S3 for an omitted or `auto`
storage backend. Explicit `local`, `oss`, unknown backends and incomplete S3
configuration fail closed. `single-node` (also `single-instance`) keeps local
development behavior; `auto` selects local there. Both Gateway and Knowledge use
this policy. This does not lift the release's separate singleton/multi-replica gates.

All replicas serving a given store must use the same bucket and `key_prefix`.
Gateway and Knowledge may use separate buckets. Gateway file list/info calls use
the shared S3 adapter's paginated listing and HEAD metadata, with exact tenant/user
prefixes, instead of creating or enumerating replica-local directories. The API
still returns the newest 50 files; each request has an explicit 100,000-key scan
ceiling and rejects inconsistent/repeating provider pages. Upload timestamps are
stored in metadata, with the encoded filename timestamp as a legacy fallback. Never point roots with conflicting
keys at one destination namespace. Existing tenant and user components remain in
object keys; migration can only add the configured deployment prefix, and cannot
strip prefixes, rewrite tenant/user path components, merge tenants, update
PostgreSQL rows, change a runtime backend, or delete local/destination objects.

Gateway uses `GATEWAY_STORAGE__BACKEND`, `__KEY_PREFIX`, `__S3__BUCKET`,
`__S3__REGION`, `__S3__ENDPOINT_URL`, `__S3__ACCESS_KEY`, `__S3__SECRET_KEY`.
Knowledge uses the corresponding `KNOWLEDGE_STORAGE__...` settings. Credentials
remain runtime secrets. Require TLS outside isolated development and enforce
bucket/prefix-scoped IAM, including denial of cross-tenant access where applicable.

## Encryption, lifecycle and URLs

Prefer provider-managed bucket default encryption and versioning; this also covers
Knowledge and producers other than Gateway. Gateway additionally accepts
`GATEWAY_STORAGE__S3__SERVER_SIDE_ENCRYPTION=AES256` or `aws:kms`, with optional
`GATEWAY_STORAGE__S3__KMS_KEY_ID`. An empty mode sends no encryption headers and
uses the provider policy, preserving compatibility with S3 implementations that do
not expose AWS KMS. Unsupported combinations fail closed. Presigned PUT responses
return the required signed encryption headers; clients must send them unchanged.
S3 presigned URLs are capped at 900 seconds, with a minimum of one second.

Configure lifecycle using exact prefixes selected from the actual inventory,
independently for uploads, artifacts, Knowledge DocIR/source and snapshots. Start
with aborting incomplete multipart uploads; define noncurrent-version retention
only against the backup/retention agreement. Do not enable expiration of current
versions as a replacement for tenant-aware platform cleanup. Encryption, versioning
and lifecycle APIs vary by provider: apply through the provider's administration
interface, read back the resulting policy, and verify encrypted PUT/HEAD/GET,
conditional PUT (`If-None-Match: *`), metadata roundtrip, list/delete-prefix and
presigned GET/PUT against an isolated bucket before production. This tool never
changes bucket policies or lifecycle settings. Ceph RGW, SeaweedFS and Garage are
candidate providers; RustFS remains the local provider pending operation coverage.

Metadata values with Unicode, significant whitespace or the reserved
`af-b64-v1:` prefix are reversibly encoded by the platform S3 transport and decoded
on HEAD only with the transport's version marker. Legacy values without that marker
remain unchanged. `agentforge-internal-metadata-format` is reserved for the transport.
Ordinary ASCII values and existing object keys remain unchanged. Metadata
keys must be lowercase HTTP tokens; migration refuses invalid sidecars and cannot
silently discard or normalize tenant metadata.

## Migration procedure

Build using the shared container workflow (serially with other native builds):

```sh
scripts/new/native-container.sh build -p ai-platform-tools --bin blob-migrate
```

Run the resulting controlled operational binary where the source volume is mounted
read-only. Manifest, state and receipt files must be outside the source root, in a
private existing directory. The tool only reads the specified subtree. Choose the
physical root **above** the deployment prefix: for a stored key
`dev/uploads/tenant-a/user/file`, the scope is `dev/uploads/tenant-a`. For an
already-prefixed subtree use `--scope dev`; distinct tenant scopes can be migrated
separately. **Legacy Gateway file uploads are an exception:** their local fast path
wrote `uploads/...` without the deployment prefix and used MD5 in `content_hash`.
Migrate that physical subtree separately with `--scope uploads --key-prefix dev`
(or its actual configured prefix). `source_key` remains `uploads/...` while the
reviewed destination `key` becomes `dev/uploads/...`, matching the runtime S3
adapter. An already-prefixed source is never double-prefixed. No arbitrary path or
tenant remapping is available. Each subtree has its own manifest/state/receipt;
all relevant subtrees and local roots must pass before switching a service.
Review every mapping and scope before issuing `copy`.

```sh
blob-migrate plan --root /source/gateway-images --scope dev \
  --manifest /private/migration/gateway.json --dry-run
blob-migrate plan --root /source/gateway-images --scope dev \
  --manifest /private/migration/gateway.json
blob-migrate copy --root /source/gateway-images \
  --manifest /private/migration/gateway.json \
  --state /private/migration/gateway-state.json \
  --receipt /private/migration/gateway-receipt.json --dry-run
```

`plan --dry-run` performs no network request or output-file write. `plan` records
SHA-256, byte length, content type, exact key and sidecar metadata for every object.
`plan --key-prefix` is the only supported source/destination path mapping, bound
to the manifest and resume identity. Missing content type defaults to
`application/octet-stream`; malformed/orphan
sidecars, symlinks, special files, path traversal and mismatched recorded
`content_hash` fail. Both legacy MD5 (32 hex digits) and SHA-256 (64 digits)
sidecars are validated and preserved. Legacy file sidecars with file/user IDs also
record the original file modification time as `uploaded_at` when that field was
missing, preserving shared file-list ordering after copy. Destination integrity
always uses a separately
computed SHA-256, never MD5 or ETag. Objects are processed serially with a default 64 MiB memory
limit per object; `plan --max-object-bytes` can select up to 256 MiB. Oversized
objects fail explicitly and must be handled through a separately authorized
streaming migration; they are never skipped or marked verified.

Inject credentials through an approved runtime secret mechanism. Supported inputs:
`BLOB_MIGRATION_S3_BUCKET`, `BLOB_MIGRATION_S3_REGION` (default `us-east-1`),
`BLOB_MIGRATION_S3_ENDPOINT` (optional), `BLOB_MIGRATION_S3_ACCESS_KEY`,
`BLOB_MIGRATION_S3_SECRET_KEY`, `BLOB_MIGRATION_S3_SSE`, and
`BLOB_MIGRATION_S3_KMS_KEY_ID`. Do not put credentials on command lines or in
manifests. Session-token credentials are not supported by the existing transport.

After approval for the destination writes, repeat `copy` without `--dry-run`.
Copy creates missing objects with conditional PUT, never overwrites a destination
conflict, and verifies **GET SHA-256 plus HEAD byte length, content type and all
metadata**. ETag is never treated as a digest. Providers that refuse conditional
creation fail the migration; there is no unsafe unconditional-PUT fallback. Existing
objects must already match. Each verified object writes an atomic, fsynced checkpoint
bound to the manifest digest and destination identity. A restart repeats validation
of completed objects instead of trusting the checkpoint. A lock prevents concurrent
runs against the same state file. After a killed process, inspect the PID and ensure
no migration is active before explicitly removing its stale `*.migration-lock`.

For final verification, quiesce **all writers and deletion/cleanup workers** that
can touch the selected source and destination scope, retain the database snapshot
and compare the metadata owners' referenced storage keys against the reviewed
manifest. Local absolute paths/`file://` references require owner-specific,
authority-approved metadata conversion before cutover; this tool deliberately
cannot rewrite them. Preserve tenant/user IDs and the service's configured prefix.
Export a `agentforge-blob-metadata/v1` references document from those metadata
owners, using the same quiesced snapshot (see [the schema example](metadata-references.example.json)).
Each object must have an owner, record ID, tenant ID, exact physical key, byte length
and content type. An existing stored SHA-256 may also be supplied. The references
must cover every inventoried object; missing objects, orphan blobs, conflicting
tenants, changed size/MIME/hash, and keys outside the selected scope prevent the
final certificate. Tenant IDs must match complete path components. Legacy unscoped
keys need an owner-specific reviewed metadata/path transition rather than a false
tenant certificate. This is an export input, not permission for the tool to query
or modify the database. Then run:

```sh
blob-migrate verify --root /source/gateway-images \
  --manifest /private/migration/gateway.json \
  --state /private/migration/gateway-state.json \
  --receipt /private/migration/gateway-receipt.json \
  --metadata-references /private/migration/gateway-metadata.json --source-quiesced
```

`verify` never uploads. Both copy and verify recheck the full source inventory at
start/end and re-read every destination object twice. Only successful `verify`
with the operator's explicit quiescence assertion **and matching metadata references**
yields `cutover_ready=true`; its receipt binds the references SHA-256 as well as
the blob inventory and destination.
A normal copy always yields false. Failed non-dry runs invalidate the selected
receipt before source/remote verification and exit nonzero. A readiness receipt is
bound to its manifest/destination and the maintained quiescent window: it is not
permission to switch backend after writers resume. Keep that window until cutover.

A verified receipt is only the **blob-transfer gate**. Cutover still requires the
metadata-owner snapshot provenance, independent bucket encryption/lifecycle evidence,
rollback approval and the real two-Gateway-replica read/authorization test. Point
both replicas at the same verified bucket/prefix, read each representative tenant's
file/image/artifact from either replica, and verify cross-tenant denial. Repeat for
Knowledge sources/DocIR and snapshot restore owners before lifting release guards.

Preserve local volumes, metadata snapshot, reviewed manifest and receipts. If
acceptance fails before new writes, revert to the original local backend and its
matching metadata snapshot. Once writes reach S3, quiesce again and reconcile those
writes through an explicitly reviewed reverse-transfer procedure before rollback;
switching directly to the stale local copy would lose data. No cutover, database
mutation, live S3 operation or multi-replica acceptance is performed by source tests.
