# Atomic official-SDK service creation

This is a native-source contract, not evidence of deployed availability or real
PostgreSQL concurrency acceptance.

## Public boundary

Typed Web, Desktop and management CLI creation submits exactly one
`POST /api/v1/service-registrations`. The body is the existing official-SDK creation
definition, including optional selected Knowledge and governed capability IDs.
`native_contract::service_registration_schema` owns the request shape; the route
validates that same schema, and the independent native OpenAPI producer exports it.
Existing SDK semantic validation, credential sealing and launch admission remain
responsible for their established byte limits and authority checks.

Authentication, `ServiceConfigWrite` and platform-admin requirements are unchanged.
Success is HTTP 200 with `{ "service_id": "...", "status": "registered" }`.
A colliding ID returns HTTP 409 (`service_id_conflict`) without replacing its row,
key, metadata, cached service, adapter or proxy configuration. A storage adapter that
cannot provide atomic insertion returns HTTP 501
(`atomic_service_registration_unsupported`), never a get-then-save substitute.

The new top-level path avoids reserving an existing service ID. Old native and
retained Python profiles may return 404/405; clients clearly explain that compatible
native registration is required and never fall back to legacy registration.
`sdk/openapi-profiles.json` marks this path native-only; the Python-hybrid path set
is unchanged. Existing generic `POST /api/v1/services`, registry `save`, config
loaders and edit consumers retain their intentional upsert behavior.

## Persistence

`RegistryStorage::insert_if_absent` defaults to a fail-closed unsupported error.
Memory insertion checks and inserts under one mutex. PostgreSQL executes the same
27-field parameter binding as legacy save, using one `INSERT ... ON CONFLICT
(service_id) DO NOTHING`; zero affected rows means collision. Existing primary-key
uniqueness provides the database arbitration. No migration is needed. Registry and
proxy cache updates happen only after storage reports insertion success.

Generated IDs and any GET-404 preflight remain convenience checks; they are not the
atomicity mechanism. Creation never retries automatically after an uncertain
response. Successful persistence followed by proxy invalidation failure, cancellation,
transport interruption or other server failure may leave a service created: recover
using an exact-ID public read. Even a matching readback cannot prove which actor
created it or establish provider-key equality. Editing remains non-CAS and is outside
this change.

## Evidence limits

Offline tests exercise production API admission and registry paths with memory/mock
storage, coordinated competing registries, collision preservation, legacy upsert,
unsupported/failing stores, authorization, validation and post-insert failure.
A production SQL construction assertion checks the exact insert prefix/bind slots
and DO NOTHING suffix. These are not live HTTP, PostgreSQL lock/concurrency,
provider, cross-platform GUI or deployed-profile tests. Build receipts and source
identities live in the external verification packet; only actually completed checks
may be claimed.
