# 06｜公开 API、SDK 与独立 CLI

## 1. 用户、现状与目标

开发者需要把平台能力接入自己的程序；CLI用户需要在明确的本地或托管环境完成任务。当前已有公开API、多语言客户端代码和独立CLI；CLI具有自己的provider配置与运行目录，不能因为Web配置成功就认为CLI已配置。见[证据E18](01-current-state.md)。

目标是让**已承诺支持的调用方式可安装、可运行、可判断失败、可中断和恢复**。跨平台制品、真实模型测试和文档支持声明分别核实，本次不新增桌面产品。

## 2. 旅程与需求

托管调用：取得限定范围的API Key → 选择可用模型/Agent → 使用最小示例调用 → 读取流/结果 → 查询历史/用量 → 处理失败 → 撤销Key。

独立CLI：安装匹配制品 → 检查运行前提 → 配置独立provider → 执行文本/工具任务 → 审批或拒绝 → 中断 → 恢复 → 导出脱敏诊断。

| ID | 优先级/类型 | 需求与规则 | 可判定验收 |
| --- | --- | --- | --- |
| CL-01 | P0 验证 | 发布明确的接口/语言/平台支持表，注明本地/托管、协议、已验证能力和限制；存在源码不等于已支持平台 | 文档、安装包与实际制品一致；已声明支持的目标必须实测；缺制品/未运行测试不能改记PASS，也不能静默撤掉原支持承诺 |
| CL-02 | P1 补齐 | 每个已支持SDK至少提供安装、鉴权、普通请求、流式、取消和错误处理示例；接口弃用有替代路径 | 从隔离环境执行示例成功；错误示例得到文档说明的状态；命令/示例不含开发者路径或真实密钥；OpenAPI与客户端可对应 |
| CL-03 | P0 验证 | V1/V2及Responses/Chat支持范围说明一致；不支持参数在执行前报错，不能静默忽略 | 同一能力的允许/拒绝结果一致；stream首帧前和中途失败都有唯一可判断结果；无界参数/慢消费者不无限占资源；授权与用量不因入口改变 |
| CL-04 | P1 验证 | CLI local与gateway兼容模式的配置、历史、权限和凭据互相独立，profile选择可解释 | 关闭Gateway只影响托管模式；CLI不自动读取Web或Codex凭据；切profile显示有效来源；配置错误能定位到字段 |
| CL-05 | P1 补齐 | headless与交互模式的退出码、stdout/stderr、SIGINT、审批、恢复一致；恢复显示任务位置与有效配置 | JSON模式stdout只有协议数据；错误非0退出；取消后不重复写文件；恢复同一线程保持原授权，无法恢复则明确拒绝 |
| CL-06 | P1 补齐 | doctor/诊断覆盖制品、协议、provider、工具前提与目录权限；输出可分享的脱敏信息 | 缺binary/不匹配架构/过期receipt可判因；诊断不泄漏密钥或私有正文；可选工具缺失不阻塞无关文本任务 |

## 3. 跨模块约束

API使用CF-02的Key权限及US-02的租户范围。CLI本地权限由本地配置决定，不能把托管“完全访问”解释为自动授予主机权限。SDK事件和错误语义与AS-02、TL-03一致，但呈现方式可不同。

首次实施前应把现有SDK语言/版本、CLI已发布OS/架构、已支持协议列入验收清单。原合同已承诺的目标缺证据就保留未完成；新增平台可作为P2另立需求，不用“支持所有系统”拉大本轮范围。

## 4. OpenAPI profile 交付边界（2026-10-12）

`sdk/openapi.json` 保留完整发布合同，不因 Python 导出而删去原生端点。
`sdk/openapi-profiles.json` 显式划分 Python/common 与 native-only 所有权，
校验重复、未知、陈旧条目及原生路由/dispatcher 证据。Python/hybrid 的
`make verify-openapi-contract` 仅验证对应投影及真实 FastAPI 导出的兼容性，
并在独立 hash-seed 进程中校验 operation ID；通过不等于完整产品或 CL-01～06 验收。

`make snapshot-gateway-openapi OPENAPI_OUTPUT=/tmp/python-openapi.json` 只输出独立
Python 候选，禁止覆盖混合发布文件。`make verify-native-openapi-contract` 仍是
必需但未完成的原生门禁：独立源声明 exporter 的 Users 增量现已重新编译，声明 33 个路径（43 个操作），其中 20 个 native-only 路径
包含 21 个操作，另有 7 个 common Roles、6 个 common Services 和 9 个 common Users 操作。仍缺 409 个发布操作及响应
schema/status 的完整原生证据；源绑定后的完整原生门禁为 FAIL431。相对历史 Services FAIL419，
6 个缺失 Users 路径已声明，新增 9 个实际鉴权差异，以及 9 个 422 detail 字段比较诊断。
比较器只遍历 $ref/allOf，未读取此次 oneOf 的 detail 字符串/数组分支；每个分支仍要求 detail，
并非运行时删除字段。实际鉴权与混合错误形状均保留，未为通过门禁改写；历史 FAIL416/419 保留。

即使提供兼容候选也以非零退出；`make compare-openapi-candidate` 仅提供制品比较
诊断，不能代替原生实现、运行或发布验收。完整命令与限制见
[命令手册](../../harness/commands.md#openapi-profile-ownership-and-export)。


### Saved Knowledge source workflow in the standalone CLI (2026-10-12)

The existing `ai-gateway gateway knowledge` branch now supports saved-source
listing, explicit `sync <dataset> <binding> --request-id <UUID> --confirm`, and
exact `status` readback with the retained UUID. It uses the existing Gateway
credentials and Python Knowledge owner, including the documented native Gateway
hybrid profile. Use [the CLI instructions](../../../sdk/cli/README.md)
for complete POSIX/PowerShell usage and exit-code contracts.

Capability preflight is informational; backend admission remains authoritative.
Mutation timeout, interrupted IO or an invalid receipt preserves uncertainty and
prints exact readback guidance. There is no automatic POST retry or redirect,
and SIGINT/SIGTERM stop local IO without claiming remote cancellation. Partial
results distinguish attempted and applied items. No new credentials, connection,
binding, scheduler or provider activation is performed.

Controlled CLI/parser/transport tests and a built executable help/invalid-argument
smoke verify source wiring. Live Gateway/provider execution, cross-platform
installation, database durability and complete KNO/CL acceptance remain open.


### Native-only source export boundary (2026-10-13)

The initial 2026-10-13 `make snapshot-native-openapi` increment produced a bounded
independent native schema and coverage report for 14 native-only paths
(15 operations). Actual handlers
share serde request declarations and route/validation constants with the small
Rust producer. The wrapper checks source identity, explicit ownership, dispatcher
proofs, deterministic output and optional strict previous-declaration drift.
The published SDK schema is not an input to production and was unchanged in that increment;
it is only read afterward to report coverage and enforce existing ownership.

The remaining native release gap is explicit: 431 common operations are not
exported, host/error responses remain incompletely declared, and deployment
identity/security/runtime fidelity still needs separate verification. The full
required native gate remains failing. This bounded source-owned foundation and
its offline tests are not full CL-01–06 acceptance or live Gateway verification.


### Source-owned native success responses (2026-10-13)

The initial 2026-10-13 response increment covered 15 native-only operations,
with bounded success-response schemas tied to
actual shared envelope constructors, serialized plugin DTOs and notification SQL
projection declarations. Null/optional serialization and dynamic DB row values are
preserved; no new database schema, filtering or response validation is introduced.
Offline legacy-wire and SQL-projection equivalence tests accompany the extraction.

Observed handler error statuses retain unconstrained bodies and an undeclared
default; they are not exhaustive middleware/error contracts. Security describes
actual admission, including JWT-only plugin routes and authenticated configuration
audit. The published audit auth omission and other genuine contract differences
remain visible. The full required native gate is still failing; this work does not
claim the remaining 431 common operations or CL-01–06/live release acceptance.


### Scheduled receipt traversal expands the current native subset (2026-10-13)

The current source-owned export and publication now contain 16 native-only paths
and 17 operations. The two additions read an exact owned schedule and an exact
execution bound to that schedule. Existing list routes accept optional paired
creation-time/UUID cursors and return `next_cursor`; no-argument callers keep the
previous 200-schedule/100-execution limits and array members. The Web console
uses these contracts for older pages and copied links, as described under
[TK-02/TK-04](05-models-governance-operations.md).

This is additive native ownership, not Python/native convergence: the 431 common
operations remain unexported. The full native gate still fails with the same
413 historical violations. New list-validation status metadata explicitly leaves
the error body unconstrained, matching the source producer; no previously
published constraint or failing comparison is removed. Stable cursor and exact
read tests, source-bound native compilation and controlled Web coverage do not
establish PostgreSQL, authenticated-browser or live service acceptance.


### platformctl official SDK service configuration (2026-10-13)

The management CLI adds typed official-SDK registration, settings edits, and
exact-ID sanitized readback using shared Rust client contracts. `sdk-services
create` and `edit ID` review through GETs unless `--yes` explicitly permits one
write; `get ID` recovers current public settings. Provider credentials are accepted
only from an explicit bounded regular file; omitted edit credentials preserve
the current key. Framework/model/instructions/turns and key-presence readback are
checked without claiming a particular key replacement is verified.

See [the native-client capability matrix](../../architecture/native-client-capability-matrix.md#platformctl-official-sdk-service-configuration-2026-10-13)
for commands, fields, safety, output, exit codes and non-atomic registration/edit
limitations. Unknown writes retain their pre-dispatch ID for exact GET recovery,
never automatic replay. Controlled helper/parser tests and file-only executable
probes are bounded source evidence; live Gateway authorization, persistence,
provider execution, cross-platform distribution and full CL acceptance remain open.


### Source-owned common Services lifecycle declarations (2026-10-13)

The existing register/list/detail/update/delete/schema operations now share
source-owned route inventory and bounded success builders with the exporter.
Legacy registration remains an upsert; the existing atomic SDK registration
endpoint keeps its separate native-only ownership. Common Services operations
remain in the Python projection and no SDK publication is rewritten.

The list is a genuine array with virtual and registered-service variants. Dynamic
request dictionaries, detail values, service schemas and availability values are
not narrowed. Existing visibility differences, masking, query handling and
post-write invalidation uncertainty are explicit in the
[Services contract boundary](../../architecture/native-common-service-contract.md).
Controlled source fixtures require fresh execution and compiler/source binding;
current PostgreSQL, provider, HTTP listener, browser and full CL acceptance remain open.


### Source-owned common account lifecycle declarations (2026-10-13)

The nine existing Users operations share native route inventory, validation
constants and production response ownership with the bounded exporter. This is
six additional common paths; the fresh source-bound export confirms 33 paths/43
operations and leaves 409 published operations absent. The unchanged full-native
comparator reports FAIL431, including nine Users auth differences and nine
oneOf/detail diagnostics; the source branches still require the detail field.
The account administration and self-profile resolver/RBAC/projection differences,
nullable field behavior, storage guards and post-write uncertainty are retained.
See [the account contract](../../architecture/tenant-account-administration.md#source-owned-native-account-contract-2026-10-13).
No published SDK schema or Python ownership profile is changed, and this increment
cannot establish full-native, PostgreSQL, browser, runtime or CL-01–06 acceptance.


### Hosted compatibility CLI UTF-8 stream decoding (2026-10-13)

`ai-gateway gateway` retains the existing hosted conversation client. Its SSE
transport uses Node's incremental UTF-8 decoder so split Chinese characters,
emoji and other multibyte code points survive arbitrary HTTP chunk boundaries.
Malformed bytes retain the existing replacement-character policy; event framing,
terminal detection, EOF frame policy, mutation counts and local cleanup are unchanged.
Listener-free regressions exercise the actual client with controlled Node streams.
This source change does not establish live Gateway/provider acceptance, remote
cancellation, reconnection or cross-platform distribution.

## Generic invocation and task source declarations (2026-10-14)

CL-02/CL-03 and TK-02/TK-03 require an inspectable contract for the existing Web
playground and generic task console. Seven existing mounted operations are declared
by `native_invocation_task_contract.rs`: POST invoke/stream/submit, GET tasks,
GET/DELETE task detail, and GET task result. Existing DTOs and validation remain in
`invocation_http.rs` and `task_http.rs`; only route constants are shared with the
routers. No endpoint, dispatcher, state machine or schema-generation framework is
added. Production parser/serde/wire-projection tests guard the separate declarations.

The verified consumers are `web/src/api/gateway.ts` and playground
`usePlaygroundStream.ts`. The separate AssistantClient uses `/assistant/*`; these
seven declarations do not establish its support. All seven current native routes
require JWT account/session authority. Database API-key compatibility remains a
separate design gap: durable submit persists a JWT-only TaskActor, and SDK read
leases require a session Principal. Publication must not silently enable keys or
turn body/header identity claims into authority.

The declaration distinguishes a pending submit acknowledgement from execution;
503 `task_admission_unconfirmed` requires exact-task observation before a caller
decides what to do. Cancellation is a conditional terminal transition, not history
deletion, dequeuing or proof that remote side effects stopped. Completed/failed
cancellation returns409; repeated cancellation is accepted. Listing always scopes
to owner and tenant; detail/cancel allows only the owner or exact admin in the same
nonempty tenant, without a generic platform_admin cross-tenant exception. Result
returns404 when an actual UnifiedResponse result is unavailable.

Stream200 is `text/event-stream`, not a JSON response or completion guarantee.
Opening occurs on first body poll; subsequent failures are `event: error` with text
data. Normal data frames carry the existing StreamChunk projection. Session headers
are optional, and local body cancellation does not prove remote execution stopped.
Request priority/timestamp lexical coercion and runtime service validation are
explicit source-owned boundaries, not fully represented by their broad JSON shape.
Arbitrary task result and native output data remain open, while internal task
request/callback bookkeeping is excluded by the existing TaskSchema projection.

Offline verification completed: 96 focused wrapper/source-binding tests and scoped
Python lint passed. Six real foundation production parser/projection tests passed
with no failures, ignored or filtered tests; this compiled the current foundation
library and integration-test target. The fresh OpenAPI producer also compiled and
linked. Both native commands emitted no compiler warnings. The original test-run
finalizer scanned an unrelated retained dep-info variant; its failed receipt is
preserved alongside an evidence-only correction selecting the actual emitted
library/test artifacts and their 151 current-source prerequisites. No successful
test was rerun, and no runtime source was changed for that evidence repair.

The actual fresh export measures 49 paths and 70 operations, with 382 published
operations absent; 101 Knowledge owner operations remain separately composed.
Actual exported schema-shape checks passed 10 positive and 32 negative cases,
separate from the production projection tests. Full native compatibility remains
**FAILED (466 violations)**, previously 457: publication removes six missing-path
diagnostics and exposes seven JWT security differences, four 422 body differences,
three absent 422 statuses and one SSE-versus-JSON media difference. These are
recorded differences, not new runtime behavior or a claim of full compatibility.
The published SDK schema and Python/hybrid ownership were not weakened to hide them.

Source, toolchain, dependency, artifact and actual-output identities were verified
with the 8 GiB native admission floor retained. The generated temporary fixture tree
from the passing Python run was inventoried and removed separately; immutable tested
sources, logs and receipts remain. These checks do not establish live models,
storage, whole Gateway binary or AssistantClient acceptance. API-key compatibility
requires an explicit authority/session design rather than changing these declarations.


## Generic stream success media declaration

The Python `/api/v1/stream` route returns SSE using `StreamingResponse`, as does
the native route. Its previous undecorated response metadata let FastAPI publish
an empty `application/json` success response, which was copied into the mixed SDK
snapshot. Explicit response metadata now describes only `text/event-stream` for
200; the JSON request and generated 422 validation response remain unchanged.
This is a developer-facing contract correction, not a runtime transport change.

The published operation's 200 response must be regenerated from the actual
FastAPI application export, preserving every other published byte-equivalent
JSON value and all native-only operations. The existing exporter continues to
reject direct overwrite of the mixed publication. The comparator is unchanged.
Actual FastAPI export and the isolated 200-response regeneration passed. All 18
focused tests and 80 subtests passed, including actual application OpenAPI
compatibility, in-memory production SSE frames and post-header errors, JSON
preflight/validation failures, and existing shared SDK parser/transport regressions.
Baseline and candidate scoped lint were clean. The run retained 48 warnings
(deprecations and existing duplicate Knowledge operation IDs); these were not hidden
or changed by this slice.

Initial isolated attempts exposed missing ownership proof inputs, missing optional
application-import dependencies, and two generated log files inside the snapshot.
All failures and original identities remain recorded. The successful run includes
only ownership-listed proof files, uses the existing logging-directory override,
and restores the exact locked `jsonpath-ng==1.8.0` and `sse-starlette==3.4.4` wheels
into a separate hash-pinned layer. A temporary earlier 3.4.8 fallback was removed
from the successful run. The base environment and 41 added dependency-file hashes
were verified before/after; this scoped result does not certify the full lockfile
closure or execute conversation-service behavior. No application lifespan ran.

No shipped generated client defect was identified: Python, Java, Dart and CLI
Assistant clients use handwritten `/assistant/chat/stream` transports. The Web
playground consumes generic `/stream` as SSE already. This correction does not
establish new SDK coverage, change authentication or close the remaining native
JWT/API-key, validation-shape, whole-Gateway or live-service acceptance gaps.

The earlier 466-violation native comparison predates this publication correction.
No native comparison was rerun here, and no updated native pass/count is claimed.


## CL-06 / J28 local read-only doctor

The standalone launcher intercepts `doctor [--json] [--cli-provider <id>]` before
credential fail-fast, compatibility proxy startup, runtime config writes or native
spawn. It reuses the existing product configuration and native artifact resolver;
there is no second runtime, authority policy or automatic repair path. Independent
checks remain visible when local configuration or credentials are missing.

Stable fixed check codes and remediation text avoid printing exceptions, secrets,
private paths, provider URLs or history. Exit0 means no observed blocking local
prerequisite; exit1 means blocked; exit2 means invalid doctor syntax. Warnings for
optional tools and explicitly unverified unsafe development identity are not live
readiness. Directory checks observe permissions/nearest parent without writes.
Protocol checks describe configured wire support only, not a runtime handshake.

Validation on 2026-10-14 passed TypeScript, 16 existing production launcher/config
tests, the canonical JavaScript bundle build, 19 actual built-entry doctor process
cases, and native-package verifier/package-script syntax checks. All six bounded
phases exited zero with unchanged source/dependency identities and empty stderr.
The process tests use isolated temporary homes, synthetic credential canaries and
a sentinel binary. They verify unchanged fixture files/modes, redacted diagnostics,
provider selection, missing/invalid inputs, and executable availability. Outbound
network, listeners and native child spawning are prohibited in those doctor cases.
The existing independent CLI gate now runs this regression after its fresh build.

Dependencies were restored into a fresh external target from the unchanged npm
lock using official integrity-pinned packages and disabled install scripts. The
initial certificate-chain failure is retained; a bounded retry succeeded using the
existing, hash-pinned OS CA bundle with TLS verification enabled. The tested bundle
is bound to its same-attempt build receipt. This closes the offline diagnosis step;
actual native platform execution, provider health and package installation acceptance
remain separate. Linux x64 remains the declared release target.

The subsequent unchanged `make independent-cli-gate` passed on the exact current
source: 136 Vitest tests across 11 files, 78 npm posttest cases and 19 doctor process
cases (233 passes, no failures, skips or cancellations), plus typecheck, fresh bundle
and syntax checks. Synthetic ephemeral IPv4 loopback fixtures were permitted;
external connections were denied. Source, tools and dependency input identities
remained unchanged. The initial minimal-snapshot attempt retained 125 partial passes
and a missing-fixture collection failure; retry added only the existing SDK stream
fixture, without changing or filtering tests. This closes the canonical offline CLI
gate; broader J28 native release and live-provider acceptance remain open.


## Personal connector journey source contract (2026-10-14)

Five existing personal connector operations now have independent native declarations:
list available, list mine, begin authorization, OAuth return, and disconnect. The
unchanged eleven-route inventory is shared with the actual router; the other six
operations are not declared by this increment. Runtime, admission, storage projection
and actual host construction/mount sources are bound into producer identity.

Ordinary operations admit the current account/session JWT or database-bound API key
through the existing AdmissionService. Callback authority is issued OAuth state and
never bearer resolution. It accepts optional code or error, rejects missing/both/empty
error combinations using the existing statuses, and consumes valid-shaped issued state
atomically before validating its stored binding. Denial redirects with a fixed failure
hint without exchanging a token. Actual callback success/denial uses302 Location;
Python route metadata and its exact exported publication operation must agree.

Available/begin/disconnect responses have bounded source-owned shapes. Mine preserves
raw row Values and open items; production SQL excludes credentials but the schema does
not invent collaborator row validation. Its operation is explicitly excluded from
bounded response coverage. Redirects contain no invented200 JSON. Observed500 failures
retain JSON and plaintext variants; the default remains an undeclared host/collaborator
gap. Disconnect revokes local credentials and attempts MCP stop, without claiming
provider-side revocation. No comparator or ownership profile is relaxed.

Offline verification passed the actual FastAPI export and exact callback-response
publication regeneration, 153 focused Python checks, the changed native contract unit,
and all 12 production connector service/router tests. The source-bound producer was
freshly compiled; its actual output passed four positive and 15 negative schema and
wrapper checks. Initial producer macro-expansion and test-fixture failures are retained:
the producer now adds its seven connector hashes outside the existing large JSON macro,
and the plaintext500 fixture uses the actual serialized bad-state decoding branch.
Runtime behavior and assertions were not weakened. A separate test-formatting lint
repair preserves the tested Python AST; its original tested bytes and failure remain.

The measured export contains 54 paths and 75 operations; 377 published operations
remain absent, with 101 Knowledge owner operations separately composed. Full native
compatibility remains FAILED at 467 diagnostics, versus the immediate prior465:
five missing connector-path diagnostics disappear, exposing four actual security
differences, two missing native422 declarations and the explicitly unconstrained
callback422 detail-body gap. These are newly visible existing differences, not runtime
policy changes or full CL acceptance. Mine remains outside bounded response coverage.

Live OAuth/provider, Redis/PostgreSQL, whole-Gateway and browser acceptance remain
separate and open. No production credentials, deployment or provider calls were used.


## Self-service API-key native source contract (2026-10-14)

Five existing self-service lifecycle operations now share native route and request
length constants with independent source-owned declarations: create/list, detail,
revoke and delete. Legacy config-key management is outside this slice. Admission
uses the actual account/session JWT or database-bound API-key resolver; exact admin
removes only owner filtering, not tenant scope. Self-service issuance remains limited
to knowledge scopes, and body identity/tier cannot grant authority.

The production response projection strips credential hashes, tenant/internal IDs
and arbitrary storage fields; plaintext is exposed only on successful creation.
The rate_limit field remains arbitrary JSON, timestamps retain production parser
coercion limits, and nullable/missing output defaults remain explicit. Committed
writes may survive a later invalidation/response error, so errors do not authorize
automatic replay. Error/cancellation/host variants remain explicitly incomplete.

Offline verification passed the fresh producer build, the single bounded-document
unit test, and all seven production parser/projection/router fixtures with mocked
identity and service collaborators. The seven-test target had no failures, ignored
or filtered tests. The native commands emitted no compiler warnings. All 130 focused
wrapper/source-binding tests and scoped Python lint passed on the unchanged source.
Actual fresh output passed 17 positive and 33 negative schema checks and two-process
export determinism. These are separate kinds of evidence, not live storage/auth tests.

The measured native export contains 57 paths and 80 operations; 372 published
operations remain absent and 101 Knowledge owner operations remain separately
composed. The unchanged full-native comparator remains FAILED at 472 diagnostics,
versus the prior 467: three missing-path diagnostics disappear, revealing five
actual security differences and three absent native 422 declarations. No SDK
publication, ownership policy or comparator is changed to hide these differences.

The first Python attempt stopped at its disk-growth guard before terminal completion;
it is retained as failed/unverified. After exact generated-fixture cleanup, the retry
used pytest's standard per-passing-test temporary-directory cleanup policy, preserving
failed fixtures and all assertions/source/resource caps. No successful test was counted
from the interrupted attempt. Both generations of compiled-library provenance are
retained: the previous one in a fully stream-verified two-member archive, the new one
by current artifact references that must be protected before any future overwrite.

No real credential was created or used, no network/provider/database call or deployment
ran, and no dependency was added. Fixtures do not execute the actual PostgreSQL store,
Redis revocation or live JWT/database-key admission. Whole Gateway, live storage/cache
revocation and complete CL-01–06 acceptance remain open.


## Python LangGraph Unicode stream continuity

Remote LangGraph streaming now keeps UTF-8 decoder state across HTTP chunks.
Split multibyte text, tool arguments and tool results survive the Python adapter,
Gateway stream and session-history projection. This reuses the existing Python
adapter and standard library; it adds no dependency or execution path. Malformed
bytes retain the prior ignore policy. Existing end/[DONE], ordinary EOF success,
line framing and cancellation ownership semantics remain unchanged.

The roundtrip regression reuses the existing official LangChain-generated message
shapes with synthetic Unicode content and whole/every-byte HTTP bodies in both
messages and updates modes, including handled tool errors. It checks one final
Gateway marker, response closure and exact restored text/tool data. This controlled
fixture does not establish provider, database, native adapter or browser acceptance.

### Python instruction-skill discovery and test workflow (2026-10-15)

The explicit `client.skills` surface connects tenant skill listing, exact detail
read and an explicit instruction-test POST. It preserves server metadata and
failure results, rejects malformed responses and invalid path identities, and
never executes returned instructions or enables a skill. Runtime capabilities
remain in `tools.list_tools`; the legacy skill methods retain their routes and
return contracts. Legacy test dictionaries are finite JSON text, string input is
passed through and omitted input becomes an empty string, matching both Gateway
implementations' required text field. See the Python SDK README for migration.

Focused public-client/HTTPTransport MockTransport checks target actual route,
query, body and single-attempt contracts. These are controlled offline checks;
actual paired Gateway execution, package publication, official Agent SDK worker
restoration and full CL acceptance remain separate open gates.


### Python citation source read (2026-10-15)

The public client.knowledge.get_source method connects returned versioned
citations to the existing Gateway immutable-source GET. It requires safe encoded
dataset/document identifiers, a positive 32-bit source version and a 64-hex hash.
The response must match document/version/hash and SHA256 of full UTF-8 content;
only the five declared public source fields are returned. Unicode content is
hashed without normalization, consistently with Python and native Knowledge.
Legacy unversioned citations remain explicit; no current-document fallback,
restore, generation or other POST is introduced. Current ACL stays server-owned.

Verification uses actual SDK/HTTPTransport and controlled wire responses, with
backend routes bound as source evidence. Native/runtime/database deployment
acceptance is separate; read retries retain existing transport policy.

The source reader uses an opt-in HTTPTransport GET/HEAD byte cap: pre-read length
check, accumulated raw stream limit, identity encoding and compressed-response
refusal. Its fixed 2 MiB JSON cap matches native knowledge.rs; ordinary SDK callers
are unchanged. Oversize validation is not retried. The existing stream cleanup
wrapper preserves cancellation and primary errors. Custom transport prior buffers
and individual transport chunks are not claimed to be OS allocation-bounded.
