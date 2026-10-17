# Enterprise identity source contract

Enterprise identity is explicitly enabled after the matching canonical schema is
activated. `database/proposals/enterprise-identity.sql` is an unnumbered, inactive
proposal; source compilation does not activate it. Existing password, API key and
CLI behavior remains the default when enterprise policy is disabled. Enabling
policy against absent tables fails closed.

The host constructs `EnterpriseService` with the existing AuthService, PostgreSQL
pool, Redis connection manager, approved HTTPS BFF origins, RP ID/origin, and
strictly configured OIDC providers, then mounts `enterprise::http::router` inside
the existing BFF protection. Enable `AuthService::with_enterprise_policy_enabled`
and the credential introspection decoder's matching flag together. Browser
session authentication calls the same `authenticate_claims` authority.

## Protocol boundaries

OIDC providers are fixed to a tenant and exact issuer/client/callback/return URL.
Discovery and code exchange use HTTPS, explicit host allowlisting, no redirects,
no proxy, no transparent retries, bounded response size and timeouts. The callback
is exactly GET `/api/auth/enterprise/oidc/callback`; its cross-site exception must
not cover other methods or routes. It consumes one-use state bound to an independent
Secure/HttpOnly/SameSite=Lax `__Host-agentforge_oidc_flow` cookie, with nonce,
PKCE S256, verified issuer/audience/signature, and recent authentication. SAML
terminates at an external OIDC broker; this module does not parse SAML assertions.
Bindings use issuer/subject within configured provider/tenant. Verified-domain
JIT creates a new ordinary existing account; matching email never links or elevates
an existing account. MFA-required tenants accept only configured verified MFA ACRs.

Passkey registration and authentication use webauthn-rs with exact HTTPS RP/origin
and required user verification. Server-side challenge state expires after five
minutes and is consumed once. Authentication compares credential revisions and
counters under row locks, including revision CAS for zero-counter authenticators.
Enrollment rotates the independent account authentication epoch and returns ten
single-use recovery codes once. Recovery requires the current password plus one
account-bound hashed code and is rate limited. Recovery sessions can enroll a
replacement passkey after another password check; they cannot satisfy privileged
recent-MFA operations. Removal preserves at least one passkey.

All resulting browser grants reuse the existing opaque Redis sessions with three
hour idle/absolute bounds and credential revision. No browser bearer is returned.
`users.auth_epoch` is independent of password revision; security/lifecycle changes
invalidate existing sessions through current authority checks. Password login and
BFF password login share the central MFA gate. API keys retain their existing
noninteractive authority semantics and do not become interactive MFA proofs.

## Routes

`enterprise_contract::extend(&mut serde_json::Value)` is the pure OpenAPI producer.
OIDC begin is POST `/api/auth/enterprise/oidc/{provider}/begin`. Passkey operations
are POST `/api/auth/enterprise/passkeys/{register,authenticate}/{start,finish}`,
and DELETE `/api/auth/enterprise/passkeys/{id}`. Recovery is POST
`/api/auth/enterprise/recovery`, with authenticated rotation at `/recovery/rotate`.
Identity linking and SCIM credential issue/revoke require current privileged
authority and recent passkey or IdP MFA. Authenticated browser mutations require
the existing synchronizer CSRF protection; public proof routes enforce approved
Origin/Fetch Metadata themselves.

SCIM uses `/scim/v2/{tenant}/Users` and `/Groups`, resource CRUD/PATCH, and discovery
resources. It accepts dedicated expiring tenant/scoped credentials, not API keys
or browser cookies. Users are existing accounts with managed linkage, not a second
user table. Deletion deactivates and advances authentication epoch. Only explicit
operator-owned nonadministrator role mappings project groups into existing RBAC;
manual grants are preserved. Version CAS protects concurrent writes. Supported
filters are exact `userName eq "..."`; unsupported filters and patch paths are
rejected. User profile support is userName, externalId, displayName, active and
emails; externalId is required by this managed provisioning profile. Bulk, sorting,
SCIM password changes and arbitrary enterprise extension attributes are not
advertised. Actual Entra/Okta/Keycloak conformance remains unverified.

## Dependencies and verification boundary

Primary maintains Cargo: openidconnect 4.0.1 with default features disabled;
webauthn-rs 0.5.5 with `danger-allow-state-serialisation` and
`danger-credential-internals`; existing reqwest 0.13 provides a strict custom HTTP
adapter. Credential internals are read only to retain the library-verified initial
counter and UV state, never patched. Password work clones the original bounded
PasswordEngine semaphore.

Source references: the installed official openidconnect 4.0.1 and webauthn-rs 0.5.5
crate documentation/source; SCIM RFC 7643 and RFC 7644. No real IdP registration,
DB activation, service interaction, Cargo test or Docker execution was performed
by this implementation worker. Primary owns compilation, canonical migration,
interface fixtures and later real-provider acceptance.
