# 06 Clients and edge (Local Node, desktop, platformctl, independent CLI, SDKs)

status: proposed · domain_id: platform-upgrade-2026-10 · last_verified: 2026-10-16 (HEAD `22d9ce0`)

## 1. Role

These units run outside the server deployment. They hold the platform's most exposed credentials
(devices, laptops, CI systems) and define how third parties integrate. Each one consumes server
facts and never becomes an authority for them.

| Unit | Source | Purpose |
| --- | --- | --- |
| Local Node | `platform-local-node` (8.3k lines) | Outbound-only companion that gives agents governed access to a user's workspace, processes and (on macOS) desktop UI |
| Desktop | `platform-desktop` (13.6k lines, egui/eframe 0.33) | Native console and Local Node companion UI |
| platformctl and Rust SDK | `platform-client` (5.8k lines) | Typed `AssistantClient` and an operator CLI |
| Independent CLI | `sdk/cli` (`ai-gateway-cli` 1.5.0, TypeScript) | Launcher for the composed native Codex CLI with CLI-owned provider profiles (ADR-009) |
| SDKs | `sdk/java` (15 files), `sdk/dart` (8 files); Python retired in favour of the Rust client | Third-party integration |
| API description | `platform-openapi` (4.9k lines) → `sdk/openapi.json` (OpenAPI 3.1.0, 349 paths, 400 schemas) | Contract for SDKs, console and conformance |

## 2. Current architecture (SRC)

- **Local Node.** Protocol `ai-platform.local-node.v2` over one HTTPS control path, outbound only:
  no listener, no redirects, no POST replay. The platform signs challenges with pinned Ed25519 keys
  that carry key IDs. The device identity and device credential are stored in the macOS Keychain;
  any other OS gets `UnavailableStore` and fails closed, so the node runs only on macOS
  ([credentials.rs](../../../rust/platform-native/crates/platform-local-node/src/credentials.rs)).
  A heartbeat poll runs every 5 s by default (1–300 s allowed). Capabilities: workspace file grants
  with an approval helper, processes, a watcher, an outbox and a ledger, and macOS computer use
  through AX/CGEvent with Keychain-backed approval of the exact intent. The scoped device
  credentials and TLS composition (successor 023) are staged but not active (program blocker
  `local-node-023-and-control`).
- **Desktop.** egui/eframe with AccessKit, delegating server calls to `platform-client`. vNext
  DEC-07 treats Tauri as a candidate, not a settled choice.
- **platformctl and the Rust SDK.** `AssistantClient` covers chat, sessions, knowledge, images,
  artifacts, tools, skills, runs, threads and MCP. Its models are hand-written. Unknown mutation
  outcomes surface as `Error::UnknownOutcome`, never as blind retries
  ([SDK.md](../../../rust/platform-native/crates/platform-client/SDK.md)).
- **Independent CLI.** A thin TypeScript/Ink launcher over the lock-pinned native CLI with an
  isolated home (`~/.ai-gateway-cli`). The package declares `os: linux` and `cpu: x64` only. The
  pre-1.5 remote client remains behind `ai-gateway gateway --api-key …`
  ([package.json](../../../sdk/cli/package.json), [legacy_gateway.tsx](../../../sdk/cli/src/legacy_gateway.tsx)).
- **SDKs and OpenAPI.** A custom producer emits OpenAPI 3.1. Java (Jackson 2.17) and Dart SDKs
  are hand-written. `sdk/openapi-profiles.json` still has a `python_paths` profile. Shared
  event fixtures in `sdk/fixtures` are the conformance basis.

## 3. Assessment

Strengths to keep: the Local Node's outbound-only design with signed challenges, explicit grants
and fail-closed secure storage; exact-intent local approval for computer use; ADR-009's clean
split (the CLI never borrows the hosted Runtime); unknown-outcome semantics in the Rust SDK;
shared fixtures.

Gaps:

1. **Platform reach.** Local Node is macOS-only; the CLI is Linux x64-only; desktop GA waits for a
   shell decision (P-ADR-8).
2. **Device credentials.** Requests carry the device credential as a bearer value
   (`Authorization: Device …`, [transport.rs:99](../../../rust/platform-native/crates/platform-local-node/src/transport.rs)).
   The pairing key is an Ed25519 key derived from a Keychain-stored secret, not a non-exportable
   hardware key, so anything that can read the secret can replay or impersonate the device.
3. **Control latency.** Remote cancel and emergency stop wait for the next heartbeat poll.
4. **SDK coverage (F-28).** No Python or TypeScript SDK although these are the main languages of
   AI developers; Java and Dart drift by hand; no machine-checked SDK conformance.
5. **API description.** OpenAPI 3.1 cannot describe SSE item schemas; the custom producer carries
   Python-era profiles.
6. **CLI credentials.** The legacy remote mode takes an API key as a flag, which ends up in shell
   history and process listings.

## 4. Target architecture

### 4.1 Local Node (U-CL-02, U-CL-03, U-CL-04)

- **Secure storage on every OS (U-CL-02).** Windows Credential Manager with DPAPI, and the Linux
  Secret Service, behind the existing `SecureCredentialStore` trait. Keep failing closed where no
  store exists.
- **Device-bound keys.** Generate a non-exportable P-256 key in the Secure Enclave (macOS), the TPM
  through CNG (Windows) or TPM2 (Linux) where available. Requests carry a proof-of-possession
  signature (DPoP-style, RFC 9449) instead of presenting a bearer secret. Without hardware
  support, the device registers as a software-key class that tenant policy can refuse.
- **Control channel (U-CL-03).** Keep outbound-only, but replace the fixed poll with a long-lived
  stream: an HTTP/2 response stream or WebSocket opened by the node, carrying signed commands.
  Fall back to the poll when proxies break streaming. Target: remote stop delivered in under 1 s
  at p95 on a healthy connection.
- **Computer use (U-CL-04, on product demand).** Windows UI Automation and Linux AT-SPI drivers
  with the same exact-intent approval, ledger and emergency stop as macOS.

### 4.2 Desktop (U-CL-01, P-ADR-8)

**Recommendation: a Tauri 2 shell around the same React console build.** The console is 231 TSX
files with tests, budgets and accessibility checks; maintaining a second UI in egui (13.6k lines)
for the same product doubles the work for every feature. Native work stays in Rust behind Tauri
commands that reuse `platform-client` and the Local Node crate: pairing, Keychain and TPM access,
notifications, tray, auto-update.

Decide by ADR with measured criteria: install size and memory; accessibility in the screen readers
of each OS; startup time; IPC security (keep current with Tauri's IPC ACL hardening, such as the
May 2026 change applying ACL checks to remote origins); signing and notarization; offline
behaviour. The egui app stays until the Tauri shell reaches parity, then becomes a diagnostics
tool or is retired. Auto-update uses signed update manifests (the Tauri updater) with staged
rollout.

### 4.3 Independent CLI (U-CL-05, U-CL-06, U-CL-07)

- **Cross-platform (U-CL-05).** Ship per-platform native binaries for macOS arm64/x64, Linux
  x64/arm64 and Windows x64 as optional npm dependencies (the esbuild/biome pattern), plus
  Homebrew and winget. Sign them (Apple notarization, Authenticode, Sigstore) and verify the native
  receipt at launch, as `verify:native` already does for Linux.
- **Editor integration (U-CL-06).** Expose the local agent over the Agent Client Protocol (ACP v1;
  Zed, JetBrains, Neovim and Emacs clients). The maintained Codex adapter is built on the Codex App
  Server, which the CLI already embeds (WEB). The adapter stays local and inherits the CLI's
  provider profiles; it never talks to the hosted Runtime (ADR-009).
- **Gateway login (U-CL-07).** Replace `--api-key` with the device authorization grant (RFC 8628)
  against the Gateway (U-GW-03), short-lived tokens with refresh, and storage in the OS keychain.
  API keys stay available for CI, through environment or file only.
- **Runtime alignment.** Move the Node engine floor from 22.12 to the active LTS line (24, then
  26 after its LTS start in late October 2026).

### 4.4 OpenAPI 3.2 and generated SDKs (U-CL-08, U-CL-09, U-CL-10)

- **OpenAPI 3.2 (U-CL-10).** `platform-openapi` emits 3.2: SSE and JSONL responses gain
  `itemSchema` event types, and schemas follow JSON Schema 2020-12. Retire the `python_paths`
  profile. Publish versioned documents per release, and gate every PR with a breaking-change diff
  (for example `oasdiff`) against the last release.
- **SDK generation (U-CL-09).**
  - TypeScript and Python SDKs generated from the spec, with hand-written streaming and
    unknown-outcome helpers on top. Python returns as a generated package, not a port of the
    retired code.
  - The Rust client keeps its ergonomic layer, and its models are generated, so they cannot
    drift.
  - Java and Dart: generate them, or deprecate them with a dated notice if demand does not justify
    maintenance. Decide per language with usage data.
  - All SDKs run the shared fixtures in `sdk/fixtures` as a conformance suite in CI. Semantic
    versioning with a published deprecation policy.
- **Provider-compatible surfaces.** With U-GW-05, users can also call the platform with the
  official OpenAI and Anthropic SDKs by changing only the base URL. Document this as the default
  path for plain model access; the platform SDKs cover agents, knowledge and administration.
- **platformctl (U-CL-08).** OIDC device login, JSON output for every command, a typed
  configuration file, and the generated client underneath.

## 5. Technology upgrades

| Item | Current | Target | Notes |
| --- | --- | --- | --- |
| Desktop shell | egui/eframe 0.33 | Tauri 2 shell with the React console (by ADR) | vNext DEC-07, P-ADR-8 |
| Local Node storage | macOS Keychain only | Keychain, Windows Credential Manager/DPAPI, Secret Service; hardware keys where available | Fail closed elsewhere |
| Device authentication | Bearer device credential | Proof of possession with a non-exportable key (DPoP-style) | — |
| CLI platforms | Linux x64 | macOS, Linux, Windows; x64 and arm64 | Signed binaries |
| Editor protocol | None | ACP v1 adapter on the Codex App Server | Local only |
| API description | OpenAPI 3.1.0, custom producer | OpenAPI 3.2.0 with `itemSchema`, breaking-change gate | WEB |
| SDKs | Rust (hand-written models), Java, Dart | Generated TypeScript, Python, Rust models; Java/Dart generated or deprecated | Shared fixtures as conformance |

## 6. Work packages

| ID | Work package | Prio | Phase | Acceptance |
| --- | --- | --- | --- | --- |
| U-CL-01 | Desktop GA on the shell chosen by P-ADR-8 | P2 | Ph3 | ADR with measurements; signed and notarized builds; staged auto-update |
| U-CL-02 | Local Node secure storage on Windows and Linux; device-bound keys with proof of possession | P2 | Ph2–Ph3 | Credential replay from another process fails; fail-closed tests per OS |
| U-CL-03 | Local Node streaming control channel with poll fallback | P2 | Ph2 | Remote stop p95 < 1 s on a healthy link; fallback tested behind a buffering proxy |
| U-CL-04 | Computer use on Windows and Linux (on product demand) | P3 | Ph3 | Same approval, ledger and emergency-stop tests as macOS |
| U-CL-05 | Cross-platform signed CLI distribution | P2 | Ph3 | Install and launch tests on all five targets; signature verification at launch |
| U-CL-06 | ACP adapter for the CLI | P3 | Ph3 | Zed and one JetBrains IDE complete a governed local session |
| U-CL-07 | CLI device login and keychain storage for the Gateway mode | P2 | Ph2 | No API key in arguments; tokens refreshed and revocable |
| U-CL-08 | platformctl on the generated client with OIDC device login | P3 | Ph2 | JSON output schema per command |
| U-CL-09 | Generated TypeScript, Python and Rust-model SDKs; Java/Dart decision | P2 | Ph2 | All SDKs pass the shared fixture suite in CI |
| U-CL-10 | OpenAPI 3.2 producer with `itemSchema`; breaking-change gate (F-28) | P2 | Ph2 | Spec validates against 3.2; the diff gate blocks an unversioned breaking change |

## 7. Verification gates

- **Device security.** Credential extraction and replay attempts per OS; behaviour when the
  secure store is missing; key rotation and revocation of a lost device.
- **Control.** Emergency-stop latency, plus the stop decision held while the node is offline and
  applied on reconnect.
- **Contracts.** Each SDK and the console replay the shared fixtures; OpenAPI diff gate on every
  PR; SSE `itemSchema` validated against recorded streams.
- **Distribution.** Signature and notarization checks in the release pipeline; SBOM per
  artifact (U-PE-13).
