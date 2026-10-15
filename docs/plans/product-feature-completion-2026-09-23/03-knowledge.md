# 03｜知识库功能完善

## 1. 用户、现状与目标

知识管理员负责把资料变成可靠可用的知识；普通成员只应看到自己有权使用的内容。当前创建、文档管理、切片、检索、QA、评测、查询洞察、来源、配置及权限页面均有实现；9月22日证实了已有合成资料的混合召回及回答引用，9月7日有旧候选完整文本入库证据。依据见[证据E07、E02、E03](01-current-state.md)。

本模块目标：**任何一份资料都能回答“现在能不能用、用的是哪个版本、失败在哪里、该怎么修复、谁能访问”。** 不把更多检索参数或算法数量作为完善目标。

## 2. 两条完整旅程

**首次入库：** 新建库 → 选择资料来源 → 了解支持格式/限制 → 上传并确认每个文件 → 解析/切分/索引 → 查看原文及切片 → 用真实问题测试 → 在Assistant/Agent中选择该库 → 打开回答引用。

**日常维护：** 发现问题 → 定位文档/版本/失败阶段 → 调整配置或替换资料 → 启动重处理 → 保留当前有效版本 → 验证候选结果 → 切换 → 必要时恢复上一有效版本；撤权和删除单独管理。

## 3. 需求清单

| ID | 优先级/类型 | 用户行为与产品规则 | 可判定验收 |
| --- | --- | --- | --- |
| KNO-01 | P1 验证 | 创建/编辑/筛选/分页/归档库，说明私有/共享范围和默认检索方式；保留已填写草稿 | 普通成员只能看到授权库；翻页和搜索覆盖全目录；失效创建配置在保存前解释；同名库可区分归属 |
| KNO-02 | P1 补齐 | 批量上传呈现逐文件状态、大小/格式限制、重复处理方式及失败重试；来源URL/连接器使用不同前提说明 | 正常文本、PDF、扫描件、表格和损坏文件各有明确支持/失败结果；混合上传部分成功后可仅重试失败项；无OCR时不伪称解析完成 |
| KNO-03 | P0 验证 | 后台处理按真实阶段显示排队、解析、切分、嵌入、索引、可用或失败；已接受任务脱离页面继续运行 | 刷新或服务重启能找回同一任务；已完成文件不重做成副本；未知总量显示不确定进度而非虚构百分比；失败可定位阶段 |
| KNO-04 | P0 验证 | 重解析、重切分、重嵌入分别说明影响；新处理未完成或失败时保留原有效检索版本 | 对已有可检索文档模拟重处理失败，旧资料仍可查询；切换成功后结果只来自声明的有效版本；重复操作不造成多个有效指针 |
| KNO-05 | P0 验证 | 原文、切片、检索、QA、引用、下载使用一致ACL；权限继承和覆盖可解释 | 两租户/两角色查同样关键词不串数据；撤权后旧链接/缓存不得继续读；无权限响应不泄漏私有文档内容；用户知道是权限变化而非文件损坏 |
| KNO-06 | P1 补齐 | 文档详情能从结果回到原文位置与相关切片；显示类型、来源、更新时间、处理配置和版本 | 中文段落、跨页PDF、数字/表格资料的引用能定位相应内容；当前无法定位页码时明确降级，不编造页码/坐标 |
| KNO-07 | P1 验证 | 配置页面区分默认/继承/手动覆盖与当前有效值；修改参数说明是否只影响查询或需重建索引 | 保存后回读有效配置一致；不可用解析器/嵌入模型不能成为“已生效”；待重建与已切换状态明确；离开未保存配置有提示 |
| KNO-08 | P1 补齐 | 召回测试展示向量/关键词/融合/重排来源、耗时、筛选原因和有效配置；分数不称答案置信度 | 相同问题可对照两套已冻结配置；重排没执行时不能显示已重排；零命中、被ACL过滤和检索失败分别表达 |
| KNO-09 | P1 验证 | QA测试与Assistant使用声明一致的知识范围，支持严格资料回答和缺资料时拒答；引用与输出同步 | 有答案、无答案、冲突来源、数字问题、多轮追问分别验收；用户可打开每条引用；切库后不无提示引用已移除资料 |
| KNO-10 | P1 补齐 | 来源管理区分直接上传、在线连接和定期同步；显示最近成功同步、当前问题、增量/删除处理规则 | 模拟来源文档更新/删除/撤权后，系统按声明规则收敛；失效凭据恢复后不重复导入；同步失败不伪装成“0条更新成功” |
| KNO-11 | P1 验证 | 批量操作显示选中数量、影响对象、进度和逐项结果；删除/归档说明可恢复性 | 跨页选择不静默漏项；操作部分失败可继续查看成功项；重复删除可安全处理；删除前列出Agent引用等已知影响，不自动删相关Agent |
| KNO-12 | P1 补齐 | 从召回/QA失败直接创建待审核的评测样本，保留问题、来源版本和期望；查询洞察能定位来源问题 | 样本生成不自动冒充人工审核；从失败查询回到文档/配置/Trace；同一用例修改有版本；知识评测和通用Eval互相引用而非重复维护标签 |

## 4. 必需状态与操作

| 状态 | 用户应看到 | 下一步 |
| --- | --- | --- |
| 上传中/部分失败 | 每个文件的接受情况与限制 | 重试失败项或移除后继续 |
| 已接受/排队 | 已保存的任务身份和排队事实 | 离开后回来查看；可取消则提供真实取消入口 |
| 处理失败 | 文档、失败阶段、原有效版本是否仍可用 | 修改前提后重试，不默认清空索引 |
| 已完成但未生效 | 候选与当前版本，以及切换条件 | 查看测试结果再切换 |
| 无召回 | 查询成功但没找到授权资料 | 改写问题、检查文档/配置 |
| 服务降级 | 哪个阶段没执行，结果仍能证明什么 | 使用可用结果或稍后重试 |
| 权限失效 | 当前访问不可用；安全的联系路径 | 申请权限或移除失效引用 |

## 5. 首轮验收样本建议

固定一组合成资料，包含中文文本、英文文本、跨页PDF、扫描件、表格、重复文件和损坏文件；扫描/OCR或某格式确不支持时，验收的是正确拒绝与明确说明，不强行把新增解析器纳入首轮。

检索/问答至少覆盖：精确事实、同义表达、数字、跨段问题、无答案、相互矛盾、删除后查询、撤权后查询。指标至少包含处理成功/失败原因、检索命中、引用正确性、响应时间和成本；本轮不预设未经基线验证的质量提升百分比。

有201个库或超过单页切片数量时，目录/批量范围必须可证明完整；可选择分页或明确限制，但不能静默截断。实际容量目标由实施前基线评估固定，不能在验收失败后临时缩小输入。

## 6. 依赖与范围

依赖 TL-05 的连接状态、US-02 的角色作用域、OB-01 的缺失数据表达。KNO-12在R2只需要EV-01的最小切片：向现有权威样本库保存待审核样本及稳定版本引用；有现成能力就验证复用，不另建一份样本存储。EV-02的完整运行配置、EV-04对比与发布联动留在R3，不作为知识入库/检索维护的前置条件。

知识版本、撤权和删除对回答/产物/分享的影响统一按[AR-06](02-assistant-tools-deliverables.md)处理。R2必须联合验证KNO-05与AR-06；R1仅使用现有授权知识问答切片，不声称知识维护全部完成。维护流程优先使用现有任务、版本、ACL和索引能力。

不在本轮默认新增 GraphRAG、HyDE、多模态检索或向量库迁移；若基础用户问题在现有实现上即可解决，先完成基础流程。新算法作为独立可选需求，必须说明它解决的具体问题与对照验收。

## 7. 实施记录（2026-09-27，R2 进行中）

已落地一批可独立验收的创建/上传/失败提示修正：批量同名文件按提交位置核对回执，只有明确安全的失败才允许直接重传；创建请求采用稳定 ID，丢失响应后刷新先查原请求，不自动重建库或重传来源；损坏文件的终态详情显示最后记录阶段、下一步和文档 ID，文档 API/SSE 不外露原始 worker 错误。真实 Docker 的私有库创建、文本/PDF/损坏 PDF 混合上传以及刷新、重开后的状态和切片回看已验；丢响应重复提交只做了受控注入。短文本重提取的正文与切片已按同一发布事务修正。

后续 R2-A 安全增量修正标准文本的 `indexing` 崩溃续接和版本恢复：固定本次处理配置，纯文本 lexical_v1 的候选正文、切片、版本指针与完成终态在 PG 发布事务中一起显露，恢复来源不会被原上传覆盖。受控合成文档在本地 Docker/内置浏览器完成两次真实页面版本恢复，其中一次先暂停 Knowledge Worker、排队并刷新、再启动 Worker；同一文档恢复完成后切片与目标正文一致，刷新不产生新执行。该停启验收并非执行中硬崩溃。最终候选又在浏览器恢复 #4 至 #8，刷新后切片与执行数不变。独立 review 指出的特殊扫描/层级/图片回执路径按切片、向量点和持久图片意图拒绝不安全重放；API producer 与 Worker 共用准入守卫。文档级来源撤权和 active-only 原文、viewer 列表已补定向自动化，历史全文稳定版本仍未实现。

本记录**不表示 R2 完成**。层级/扫描图片的多 collection 候选发布、active BM25 v2 与非文本发布后崩溃窗口、执行中硬崩溃、知识来源的稳定历史全文/双角色撤权、来源同步、评测待审核样本及 J09～J13/J30 全矩阵仍待完成。实际测试层级、独立 review 和缺口见 [R2 执行记录](../../../reports/product/r2-knowledge-progress-2026-09-27.md) 与 [执行状态](../../../deploy/runbooks/r2-knowledge-lifecycle-2026-09-27/loop-state.json)。

**2026-09-28 增量：** 层级/VisionPDF 候选发布、同类特殊版本的可验证来源恢复、负 revision owner/执行清单对账及旧点/图片对象清理已接入。真实 PostgreSQL 提交后受控中断恢复、真实 Qdrant 遗留点清理分别通过；全量 KB 单测 1976 通过/1 跳过，本地 Docker 与内置浏览器核对了既有文本版本和失败回看。真实特殊 provider 的成功、失败和物理重启端到端、特殊版本恢复到更早普通文本，以及 J09～J13/J30 全矩阵未通过，**R2-A/R2 不标完成**。证据和未验证项见 [本次收口记录](../../../reports/product/r2-special-publication-closure-2026-09-28.md)。

**2026-09-28 后续局部验收：** 真实 DashScope 层级文档完成两代发布和同类旧版恢复；同一任务的 PostgreSQL 活动切片与 Qdrant base/section 点 ID 对齐，内置浏览器刷新后仍显示完成。Worker 执行 UUID 和发布状态缺陷已修；特殊版本恢复到历史普通文本的候选路径补齐并经自动化/故障注入，但当前公开扫描上传关闭且真实跨类用户路径未验。来源撤权的 SSE/历史/Quiz 保护补强，双租户验收未执行。独立 review 的普通文本层级误判、操作员禁用位置不一致和无回执旧版恢复风险已最小修正并复核。**完整 R2 仍进行中**，详见 [后续收口证据](../../../reports/product/r2-special-completion-2026-09-28.md)。

**目录、配置与批量操作前端增量（2026-09-28）：** 目录把服务端游标遍历得到的完整授权库集合按 24 个/页展示，并标出创建者、租户及保存的检索方式；创建向导在当前标签页保留文字、URL 和参数草稿，本地文件刷新后须重选。配置页区分系统默认继承与库级覆盖，回读保存值、解析器可用性，并解释查询参数和既有切片的不同生效范围；未保存编辑按用户和库隔离保留，页面顶部返回、Tab 切换和浏览器刷新给出提示。批量重嵌/删除显示跨页选择范围、每批进度、操作 ID、逐项已排队/已删除/跳过/失败/未确认结果，部分失败不清空待处理选择。拿到操作 ID 后轮询超时只查询同一操作，并暂停后续分批提交；无 ID 的未知结果需人工核对后才能解除暂停。若服务端已创建操作但 POST 响应始终未到达，前端无法自动证明未受理，人工判断仍有重复提交风险。以上为前端与定向自动化增量，尚未完成 J09/J10/J13 的真实浏览器矩阵。库级归档、Agent 引用影响预检及服务端可核验的“待重建/已切换”配置状态仍需后端合同；前端不会把永久删除称作归档或把保存参数称作既有文档已重建。

**Agent 引用预检前端接入（2026-09-28）：** owner 批量删除文档前读取 Gateway 的库级 Agent 影响快照；只有返回库 ID 匹配且可见列表、隐藏计数和草稿/活动发布/历史版本计数均完整时才能确认删除。失败或无权限不能显示成零引用。确认框说明快照覆盖整个库、引用可能变化，且不会自动删除 Agent。此项仅为前端合同和定向单测；Gateway 接口及 J13 真实影响/删除矩阵以最终联测为准。

**来源状态前端接入（2026-09-28）：** 来源页将文件上传和 URL 导入明确标作一次性操作，按接口的 `connector_status` 单独展示定期连接器的可用、未配置和故障状态；绑定展示当前问题与最近成功同步时间。来源读取失败时数量为未知，绑定有问题时不把 0 页写成成功同步；本页不展示无法执行的同步按钮。仅完成前端合同与定向单测，未据此宣称连接器已注册或 J12 的真实更新/删除/凭据恢复通过。

**R2 集成候选（2026-09-28）：** KNO-01 的 owner 归档/恢复、默认目录与检索隐藏归档库，KNO-11 Agent 引用影响预检，以及 KNO-12/EV-01 向既有 Eval 库保存待审核失败样本已接通。Knowledge 的 route-submitted 规则/执行/排队使用同一事务与文档租约，特殊 `preparing` 崩溃后可沿用原 execution；本地 Docker 中真实 DashScope 层级文档在 Worker 物理 KILL 后同 execution 完成，PG/Qdrant 28 个切片/点身份精确一致。schema authority 005/006 已先于全服务热更新应用。独立 review 的归档竞态、内部 Quiz 跨账号缓存/令牌和来源旧错误误报已修复并定向复核。具体自动化、真实 API、受控故障注入和未验证项见 [集成记录](../../../reports/product/r2-closure-progress-2026-09-28.md)。**内置浏览器当前受 Mac 锁定阻断，J09～J13/J30 的页面与双租户完整矩阵未通过，R2 不标完成。**


## Native ingestion parsing and diagnostics (2026-10-11)

The native text/Markdown/HTML decoder recognizes UTF-8 and UTF-16 LE/BE byte-order
marks. It strips the leading marker and preserves multilingual text and surrogate
pairs. Explicitly marked malformed Unicode and UTF-32 fail rather than silently
publishing a legacy-decoded replacement. Unmarked UTF-8, GBK and legacy fallback
behavior is unchanged. The existing 16 MiB source and extracted-text budgets still
apply. UTF-16 without a BOM is not auto-detected; export it with a BOM or as UTF-8.

Document list/detail and document-shaped mutation responses add
`processing_diagnostics`: `last_recorded_stage` (nullable), `next_action`,
`retry_blocked_reason` (nullable), and `retry_requires_editor`. These are advisory
observations from the document's durable state, not authorization or proof of
retrieval availability. An indexing timestamp does not distinguish embedding from
vector publication. No timestamp means an unknown last stage. Errors may point to
retrying the existing document, replacing a failed upload, inspecting source sync,
or resolving an in-progress lifecycle/publication operation. Existing editor,
dataset, lease and enqueue checks remain authoritative on every retry. Unknown
states do not advertise retry. Document and execution error fields use the same
fixed safe message as SSE; raw worker/provider errors are not returned there.

This increment does not deploy an embedding model, enable optional reranking,
prove historical-version availability, or mark KNO-02/KNO-03/KNO-04 fully accepted.
Offline verification artifacts are kept outside the repository; unified native
regression and live ingestion/permission checks remain release gates.

### Selected-document chunk refresh after processing (2026-10-12)

The open chunk panel now keys its read cache by the selected dataset/document
and the observed document processing phase plus `completed_at`. This timestamp
is a refresh observation, not a verified content revision or historical source
identity. Existing document-list SSE updates and active-row polling cause a new
read when a retry/reprocess reaches a terminal state, including a fast
completed-to-completed rerun whose intermediate stages were not observed.
Active stages share one cache token; chunks are not polled on every progress
update. Existing segment-mutation invalidation prefixes remain unchanged.

A late read from the earlier observation cannot replace the new panel result.
Changing document, dataset or terminal observation does not reuse old chunk
placeholders; loading and failed reads are distinguished from confirmed empty
results. Read cancellation is passed to the existing Gateway client, and a
refresh only repeats the read, never the ingestion mutation. The segment page
resets when the observed completion timestamp changes.

The selected document also has an account-scoped, cancellable detail read.
It polls every two seconds only while that document is in flight, including a
linked source outside an otherwise idle list page, and stops at a terminal
state or read error. A detail error never falls back to a stale list row; manual
retry repeats only that read. The existing capped one-time conflict retry handles
transient generation-switch 409 reads, while 403/404 are not retried. Lifecycle
refreshes explicitly cancel a pending first detail GET before rereading, and
pipeline receipts refresh the submitted document even if selection has changed.
Source selection resets on URL/dataset/account
changes, while dismissal remains closed during unrelated list refreshes.
Node tests exercise the production helpers with the actual query cache and
controlled read ticks. Live browser,
Gateway/Knowledge ingestion, cancellation, provider and database-role acceptance
remain separate gates; the known worker progress-trigger permission blocker is
unchanged.


### Retrieval and QA source opening (2026-10-12)

Each Knowledge retrieval hit and QA context segment now offers the same read-only
source dialog used by Assistant. The action carries the exact document ID,
originating dataset, saved excerpt and top-level source version/hash returned
with that result. QA retains the request's dataset even if the surrounding route
changes. Missing, partial or conflicting references, including metadata-only
versions, show unavailable rather than resolving the latest document. No source
URL is used as a fallback, and no page number or file location is invented.

The existing historical source endpoint remains authoritative for current
dataset/document permissions and immutable version/hash checks. The dialog
validates the returned document, version and text digest before showing source
text or the saved excerpt. Its view is now scoped to the current account, session
and locally observed permissions; changing that scope immediately hides the old
view and cancels its historical GET. Late responses cannot restore it. Failed
rechecks hide both excerpt and document; reopening or polling only repeats the
source read and does not rerun retrieval, generation or ingestion. Existing
Assistant legacy-event behavior remains an access-checked excerpt with an
explicitly unverified historical-full-text notice.

Offline tests execute the production mapping, source action, QA/retrieval
components and dialog with controlled React/auth/HTTP boundaries. They cover
missing identities, request-dataset retention, version mismatch, stale reads,
account/session/permission changes, logout and revoked/deleted-source responses.
These tests do not establish browser acceptance, live cross-tenant permissions,
page/chunk coordinates, original-file downloads or the full KNO-06/KNO-08/KNO-09
and AR-06 acceptance matrix.

### Configuration write/readback recovery (2026-10-12)

The chunking, retrieval and direct empty-dataset embedding editors retain their
drafts and navigation warning until a scoped readback confirms the exact submitted
values. A successful mutation followed by a failed read is shown as unconfirmed,
not as a rejected save. Mutation errors also reconcile by GET because a service
error can follow a committed update or failed compensation. Recovery never replays
a mutation automatically; refreshed interrupted drafts retain the recovery marker
when browser session storage is available. Storage denial/quota errors preserve
in-memory editing, recovery and the navigation warning, but refresh recovery
cannot be guaranteed without storage; no persistence framework is introduced.
Known mutation acceptance is retained separately from an unknown write outcome.
For an unknown write, even a matching GET proves only the values observed by that
read, not completion or ordering of the earlier request. The operator explicitly
chooses those observed saved values before the recovery marker can be cleared;
this is not a revision/CAS or exactly-once guarantee.

Readback checks the dataset identity and submitted section, preserving nested
retrieval PATCH and whole-object chunking replacement semantics. A mismatch stays
visible for review, with a read-only retry or explicit draft discard in favor of
the observed saved values. Later editor changes remain unsaved after an earlier
save is confirmed. Account/session, dataset-navigation and operation fencing
prevent late responses from clearing a newer editor. Migration refreshes do not
discard an active draft or unresolved save. No authorization, parser availability,
index version, lexical activation, or embedding migration rule changes. Offline
production-route/serialized-Web tests are bounded evidence; live authenticated
browser, database/provider and complete KNO-07 acceptance remain unverified.

### Single-document processing request uncertainty (2026-10-12)

Pipeline submissions retain an account/dataset/document-scoped tab receipt before
POST dispatch. An interrupted or ambiguous response pauses further processing
actions for that document across refresh. Status rechecks only read the original
document; an old or new terminal status is not proof that the original request
was accepted or completed without its execution identity. No mutation is replayed.
An explicit acknowledgement after reviewing document/execution history unlocks
controls with a duplicate-processing warning, but does not submit a request.
Known request rejection clears the guard. Confirmed acceptance preserves a returned
execution ID and stays guarded if the subsequent list refresh fails; a successful
refresh releases it. A local attempt token fences late responses and is not a
new backend idempotency or execution identity. Batch re-embedding cannot bypass
an unresolved single-document receipt for an included document.
Storage failure blocks dispatch rather than losing the uncertainty guard.

This is controlled production-module verification, not browser, real worker,
provider, database, auth/tenant, lexical activation, or version-publication acceptance.

### Upload confirmation dispatch ownership (2026-10-12)

The upload confirmation button disables while its request is running, and the
controller synchronously guards the full configuration-update/upload attempt.
Repeated callbacks before or after a render do not submit the same selection
again. Files and processing options are captured by the original attempt;
files added later remain selected for an explicit later submission. Only the
current owner can settle the selection, close the dialog or clear its busy state.
Account/dataset navigation retires an old owner; a late configuration response
does not begin a retired upload, and a late upload response does not settle a
new owner. This is a mounted-controller dispatch guard, not backend idempotency
or cross-tab deduplication. Existing uncertain network outcomes remain unsafe
for direct source re-upload unless the existing rejection protocol proves it
safe. No upload mutation is replayed automatically.

Verification uses production dialog/controller/API modules with controlled I/O;
real browser, storage, provider, worker and duplicate-document admission remain
unverified.

### Assistant source target completeness (2026-10-12)

Assistant context aggregation preserves distinct document/segment identities and
saved source version/hash pairs, including metadata-only legacy identities.
Equal excerpt text is not sufficient to merge two source targets. Structured
identity keys also avoid collisions between missing fields or embedded delimiters;
conflicting top-level/metadata provenance remains available to the existing
citation validator rather than being silently replaced by another hit.
Exact repeated target/excerpt observations still collapse across search events.

The expanded Assistant source panel initially mounts three hits per dataset.
A keyboard-accessible expand button reveals twenty more per click, so every
retained hit is reachable without eagerly mounting the accumulated history.
Reveal counts are scoped to the mounted component and dataset: reordered or
replaced contexts retain their count; a fresh component starts at three.
Entrance animation delay is capped at
100 milliseconds. The scrolling height bounds layout only: aggregation and
progressively revealed DOM remain proportional to retained/revealed hits, with
no hard total memory cap. The previous first-three preview plus inert remainder count
could leave a cited fourth or later source impossible to open. Every source
uses the existing immutable-source/access-checked dialog; no URL, page number,
new version, or current-document fallback is invented. This applies equally to
saved contexts rendered after history reload. Controlled production function and
component tests verify target retention and opening; live history transport,
authenticated browser, provider and full KNO-06/KNO-09 acceptance remain unverified.

### Batch submission interruption recovery (2026-10-12)

Batch re-embedding and deletion now persist an account/dataset-scoped tab receipt
before dispatching each chunk, including a chunk whose POST response never arrives.
Refreshing or remounting that tab retains the uncertainty guard instead of silently
allowing another batch. Unavailable or malformed receipt storage blocks dispatch.
A local attempt token and view generation fence retired responses, receipt writes
and later chunk submissions across account/dataset navigation and remounts.
An unresolved re-embedding batch also guards processing actions on its affected
document rows; unrelated documents and delete/enable/archive actions are unchanged.

Known operation IDs can still be rechecked through the same read-only status API.
Partial receipts retain per-document outcomes and failed selections. A missing
operation ID still requires explicit manual reconciliation with a duplicate-work
warning; acknowledgement unlocks controls and does not submit anything. The token
is not backend idempotency, and session storage is not cross-tab or closed-tab
recovery. Persistence failure after acceptance conservatively retains the earlier
unknown receipt and reports the storage issue, rather than claiming reconciliation.

Verification uses the production DocumentsTab, batch helpers and API modules with
controlled hooks, storage and HTTP outcomes. Real browser, server admission, worker,
database, provider and the full J13 matrix remain unverified.

### Read-only connector status recovery (2026-10-12)

The Sources tab exposes a source-status refresh for successful reads that report
an unavailable, unconfigured or unknown connector, as well as known binding
problems and previously successful snapshots. Previously only a failed HTTP
read exposed Retry, leaving a successful response with an unavailable connector
without a local recheck after an operator repaired the underlying service.
Refresh uses the existing dataset-sources GET and is disabled during its fetch;
repeated callbacks share an in-flight fetch rather than cancelling/restarting it;
a failed recheck still hides old binding results and reports unknown counts.
It does not configure credentials, start sync, replay imports, or imply that
the generic connector catalog controls the Knowledge Confluence service.
No new settings link or permission scope is introduced.

Controlled production-component, hook and API tests, including an actual query
observer repeated-callback case, cover response-level failure,
unknown/unconfigured states, recovered reads and failed rechecks. Live browser,
authentication, provider/credential recovery and the full J12 remain unverified.

### Retained Confluence discovery recovery (2026-10-12)

The retained Python Confluence sync coordinator now treats failed root or nested
child discovery as an incomplete observation: it records the existing failed/error
terminal states before importing or deleting any documents, and preserves the
previous successful-sync timestamp. A later caller-triggered sync can re-read the
same binding normally; there is no automatic mutation replay. Complete discovery,
including a confirmed empty subtree, retains the existing removal reconciliation.

Child pagination follows the advertised next cursor even for short or empty pages.
Missing/malformed child results and missing/repeated pagination cursors fail rather
than becoming an empty or complete tree. This prevents an incomplete discovery
snapshot from serving as evidence that a previously imported page was removed.
The existing deletion authorization and source-generation fences are unchanged.

Offline tests exercise the production client/coordinator with controlled HTTP and
repository outcomes, including late-root/nested/pagination failure and a subsequent
successful recovery. They do not prove database mutation, real provider pagination,
process recovery or live J12 acceptance. No current public configuration/sync route
or startup registration for this retained service was found; the read-only Sources
route only inspects an optional app-state service. No new public route, registration,
credential flow or scheduler is added here. Existing record-list page-size limits,
per-page processing failures and incremental-sync admission/status behavior remain
separate limitations, so this is not a completed end-to-end connector product flow.

### Complete existing-record snapshots for retained sync (2026-10-12)

Full Confluence synchronization now explicitly reads the complete existing-record
set for the binding, so whitelist processing and root-based removal reconciliation
include records beyond the repository's default 100-row UI page. Incremental
removal checks use the same complete read. The repository keeps its default limit
and offset behavior for paginated callers; internal reconciliation passes `None`
to the existing PostgreSQL LIMIT parameter, requesting one uncapped statement
snapshot instead of iterating offsets while records may change.

This resolves the preceding existing-record page-size limitation for those two
sync reads only. The set still occupies memory proportional to the binding size;
no bounded-memory, throughput or latency guarantee is claimed. Binding scope,
complete-discovery requirements, deletion fences, explicit-404 incremental removal,
and per-page/terminal status semantics are unchanged. Stats and other maintenance
callers retain their current paging behavior and are not broadened here.

Offline tests use the production repository query assembly and sync coordinator
with a controlled pool and provider outcomes: 0/99/100/101/201-record whitelists,
root-mode removal beyond row 100, incremental tail checks, non-404 preservation,
read failure and ordinary page/offset compatibility. This is not actual PostgreSQL,
provider or public-user-journey acceptance. The retained service still lacks current
public configuration/sync routing and startup registration; full J12 remains open.


### Confluence synchronization terminal accuracy (2026-10-12)

Retained full and incremental synchronization now finish as failed when any page
processing, removal, or incremental source-existence check fails. The last
successful synchronization timestamp is preserved; completed page progress and
per-page errors remain in the failed task result rather than becoming a false
zero-update success. Already applied page changes are not rolled back, and a
later explicitly requested sync retains the existing source-generation retry
behavior. A subsequent genuinely successful run clears the previous issue.

An incremental run without a previous success hands the same task to full sync
and returns that actual outcome; the handoff no longer prewrites completed.
This does not add scheduling, public activation, process recovery, current-actor
execution authorization, or shared-binding document-removal ownership. Offline
controlled coordinator tests are separate from live provider/database/J12 gates.

Binding and task terminal persistence remains two separate writes. A database
failure after the success-watermark write can still leave a failed task with an
advanced watermark; this increment does not establish atomic terminal persistence.


### Explicit saved-binding manual sync workflow (2026-10-12)

Python Knowledge API/all startup passively registers the manual synchronization
owner with the existing durable document producer and storage. It reads no saved
credentials or binding rows and starts no sync or polling scheduler during
registration. Worker-only role exposes no manual API. The optional
KNOWLEDGE_CONFLUENCE__MANUAL_SYNC_ENABLED gate disables registration; its default
is enabled. No connection creation, credential editing, or automatic binding
creation sync is exposed by this workflow.

The signed Gateway-to-Knowledge management path adds POST
/knowledge/{dataset_id}/sources/confluence/{binding_id}/sync with a caller-retained
request_id UUID, and GET of that path followed by /{request_id}. One binding-row
transaction admits the task and binding state. Repeating the same request identity
returns its existing receipt; another active task blocks admission even if an
intermediate binding status says completed. GET reads only the named task and
returns sanitized progress, terminal facts and partial-success/error counters.
Sources adds per-binding manual_sync_available and current_sync_task, so the
existing Sources screen can submit an explicit full reconciliation and recover
its exact task across reads. A typed pre-admission NOT_ACCEPTED response can
release a local submission guard; a GET404 or service-unavailable GET cannot prove
that an earlier POST was never accepted.

Submission requires the same current user to own both the binding and saved
connection, remain in the same tenant, and hold current dataset editor access.
There is no cross-tenant/admin ownership shortcut. Provider/storage operations
and source-generation write boundaries re-resolve the current actor through
the existing signed Gateway actor endpoint and recheck scope/ACL. Publication
reuses its held database connection and canonical grant policy, avoiding nested
pool acquisition. Readback requires current dataset visibility and binding
ownership; an inactive connection or newly unwritable index does not by itself
hide the failed task. Late task/binding terminal writes remain fenced to their
admitted identity. Graceful close drains admission and joins/cancels owned work,
including a task cancelled before its first coroutine dispatch.

Source reconciliation now removes exactly one observed binding/page reference
inside the existing exclusive dataset deletion lease. Another reference to the
same Confluence document preserves the document and its assets. The last-reference
path requires canonical or explicit legacy source identity, then retains the
same lease through existing vector/assets cleanup. Exact page removal, document
delete and fence-clear commit together, preserving retry ownership on failure.
Reference-creating repository entrances take the matching shared lifecycle fence;
legacy arbitrary page-row IDs are preserved through the natural binding/page key.
A stale/rebound observation cannot authorize deletion of a newly attached source.

This is a retained Python/hybrid source workflow, not activation of native Rust
connector synchronization. Accepted task identities and status survive readback,
but execution is process-owned: hard-crash pending tasks remain blocked for
operator reconciliation, with no blind stale-owner takeover or automatic replay.
A failed sync may already have applied individual page changes; explicit retry
uses the existing source-generation identity and does not roll them back.
Binding/task terminal writes retain the previously documented non-atomic
persistence boundary. Real browser, service restart, actual database roles/locks,
provider credentials and complete J12 acceptance remain separate release gates.

Offline connected tests execute production ASGI route, admission owner, sync
coordinator, shared-source DocumentService and exact receipt serialization with
controlled boundaries. Additional tests cover changed-source publication with
current grants on the held connection, revocation before publish, scope changes,
shared/last-reference deletion and retry, duplicate/active-task admission,
shutdown, and passive startup construction. Shared generated receipt/error JSON
fixtures are consumed by the Web Sources contract tests.

### Existing-binding manual sync controls (2026-10-12)

The Sources tab consumes the server's per-binding `manual_sync_available` signal;
older/unavailable/read-only responses do not expose a write action. An explicit
click starts full reconciliation, with a warning about source-deleted documents
and retention while another binding still references a document. Saved bindings
are not described as proof that a scheduler is enabled. Connector setup and
credential creation remain outside this view.

Before POST, the UI saves a newly allocated request UUID in account/dataset/binding
scoped session storage. It follows the exact returned task identity through the
existing Gateway API. Accepted pending/processing tasks use read-only polling;
completed/failed states stop polling. Progress counts attempted pages separately
from applied updates. Failed runs with applied updates are shown as partial
execution, never as a successful zero-update run. A terminal failure allows only
an explicit new sync. The backend remains the permission and admission authority.

Lost/invalid POST receipts and restored requests require an explicit exact-task
GET. A GET404 remains inconclusive and does not enable a new submission. Read
errors hide retained task details and stop polling. Only the service's explicit
`CONFLUENCE_SYNC_NOT_ACCEPTED` pre-admission rejection releases the uncertain guard;
HTTP status alone is insufficient. No mount, navigation, refresh, retry of a GET,
or focus/reconnect event submits a sync. Storage failure blocks submission.
A stale mounted control cannot clear or replace another producer’s newer stored
request; changed storage ownership blocks further writes from that control.
Session recovery is tab-scoped, not a cross-device transaction ledger; the server
may additionally expose its latest safe receipt for read-only recovery.

Offline production-component/API tests and a real TanStack query observer cover
explicit dispatch, duplicate-click guards, exact identity, uncertainty/readback,
access/read errors, failed/partial recovery, capability gating and storage failure.
These are controlled source-contract checks, not browser, provider, worker restart,
live database, scheduler, or full J12 acceptance.


### Manual-sync additive API contract

Public Python management requests use the existing signed Gateway Knowledge
proxy. The downstream routes are:

- `POST /api/v1/knowledge/{dataset_id}/sources/confluence/{binding_id}/sync`,
  JSON body `{ "request_id": "<UUID>" }`, response `202` with the exact receipt.
  Extra request fields are rejected. Reusing the same admitted UUID reads the
  same task rather than dispatching it again.
- `GET /api/v1/knowledge/{dataset_id}/sources/confluence/{binding_id}/sync/{request_id}`,
  response `200` with that exact receipt. This does not submit or retry work.

A receipt contains `task_id` (the admitted UUID), `binding_id`, `status`
(`pending`, `processing`, `completed`, `failed`, or retained `cancelled`),
nullable `progress`, `total_items`, `processed_items`, `started_at` and
`completed_at`, nullable `synced_items`, `failed_items`, `error_count`, and
nullable sanitized `error_summary`. Attempted/processed count is not a success
count. Failure readback does not expose provider errors, source contents or
credentials. A failed receipt may represent partial page progress.

The existing Sources Confluence-binding projection adds `manual_sync_available` and
`current_sync_task` (a nullable receipt). Eligibility reflects current binding/
connection ownership, active saved connection and dataset editor access; the
server still revalidates authority during execution. These are additive fields;
existing Sources readers can ignore them.

A definitive pre-admission POST rejection uses an HTTP error body with
`detail.code = "CONFLUENCE_SYNC_NOT_ACCEPTED"`, `detail.accepted = false`, and a
sanitized `detail.message`. Mapped permission/validation/conflict errors and an
unavailable manual service use this shape. Other errors, disconnects and
unrecognized responses are ambiguous: retain the request UUID and read back
only. A GET error, including `404` or a `503` carrying the same marker, never
proves that an earlier POST was not admitted. Only a terminal receipt or a
recognized definitive POST rejection permits the UI to release the uncertainty
guard. Explicit retry after a terminal failure uses a new UUID.

`sdk/openapi.json` is the actual Gateway application snapshot; its published
Knowledge paths are wildcard proxy operations (`/api/v1/knowledge/{path}` and
`/v1/knowledge/{path}`), not a merged downstream Knowledge schema. Do not insert
these downstream paths by hand. The actual Knowledge router can independently
produce its two-route OpenAPI schema in-process without lifespan startup. The
existing in-process Gateway contract gate remains the required snapshot check;
missing local import dependencies are gate failures, not successful comparisons.


Supported target: the default Python Gateway + Python Knowledge API/hybrid
profile. `src/api/v1/knowledge.py::proxy_knowledge` accepts the GET/POST suffixes
through its existing wildcard, enforces read/write scope and forwards through
`proxy_to_kb_service`; the Knowledge API/all startup mounts these handlers.
The full-native Knowledge application has no manual-sync coordinator/handlers
and constructs its ingest service with `confluence_sources_enabled = false`.
These Python tests do not establish native-profile functionality.

The explicit [native Gateway + retained Python Knowledge profile](../../design/knowledge-native-python-hybrid.md)
also supports this workflow at the source/controlled-contract level. Native
Gateway's existing signed wildcard forwards all Knowledge traffic to the Python
owner selected by its explicit `environment.KB_SERVICE_URL`; Python's
`GATEWAY_URL` points back to native Gateway for current-actor verification. The
profile documents both signing directions, matching keyrings, Sources capability
truth, process roles, existing-binding prerequisites and release gates. Production
request-construction/ASGI interoperability checks do not activate the profile or
establish live acceptance. Native Knowledge as origin still does not implement
manual sync; no full-native Confluence capability is added.

### Existing saved connection to manual source binding (2026-10-13)

KNO-10 / J12 now has a bounded setup path before the previously delivered manual
sync: Sources → choose an existing owned active Knowledge connection → enter an
exact Confluence space key → verify and display that space and its homepage →
explicitly save the manual binding → use the existing explicit sync control.
The displayed scope is the verified homepage and its descendants through depth
10. It is not every page in the space; disconnected pages, images, attachments
and comments are excluded. A space without a verifiable current homepage cannot
be bound by this view. No mount, preview, save, receipt recovery or page navigation
starts synchronization. The saved binding is manual with automatic polling disabled.

The four additive signed Knowledge management endpoints are:

- GET `/knowledge/{dataset}/sources/confluence/connections?offset=0`: 50 owned,
  active same-tenant connections, safe ID/name fields only, explicit next offset.
  Selection resets between pages; this is a paginated observation, not a frozen
  catalogue snapshot while connections change.
- GET `/knowledge/{dataset}/sources/confluence/connections/{connection}/space?space_key=KEY`:
  exact provider-backed space/homepage read with current actor, dataset editor,
  connection owner and tenant checks. No credentials are returned.
- POST `/knowledge/{dataset}/sources/confluence/bindings`: caller-retained UUID
  `request_id`, selected `connection_id`, `space_key`, `space_id` and
  `root_page_id`. The UUID becomes `binding_id`; normal response is 201.
- GET `/knowledge/{dataset}/sources/confluence/bindings/{request_id}`: exact
  binding receipt under current dataset visibility and binding ownership.
  An inactive connection does not hide the existing receipt.

These paths receive the normal `/api/v1` router prefix downstream and use the
existing Gateway wildcard proxy; no Gateway route or native Knowledge owner is
added. Sources exposes the optional `source_binding_available` capability hint.
Admission always revalidates current authority and rechecks the verified provider
space/root identity. A short connection-row transaction uses the held connection
for canonical dataset grants and INSERT with `ON CONFLICT DO NOTHING` against
the existing binding-ID and connection/space/dataset constraints. A repeated
matching UUID returns the same binding without resetting its sync state. A
changed payload or a different UUID for the same natural binding is rejected;
no new schema, background task or automatic sync is introduced.

Web saves the non-credential request identity in account/dataset-scoped session
storage before submission. Missing/invalid responses retain uncertainty and allow
only exact readback. GET404 is not proof that POST failed. The explicit
`CONFLUENCE_BINDING_NOT_ACCEPTED` / `accepted:false` marker is emitted only for
checks before INSERT or errors in its rolled-back transaction; unknown database
commit/transport errors remain ambiguous. Stale controls cannot overwrite or
clear another producer's stored request. Navigation, changed selections and lost
capability invalidate pending discovery; repeated save clicks are fenced.

Prerequisite: an active, owned Knowledge Confluence connection must already be
provisioned. Knowledge's existing stored Basic-auth connection is distinct from
Gateway/Assistant OAuth. This increment neither creates nor edits credentials,
nor presents that earlier plaintext credential persistence as a new approved
credential onboarding flow. It does not enable scheduling or native canonical
registration. Existing document source publication and shared-source retention
remain authoritative during the explicit subsequent sync.

Controlled production ASGI/owner/coordinator/source-publication tests cover
binding-to-first-text-source queueing, duplicate identities, current authority,
provider mismatch, exact receipt recovery and no automatic sync. Web production
component/API tests consume the backend-checked shared receipt fixture. These
are source-level tests with controlled provider, database and queue boundaries;
real browser, database roles/locks/concurrency, saved provider credentials,
embedding/retrieval and restart/J12 acceptance are still required. Queued source
text is not evidence that downstream parsing/indexing has completed.

### Secure Cloud connection onboarding (2026-10-13)

The Sources setup now starts with an explicit **Atlassian Cloud only** connection
form. It accepts a single-label `site.atlassian.net` hostname (no URL, port,
userinfo, path or alternate origin), account email and API token. This is the
existing site-host Basic-auth integration, not Gateway/Assistant OAuth, scoped
API-gateway token onboarding or a new self-hosted connector. Existing self-hosted
and plaintext provisioned connections are not modified.

POST `/knowledge/{dataset}/sources/confluence/connections` verifies and creates
an immutable owner-global connection with caller UUID `request_id`, `name`,
`domain`, `email`, `api_token`. GET at the same path plus `/{request_id}` returns
only `connection_id`, `name`, `domain`, `status`, `sync_mode`. Create requires a
fresh authenticated actor and dataset editor rights, rechecks after provider I/O
and checks canonical grants again in the short dataset-row transaction. Receipt
read requires fresh dataset visibility and exact connection ownership/tenant.
The dataset is an authorization context, not the connection's immutable intent:
the same owner may reuse the connection in another accessible dataset. Repeated
matching UUID/name/domain/credentials returns the same connection; changed intent
or ownership conflicts. Concurrent INSERT uses `ON CONFLICT DO NOTHING`, never
upsert or overwrite. No connection creation starts binding, scheduling or sync.

The fixed read-only verification uses
[Atlassian's current-user endpoint](https://developer.atlassian.com/cloud/confluence/rest/v1/api-group-users/#api-wiki-rest-api-user-current-get),
which documents the currently logged-in user and Confluence access permission.
The implementation requires HTTP 200, `type: known` and a nonempty `accountId`;
anonymous, null, malformed and redirect responses fail. The identity is discarded.
[Atlassian's Basic-auth guide](https://developer.atlassian.com/cloud/confluence/basic-auth-for-rest-apis/)
documents account email plus API token for site-host requests. Checked 2026-10-13.
No redirect or environment proxy is used by this one-request verifier. Provider
bodies and exception messages are never returned, logged or stored by setup.
This verifies authentication only; later exact-space/homepage verification still
establishes the allowed source scope independently.

**Operator prerequisite:** configure a dedicated `GATEWAY_ENCRYPTION_KEY` through
the existing Knowledge settings/deployment secret mechanism on API and worker
processes. This change neither generates nor configures a key, never falls back
to an internal signing token, and never reads a deployed secret during tests.
The Sources response includes `source_connection_setup.available/reason`; missing
key is visible before a user enters credentials and rechecked before provider
I/O or persistence. Wrong/missing key and malformed envelopes fail closed at
provider-use boundaries. No key rotation or migration is provided here; keep the
same configured key available for existing encrypted records and arrange any
future migration explicitly. This does not claim KMS-backed key management.

New records keep an explicit `encrypted:cfcred:v1` email sentinel and a
`cfcred:v1:enc:` authenticated envelope in the existing token column. The envelope
binds connection ID, tenant, owner and domain and contains email/token. Shared
core Fernet helpers are reused with strict round-trip/failure checks; their
legacy plaintext fallback cannot become a successful setup write. Decryption
occurs only at provider credential consumption. Either sentinel/prefix indicates
a versioned record, so damaged ciphertext cannot silently downgrade to legacy.
Prior plaintext rows retain explicit read compatibility, unchanged and unmigrated;
the dormant legacy updater refuses the new envelope. Receipts use SQL metadata
projection and remain readable after key loss or connection inactivity.

The form holds credentials only in memory and clears them on submission or lost
setup capability; recovery stores only UUID/name/domain in account/dataset-scoped
session storage. It never automatically resubmits credentials. An ambiguous POST
retains the UUID for exact GET recovery; GET404 does not establish rejection.
Only a recognized `CONFLUENCE_CONNECTION_NOT_ACCEPTED` POST rejection can release
a failed attempt. Typed encryption errors give administrator setup guidance.
The exact new POST bypasses generic response idempotency caching so every
submission executes current authorization and request-intent checks; other
middleware, signature verification and routes keep their existing behavior.
Parsing errors are sanitized within the handler so FastAPI cannot echo raw
credential input, even for extra keys or malformed fields.

Controlled tests exercise the production handlers/envelope/provider transport,
new connection → exact space → manual binding → explicit sync queue publication,
concurrent intent conflicts, revoked actors/grants, key failures, secret-free
responses/logs/recovery, redirects and ambiguous readback. No real credentials,
provider, database migration, live locks, browser, deployment or embedding/index
acceptance is established by these checks. Python Gateway/Python Knowledge and
the explicit native-Gateway/Python-Knowledge hybrid remain the supported owners;
full-native Knowledge does not gain this flow.


### Reversible saved connection controls (2026-10-13)

The Sources page exposes owner-scoped metadata management independently of the
credential-encryption key and current index writability. Current dataset editor
rights, current authenticated actor, exact connection owner and tenant remain
required for changes; canonical grants are rechecked in the held transaction.
Pause is an application admission control, not provider-token revocation. It
applies to every binding using the connection. Resume never schedules or starts
work and does not imply index readiness. Credentials (both versioned encrypted
and legacy plaintext), binding configuration and task records stay unchanged.

GET `/knowledge/{dataset}/sources/confluence/connections/manage` includes active
and disabled owned connections, with `offset`/`next_offset` pagination. GET
`/knowledge/{dataset}/sources/confluence/connections/{id}/control` returns current
metadata plus opaque `revision`, the same-tenant connection's total binding count
and paginated impact. Binding names/IDs/datasets/status are disclosed only for
owned bindings in currently visible same-tenant datasets; inaccessible bindings
have only a safe per-page hidden count. Impact is a point-in-time preview; changes
apply to all bindings, including bindings added subsequently. Legacy string
connection IDs are supported. Other legacy statuses are visible but not mutable.

PATCH of the latter path accepts only `expected_revision`, `expected_status`
and desired `status` (`active` or `disabled`). A short transaction locks connection
then dataset (matching binding admission order) and compares the exact timestamp serialization and status. It
updates only status and the existing updated_at execution fence. The schema's
BEFORE UPDATE trigger assigns transaction NOW(), so the SQL also requires
`transaction_timestamp() > updated_at`; a non-advancing clock/older transaction
fails without a write, rather than reviving an old execution generation. A stale
CAS returns 409. Generic response idempotency caching is bypassed for this exact
PATCH path, preserving current authority checks. Existing Python Gateway and
native-Gateway/Python-Knowledge hybrid wildcard forwarding retain PATCH support;
this is not a new full-native Knowledge handler.

Pause blocks new sync admission and pre-pause runs at their next current-authority
or publication check. A subsequent resume advances the revision again, so it
cannot revive an already-admitted pre-pause task. This is not instantaneous
cancellation or a globally linearized I/O barrier: an already-admitted provider
request or current publication step may finish; the UI may show processing
briefly, and partial effects remain. Published/indexed content is not removed.

Web requires an impact review and explicit all-bindings confirmation, stores only
non-secret CAS metadata before PATCH, and fences repeat clicks. Any uncertain or
rejected mutation response requires metadata GET readback, never automatic PATCH
retry. GET404 does not prove failure. Readback describes the current state, not
which historical request committed; the user explicitly adopts it before making
another decision. Account/dataset recovery ownership prevents stale controls
from overwriting another view's pending change. Capability changes invalidate
in-flight reads and impact confirmation. Backend authorization is authoritative.

Controlled tests cover exact/legacy ownership, tenant/current editor checks,
key/index-independent pause, hidden impact and pagination, encrypted/plaintext
credential preservation, stale/unchanged revision refusal, cache bypass, lost
acknowledgment readback, active-task fencing and publication rechecks after pause
and fast resume. Production component/API tests cover confirmation, recovery,
no blind retries and stale capability/read fencing. These use synthetic I/O and
persistence; real PostgreSQL trigger/lock/concurrency, browser, live provider and
deployment acceptance are not established. No schema, scheduler, credential
rotation/revocation, automatic resync or delete behavior is added.


### Explicit saved credential reconnect (2026-10-13)

Owned saved Cloud connections now support POST
`/knowledge/{dataset}/sources/confluence/connections/{id}/credentials`, forwarded
through the existing Python Gateway and native-Gateway/Python-Knowledge hybrid
public proxy. This is not a full-native Knowledge implementation. The request
contains only `expected_revision`, `expected_status` (`active` or `disabled`),
`email` and `api_token`. The saved name, hostname, ownership, status, sync mode,
bindings and tasks are not changed. Disabled connections remain disabled; neither
replacement nor resume starts synchronization. Users explicitly resume if needed,
then choose the existing manual-sync command for the selected saved binding.

Before transmitting replacement credentials, the service checks current actor,
dataset editor permission, exact owned tenant/connection, exact revision/status,
the saved hostname against the Cloud allowlist, and strict encryption readiness.
It performs one fixed-origin identity verification without redirects or environment
proxies. It then resolves the current actor again inside a short transaction,
locks connection before dataset, rechecks canonical current editor grants and
exact revision/status, and atomically replaces the existing credential fields
with the owner/tenant/origin-bound authenticated envelope. It rechecks encryption
at persistence. The existing trigger-safe `transaction_timestamp() > updated_at`
condition guarantees the execution revision advances. Verification, authorization,
encryption and CAS failures leave previous encrypted/plaintext credential bytes
untouched. No migration, key generation/configuration, decrypt-old prerequisite,
new table, new dependency, scheduler, provider revocation or automatic sync is added.
Old already-admitted tasks fail at their next authority/publication check; an
already-admitted I/O or publication step can finish. This is not immediate I/O
cancellation, and prior published content is not removed.

Sources exposes a separate `source_connection_reconnect.available/reason` capability:
current editor authority plus a synthetic strict encryption round-trip probe,
independent of index writability. The UI hides credential entry when unavailable,
without hiding keyless pause/resume management. Capability changes clear secrets,
invalidate previous impact/confirmation and fence stale read responses. Every
replacement requires impact review and explicit all-bindings confirmation. Form
credentials are cleared immediately on submission, permission loss, metadata
navigation or a control submission. They never enter session recovery state.

The narrow credential POST bypasses generic idempotency response caching but
retains all authentication/signature checks. Its handler sanitizes validation
failures, including malformed and extra fields, so raw credential input is never
returned. Explicit success is a verified replacement acknowledgement. Any lost,
malformed or rejected response keeps only connection ID, expected revision/status
and the non-secret `credentials` operation tag in account/dataset-scoped recovery.
No credential-bearing request is automatically retried. Metadata-only readback
works without a key and shows only the current revision/status/impact: even a
changed revision cannot attribute an ambiguous replacement to this request.
GET404 does not establish rejection. The user explicitly adopts current metadata
before making another decision, and any new replacement requires fresh secrets
and confirmation. Readback is never represented as credential verification.

Controlled route/encryption/provider-transport, execution-fence and component/API
checks exercise replacement failure and success, authority changes during provider
verification, stale/unchanged revision refusal, disabled/legacy compatibility,
metadata-only recovery and explicit continuation into manual sync. These tests
use synthetic credentials and in-process persistence/transports. They do not
establish real PostgreSQL trigger/lock/concurrency, live provider, browser,
deployment, indexing or complete J12 acceptance. Native signing/export evidence
is recorded separately with its exact producer and consumer checks.


## Native Desktop saved-source reconciliation (2026-10-13)

Desktop Sources now consumes the retained Knowledge owner's `manual_sync_available`
and `current_sync_task` facts for saved Confluence bindings. A separate review names
the dataset and binding and explains full reconciliation, including source removals
and preservation of documents referenced by another binding. Confirmation sends one
POST with a UUID retained before dispatch; progress and recovery use only the exact
request GET. No scheduler, connector credentials, endpoint or execution authority is added.

Restored tasks, unknown submissions, failed reads and mismatched receipts remain
blocked until an exact valid receipt is read. HTTP status alone does not prove
pre-admission rejection: this transport does not expose the service's structured
rejection body, so it conservatively keeps the guard. Pending/processing receipts
are observed with GET; read failures stop automatic observation until explicit recheck.
Terminal failure is distinct from successful zero updates and preserves partial counts;
unknown totals stay indeterminate. A new sync requires another review after terminal recovery.

Dataset navigation retains request identities in the signed-in session and closes
unsubmitted reviews. Account/origin changes clear private state and detach observation,
without claiming cancellation of server work. Copy the visible request UUID before
closing the app: local request retention is session-only. On reopen, refresh sources
and recover the server's current task, or paste the retained UUID into the read-only
exact-ID recovery control. A missing or stale current task is never proof of rejection.
No GUI, live connector, database, provider, restart-process or cross-platform acceptance
is implied by isolated native tests and Linux compilation.


## Native Desktop saved-connection maintenance (2026-10-13)

Desktop Knowledge now adds a bounded saved-connection list, affected-binding impact
review, and one explicit pause/resume action using the existing Python Knowledge
owner. Each request sends the reviewed exact revision and current status. The owner
rechecks connection ownership and current dataset editor authority; client snapshots
never grant access. List and impact pagination use the owner's 50-row pages. Total
binding count covers all datasets; hidden count refers only to the displayed page.
Hidden binding identities are never inferred or shown. Unsupported statuses offer no
pause/resume action, and malformed/mismatched metadata cannot enable confirmation.

Pause blocks new sync admission and existing work at its next authority check.
Already-issued work can finish and partial effects remain. It does not revoke the
provider token, remove indexed content, delete bindings, or edit credentials. Resume
does not start synchronization. Onboarding, credential replacement and scheduler
changes remain outside this Desktop workflow.

The exact CAS intent is retained before PATCH. Success, rejection, malformed reply
and lost response all require metadata-only impact readback and explicit “Use this
current state” before a fresh review. A changed revision or matching status does not
prove this particular attempt committed. No PATCH retries, automatic resume or sync
are issued. A failed or unauthorized read retains the pending intent and cannot unlock
a new action. Dataset/route navigation and capability changes clear confirmations and
queued observations while retaining session-scoped pending metadata. Account/origin
changes clear private state. Recovery controls remain reachable when source loading
fails or current write capability is absent; the service still checks read authority.

Retention is session-only. Save the displayed non-secret dataset/CAS recovery JSON
before closing the app; paste it into the same dataset after reopening for read-only
recovery. This does not recover unsaved intent automatically, persist credentials,
or establish real process-restart acceptance. Unit checks cover requests, validation,
production state transitions and closed queued-read exclusion; Linux compilation is
not GUI, live service/ACL/database/provider, cross-platform or release acceptance.

During a pending connection decision, new manual-sync reviews are blocked while exact
submitted UUID recovery remains available. Accepting current connection metadata
requests a dataset-keyed saved-binding invalidation and read-only source refresh after
navigation and any prior source job finish. Sync controls stay unavailable until that refresh succeeds; failed
refresh retains recovery and never submits a synchronization.


### Query-observation diagnosis handoff (2026-10-13 candidate)

KNO-12 query history and negative-feedback observations can hand their saved
question to the existing retrieval workbench, open their exact RAG trace in Eval,
or navigate to the current dataset's documents/configuration. Handoff is a local
draft only: it preserves workbench settings, clears prior results and requires
confirmation before replacing a different nonempty question. Running the test
and saving a pending Eval example remain separate explicit actions. History rows
do not supply a complete source-version snapshot or establish historical replay;
new results retain their own trace/version evidence. Missing questions and traces
are distinguished from failed reads. Trace visibility uses existing Eval viewing
permission, and its reader owns inaccessible or expired target handling.

Observation requests and actions are scoped to the current session, locally
observed authority, dataset and filters; retired reads cannot seed a new draft.
A 401/403/404 from either observation pane retires both panes and any delivered
draft/results until explicit GET-only recheck. Generic read failures hide that
pane's stale results. Question text and arbitrary metadata never enter URLs.
The optional hit-test identity scope fences dispatch, success and failure without
changing retrieval algorithms or defaults for callers that omit the scope.

The downstream failure-save bridge checks the original session/authority and
current observation before and after asynchronous reads, pagination, lazy Eval
dataset creation and final save. Retirement is latched; restoring an earlier
authorization snapshot does not resume an in-flight save. Unmounted/replaced
views do not advance that save or show stale completion notices. A request
already dispatched may have committed; retirement does not cancel or roll back
server effects, replay a mutation or create a second example store.

This is a reconstructed source candidate after the cloud environment reset.
Historical focused checks do not cover these bytes. Fresh controlled production
component/API/i18next regressions passed 31/31 with zero skips on the reconstructed
focused snapshot. Three additional current-code save cancellation cases then
passed within all seven Web gates: app/node TypeScript, lint, 1,307/1,307 Node
tests (zero skips), i18n, production build and bundle budget. The 641-file
current-main-plus-candidate closure and 49,775 dependency entries were unchanged.
Independent review and live Knowledge/Eval/provider/browser acceptance remain
pending; this offline evidence is not a KNO-12 live completion claim.


## KNO-09 QA service-stream outcomes (model-independent candidate)

The QA console submits one stream request for one explicit Send. It does not
silently repeat an empty, rejected, disconnected or malformed stream through the
non-stream QA endpoint. A validated `done.data.result` for the submitted question
is required before adding successful history and answer citations. A valid empty
answer remains a completed service result rather than reviving provisional text.
Explicit error, malformed terminal or EOF before that receipt retains partial text
with an interruption warning; raw transport/provider error bodies are not rendered.
“Put question back in input” only restores the question. The user must press Send
again to start a new request, which may repeat upstream work.

The component reuses the dataset page's authority scope, fences stale navigation
and authority callbacks, aborts its owned reader on disposal and synchronously
prevents double submission. Old scoped messages/history are hidden on scope change.
Aborting the reader does not promise provider cancellation. The receipt confirms
only the service's declared outcome. The pre-provider-terminal native implementation
also produced `done` after provider EOF; the following isolated provider-terminal
slice addresses that source behavior. Real deployed/provider behavior and answer
quality remain separate backend/live acceptance. No endpoint, backend schema or Python contract is changed.

Deterministic reducer and production component regressions cover interrupted/empty/
malformed streams, partial text, valid empty terminal, preserved citations, explicit
reuse without replay, double clicks and late responses after navigation/authority
change/disposal. These regressions passed the bounded checks below; no model,
service startup, native build, full Web suite or browser acceptance is claimed.

Validation: all 30 focused QA reducer and citation/component regressions passed
(0 failures, 0 skips), together with app and Node TypeScript checks, scoped
changed-source ESLint (exit 0, zero errors, three nonblocking exhaustive-deps
warnings on the scoped empty-message fallback) and the i18n key check. Receipts bind the exact external
Web snapshot, pinned Node 26.11.1 and unchanged existing dependency inventory.
These are model-free checks; no full suite, build, browser timing/performance
measurement, live scheduler execution or real-model acceptance was performed.


## KNO-09 text-only provider terminal contract (candidate)

Native and Python Knowledge QA streaming now require an explicit choice-zero
`finish_reason: "stop"` before publishing their service `done` receipt. An empty
answer with that semantic terminal is valid. A transport `[DONE]` alone, ordinary
EOF without stop, length/content-filter/tool-call termination, explicit provider
error or malformed data cannot become a successful answer. Stop followed by EOF
or a trailing usage frame and `[DONE]` remains supported. This text-only QA route
has no tool execution; the generic OpenAI adapter's tool completion modes are not
adopted. Existing partial deltas remain visible through the already-defined error
path; no provider request is automatically retried.

SSE data lines are assembled per event, with comments/event/id/retry metadata
ignored and multiline JSON data joined correctly. Existing bare JSON line
compatibility is retained separately, never merged into an unfinished SSE event.
A final unterminated event is parsed at EOF but still must supply semantic stop.
Native reuses the adapter's bounded line framing and strict UTF-8 handling; both
classifiers bound buffered SSE event data to 4 MiB. This does not claim a global
answer-size limit or bound httpx's own line allocation.

A shared synthetic fixture corpus covers Python/native parity and production
Python coroutine wiring without network I/O. Isolated parser compilation/testing
can verify this subfunction with existing Rust dependencies; it is not compilation
or linking of the product assembly. Full native assembly, live HTTP framing,
provider support/behavior and model answer quality remain separate acceptance.
Bounded isolated validation passed: 37 shared fixture cases in the native parser,
Python parser and exact production Python coroutine with a controlled HTTP double.
The Rust harness passed 2 tests; Python unittest passed 3 methods containing the
shared matrices and event-budget checks, with zero failures/skips. Pinned Rust
1.99.0 compiled only the parser/test module against the unchanged recorded 8-rlib
closure; dep-info, actual link map, fresh binary and compiler/sysroot/linker hashes
were retained. Native application assembly remains uncompiled, and no complete
Python service import/startup or real provider/model acceptance was performed.


## KNO-04/06 older-version navigation (model-independent candidate)

The document history dialog uses the existing authorized 20-row metadata pages,
with explicit ranges, previous/next and GET-only refresh/retry. Two selected
version identities stay visible across pages, so a recent revision can be compared
to an earlier known-good revision without silently limiting the user to the first
20 entries. Returning from comparison retains the page. A deleted or shrunk last
page makes one bounded correction to the current last page; list errors hide the
old rows and do not present unknown history as zero saved versions.

Reads, selections, comparison and explicit restore confirmation belong to the
current dataset/document, authority, dialog visit and request. Closing, navigation,
authentication changes or page replacement retire obsolete actions/results.
Changing pages clears a restore confirmation; restoration still requires the
existing explicit confirmation and invokes the unchanged endpoint once. A late
reply cannot close another document. No model work, new permissions or automatic
restore is introduced. Metadata pages are live reads, not a frozen history snapshot
or proof of full-content ACL acceptance. Restore/publication/provider and real
browser/database acceptance remain separate.

Validation: 10/10 focused tests, app/node TypeScript, zero-warning scoped lint and i18n passed on source manifest `2ac8408ef9a71b86f6a03a7ca5921ae2c1dc88e734b6fe6edfd782ca939d2340`. All five serialized gates used one immutable Web snapshot and one hash-bound dependency inventory, freshly rechecked before/after each command. Controlled production hook and dialog regressions cover 45
versions, cross-page comparison, shrink/deletion, failed page/retry, out-of-order
responses, close/document/authority retirement and explicit restore ownership.

## Fixed-token Unicode integrity

Fixed token windows in the Python Knowledge owner and native ingestion owner must
end and resume at Unicode scalar boundaries. Tokenizer tokens are byte fragments;
decoding arbitrary windows with replacement can corrupt Chinese, emoji and combining
text before embedding or preview. Python's strict token-enforcement splitter shares
the same corrected window implementation as its fixed-size chunker.

Each emitted trimmed span is checked against the actual cl100k_base encoding count,
not the language-specific token-count estimate. Existing whitespace trimming remains;
no Unicode normalization or replacement is introduced. A source U+FFFD is preserved.
Overlap is a maximum token budget: it can be reduced to a valid scalar boundary and
further reduced when needed to cover new content. Coverage remains ordered without
gaps in non-trimmed text, and a completed document emits no duplicate overlap tail.
If the next indivisible Unicode scalar cannot fit, chunking fails with an actionable
validation error requesting a larger token limit. It never emits a corrupted scalar
or silently exceeds the limit. This does not promise grapheme-cluster preservation.

The existing whole-encoder materialization and output-count fences remain active.
Shared deterministic fixtures cover Chinese, emoji, combining characters, literal
U+FFFD, Arabic, overlap, a realistic 512-token boundary and impossible tiny budgets.
Validation on 2026-10-14 passed 160 actual Python tests, including both splitters,
document processing and existing configuration/output fences. One existing
Starlette/httpx deprecation warning remains. Ruff exited 1 on both baseline and
candidate with the same three pre-existing diagnostics; independent source review
reconciled an embedded F811 reference moving from line 701 to 702 after the import.
The initial finalizer failure is retained; no test rerun or diagnostic suppression
was used to claim a clean lint exit.

A fresh current-source integrations-crate check and four actual Rust production
tests also passed. Native tests cover the same 15 shared fixtures and 108 small
limit/overlap combinations, document processing and terminal-tail behavior. None
failed or were ignored; 66 unrelated Rust tests were filtered by the focused target.
Selected emitted-artifact dep-info binds the changed owner and shared fixture.
Source, dependency/tool and protected prior-artifact identities remained unchanged.

Candidate spans are re-encoded to enforce the cap; no linear-time or throughput
claim is made. Existing boundary whitespace trimming remains outside byte-for-byte
reconstruction promises. These offline results establish both owner implementations,
not live ingestion, embedding/retrieval quality or whole-Gateway acceptance. No
model, provider, network or service was used for these checks.


## Explicit batch selection recovery (candidate)

After returning to a partial explicit-document batch, “Select remaining documents”
restores its failed, skipped and not-submitted IDs across pages. Proven queued or
deleted successes are excluded. This replaces the current local selection and opens
batch mode only; the existing action review and confirmation remain required.
The saved operation receipt stays visible and unchanged. Unknown outcomes, including
manually acknowledged uncertainty, truncated or unreconciled per-item results cannot
supply this selection. All-document batches have no complete requested-ID snapshot
and retain their existing read-only recovery controls. Selection is not proof that
an item is currently authorized or eligible; the owner rechecks each new operation.
No automatic mutation or retry is added. Offline regressions are separate from
browser, service, database, provider and complete J09–J13 acceptance.


## J09 upload navigation recovery (candidate)

Dataset-detail uploads retain bounded metadata receipts in the current browser tab,
scoped to the authenticated authority and the dataset's tenant/identity. The summary
remains visible outside the closed upload dialog and can be inspected after returning
to the dataset without selecting another file. It records each original position,
filename and size, never file bytes, source content, configuration or raw errors.
Accepted means admitted for processing, not indexed or searchable. A known document
record is distinct from a rejected source; an unacknowledged dispatch is unknown.
Files not reached by a sequential upload remain explicitly not submitted.

The receipt must be saved before each actual upload dispatch. A completed response
is recorded before refreshing document queries, so a read failure cannot erase known
acceptance. Navigation stops further client-side dispatch; already accepted server
work is not cancelled. A response arriving after the view retires may remain unknown
locally and requires checking the existing document list. Refresh and receipt review
never upload, retry, restore or delete sources. Existing same-dialog retry selects only
explicitly safe rejections. After navigation, the user must reselect rejected or
not-submitted files themselves and must inspect uncertain outcomes before acting.

Receipts expire after 24 hours, are capped at 10 attempts with 50 files each, and the
entire upload-receipt namespace is bounded to 256 KiB of encoded keys and values.
Reaching a bound blocks new dispatch rather than evicting an unresolved receipt.
Explicitly clearing local receipts warns that it neither cancels server work nor proves
non-acceptance. Global auth lifecycle cleanup also runs when Knowledge is unmounted;
login/token/authority changes clear these records. Storage cleanup failures fail closed
for the current document. Same-origin session storage is a convenience record, not a
server authorization or durable cross-device processing ledger.

This slice concerns the dataset-detail upload dialog. The create wizard and full J09
browser/service/restart/format matrix remain separately unverified. Candidate tests
and checks must pass on their exact source before this is recorded as verified.


## KNO-01–03 create-wizard interruption ownership (candidate)

The existing create request identity remains in tab storage until the active wizard
finishes. Cancel/Back retires this wizard synchronously; unmount, token, account or
authorization changes retire its current reads and submission before any later file
or URL dispatch, result publication, navigation or recovery-draft deletion. A store-
observed A–B–A transition cannot revive an older callback. Text/URL drafts remain in existing per-user tab storage; this slice does not
introduce global logout cleanup. The new authenticated view
remounts its own form and rechecks any stored dataset identity before allowing work.

Leaving does not cancel or roll back an already dispatched server request. The UI
states that those requests may still complete. Returning uses the existing stable-ID
lookup and opens a known dataset; it does not automatically recreate the dataset or
replay source uploads. Local file bytes still require re-selection after remount, and
unknown upload results require inspecting the existing dataset before resubmission.
The existing safe-failure-only retry and completed-source exclusion stay unchanged.
Controlled component/API regression evidence is distinct from browser, backend,
provider and complete J09–J13 acceptance.


## KNO-08 retrieval outcome and scoring retry recovery (candidate)

The retrieval test distinguishes an untouched workbench, a failed query and a
completed query with no returned hits. A failed request does not claim zero vector
or keyword hits or display successful retrieval statistics. Completed empty results
show only reported nonnegative integer stage counts; missing or malformed counts
are explicitly not reported. An empty result does not infer that ACL filtering
occurred or identify inaccessible documents. Query and configuration remain available
for an explicit retry; no request is replayed automatically.

Starting an explicit RAGAS scoring retry clears the previous scoring error while
preserving the accepted retrieval results, metadata and trace identity. A fresh
failure remains visible; a successful retry no longer displays the old failure.
Existing query/configuration invalidation and dataset/authority result ownership
remain in force. This is a controlled Web recovery slice, not retrieval quality,
provider, service, browser, ACL or complete KNO-08 acceptance.

## KNO-06 exact source from document version history

Version History now opens an explicitly selected version through the existing
current-access historical source dialog. The row supplies its own version number
and content hash; no current-document hash or URL reconstruction is used. Missing
or malformed hashes and unsupported/pending version rows cannot open a source.
The viewer validates the response identity and recomputes its content hash, with
no fallback to current content. Viewing never restores a version.

Preview ownership follows the validated history page and authenticated visit.
Page/compare/restore/close and document/dataset/authentication transitions retire
the preview and stale row handlers. A source denial or missing historical body
remains unavailable; it does not initiate reconstruction, rollback or ingestion.
Controlled composition tests are distinct from browser, live service, database
and complete KNO-06 acceptance.

### Governed capability retrieval source identity (2026-10-15)

The Python Knowledge internal capability retrieval adapter now uses the same
published-source identity projection as its public retrieval routes. The helper
lives within Knowledge Service; the Gateway does not import this service. The
public route's existing helper exports remain available.

A retrieved excerpt receives source version/hash only from the tenant-scoped
current published document-version lookup, with dataset generation and viewer
access rechecked around that lookup. The existing SQL verifies active document
state, excludes pending versions, and compares current content and its SHA256.
The route stamps its admitted dataset and actual hit document/segment identities;
stale index metadata cannot replace them. Manual overrides and missing authority
remain unversioned, and stale top-level metadata version/hash fields are removed.
An unavailable or malformed lookup fails rather than attributing an invented
version. Existing public retrieval behavior and valid response shapes remain.

Focused handler checks use synthetic capability proofs and controlled IO, plus
source-bound SQL assertions. They do not prove real database isolation, worker
transport, native parity or live provider behavior. Gateway non-streaming context
aggregation is the next separate slice; this producer change alone does not make
citations appear in non-streaming answers or establish full KNO-09 acceptance.
