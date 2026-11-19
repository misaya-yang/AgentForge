# ADR-013: Tauri 2 candidate for the shared Console desktop shell

Status: proposed implementation target; **P-ADR-8 is not accepted and U-CL-01 GA is not complete**.
Owner: clients. Date: 2026-10-17.

Recommend Tauri 2 around the existing React Console instead of maintaining a second
product UI in egui. The source candidate is `apps/desktop`; the existing
`platform-desktop` egui binary, its capability matrix, packaging and CI remain intact.
No feature is retired by this ADR. Native dependencies have not been resolved or
compiled in this task; a reviewed dependency lock and current Tauri IPC ACL patch
level are prerequisites for an executable candidate.

## Structure and authority

The shell stages the **unaltered existing Web build**, including its manifest and
assets, alongside a small bundled native companion. `scripts/assets.mjs` copies
bytes, refuses links/overlap/reserved paths, and records per-file SHA-256. It never
patches Console auth, intercepts fetch, injects a bearer token, creates a generic
native HTTP proxy or introduces another Agent runtime.

Three distinct window labels enforce the trust boundary:

| Window | Content | Native capability | Authentication |
| --- | --- | --- | --- |
| `companion` | Bundled `native/index.html` only | Eleven bounded shell commands; explicit local-origin/path check in each handler | Device grant and secrets stay in Rust and the existing OS vault |
| `console` | Configured exact HTTPS Gateway origin, serving the same React Console product | None, including for iframes and same-origin API responses | Existing Gateway HttpOnly cookie BFF, CSRF and Console Trusted Types/CSP |
| `console-offline` | Packaged React build, asset origin | None | No Gateway session, API bridge, remote networking or queued mutations |

Serving the online Console at its Gateway origin preserves real cookie authority,
SSE, file downloads and session renewal. A bundled-origin reverse proxy would
require duplicating those contracts and could turn a Console XSS into native
credential authority. We choose origin separation. The offline build is a UI
preview and diagnostic fallback; it is not an offline data cache or an authenticated
workspace. Equality of the online release and staged build must be measured in the
release fixture; a common source repository alone does not establish byte equality.

The companion is part of the trusted computing base. It never loads remote HTML,
uses `innerHTML`, stores credentials in browser storage or exposes raw filesystem,
HTTP, shell, opener or vault primitives. The only capability is local and scoped to
`companion`, with no remote origin grants. Every command also checks exact window
label, asset origin and `/native/index.html`, denying remote content even if an IPC
ACL regression occurs. The production candidate uses no dev-server IPC origins.
CSP forbids frames, object embedding, form submissions and external connections;
Trusted Types permits only the Console's sanitizer policy. Native dialogs confirm
Gateway selection, login, pairing, revocation and installation.

## Native integration

Device login delegates PKCE, polling, origin binding, refresh uncertainty and
revocation to `platform-client::device_auth::DeviceLogin`. The explicit native
identity check refreshes through that same rotation fence and verifies the session
with a real authenticated `Client::me` request; its result contains identity/expiry
only. The registered ID is
currently `agentforge-cli`; sessions are therefore shared with platformctl for the
same exact Gateway origin. Revocation makes this explicit in a native dialog.
A separately registered desktop ID and vault namespace can replace it only after
the client/Gateway owner implements that public contract. Device codes, verifier,
access tokens and refresh tokens are never command results or events. The companion
receives only the human verification URI/code and session identity/expiry. The
Console uses its independent cookie session and never receives the native token.

Pairing reads an explicit, absolute startup `AGENTFORGE_LOCAL_NODE_CONFIG` path.
It uses the existing `OperatorConfig`, pinned workspace descriptors, permission
snapshot, pinned platform verifier, `DeviceIdentity`, hardware/software key policy,
OS credential store and `HttpsTransport::redeem_pairing`. The configured node
endpoint must share the selected Gateway origin. It neither issues new grants nor
starts a listener/daemon. A stored device credential is reported separately from
control-channel liveness, which remains `not_observed`. Windows grant dispatch
remains subject to the Local Node owner's existing fail-closed boundary.

Tray operations show the companion or quit this shell. Notifications contain only
fixed native text for completed login and pairing. Native OS notification/vault
permissions can fail; there is no plaintext fallback. The shell's exit does not
claim to stop an independently running Local Node or revoke an active Console cookie.

## Signed updates

Updates are disabled and bundle generation is inactive in the checked-in candidate.
An enabled public build-time policy requires an HTTPS update origin, a 32-byte
Ed25519 policy public key and the official Tauri artifact-verification public key.
No private signing data may enter that six-field public configuration.

A detached Ed25519 signature covers exact Base64-decoded policy bytes. The policy
binds channel, target, semver, monotonic sequence, issued/expiry times (at most seven
days), deterministic per-install rollout cohort, artifact size and the entire
Tauri manifest. The native updater rejects invalid signatures, expiry, rollback,
unknown fields, stable-channel prereleases, wrong target, absent artifact signature
or any manifest/artifact URL outside the pinned HTTPS origin. Sequence and payload
digest are persisted even for withheld cohorts. Reusing a sequence with changed
rollout metadata is rejected; every rollout change requires a new signed sequence.

The official Tauri updater fetches the manifest independently; its complete JSON
must equal the signed policy manifest before downloading. Tauri verifies the
artifact signature with the pinned updater public key. Size must match before
installation. A native confirmation and a second expiry check gate installation.
No update check triggers automatic installation or publication. Local settings are
not an anti-rollback boundary against an attacker controlling the OS account or
installed executable. Install failure has no automatic retry.

The offline policy signing tool requires a matching private Ed25519 key file and
creates a new envelope only. It never publishes. Publisher verification requires
real signing inputs and verifies macOS Developer ID team, Gatekeeper and stapled
notarization, or Windows Authenticode thumbprint. Linux requires a separate pinned publisher public-key fingerprint and detached
minisign verification, in addition to the official updater signature. These checks
are not a substitute for the official updater artifact signature.

## Decision and GA gates

The recommendation becomes accepted only after an exact, signed candidate is
measured against the retained egui application on every advertised target. Source
and L0 tooling tests do not establish any of the following:

- Original install/download size and installed footprint; cold/warm startup;
  resident memory at idle and during real streaming/tool/task journeys.
- VoiceOver, Narrator and Linux screen-reader navigation, scaling, keyboard focus
  and native-dialog accessibility; notification/tray behaviour per OS.
- All flows in `native-client-capability-matrix.md`: login/password change,
  streaming/cancel/reconnect, tasks/schedules/inbox, management editors, knowledge,
  pagination, artifacts, node diagnostics/events, expired sessions, tenant denial
  and unknown mutation outcomes.
- Offline startup, packaged router/assets/Trusted Types, no replay on reconnect,
  stale credential handling and separate Console/native sign-out.
- Hostile remote/top-level/iframe/pop-up IPC probes, local XSS probes, navigation
  denials, absence of token data in devtools/storage/events/crash reports, OS-vault
  unavailable/locked/revoked cases and pairing proof/key policy on supported OSes.
- Wrong signing keys, manifest tampering, payload swap, changed rollout sequence,
  rollback/expiry/clock failure, interrupted install and recovery on exact binaries.
- Native build/lock/SBOM/license review, current Tauri IPC ACL advisories, publisher
  signing, macOS notarization/stapling and clean-machine installation/uninstallation.

All these measurements remain `not_run`. The existing egui application stays until
parity evidence and the product owner's retirement decision are recorded. Updating
this ADR to accepted requires measured artifacts; packaging success alone is not
a shell decision or GA acceptance.
