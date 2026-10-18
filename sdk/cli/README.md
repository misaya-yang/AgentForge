# AI Gateway Independent Agent CLI

`ai-gateway` is a standalone local Agent product. It does not use the Web
console or Gateway model configuration. The launcher selects a CLI-owned
provider profile, creates an isolated runtime home, and starts the lock-pinned
composed Rust CLI from `codex-harness`. The Rust App Server remains the only
Agent loop and owns threads, turns, tools, approvals, interruption, compaction,
resume, and interactive/headless rendering.

## Product path

```text
ai-gateway launcher
  -> ~/.ai-gateway-cli/providers.json
  -> composed Rust codex CLI + in-process App Server
  -> provider /v1/responses
```

Providers that expose a real Responses API are called directly. A provider
that only exposes Chat Completions uses a loopback adapter which translates a
strict text/function-tool subset to Responses events. The adapter is a wire
converter, not a second Agent loop.

## Native Runtime binary

Node.js **24 or newer** is required. The source release matrix contains Linux
x64/arm64, macOS x64/arm64, and Windows x64. Each optional native package is named
`@agentforge/cli-<platform>-<arch>` and carries the composed `codex` binary, the
portable `platformctl` credential helper, their independent signed receipts and
locked attribution/SBOM/source materials. Embedded `vendor/<target>/` directories
remain usable for candidate staging. Windows executable names end in `.exe`.

Normal launch verifies target, source identity, SHA-256, release version and an
Ed25519 signature against public keys embedded in the main CLI package's
`dist/native-source.json`. Replacing a binary and its hash receipt does not satisfy
the signature check. `verify:native` also checks ELF, Mach-O or PE architecture
and every locked material. Optional-package keys cannot replace the main package's
trusted keys. A signed helper is required for device login and OS-vault access.

This repository's `release/trusted-keys.json` deliberately starts empty. A release
owner must add reviewed public keys and supply the corresponding private key only
to an authorized signing job. Empty pins, an unknown signer, stale source/version,
missing targets/helpers or changed materials fail closed. No production signing
key, notarized artifact, published npm package or five-OS acceptance is implied by
these source changes.

Offline staging validates all inputs before creating a new external directory:

```text
node sdk/cli/scripts/stage-native-release.mjs \
  --target darwin-arm64 --binary /external/codex --platformctl /external/platformctl \
  --materials /external/locked-materials --output /external/new-cli-darwin-arm64 \
  --signing-key-file /private/release-key.pem --key-id reviewed-release-key \
  --os-signing-policy-file /private/os-signing-policy.json
```

The command never downloads, publishes or deletes existing targets. Its output is
a target npm package ready for an explicitly authorized `npm pack`/distribution
job. Perform Apple signing/notarization or Windows Authenticode signing on native
binaries **before** staging their hashes and Ed25519 receipts. Homebrew/winget
publication remains a release-owner action against those verified archives.
The client-only hosted CI matrix checks source contracts on all five targets;
it does not publish packages or establish hardware/editor acceptance.

For development, an explicit native override still requires
`AI_GATEWAY_UNSAFE_DEV_RUNTIME=1`; an optional
`AI_GATEWAY_AGENT_RUNTIME_RECEIPT` checks its identity but does not turn it into a
release. Default CLI mode continues to use its isolated provider profile and
local App Server, with no hosted Gateway credential injected into that Runtime.

## Configure

The default product home is `~/.ai-gateway-cli`; override it with
`AI_GATEWAY_CLI_HOME`. It is separate from both `~/.codex` and the legacy
Gateway chat CLI config.

```bash
ai-gateway config init
ai-gateway config path
```

Add a Responses-compatible provider without putting its key on the command
line or in the config file:

```bash
ai-gateway provider add qwen-responses \
  --base-url https://provider.example/openai/v1 \
  --model provider-model-id \
  --wire-api responses \
  --api-key-env QWEN_RESPONSES_API_KEY
export QWEN_RESPONSES_API_KEY='...'
```

For a Chat Completions-only provider:

```bash
ai-gateway provider add qwen-chat \
  --base-url https://provider.example/compatible-mode/v1 \
  --model provider-model-id \
  --wire-api chat-completions \
  --api-key-env QWEN_CHAT_API_KEY
ai-gateway provider use qwen-chat
```

Advanced configuration is stored in `providers.json`:

```json
{
  "schema_version": "ai-gateway-cli/providers/v1",
  "active_provider": "azure-responses",
  "approval_policy": "on-request",
  "sandbox_mode": "workspace-write",
  "providers": {
    "azure-responses": {
      "name": "Azure Responses",
      "model": "deployment-name",
      "base_url": "https://example.openai.azure.com/openai/deployments/deployment-name/v1",
      "wire_api": "responses",
      "auth": {
        "type": "header",
        "header": "api-key",
        "api_key_env": "AZURE_OPENAI_API_KEY"
      },
      "query_params": { "api-version": "2026-01-01-preview" },
      "request_max_retries": 4,
      "stream_max_retries": 5,
      "stream_idle_timeout_ms": 300000
    }
  }
}
```

Static credential headers and secret query parameters are rejected. Only the
public `version`, `x-client`, `x-client-version`, and `x-sdk-version` static
headers are accepted; every provider-specific/credential header uses
`env_http_headers` whose value is an environment-variable name. Query keys and
values are restricted to URL-unreserved characters because the pinned upstream
provider joins them without a second encoding pass.

## Use

Interactive mode and native subcommands are forwarded to the Rust product:

```bash
ai-gateway
ai-gateway exec --json "Summarize this repository"
ai-gateway exec resume --last "Continue"
ai-gateway resume --last
```

The launcher preserves a native child's numeric exit code. When the child exits
because of a signal, it returns `128 +` that signal's operating-system number
(for Linux: SIGINT 130, SIGTERM 143, SIGKILL 137), retaining fallback 129 if
the OS exposes no number for the signal. It forwards SIGINT/SIGTERM
while the child is running, removes its forwarding listeners on exit, and does
not restart or replay the command. This applies equally to interactive and
headless invocations; the launcher adds no terminal-status text to JSON stdout.
A signal exit does not prove remote work stopped or that file effects rolled back.
The controlled Node-child regression checks launcher behavior only, not native
runtime/provider interruption or safe resume acceptance.

Select a provider for one invocation without changing the saved active
provider:

```bash
ai-gateway --cli-provider qwen-responses exec "Run the tests"
```

The explicit hosted Gateway mode uses device authorization and the native OS
vault. Public settings remain in `~/.ai-gateway/config.json`; saving settings
strips both legacy API keys and access tokens. Existing plaintext credentials are
not used as an implicit fallback. No browser is opened automatically:

```text
ai-gateway gateway auth login --base-url https://gateway.example
ai-gateway gateway auth status --base-url https://gateway.example
ai-gateway gateway --base-url https://gateway.example
ai-gateway gateway auth logout --base-url https://gateway.example
```

Open the printed same-origin verification URI and approve the displayed code in
your existing browser session. The public client is `agentforge-cli`, with the
`platform` scope and S256 PKCE. Tenant and user come from that approval, rather
than command-line identity selectors. Refresh cannot change that binding.
Native vault backends are macOS Keychain, Windows Credential Manager/CurrentUser
DPAPI and Linux Secret Service; missing or locked storage fails closed.

Interactive hosted requests share a single refresh provider. Independent native
processes serialize refreshes with OS-released locks. A durable in-flight marker
prevents reuse of a rotating refresh token after a crash or unknown response;
explicit login is then required. Logout attempts remote revocation once and
always deletes the local session. A lost revoke response remains uncertain.
Server-side parent-session, tenant, account and revocation checks remain required.

CI may use `AI_GATEWAY_API_KEY` or `--api-key-file /private/key`; API keys in
`--api-key`, `-k` or URLs are rejected before parsing/persistence. Device login
also supplies owner session/history reads. Explicit `AI_GATEWAY_SESSION_TOKEN`
remains available for API-key CI sessions without becoming a model API key.
The independent provider mode never reads these hosted credentials.

Hosted JSON requests keep the configured timeout active through response-body
consumption and settlement of the JSON promise, including successful responses.
This does not preempt synchronous JSON parsing. A body timeout rejects
the command and releases the local busy state; it does not prove that a remote
mutation stopped or failed, and the client does not automatically replay it.

Session reads require an HTTPS Gateway origin; local development may use an
explicit loopback IP such as `http://127.0.0.1:8080` rather than `localhost`.
These GETs never follow redirects or retry and have a bounded timeout and 8 MiB
response limit. API-key model requests retain their existing transport behavior.

Inside this Gateway compatibility UI, `/history` displays complete session IDs.
Use `/resume <session_id>` to load the owner-authorized `/history` projection and
select that existing session for the next message. Recovery is one read-only GET;
it does not start, cancel, or replay execution. The session ID must match the
response before the current conversation changes. Failed or malformed reads leave
the current session and messages unchanged. The UI shows the latest 20 messages
from the server's bounded history response, not a complete export or live stream
reconnection. These slash commands do not change the independent local Runtime's
`ai-gateway resume` behavior.

Gateway streaming replies retain an explicit `Error: ...` or `Cancelled: ...`
line for server-reported failure/cancellation, including a native interrupted run
reported as `run_error` with `status: cancelled`. Partial output is kept with that
outcome. A successful terminal event without text displays `Completed without text
output.`; it is distinct from a stream ending without a terminal outcome. Error
messages are rendered as text. This display neither retries/replays execution nor
proves that local Ctrl+C stopped a remote run or rolled back side effects.

This presentation is local to the compatibility UI: `/resume` still loads the
server's bounded message history and does not reconstruct terminal states that
history does not contain. Generic task-result retrieval (including typed
empty/text/JSON/bytes payloads) is not a command in this CLI; these changes do not
add a task API or alter the independent Runtime's rendering or resume behavior.

## Security and compatibility boundary

- Provider secret values live only in named environment variables. They are
  never written to JSON/TOML, passed in argv, or copied into error output.
- Generated Runtime config excludes key/token/secret/password/credential
  variables from model-generated shell commands.
- Chat-only provider secrets stay in the Node adapter; the Rust child receives
  only an ephemeral loopback credential.
- Non-TLS provider URLs are rejected except explicit loopback development
  profiles.
- The Chat adapter supports text, function tools, tool results, usage,
  reasoning-effort request passthrough, structured reasoning-summary deltas,
  retries after explicit retryable HTTP responses, and strict SSE projection.
  A transport failure with an unknown provider outcome is not retried. Stream reconnect is
  intentionally disabled because replay could duplicate output or tool intent.
  Plain reasoning summaries produced by the adapter replay as Chat
  `reasoning_content`, separate from assistant text. Parallel function calls
  replay as one assistant tool-call batch followed by all matching results.
  Namespaces retain distinct deterministic aliases in tool definitions, forced
  choices, returned calls, and subsequent history. Named results must agree
  with their call identity.
  Opaque reasoning state, Responses Lite embedded tools/context controls,
  unpaired named tool results, images, hosted tools, and input forms that
  cannot be represented losslessly fail closed.
  Stream limits apply to individual SSE frames and total content; coalesced
  network chunks are decoded in bounded slices. Slow consumers pause upstream
  reads, and disconnects cancel the provider stream. A non-null top-level
  `error` payload in a Chat provider SSE frame terminates projection with one
  `response.failed` event and the static `provider_stream_error` code. Provider
  error details are not forwarded; later output or `[DONE]` cannot change that
  failure into success. This also applies after partial text or tool arguments;
  the adapter cancels the remaining stream and does not replay the request.
- The native build and package verifier derive source identity from the Runtime
  source receipt, overlay manifest, and lock; package assembly fails when they
  disagree. The launcher re-checks receipt target/name and binary SHA-256.
- Pointing at an arbitrary binary or system `codex` requires explicit unsafe
  development opt-ins and is never release evidence.
- Linux `--approve-for-me` requires `bubblewrap` (`bwrap`). The launcher fails
  before spawning when it is absent, avoiding an unsafe escalation fallback.

## Development checks

```bash
npm --prefix sdk/cli ci
npm --prefix sdk/cli run typecheck
npm --prefix sdk/cli test
npm --prefix sdk/cli run build
```

These checks prove the launcher/config/adapter layer. Composed Rust behavior,
native packaging, and real provider compatibility require their separate
hosted/Docker/live receipts.

## Hosted Knowledge: saved Confluence source → manual sync → exact result

This explicit subcommand uses the existing Gateway API key/tenant configuration
in `~/.ai-gateway/config.json`. It does not initialize or write configuration,
acquire credentials, start the independent Runtime, or read provider profiles.
The key must map to the current source owner with dataset editor access and
appropriate Knowledge scopes. The server rechecks authorization. This is a
Python Knowledge workflow, including native Gateway with the explicitly configured
retained Python Knowledge profile; full-native Knowledge has no manual sync.

List saved source bindings and server capability first:

```sh
ai-gateway gateway knowledge sources DATASET --base-url https://gateway.example
```

`manual_sync_available: true` means an explicit submission may be attempted;
it is not proof that a scheduled sync exists. `current_sync_task` is the latest
safe receipt. No credential or connector creation is provided. An unavailable
connector yields a nonzero read outcome; missing/false capability blocks sync.

Generate and retain one request UUID **before** the explicit mutation. Node is
already a prerequisite; no additional utility or dependency is required.

POSIX shell:

```sh
REQUEST_ID=$(node -p 'require("node:crypto").randomUUID()')
printf '%s\n' "$REQUEST_ID"
ai-gateway gateway knowledge sync DATASET BINDING --request-id "$REQUEST_ID" --confirm --base-url https://gateway.example
ai-gateway gateway knowledge status DATASET BINDING --request-id "$REQUEST_ID" --base-url https://gateway.example
```

PowerShell syntax (these commands do not assert Windows native-package support):

```powershell
$requestId = node -p 'require("node:crypto").randomUUID()'
Write-Output $requestId
ai-gateway gateway knowledge sync DATASET BINDING --request-id $requestId --confirm --base-url https://gateway.example
ai-gateway gateway knowledge status DATASET BINDING --request-id $requestId --base-url https://gateway.example
```

Save the UUID in your own task record so it survives closing the terminal. For
a later read, replace the variable with that saved literal UUID. The CLI has no
persistent request ledger. Do not lose the UUID or regenerate one after an
uncertain submission. `--confirm` acknowledges full source reconciliation:
source-deleted documents can be removed when no other binding references them.
Already applied page changes are not rolled back by failure or interruption.

Only `sync ... --confirm` sends a POST, once, after a read-only capability
preflight. Every status invocation makes one exact GET and never submits or
retries work. An already-active task blocks a new submission and prints its
UUID. Pending/processing means accepted, not completed: repeat the printed
status command until terminal. There is no automatic polling, mutation retry,
resubmission, cancellation or recovery takeover. A process crash may leave a
pending task requiring operator reconciliation.

Lost, malformed, oversized or mismatched receipts are uncertain. Keep the same
UUID and use `status`; even GET 404 or access failure is inconclusive. Only an
explicit POST `CONFLUENCE_SYNC_NOT_ACCEPTED` with `accepted: false` is a definite
pre-admission rejection. After a terminal failed/cancelled receipt, inspect the
source connection/access and attempted/applied/failed counts, fix the prerequisite,
then explicitly choose a new UUID for a new sync. Applied counts are separate
from attempted pages; null is unknown, not zero.

Network outcomes emit one JSON object on stdout and human-readable recovery
guidance on stderr. Help is plain text; invalid arguments/local configuration
failures emit only stderr with empty stdout. Exit codes: `0` successful read or valid accepted/
completed receipt, `1` unavailable/read failure/definite rejection/failed or
cancelled task, `2` invalid arguments or local configuration, `3` uncertain
submission or exact-task read. Automation must inspect `outcome`/task status;
exit 0 for pending is not completion. Raw provider/HTTP errors are not printed.
The transport uses OS-vault device login or explicit CI environment/file keys, rejects credentialed
URLs and non-TLS non-loopback origins, refuses redirects, bounds each response
to 8 MiB and the request to at most 300 seconds. The existing model/session
transports keep their original defaults.

Controlled tests exercise the real executable dispatch, parser, transport and
JSON/guidance output with synthetic fetch responses, plus existing session/result
recovery checks. They do not establish live Gateway/provider, worker restart,
database, scheduler, package/native-platform or full KNO-10/CL-01–06 acceptance.

SIGINT/SIGTERM aborts the local management request. If POST has begun, the CLI
still prints an uncertain JSON outcome and exact-UUID recovery guidance (exit 3).
It does not claim that interruption cancels server-side work or rolls back pages.
A hard kill/power loss cannot emit a final receipt; use your retained UUID.

中文快速说明：先用 `sources` 查看已保存的绑定和手动同步权限，再生成并保存请求
UUID。只有带 `--confirm` 的 `sync` 才提交一次全量同步；来源删除可能移除最后一个
绑定引用的文档。用同一 UUID 的 `status` 查询真实终态。丢响应、断网、读回 404
都不能认定未提交，不能换 UUID 盲目重试。失败时分别查看已尝试、已应用、失败数量；
修复连接或权限后，再明确发起新 UUID 的同步。独立 Agent 的 provider 配置、模型调用
和恢复命令不受影响；此管理入口不会启动原生 Runtime。


### Offline local diagnostics

Run `ai-gateway doctor` before launching a local session, or `ai-gateway doctor
--json` for one shareable JSON report. `--cli-provider <id>` checks a configured
local profile without changing the active provider. `doctor --help` prints usage;
top-level launcher help/version forwarding is unchanged.

Doctor collects configuration, selected wire format and credential-variable
presence, packaged artifact/source/receipt checks, local directory access, and
optional Linux `bwrap` lookup. It works without configured credentials and never
creates directories, changes permissions/configuration, starts a compatibility
proxy, executes the native binary, or contacts a provider. Missing optional `bwrap`
is a warning: it is required for Linux `--approve-for-me`, not unrelated setup.

Exit codes are `0` for no observed blocking local prerequisite, `1` for a blocked
check, and `2` for invalid doctor arguments. Neither exit0 nor a matching artifact
hash proves provider connectivity or runtime protocol readiness. Unsafe development
binary/system selection is explicitly unverified; current release support remains
the five source release targets; installed artifacts require signature checks. Directory access is an observation without a write probe.

Reports contain fixed check codes and instructions, not raw exceptions, private
paths, environment values, profile URLs or prompt/history data. Use `ai-gateway
config path` separately when you need the private local configuration location.
Nothing is uploaded or auto-fixed. Gateway compatibility commands remain separate.


## ACP editor entrypoint

Configure a custom ACP agent in Zed or a JetBrains ACP client to launch
`ai-gateway acp --cli-provider PROFILE` with the selected provider's environment
variables. It speaks ACP v1 JSON-RPC on stdin/stdout and opens the CLI-owned
composed App Server over private stdio. No hosted Runtime or new Agent loop is
introduced. Provider selection, sandbox and approval policy remain CLI-owned.

The implemented profile supports initialize, session/new, session/load,
text session/prompt, streamed assistant text/tool status, exact-session/turn local
permission requests and session/cancel. Runtime command/file approvals offer
allow-once or reject; unknown requests, foreign sessions/turns and late approvals
fail closed. Frames, pending calls, sessions and output buffers are bounded.
Cancellation interrupts the existing turn and never restarts or replays it.

Images, embedded context, editor-supplied MCP server configuration and permission
policy changes are not advertised or accepted. Configure optional tools through
the existing trusted CLI configuration. Unit tests exercise the real bridge with
isolated stdio peers; actual Zed and JetBrains governed-session acceptance has not
run. This is an ACP v1 text/local-tools profile, not a claim of every optional ACP
extension or arbitrary editor-MCP interoperability.

The offline [release source workflow](release/README.md) verifies Apple
team/Gatekeeper, Authenticode signer, or offline Sigstore identity before staging
Ed25519-signed native receipts. It also generates reviewable Homebrew/winget
metadata from real immutable signed asset URLs. Production trust pins, composed
artifacts, a real MSI and five-target installation acceptance are release inputs;
no publishing or installer execution is performed by source tests.
