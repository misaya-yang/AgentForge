# R2 知识生命周期执行记录（进行中）

前一安全增量 `bc57e429` 已合入本地 `main`；本记录继续跟踪 `codex/r1-final-boundaries` 的 R2-A 安全增量。13 份 RP04 未跟踪证据保留。**完整 R2 未完成**，旧 KB `CONDITIONAL PASS` 不算本次验收。R1 图片产物正例仍受外部 CDN DNS 阻塞。

## 需求—现状—缺口—验收映射（2026-09-28 现状复核）

下表记录当前能力；本报告后面的 2026-09-27 测试数字是当时的历史回执。PRD §5 对未支持扫描/OCR或格式要求明确拒绝和说明，**不要求在 R2 开放新解析器**。已开放路径仍须通过正常、失败和恢复验收。

| 需求 | 已有实现/当前证据 | 当前缺口与 R2 验收 |
| --- | --- | --- |
| KNO-01 | 库 CRUD、游标目录/前端翻页、草稿及 owner 归档/恢复已实现；本地 API 归档/恢复、隔离 PG ACL 已验 | 201 库容量与授权可见性的真实浏览器 J09 未验收 |
| KNO-02 | 同名批量失败按位置回配；仅明确 `retry_safe:true` 才允许直接重传，未知结果保留诊断与核对指引；真实文本/PDF/损坏 PDF 混合上传已验。公开扫描/多模态上传目前明确拒绝 | J09 需验证不支持格式的拒绝说明、无副作用、容量边界和仅失败项重传；不以开放扫描新解析器为前置 |
| KNO-03 P0 | 持久队列与进度 SSE 已有；真实 DashScope 层级执行在 `preparing` 被物理 KILL 后沿同 execution 完成，仅新增 1 行账本；失败文档历史可回看 | 其他阶段的物理中断、长任务 UI 进度及浏览器刷新 J09/J10 待验 |
| KNO-04 P0 | 标准文本与层级候选发布、历史版本恢复及真实 DashScope 故意无效模型 4xx 后保旧已验；跨类恢复使用受控配置夹具，物理 KILL 同 execution 与 PG/Qdrant ID 对账通过 | 普通用户配置变更的跨类路径、其他崩溃阶段与浏览器 J10 未验；未公开 Vision 不作为新增解析器前置 |
| KNO-05 P0 | 数据集 ACL、文档级来源校验、active-only 原文/viewer 列表及 Gateway SSE/历史/Quiz 撤权补强已有自动化 | 双租户双角色原文、切片、检索、QA、缓存、下载、分享的真实 J12/J30 尚未跑完；不可核验的历史全文须安全降级 |
| KNO-06 | 文档原文、切片与引用入口已有 | 页码/片段定位及无法定位时降级待 J11 |
| KNO-07 | 配置读写已有 | 默认/继承/覆盖、有效值、需重建与离页草稿待 J10 |
| KNO-08 | 召回组件有向量/BM25/融合/重排信息 | 两套冻结配置与零命中/ACL/故障区分待 J11 |
| KNO-09 | QA 严格模式与引用摘要已有 | 有/无/冲突/数字/追问及引用打开、切库后失效待 J11 |
| KNO-10 | 来源页区分一次性文件/URL 和定期连接器；本地 API 返回 `not_configured`，成功后旧错误误报已修 | 当前 Runtime 未配置 Confluence；真实来源更新/删除/凭据失效恢复 J12 未验 |
| KNO-11 | 跨页批量与逐项收据、Agent 引用影响预检接口及前端合同已实现，本地 API 预检 200 | 真实跨页/部分失败与重复删除的浏览器 J13 未验；无操作 ID 的丢响应仍须人工对账 |
| KNO-12 / EV-01 切片 | KB 失败样本进入既有 Eval 数据集的 pending 修订，来源版本核权与旧 revision 409 已实现；本地真实 API 零命中保存/readback 通过 | J13 浏览器审核与多角色授权未验；R2 KB 样本不进入 evaluator/live_candidate |
| AR-06 | 内部会话/Quiz 分享冻结来源范围并每次重核权；本地 API 登录读取/匿名拒绝/撤销通过，跨账号 Quiz token/cache 缺陷已修 | 双角色撤权、来源更新后的内置浏览器 J30 未验 |

## 本轮已复现与修正

- **KNO-02 创建与上传结果。** 原批量回执用文件名定位，同名失败会回配错误；现在服务端返回 `file_index`，客户端核对位置、名称和收据总数。旧服务或异常回执缺少明确 `retry_safe:true` 时结果按未知处理，隐藏可能含内部信息的原始错误，不提供直接重传。创建向导先持久化用户标签页内的稳定 dataset ID；POST 丢响应后刷新 GET 原 ID，不自动重复建库或上传。创建/来源上传的未知结果标题也明确写“结果未知”，显式结束上次流程说明旧请求可能仍在执行。
- **KNO-03 失败详情。** 真实损坏 PDF 被接收进入 worker 后终态失败；旧切片面板仍误报“处理完成后将显示”。现在按最后记录的阶段显示失败、手动检查/重试指引及文档 ID；成功的既有切片仍可查看。文档读取和进度 SSE 对内部错误做安全投影，含归档失败文档。
- **KNO-04 旧正文与新切片分裂。** 已用重提取+Qdrant 失败回归复现：旧切片保留而 `documents.content` 先变成新正文。现让短文件重提取正文暂留本次处理变量，使用既有数据集修订栅栏在文本切片发布事务里一并写入。失败、发布前旧正文可读、成功同版和无向量变更提交的三项新增回归通过；旧扫描/大文件路径尚需继续核对，未称完整 KNO-04 通过。
- **来源同步调查。** 曾对未接入的旧 Confluence 类做探索性修正并运行定向 Python 18 passed；确认当前 Runtime 不实例化该类、旧控制台入口已删除后，已撤回那两份文件的所有本轮改动。这 18 项只说明旧内部类的静态测试，**不计为 R2 可用来源同步通过**。

## 实际验证与独立 review

- **此前安全增量自动化：** `make kb-unit-gate` 为 **1890 passed, 1 skipped**（缺 `KB_BACKFILL_POSTGRES_TEST_DSN`），定向 Python 125 passed；Web Node 24 passed；app TypeScript、Web lint、i18n、build、Ruff、`make harness-check`、`git diff --check` 通过。Node 当前 24.14.0，项目期望 22，lint/build 有 engine warning。本轮最终候选检查见下方，不沿用这些旧数字。
- **受控故障注入：** Playwright `knowledge-create-recovery.spec.ts` **2 passed**，模拟服务已接受建库但响应丢失，以及旧请求身份失效后显式结束；断言没有第二次 POST。见 `reports/r2-knowledge/controlled-create-recovery-final.log`。这不是实际网络丢包或真实服务重复提交。
- **真实本地 Docker 与内置浏览器：** Compose 的 Backend/Frontend/Runtime/Knowledge 工作目录标签均为本仓库；最终 Knowledge 源码、Frontend `index.html` SHA 与容器一致，`make status` 全部健康。内置浏览器登录既有专用账号，真实创建私有库 `kb_7ec701f675fb465797bf7bfc20cc9203`，上传合成文本、有效 PDF、损坏 PDF；前两份完成并产生共 2 个切片，损坏 PDF 失败。最终代码更新后刷新与从知识库目录重开仍为同 3 文档，损坏 PDF 显示“最后记录阶段：解析”、安全下一步、文档 ID；有效 PDF 1 个切片可回看。截图：`tmp/r2-knowledge/failure-after-fix.png`、`tmp/r2-knowledge/success-history-after-fix.png`。此流程没有重启 Knowledge worker，不能当作服务重启续接通过。
- **独立只读 review：** `closure_review` 发现归档失败文档泄漏、未知标题误报、旧 batch 回执默认重传、失败阶段旧时间戳误报；均已最小修正并复核。对拟合入的创建/上传/失败提示增量未发现剩余确定 blocker。Reviewer 未操作 Docker 或浏览器；其余 R2 缺口仍存在。

## 当前待执行

1. KNO-03：停启前已排队的文本任务续接已验；执行中硬崩溃、同任务 owner/回执与长任务阶段仍待验证。
2. KNO-04：普通文本候选与版本恢复已修；层级/VisionPDF 多 collection 图片发布、active BM25 v2 和非纯文本发布后失败窗、execution ledger 终态对账仍是完整 R2 的 P0 阻断，须完成真实 PG/Qdrant/图片存储故障矩阵及 J10。
3. KNO-05/AR-06 已有文档级撤权守卫，但稳定历史全文与受控内部分享、双租户双角色的原文/QA/引用/下载/分享矩阵未完成。KNO-01/06～12、EV-01 切片及 J09～J13/J30 仍待实施/验收。旧 Confluence 类未挂到当前 Runtime，不计为有效来源同步。R2 不标完成，也不借本轮合入宣称 R3～R5 已推进。

已有五个既有 `model_tester` 测试账号曾被旧 E2E setup 重置密码，原密码无法恢复；RP04 已披露。当前 setup 默认仅登录既有账号，R2 不创建或重置账号。

## 后续 P0 安全增量（本地 Docker 已更新；不计 R2 通过）

`main@bc57e429` 后继续调查并修正标准文本路径的两个崩溃窗：`indexing` 可早于新候选正文/切片的持久发布，旧 `recover(indexing)` 可能重嵌旧切片而错误完成；版本恢复原先先写正文、先推进版本指针。当前候选改为原固定配置的增量续接，版本恢复使用隐藏的前快照/目标候选，同一 PostgreSQL 发布事务才显露版本、正文和切片；短恢复正文不再从原上传重新提取，等待锁后撤权会拒绝。active BM25 v2 的 stale claim 只在 kill switch 开启时放行。扫描/层级旧文档的重处理在多 collection 原子发布完成前按切片和 Qdrant 点实态拒绝；API 准入阶段不创建无效 execution，首次无任何部分写入的任务仍可恢复；含部分写入但无法安全判定的任务明确拒绝，不谎称旧版可用。

**本地 Docker 与内置浏览器新增证据（2026-09-27 夜）：** `make hot-update ARGS="--knowledge --frontend"` 已成功，Knowledge API/Worker 和 Frontend 容器哈希与当时候选一致，`make status` 全健康。用既有 E2E 登录对合成私有库的 `r2-policy.txt` 先在受控本地 PG 事务中建立版本 #1（500 CNY）与 #2（730 CNY）作为夹具；这是**受控数据准备**，不是 UI 创建历史版本。浏览器真实点击恢复 #1：先显示排队，完成后当前版本 #4、切片为 500 CNY；刷新后仍为同一文档、同一切片。随后停止 Knowledge Worker，浏览器点击恢复 #2 并刷新：仍排队、当前版本保持 #4，隐藏候选未露出；启动 Worker 后状态完成，当前版本 #6、切片为 730 CNY；刷新后仍有 3 文档、该文档 1 个 enabled 切片。真实 PG 只见 2 条预期的 reprocess execution（对应两次有意恢复），没有刷新产生的第三条。截图：`tmp/r2-knowledge/version-restore-queued.png`、`version-restore-refreshed.png`、`worker-stopped-queued.png`、`worker-restart-completed.png`。这证明**停启前排队任务**的续接，不等于执行中硬崩溃、多 collection 原子恢复或真实扫描/OCR 通过。

**当前代码与回归：** 纯文本 `lexical_v1` 在同一 PG 事务公开正文、切片和 `completed` 终态；版本恢复还在该事务切换版本指针，并清除持久排队标记。图片回执、生命周期重建与 active BM25 v2 保留旧路径。已补同库文档级授权：助手继承的 context event 文档 ID、会话/事件流/产物/测验/分享重核权；KB viewer 列表和切片只返回 active 行，引用原文走 active-only 路由。畸形 context event 以未知来源拒绝。引用弹窗明确提示历史摘录可能与当前原文版本不同，未伪称已保存稳定全文。

**当前最终自动化：** `make kb-unit-gate` **1923 passed, 1 skipped**（缺 `KB_BACKFILL_POSTGRES_TEST_DSN`）；`tests/database/test_kb_dual_verb_queue.py` 对真实临时 PG **36 passed**；Gateway 来源/分享/会话/图片等受影响范围 **81 passed**；Web TS、lint、i18n、build，所有改动 Python 文件 Ruff、`make harness-check`、`make architecture-boundary-gate` 与 `git diff --check` 通过。扩展到整个 Knowledge 源码/测试目录的 Ruff 检出 21 个**未改动文件**的既有错误，本轮未顺带改动；以所有改动 Python 文件通过作为受影响范围结果。Node 24.14.0 与项目期望 22 不同，Web 工具有 engine warning。特殊路径的旧行/孤点与持久图片回执拒绝仅自动化验证；未把模拟写成真实扫描 provider 验收。

**当前 Docker/浏览器：** `make hot-update ARGS="--gateway --knowledge --frontend"` 后又更新 Gateway/Knowledge 及最终 Knowledge 代码，全部退出 0；`make status` 全健康，Gateway 来源文件、共享 proxy、Knowledge API/Worker 入库文件和 Frontend `index.html` 的 host/container SHA 一致。Codex 内置浏览器对合成 `r2-policy.txt` 恢复版本 #4 时先遇到真实 500：API 角色 `DurableEnqueueProxy` 未实现 Worker 准入方法；已抽取共享只读准入并增加 API 角色回归，重新热更新后同一 UI 操作显示排队→已完成，当前版本 #8，1 个切片为 500 CNY。刷新后仍 #8；PG 执行账本在刷新前后均为 `ingest completed=1, reprocess completed=3`，没有刷新新增执行。截图：`tmp/r2-knowledge/final-candidate-restored-segment.png`、`final-candidate-version-history.png`。版本 #1/#2 早先由受控本地 PG 夹具预备；本轮恢复 #4 是浏览器真实操作。未测试执行中硬崩溃。

**独立只读 review：** reviewer 指出图片回执但暂无图片行时的 replay/restore 漏拦及畸形来源事件漏拦，并静态复核了修正。API 角色实际 500 由浏览器发现，已通过共享准入和真实页面复测关闭。reviewer 对其余最终差异未发现新的确定代码 blocker，但没有亲自跑测试或 Docker。完整 KNO-04 仍受层级/VisionPDF 多 collection 直接发布、active BM25 v2/非纯文本发布后失败窗和 execution ledger 关闭窗口阻断。完整细节和下一步见 [交接记录](../../../deploy/runbooks/r2-knowledge-lifecycle-2026-09-27/HANDOFF.md)。
