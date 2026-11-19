# 06 Clients and edge (Local Node, desktop, platformctl, independent CLI, SDKs)

按完整模块交付；进度只在[现有账本](../../../deploy/runbooks/native-enterprise-upgrade/loop-state.json)。
原始工作包用于需求与验收核对，执行顺序使用[主计划的三个批次](README.md#6-roadmap)。

## 1. 本模块要交付什么

让 Local Node、CLI、desktop 和生成 SDK 通过同一身份与协议完成真实任务，不为每个客户端重建一套后端契约。

| 交付部分 | 原需求 ID | 连通的结果 |
| --- | --- | --- |
| 设备与控制链 | `U-CL-02`, `U-CL-03`, `U-CL-07`, `U-CL-08` | 设备登录、安全存储、撤销、事件控制和停止/重连接到 Gateway。 |
| 客户端发布 | `U-CL-01`, `U-CL-05` | 按既有 shell 决策完成桌面/CLI 产物；签名、公证和更新在正式发行阶段验证。 |
| 统一 SDK 契约 | `U-CL-09`, `U-CL-10` | OpenAPI 生产、生成客户端和共享流式 fixture 作为一个交付单元。 |
| 编辑器接入 | `U-CL-06` | ACP 接入复用现有本地受控会话。 |
| 按需 OS 能力 | `U-CL-04` | Windows/Linux computer use 仅按原产品需求触发。 |

以上部分在同一模块中连续完成，不分别创建小任务、审批、审核或交接记录。

## 2. 接线与真实依赖

复用 Gateway 的设备授权和 API、Runtime 的事件契约及已有 OS 安全存储。先完成实际调用链，再集中做对应 OS 和分发验证；不因尚未发行而阻塞已支持平台的功能实现。

## 3. 一次集中验证与收尾边界

共享契约 fixture 执行一次，客户端补其独有消费行为。Local Node 统一验证配对、批准读写、远程停止和离线重连；各 OS 凭据隔离、签名与发布验收按原范围保留。

没有测量理由不更换 desktop shell；命令输出外观、模块命名和重复 SDK 封装不占当前关键路径。

<details>
<summary>设计约束与技术选择（实现涉及该边界时按需查阅）</summary>

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

</details>

## 6. Work packages

<details>
<summary>原始需求和验收对照；Prio / Phase 是原计划标签，不是逐项执行流程</summary>

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

</details>

## 7. Verification gates

以下场景在上述模块验证和最终验收批次中覆盖。同一场景由一个执行者运行一次，关联
多个需求即可；未变化的适用结果直接复用。关键边界和原门槛不能由重复的局部测试替代。

- **Device security.** Credential extraction and replay attempts per OS; behaviour when the
  secure store is missing; key rotation and revocation of a lost device.
- **Control.** Emergency-stop latency, plus the stop decision held while the node is offline and
  applied on reconnect.
- **Contracts.** Each SDK and the console replay the shared fixtures; OpenAPI diff gate on every
  PR; SSE `itemSchema` validated against recorded streams.
- **Distribution.** Signature and notarization checks in the release pipeline; SBOM per
  artifact (U-PE-13).
