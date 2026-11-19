# R2 当前集成与实机证据（2026-09-28，进行中）

本记录继承 `main@d51201c9` 的已合入安全增量；本批在 `codex/r2-closure-20260928` 开发，最终 Git 状态以 `deploy/runbooks/r2-knowledge-lifecycle-2026-09-27/loop-state.json` 为准。R2 未完成。保留既有 13 份未跟踪 R1 JSON。

本批安全增量提交为 `b8362ff0`，已本地 fast-forward 合入 `main`，未推送；当前 R2 验收状态仍为进行中。

## 需求边界

PRD `03-knowledge.md` §5 明确：扫描/OCR或某格式若确不支持，验收正确拒绝与说明，不强行新增解析器。当前 `scanned`/`multimodal` 公开上传本就拒绝；需测提示及无副作用，不把未发布 Vision 当完成 R2 的前提。R2 重点是已开放层级/普通文本的成功、失败保旧、版本恢复和中断续接。最新逐项映射在 `reports/product/r2-knowledge-progress-2026-09-27.md` 开头表格；该报告后文保留历史回执。

## 真实本地服务与受控夹具

- Compose 工作目录标签核对为本仓库，服务均健康；按 `docs/harness/runtime-and-secrets.md` 用现有容器热更新 Knowledge。专用 E2E 账号仅在脚本执行时读取，未输出凭据；未创建账号。
- **跨类恢复**：用本地认证 API 创建私有库 `kb_10915683cb20` 和合成文本 `b9de04df-eb5f-4b1e-ae48-1acceb8863c1`，真实 DashScope 初次普通文本入库。为构造历史跨类路径，**受控直接修改该专用库的 index_config** 从 automatic 到 hierarchical（公开 API 对已有文档的索引身份变更会拒绝）；随后通过正常 reprocess API 发布 hierarchy #2，再通过正常版本恢复 API 以历史 #1 的正文/hash/source receipt 发布普通文本 #4。PG 当前 1 个 text 切片，Qdrant base 精确同 ID，`_sections` 为 0；版本 #1/#2/#3/#4 分别保留其事实。此证据证明后端候选与真实 provider/跨存储路径，不证明普通用户可在 UI 直接改索引模式。
- **禁用决定**：同一专用文档后续正常重处理为 hierarchy #5，通过公开 API 禁用 position 0 的 text 段，再恢复历史 text #1。当前 #7 的 PG 唯一 text 段 `enabled=false` 且 `disabled_by` 非空；Qdrant 唯一点 `enabled=false` 且 ID 与 PG 相同，`_sections` 为 0。验证了 reviewer 指出的 PG/Qdrant 禁用状态边界；操作均有意触发，没有自动重复恢复。
- **真实 provider 错误保旧**：另建私有层级合成库 `kb_43a829ae3a1c`、文档 `d47c4b1d-5e11-41a8-9fc0-0ef8c3a44d47`，先用真实 DashScope 完成版本 #1、28 个切片。为制造 provider 4xx，**仅对该专用库受控暂改 embedding_model 为无效名称** 后提交一次 reprocess；日志分类确认请求到达 DashScope 并返回 `InvalidParameter`，执行终态 `error`。随后恢复原模型；文档仍为版本 #1、28 个原切片，PG segment ID 集合未变，Qdrant base 23 点与 `_sections` 5 点逐一匹配 PG；正常检索 HTTP 200，问题标记 `ANCHOR-R2-050` 仍命中。这是真实 provider 对故意无效配置的拒绝，不等于外部服务故障或自然限流。
- **未支持模式正确拒绝**：向该专用库经公开上传 API 提交有效合成 PDF 且指定 `processing_mode=scanned`，HTTP 400 明确说明当前禁用；请求前后文档数和 execution 数均为 `1|4`，未创建任务。这符合 PRD 对未支持格式的拒绝边界；内置浏览器的对应提示仍待解锁后验证。
- **执行中 Worker 硬中断**：在同文档再次 reprocess，API execution `f92a553a3f1746c0a421887aa9c606a2` 已是 `running`、文档 `parsing` 50% 时对 Knowledge Worker 发送 KILL 并启动容器。自动恢复后原 execution 记 `error`，新 `recover` execution `3c9ead8476ae43bd9b40910050f7e401` 完成；文档版本 #2、28 个活动切片。修复通用完成回执覆盖 manifest 后再次在同样运行状态 KILL：原 execution `7bad7bab68894f6786875c8915ee54e8` 记 `error`，新 `recover` execution `0070823db0f14e7aa03738ed3d3e7325` 完成，文档版本 #3。两次均证明同一文档被实际续接，但**均未沿用原 execution ID**。第二次还证实特殊 preparing 崩溃路径未持久保留原/新 execution 的 lineage，且先 abort 再 enqueue 有新的崩溃窗口；这一真实缺口仍在修正，不能称同任务回执闭环通过。
- **准备阶段修正后的自动化**：仅对尚未冻结候选点的 `preparing` 阶段，清理已声明对象后在一个 PostgreSQL 事务中保留原执行账本与 pinned snapshot、重排同一文档/执行 ID，并释放负 revision。真实隔离 PG/Qdrant/本地对象存储测试 **1 passed**，Worker 准入单测 **1 passed**；正常失败仍保留原有终态错误语义。此代码尚未更新最终 Docker、也尚未经历第三次物理 KILL，不能把前两次的不同 ID 结果改写成已通过。
- **QA/召回来源身份**：本地认证 `/hit_test` 返回 3/3 完整 source_version/hash；真实 DashScope QA 请求 HTTP 200、3/3 context 带版本/hash，答案非空。QA/命中测试在同一检索代数核对当前持久来源；变化时拒绝误归属。带非 allowlist 模型的首次 QA 请求 HTTP 400，随后按服务端默认模型成功。

## 自动化与 review 状态

- 已加普通文本缺点/外文档点续接拒绝、层级 provider 向量维度/有限值与模型代数绑定的回归；OpenAI-compatible `MockTransport` 503 是**受控模拟**，和上述真实 DashScope `InvalidParameter` 分开记录。Agent A 的候选 KB 门禁 2006 通过/1 跳过，最终集成后需重跑。
- 新 Eval 待审核样本、目录/配置/批量前端由并行代理实现并执行定向门禁；独立 UI reviewer 已发现 pending 样本误入运行、审核分页/重复点击、跨库草稿、批量 operation 重提、深链和 case revision 等确定缺口。责任代理正在修复，**尚不能称最终 review 通过**。
- Knowledge QA 来源版本、hit_test 权威版本及 recovery manifest 合并由主代理接线；新增定向测试已跑，最终全量与 Docker 源码哈希待集成后执行。

## 当前阻塞/未验证

- Mac 锁屏使 Codex 内置浏览器最终验收暂挂；此前浏览器截图仅属于前一安全增量，不能代替本批 UI 测试。继续 API、自动化与代码工作，不反复操作锁屏。
- 只有一个可确认的专用 E2E 账号；没有足够的既有身份完成双租户双角色 J12/J30 实机矩阵。已有自动化不能冒称该矩阵通过，不创建替代账号。
- 数据集归档、Agent 影响预检、受控内部知识分享、KNO-06/08/09/10/11/12 的全部旅程和 J09～J13/J30 仍需逐项验收。POST 服务端已创建批量操作但响应完全丢失时，客户端没有 operation ID，目前只能暂停并要求人工核对。

## 2026-09-28 集成候选与最新验收

本节覆盖上文旧候选状态。实现已包含 owner 归档/恢复、默认目录及授权隐藏归档库、Agent 引用影响预检、来源状态页、跨页批量回执、配置草稿与有效值、KB 失败样本写入既有 Eval 审核队列、受控内部会话/Quiz 分享。Eval 的 KB 样本在 R2 无论审核状态均不进入实际 evaluator/live_candidate，直到后续运行授权合同完成。

**执行/归档安全修正。** Reviewer 发现恢复等待态可被新请求换掉 execution/rule、特殊 `auto→scanned` 重放会重复检测 CAS、规则快照失败后特殊准备无法续接、提交与归档存在 orphan ledger 窗口。现 API 提交的规则/执行/队列认领在同一文档租约和 PostgreSQL 事务内完成；Worker 新建执行与文档链接也同事务。特殊 `preparing` 清理后沿用原 execution/snapshot，恢复原 rule pin，`auto` 重用已持久化的检测结果；归档独占租约下只终结无活跃 special manifest 的孤儿或终态漏关账本，仍拒绝活跃执行。真实隔离 PostgreSQL 认领/回滚与归档测试通过；独立只读 recovery reviewer 复核最后 SQL 未发现确定误终结路径。

**真实 Docker/provider 与 API。** 先运行 schema authority 迁移 005/006，再 `make hot-update ARGS="--all"`；`make status` 全部健康。Knowledge Worker、Gateway Quiz 源码和前端 `index.html` 的宿主/容器 SHA-256 各自一致。专用层级文档 `d47c4b1d-5e11-41a8-9fc0-0ef8c3a44d47` 再次经正常认证 reprocess，执行 `ef20b082c1c047879ff4e55af94a6ab3` 在持久 `preparing` 时**受控物理 KILL** Knowledge Worker 并启动容器：同一 execution 最终 `completed`，该操作仅新增 1 行 execution，文档由版本 #3→#4、28 个切片；PG L3 23/L2 5 个 vector_id 与 Qdrant base 23/`_sections` 5 点集合逐一相等，无多余点。使用真实 DashScope 与真实 PG/Qdrant；KILL 是故障注入，不是自然宕机。前两次旧代码 KILL 后换新 execution 的失败记录仍保留在上文，不能改写为通过。

现有专用 E2E 账号在本地 API 登录成功；库 `kb_10915683cb20` 的 Agent 影响预检 200 且有完整计数合同，来源 200 明示 connector `not_configured`；owner 归档 200、默认详情 404、归档目录包含该库、恢复 200。新建一个与该库关联的 Eval 数据集，零命中失败样本保存为 `pending` revision 1，错误旧 revision 修改返回 409，回读 200。另在一条已有可核权会话上预览并创建内部分享：同一账号读取 200、无登录读取 401、撤销后读取 404。以上是本地真实服务/API/数据库验收，不等于内置浏览器或双账号授权矩阵。

**本次自动化门禁。** `make kb-unit-gate`：2020 passed、1 skipped（缺独立 backfill DSN）；`POSTGRES_CLIENT_CONTAINER=ai-gateway-pg make kb-migration-gate`：150 passed（使用现有 PG 容器客户端完成 dump/restore）；受影响 Gateway/分享/Eval 182 passed；Web Node 188 passed。Web app/node TypeScript、lint、i18n、build、`make harness-check`、`make architecture-boundary-gate`、全部改动 Python 文件的定向 Ruff 与 `git diff --check` 通过。Node 实际 24.14.0，高于项目期望 22；构建给 engine warning。一次扩大到整个 Knowledge 目录的 Ruff 运行发现未修改文件的既有 18 项，已用所有改动文件的 Ruff 通过作本候选判定，不称全目录通过。

**独立 review 与待验。** UI reviewer 找到内部 Quiz token/sessionStorage/localStorage 跨同浏览器切换账号复用，以及 Confluence 成功同步后旧 `last_error` 误报；修复后其定向复核未见原路径残留。Recovery reviewer 找到上述 owner/rule/归档竞态；最后只读复核无确定剩余 blocker。Mac 锁定导致 Codex 内置浏览器返回明确错误，本候选的 UI 刷新/重开及 J09～J13/J30 **仍未验收**。只有一个可确认 E2E 账号，真实双租户双角色/撤权矩阵未跑；连接器未在当前 Runtime 配置，J12 真实增量/失效凭据恢复也未跑。PRD §5 对不支持扫描/OCR的正确拒绝已由 API 验证，页面提示待浏览器验证。**R2 不标完成。**

**前端 smoke 复测。** `pnpm -C web e2e:opensource` 首次因宿主 HTTP 代理把本地 Vite 探活变成 502 而 120 秒超时；设置 `NO_PROXY=127.0.0.1,localhost` 后实际启动 Chromium，旧测试用未验证 localStorage 成绩假定能在分享详情失败时回看，得到 48/49。已改为仅在服务端确认 attempt 结果且详情临时 5xx 时展示已保存成绩；404 撤销必须隐藏旧结果。定向 4/4 及最终整组 **50/50 passed**，均为受控路由数据的无账号写入 Playwright smoke，不是本地 Docker 真实服务的内置浏览器验收。此 UI 修正后再次 `make hot-update ARGS="--frontend"` 成功，Web app/node TypeScript、lint、i18n 与构建均通过；Node 24 engine warning 仍在。

**追加集成 review。** 独立 backend reviewer 发现归档事务误将数据库的 `tenant_role` owner grant 写成 `role`，导致仅靠同租户角色成为 owner 的用户 403；已改为核对 `tenant_role` 与 `subject_tenant_id`，隔离真实 PostgreSQL 用该角色账号完成归档/恢复 1 passed。最后改动通过 `make hot-update ARGS="--knowledge"` 更新 API/Worker。独立 headless `knowledge-eval-cases.spec.ts` 初跑 9/11：两例首次读取尚不存在的 case revision 预期 404 被通用 console-error 断言误当异常；限定这两个新 case 用例允许该预期 404 后 **11/11 passed**。它是路由模拟 UI 验证，真实 Eval 数据库存取以本节前述本地 API 的 revision 1/409/readback 为证。
