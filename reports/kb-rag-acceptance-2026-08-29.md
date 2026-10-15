# 知识库 RAG 升级验收报告（2026-08-29）

## 1. 结论

**最终结论：CONDITIONAL PASS**

本地 `main` 已从 `origin/main@47b7a9b9` 安全快进 RAG 代码至 `157e5918`，
随后提交架构/harness 收敛 `ef824bc3` 和最终 review 修复 `5d43bd6f`；
`worktree-kb-frontend@f75e709b` 是 `worktree-kb-rag-upgrade` 的祖先，因此两个
worktree 的提交均已包含。此前受测代码提交为 `966c9168`，本轮新增逻辑提交为
`73faefa7`、`701b3b0f`、`5d43bd6f`。本轮没有发现仍未解决的 P0/P1 正确性、安全性或数据一致性
缺陷；知识库核心 API、真实 PostgreSQL/Qdrant/Redis 链路、付费模型 QA、真实
双嵌入模型回填、文档进度 SSE、批量部分成功、DashScope Qwen-OCR 单页实机和内置浏览器 UI 主链路均已通过。
最终 review 没有重启共享 Compose 栈或重复内置浏览器；迁移 112 在独立临时
PostgreSQL 16 容器完成全门禁，容器随后删除。既有浏览器证据保持不变。

不能给出 PASS，原因是以下强制发布证据尚未完成：

1. T0 的 200–400 条真实/半真实双语黄金集、来源证明和人工复核未完成；正式
   release pointer 不存在。
2. 真实 `text-embedding-v3 -> text-embedding-v4` 影子回填已成功，但 T0 门禁、
   原子 cutover、真实检索回归和 rollback 没有执行，故 H3 场景 4 仍为 BLOCKED。
3. T6 全局 `bm25_v2` 升级目标已获用户确认，但 T0 gate、全局 cutover、并发
   检索和 rollback 收据尚未完成，H1 #29 仍只能显示阻塞状态。
4. T4 的 50–200 页真实解析评测、T7 的 Helm/Grafana 告警验证、T9 的真实语料
   灰度净收益证据未完成；T8.3 已按用户决定明确为当前 Agent 架构不需要新增。

因此，本报告确认的是：**实现分支 merge-ready；正式发布仍被上述证据和执行
前置条件阻塞。** H1 #4 的 SSE 合同和 H1 #7 的批量部分成功实机证据已在本轮补齐，
不再作为剩余阻塞。

## 2. 范围与代码盘点

验收合同：

- `docs/plans/rag-upgrade-prd-2026-08.md`
- `docs/plans/rag-upgrade-prd-addendum-dify-2026-08.md`
- `reports/kb-rag-ui-t5/backend-dependency-handoff.md`

开始修改前已确认 Claude 任务停止；之后没有并发写入。最终本地 main 相对
`origin/main` 共 357 个路径：200 新增、151 修改、6 删除，最终无未跟踪或
未提交文件。删除项逐一复核如下：

| 删除项 | 复核结果 |
| --- | --- |
| `retrieval_v2.py` | 无运行时引用；旧检索重复实现退役 |
| gateway `knowledge_repository.py` | gateway 直读 KB 表的旧边界退役 |
| gateway `schemas/kb_tools.py`、`schemas/knowledge.py` | schema 权威迁回 Knowledge Service；边界门禁通过 |
| `web/.../SegmentCard.tsx` | 拆分后的空转组件，全仓无引用 |
| `tests/api/test_upload_session_cleanup.py` | 覆盖已迁入知识库上传/清理回归组；相关门禁仍执行 |

检查了 diff、迁移、删除、未跟踪文件、重复实现、生成物和敏感信息；没有发现
凭据或需要提交的构建产物。`git diff --check origin/main...HEAD` 通过。临时脚本、
浏览器附件和数据库备份均未纳入 Git。

## 3. T0–T9 验收矩阵

状态只使用 PASS / FAIL / BLOCKED；“代码已实现”不能替代正式数据或用户放行。

| 主题 | 状态 | 实际证据与缺口 |
| --- | --- | --- |
| T0 评测基础与门禁 | **BLOCKED** | 单元、迁移、真 Qdrant、金夹具数学和回放入口通过；release gate 按设计退出 2：仅 18 条 dev fixture，缺 200–400 条、完整 provenance、人工复核、可验证 source mix 与 `release-pointer.json`。 |
| T1 摄入生命周期与增量 upsert | **PASS** | 状态机、稳定 ID、staging generation、recover/retry、公平认领、reprocess/reembed、禁用/启用/归档闭环均有回归；活栈旧向量持续可读，失败/竞争自动回队。 |
| T2 检索质量 | **BLOCKED** | 中文 jieba lexical shadow 与 12-case 真实 qwen3-rerank bake-off 通过，遥测/阈值/预算/降级/缓存版本已实现；正式默认模型晋升仍受 T0 阻塞。 |
| T3 嵌入蓝绿迁移 | **BLOCKED** | 版本元数据、绑定、持久 202/job、回填/恢复/冲突/取消/授权代码与测试通过；真实 v3→v4 回填成功，但未执行 gate/cutover/rollback。 |
| T4 解析与无损 IR | **BLOCKED** | IR、页块/附件、解析器级联、缓存/并发和 PDF 内存上限有代码与回归；本轮新增真实可视 PDF 单页 Qwen-OCR 调用通过（`qwen-vl-ocr`/`document_parsing`），但 35 页候选文件渲染后为空白，批量尝试出现 `HTTP 400 InvalidParameter.DataInspection`，已排除为质量证据；50–200 页有效真实形状与 TEDS/CDM/VLM 指标仍未完成。 |
| T5 前端优先管理面 | **BLOCKED** | 主管理、段落、评测、遥测、迁移面均已实现并通过 build/mock/内置浏览器；H1 #4 SSE 与 #7 批量部分成功本轮补齐，仍因 H1 #29 的 T0/cutover 前置条件保持阻塞。 |
| T6 BM25 v2 生命周期 | **BLOCKED** | 排他锁、持久 lifecycle、并发、回滚、kill switch 和 UI 收据通过测试；用户已确认全局升级，但实际 backfill 后 gate/cutover/并发检索/rollback 仍受 T0 阻塞。 |
| T7 可观测性与供给可靠性 | **BLOCKED** | `/metrics`、持久队列状态、锁定镜像、PDF offload、Redis 幂等和配置收敛通过；Helm/真实 Grafana 告警未在本轮环境验证。 |
| T8 gateway 契约与权限 | **PASS** | T8.1/2/4 PASS：scope、ACL 权威、gateway 无直读 KB 表/自定义 KB schema；双账户活栈越权返回 403。当前 Agent 已通过 `search_knowledge_base` 的签名 internal retrieval 接入 RAG，KS 不新增 tool 工厂；T8.3 不属于当前 Agent 路径。 |
| T9 父子检索与结构路由 | **BLOCKED** | 父子/摘要/附件结构代码与回归存在；缺真实语料灰度、质量净胜和默认开启证据。 |

## 4. H1 前端 1–29 项矩阵

| # | 管理操作 | 状态 | 证据 |
| --- | --- | --- | --- |
| 1 | 数据集创建与配置 | **PASS** | 内置浏览器创建真实 v4 数据集，刷新后配置持久。 |
| 2 | 数据集删除 | **PASS** | 活栈 API 清理成功；UI 密码门禁有 mock 回归。验收展示数据集按约保留。 |
| 3 | 文件/URL/文本与批量上传 | **PASS** | 内置浏览器真实 PDF；活栈 API 文本与批量；上传进度可见。 |
| 4 | 阶段进度与时间戳 | **PASS** | 迁移 111 持久化数据集范围事件；SSE 返回单调 `dataset_id:sequence`、terminal 和 `Last-Event-ID` 续传；后端 6 个定向回归、真实网关续传 5 个事件，内置浏览器触发 reembed 并显示阶段进度；条件轮询保留为断线 fallback。 |
| 5 | 单文档 reprocess | **PASS** | 内置浏览器 Queued→Completed；API 与状态机回归通过。 |
| 6 | 单文档 reembed | **PASS** | 内置浏览器 Queued→Completed；重复请求 409；旧 generation 持续可读。 |
| 7 | 批量重索引/删除 | **PASS** | 内置浏览器实际制造 1 个禁用文档 + 1 个活动文档的 `1 queued, 1 skipped`；全跳过显示逐项 skipped，启用后重试成功，刷新后状态持久；临时探测文档已清理。 |
| 8 | 失败文档 recover/retry | **PASS** | 持久状态分支、worker 重启和一键重试的集成/mock 回归通过。 |
| 9 | 文档启用/禁用 | **PASS** | 内置浏览器验证 Disabled→Queued→Completed，并核对“重建完成前仍禁用”。 |
| 10 | 文档归档/恢复 | **PASS** | 内置浏览器验证 reason、Archived→Queued→Completed，并刷新确认。 |
| 11 | 类型化文档元数据 | **PASS** | registry、类型校验、批量编辑前后端回归通过。 |
| 12 | 版本历史/比较/恢复 | **PASS** | 原功能保留，Node/mock/后端回归通过。 |
| 13 | chunk 浏览与全字段编辑 | **PASS** | 内置浏览器保存 text、answer、keywords，刷新后持久。 |
| 14 | chunk 启用/禁用 | **PASS** | UI 合同、活栈 API 与 Qdrant 同步回归通过。 |
| 15 | chunk 关键词编辑 | **PASS** | 内置浏览器保存 `AURORA-7291, emergency, 验收`。 |
| 16 | chunk hit_count | **PASS** | 检索后 UI 显示并增长；最终展示值 16。 |
| 17 | 分块预览 | **PASS** | 既有功能保留；前端与 API 回归通过。 |
| 18 | 全模式分块配置 | **PASS** | D6 全字段 schema 往返与前端保存回归通过。 |
| 19 | 检索配置无损保存 | **PASS** | D6/配置 round-trip 测试；内置浏览器预设与配置面可见。 |
| 20 | 检索预设 | **PASS** | 内置浏览器 Balanced/SOTA 实际 A/B 执行。 |
| 21 | hit-testing | **PASS** | `AURORA-7291` 命中 2 段并展示阶段分与引用。 |
| 22 | 评测工作台 | **PASS** | 内置浏览器新增、标注、保存并运行 A/B；两预设最终均 rerank applied。 |
| 23 | 检索结果送黄金集 | **PASS** | UI mock + 活栈持久化 API 验证；没有冒充正式 T0 黄金集。 |
| 24 | QA + SSE + 引用/计时 | **PASS** | 真实 `qwen3.7-plus` 回答、2 引用、16.569s LLM/2.388s retrieval。 |
| 25 | 反馈与理由码 | **PASS** | 内置浏览器点击 Useful；D3 写端与查询读回通过。 |
| 26 | 查询日志/零结果 | **PASS** | 内置浏览器 Query Insights 显示查询、2 hits 和阶段计时；D5 读端通过。 |
| 27 | 嵌入模型蓝绿管理面 | **PASS** | 设置页展示 serving generation、collection、任务与回滚操作；真实 v3→v4 backfill job 成功。发布切换仍归 H3 BLOCKED。 |
| 28 | 集合/索引健康收据 | **PASS** | 内置浏览器展示 collection 与健康收据，未知状态不会伪装健康。 |
| 29 | lexical_v1/bm25_v2 配置 | **BLOCKED** | 用户已确认全局 `bm25_v2` 升级目标；当前仍未悄悄启用，实际切换等待 T0 gate 与全局 cutover/rollback 收据。 |

**H1 总结：BLOCKED**（#29）。

## 5. 后端依赖 D1–D8

| 合同 | 状态 | 证据 |
| --- | --- | --- |
| D1 文档列表补列 | **PASS** | lifecycle、阶段时间戳、错误、archived、embedding generation 等列表字段完整；活栈/UI 验证。 |
| D2 `archived_reason` 宽度 | **PASS** | 迁移与 255 字符边界回归通过。 |
| D3 反馈表/端点 | **PASS** | tenant-scoped upsert/read；内置浏览器 Useful 反馈可在查询侧读回。 |
| D4 文档/段落分页 | **PASS** | cursor/limit 与 >200 批量回归通过。 |
| D5 查询遥测读端 | **PASS** | `/queries` 活栈返回；修复 JSON 字符串 `stage_timings` 导致的 500。 |
| D6 配置字段往返 | **PASS** | `ChunkingConfigSchema` 与 retrieval config 全字段 round-trip 测试通过。 |
| D7 黄金用例删除 | **PASS** | tenant-scoped 持久删除端点及 UI 回归通过。 |
| D8 QA/hit-test `trace_id` | **PASS** | tenant-scoped QA/hit-test 响应已暴露 `trace_id`，前端已接通反馈与 trace-backed 黄金集桥接；schema 回归通过。 |

## 6. H2/H3 七个场景

| # | 场景 | 状态 | 实际或最高保真证据 |
| --- | --- | --- | --- |
| 1 | 修改单段后 reprocess，仅变化块更新且 ID 稳定 | **PASS** | PostgreSQL/Qdrant 集成断言点集差异；内置浏览器编辑后 reprocess 完成。 |
| 2 | reprocess 并发检索无空窗 | **PASS** | publication fence 与 zero-window 集成测试；活栈操作期间旧结果持续可检索。 |
| 3 | indexing 时 worker 中断续跑且不重解析 | **PASS** | 真 PostgreSQL 持久任务的 crash/reclaim 测试，恢复后 `parser_calls=0`。本轮未杀共享活栈进程。 |
| 4 | 嵌入模型切换、中断续跑、cutover、rollback | **BLOCKED** | 真实 v3→v4 影子回填完成；T0 gate、cutover、检索复验与 rollback 未获放行/未执行。 |
| 5 | 文档禁用→启用恢复一致 | **PASS** | 内置浏览器与 API 验证 DB/Qdrant/缓存状态和最终检索。 |
| 6 | 双文档重嵌 + 批量任务公平、无双认领 | **PASS** | 两租户公平队列/`SKIP LOCKED` 真 PG 测试；活栈 batch contention 自动回队。 |
| 7 | 重嵌失败保留旧向量并可重试 | **PASS** | Qdrant/PG publication 与故障注入测试；活栈 lease contention 不清空旧代并自动回队。 |

**H2：PASS。H3：BLOCKED（场景 4）。**

## 7. 实际执行的门禁与结果

| 命令/操作 | 结果 |
| --- | --- |
| `make harness-check` | PASS：36 targets、18 required docs、99 links；1 个已知 warning：`deploy/runbooks/kb-rag-ui-t5` 的旧 loop-state schema。 |
| 触及 Python 路径的 `ruff check` | PASS，0 error。 |
| `uv run --all-packages --extra test pytest -q --no-cov tests/services/knowledge/test_document_progress_sse.py` | PASS：9 passed；覆盖 fresh watermark、有效续传、过期/未来游标 reset、terminal、重复/非单调行、非法游标、403/404/503。 |
| `node --experimental-strip-types --test web/src/lib/sseEventParser.test.ts web/src/pages/knowledge/detail/documentProgress.test.ts` | PASS：4 passed。 |
| `make kb-unit-gate` | PASS：1835 passed，2 skipped（缺专用 backfill PG DSN；共享 Qdrant 栈未启动）。 |
| 隔离 PostgreSQL 16 + `make kb-migration-gate` | PASS：132 passed；包含 100–112 三种布局/restore/账本链和 112 的 7 日/10k retention 实测。临时容器已删除。 |
| `pytest tests/scripts` 对应门禁 | PASS：357 passed，2 skipped（专用 KB DSN；本地无 jieba 的分支）。 |
| 备份安全回归 | PASS：仓库路径及 `..`/symlink 解析后 fail-closed；外部目录创建成功、文件 mode `0600`、失败无残片。 |
| 真实中文 lexical shadow（含 jieba） | PASS：simple 0.167、bigram 0.833、jieba 0.917；BGE-M3 未配置，明确 skipped。 |
| `make gateway-kb-boundary-gate` + gateway KB 回归 | PASS：114 passed。 |
| 前端 Node 单测 | PASS：94 passed。 |
| `pnpm -C web lint` / i18n / build | PASS；`oversizedRoutes=[]`。Node 24 与项目期望 Node 22 有 warning，但未影响结果。 |
| `pnpm -C web type-check` | exit 0，但确认它是假门禁，不作为证据。 |
| `pnpm exec tsc --noEmit -p tsconfig.app.json --ignoreDeprecations 6.0` | exit 2：97 个仓库存量诊断；与干净 base 差分后本分支新增 0，Knowledge 路径 0。 |
| mock Playwright 全量 | PASS：40 passed（46.9s），含持续 >30s 后出现 503 的轮询场景。 |
| `make kb-golden-gate` | PASS：18 条 dev fixture；不等同正式发布集。 |
| `make kb-image-lock-gate` | PASS：6 passed。 |
| `make kb-release-evidence-gate` | **BLOCKED（预期 exit 2）**：5 个正式证据缺口，见 T0。 |
| 真实 qwen3-rerank bake-off | PASS（开发证据）：12 case，nDCG 0.967911，MRR 1.0；不是 release evidence。 |
| 共享栈 `make migrate` + `make migrate-status` | 既有证据：迁移 111 已登记。最终 review 未启动共享栈；迁移 112 仅在隔离 PostgreSQL 完整验证，部署时仍须正常执行 `make migrate`。 |
| `make hot-update ARGS="--all"` | PASS：源代码与前端产物更新到功能 worktree；未执行 pip install 或镜像重建；Node 24 对项目期望 Node 22 仅有 engine warning。 |
| `make validate` + `make status` | PASS：PG/Redis/Qdrant/Knowledge/worker/Gateway/frontend/Agent Runtime 健康；仅本地默认密码 warning。 |
| `docker compose config --quiet` + `docker compose up -d --no-build knowledge-service knowledge-worker` | PASS：先核对 Compose owner 指向功能 worktree；仅重建知识服务容器以应用 OCR 专用环境变量，没有删除 volume 或重建镜像。 |
| `uv run ... pytest` 受影响 OCR/配置/worker/容器分发集合 | PASS：86 passed，1 个既有 Starlette warning；覆盖 Qwen 原生 payload、OCR 独立端点、启动配置、worker provenance 与 env 注入。 |
| `bash -n scripts/new/init-env.sh` + `git diff --check` | PASS。 |
| DashScope Qwen-OCR 真实单页 smoke（重建后） | PASS：真实可视 `/Users/yang/Downloads/PDF文档/导入备份文档.pdf` 第 1 页；运行时报告 `provider=dashscope`、`model=qwen-vl-ocr`、`task=document_parsing`、非空输出 34 字符。未输出正文或凭据。 |
| 35 页候选扫描批量尝试 | **未验证/排除**：`Agent-工程核心模块教学白皮书.pdf` 的渲染页实际为空白；批量请求产生 `InvalidParameter.DataInspection`，且流式计数异常，未计入任何 PASS。 |
| 活栈 API 回归脚本 | PASS：64 个请求断言；文本/PDF、检索、QA、反馈、遥测、生命周期、批量、迁移、409/404/403、刷新持久性和清理。 |
| 实际网关文档进度 SSE（`Last-Event-ID=kb_35d11ebc320f:6`） | PASS：HTTP 200，续传 5 个事件（7–11），dataset-scoped、严格单调并包含 terminal。 |
| 内置浏览器人工回归 | 核心 UI 主链路 PASS；详见 §9。 |

没有把 503 mock、开发 fixture、聊天模型 QA 或影子 backfill 分别冒充真实健康
服务 503、正式黄金集、嵌入模型切换 gate/cutover/rollback。

## 8. Docker、代码版本与迁移状态

在 Docker/E2E 前已阅读 `docs/harness/runtime-and-secrets.md`。没有并行启动冲突
栈；根 checkout 的栈先安全停止，未删除 volume、未 prune。根数据库备份
`gateway_20260829_064104.sql.gz`（约 80 MiB）已按最终 review 从仓库移至
operator-owned 外部目录，移动前后 SHA-256 一致，权限收紧为 `0600`。仓库内
不再存在或忽略任何备份副本；`make backup` 也已改为强制仓库外目标。

Compose 标签确认 `config_files`、`working_dir` 均指向本功能 worktree。镜像：

| 服务 | 镜像 ID |
| --- | --- |
| Gateway | `sha256:3f95168a97cb32ed94a3d890f04c8cfaa1b80a195b3f442ccf125a61f9406480` |
| Frontend | `sha256:22844d4f0b1deebfa848ac267c6517d023c1e80d6aaa3109991c94b5f677f418` |
| Knowledge / Worker | `sha256:38bc87f86979dcd7d8509cbdff577e5fd69cf42f6f9119e2bb610b7d3229a6ff` |
| Migrate | `sha256:3a920302ab6341572370c83a09449d48e8fd34641d1befa4a25dafa2b2c8a66c` |

合并 main 后使用 `make hot-update ARGS="--all"` 更新 Python 源码和本地前端构建，
避免无必要的依赖层重建；Knowledge、Gateway、Frontend 的宿主/容器文件 SHA
逐项一致。上述镜像 ID 是构建基底，热更新后的容器文件校验才是“实际运行
HEAD 代码”的证据。

本轮 `701b3b0f` 未修改依赖锁、Dockerfile 或镜像构建输入，镜像 ID 保持不变；
最后一次 `make hot-update ARGS="--all"` 后，Knowledge worker 内的
`vlm_ocr_service.py` 与 `worker.py` SHA-256 均与功能 worktree 相同，运行时配置
为 `dashscope/qwen-vl-ocr/document_parsing`。随后 `make validate`、`make status`
和真实 Qwen 单页 smoke 均在该热更新栈上执行。

迁移 100–105 在 fresh public layout、从 main schema 升级、已完成 per-service
split 的旧库中执行；canonical CLI 与 shell 都设置 Knowledge-first
`search_path`。legacy `run_migration.py` 对这些迁移 fail-closed。重复启动、账本、
约束、并发 runner 和数据保留均通过。隔离 fresh/public/main/split 门禁验证迁移
100–112 共 13 条进入 public ledger；共享运行栈此前登记至 111，部署本提交时
必须由 canonical `make migrate` 应用 112。

100–105 新增的 8 张表均位于 `knowledge` schema，owner 为执行 canonical runner
的数据库角色（本地为 `postgres`），不存在 public/其他 schema 重复表：

- `dataset_collection_bindings`
- `document_pipeline_executions`
- `embedding_migration_progress`
- `embedding_migrations`
- `embedding_vector_cache`
- `kb_bm25_v2_lifecycle`
- `kb_eval_golden`
- `kb_eval_golden_release`

迁移 111 新增 `knowledge.kb_document_progress_events` 和
`knowledge.record_kb_document_progress_event()` 触发器；真实运行栈探针验证了
waiting/parsing/splitting/indexing/completed/deleted 的单调事件序列，删除临时
文档后事件收据仍可续传。事件读取仍受 dataset viewer scope 保护。

迁移 112 为事件账本增加 7 日与每数据集约 10k 的保留边界。Fresh SSE 连接从
当前 dataset watermark 开始，不再重放全部历史；合法 `Last-Event-ID` 保持无损
增量续传，裁剪后或数据库恢复造成的失效游标收到单个 `reset` 并刷新权威快照。
真 PostgreSQL 测试验证 10,005 条种子事件清理为 10,000 条且过期事件为 0。

迁移 101 会改写状态/hash、替换唯一约束并扫描/锁表。N-1 Knowledge binary 不能
作为安全回滚路径；本升级明确标记为 **restore-required**。真 PostgreSQL
`pg_dump -> migrate -> pg_restore` 验收通过，不能把“部署旧镜像”写成已验证回滚。

## 9. 内置浏览器实机验收

使用仓库规定的专用 E2E 账号，在 Codex 内置浏览器完成登录；未修改管理员密码，
未打印或保存凭据。创建并保留数据集：

- 名称：`iab-rag-acceptance-1788003121114`
- ID：`kb_35d11ebc320f`
- 当前页面：`http://localhost:8081/knowledge/kb_35d11ebc320f?tab=documents`
- 嵌入：真实 `text-embedding-v4`

实机结果：PDF 上传→摄入完成（2 segments）；`AURORA-7291` 检索命中 2 段；
付费 `qwen3.7-plus` QA 返回正确答案和 2 个引用；反馈、Query Insights、
hit_count、段落全字段编辑、文档禁用/启用、归档/恢复、reembed、reprocess 均
完成并在刷新后保持。评测工作台完成用例新增、标注、保存和 Balanced/SOTA
A/B；一次 SOTA 瞬时 fallback 被 UI 如实显示，随后的重试两侧均为
`dashscope/qwen3-rerank` applied、IR 指标通过。设置页展示迁移任务、集合健康和
H1 #29/T6 的 gate 阻塞状态（目标已确认，不是用户决策待定）。当前 Agent 的
RAG tool bridge 走既有 `search_knowledge_base` capability；本轮未新增 KS
tool 工厂或 `/kb-tools` 检索面。

修复阶段时间戳后，再次在内置浏览器触发 reembed：新 generation 不再显示上一
代的 “Splitting 2m51s”，最终 Completed。数据集故意保留，便于用户直接查看。

本轮新增实机证据：在同一展示数据集触发 reembed，文档行实际经历 Queued→
Completed；浏览器打开文档页期间网关日志记录了
`GET /api/v1/knowledge/kb_35d11ebc320f/documents/progress -> 200`，并由前端
按事件刷新列表，原有条件轮询仍作为 fallback。为验证批量部分成功，临时创建
并摄入一篇文本后将其禁用，与原 PDF 一起批量 reembed，UI 显示
`1 queued, 1 skipped`；全跳过场景显示逐项 skipped，启用后文档先保持 disabled
直至重建完成，再次 reembed 成功。点击 Refresh Data 后两条状态均持久；最后仅
删除本轮创建的临时文档，刷新确认展示数据集恢复为原 PDF 单文档、Completed、
16 hits。内置浏览器控制台 error/warn 为空。

## 10. 本轮发现并修复的问题

| 严重度 | 问题 | 修复与回归 | 提交 |
| --- | --- | --- | --- |
| P1 | Qdrant 非瞬态 404 被重试，删除可能卡住 >120s | 404 分类为终止错误；14 个定向测试；活栈清理约 0.64s | `4e332b24` |
| P1 | Compose 将幂等后端降级为 memory | 目标栈强制 Redis，验证跨副本 fail-closed | `4e332b24` |
| P1 | `/queries` 遇到字符串化 `stage_timings` 返回 500 | 行解码统一 JSON 反序列化并补回归 | `4e332b24` |
| P1 | API 角色的 enable/unarchive 错判“无 worker” | 接入 durable enqueue proxy；路由与服务测试 | `4e332b24` |
| P1 | batch reembed 与蓝绿 publication lease 竞争会把文档标成普通失败 | 明确传播 lease contention，worker 原子回队；旧向量不清空 | `4e332b24` |
| P1 | 迁移 action 是同步 HTTP，可能超过 Gateway 30s | backfill/verify/gate 改 durable `202 + job_id`，覆盖断连、重试、并发 | `4e332b24` |
| P1 | 100–105 未限定对象名且 legacy runner 继承默认 search_path | canonical Knowledge-first、全限定对象、legacy fail-closed；三种 layout 真库测试 | `4e332b24` |
| P1 | UI 暗示“清理后重试”，与旧代持续可读合同冲突 | 文案和行为改为原子完整重试；失败/取消恢复旧代 | `4e332b24` |
| P1 | 新 generation 继承旧阶段时间戳，UI 显示虚假长耗时 | 仅新 generation 清理阶段时间戳；same-generation recover 保留 | `966c9168` |
| P1 | >30s mock 在响应落地前断言，偶发失败 | 等待注入 503 后再断言；产品持续轮询语义不变 | `966c9168` |
| P1 | 文档进度只支持条件轮询，缺少 PRD 要求的 SSE/`Last-Event-ID` | 迁移 111 追加事件账本与触发器；Knowledge SSE 端点完成 scope/单调 ID/续传/terminal/断开清理/403/404/503；后端 6 个定向回归、前端 4 个游标/解析回归和真实网关续传通过 | `73faefa7` |
| P1 | 批量部分成功未做内置浏览器实机验证 | 真实禁用+活动文档批量操作得到 `1 queued, 1 skipped`；全跳过逐项提示、启用后重试、刷新持久性均通过；临时文档已清理 | `73faefa7` |
| P1 | OCR 默认仍为 Gemini，DashScope 适配未使用 Qwen-OCR 原生任务/独立地域端点 | 默认切换为 `dashscope/qwen-vl-ocr`；原生 OCR `document_parsing`、像素边界、自动转正、`max_tokens`、独立 OCR key/base URL、资源关闭和文档 provenance；定向回归与真实单页调用通过 | `701b3b0f` |
| P1 | Fresh SSE 从 sequence 0 重放无界历史，事件账本无 retention | Fresh 从 watermark 开始；合法游标增量续传；过期/未来游标 reset；迁移 112 强制 7 日/约 10k 保留 | `5d43bd6f` |
| P1 | `make backup` 默认把敏感 dump 写入开源仓库且文件为 `0644` | 默认改为 XDG/HOME 外部状态目录，仓库路径 fail-closed，创建权限 `0700/0600`；现有 80 MiB dump 校验后移出仓库 | `5d43bd6f` |
| P1 | OCR 初始化前 Redis fail-closed 会在清理阶段触发 `UnboundLocalError` | 生命周期入口预置 OCR 资源句柄；原始失败测试和 KB 全门禁通过 | `5d43bd6f` |

功能提交与验收提交保持可审查分组：

- `62587dc9 feat(kb): implement RAG upgrade backend and migrations`
- `df6536a4 feat(web): complete knowledge upgrade workflows`
- `4e332b24 fix(kb): close acceptance and release-gate gaps`
- `f8c91b05 Merge remote-tracking branch 'origin/main' into worktree-kb-rag-upgrade`
- `966c9168 fix(kb): close live acceptance findings`
- `73faefa7 feat(kb): add durable document progress SSE`
- `701b3b0f feat(kb): use DashScope Qwen OCR for scanned PDFs`
- `5d43bd6f fix(kb): close final acceptance review findings`

## 11. 未验证项与执行前置条件

交给后续 Luna 或发布负责人时，不应再花时间重复已通过的 1835 单测和主 UI
链路；优先处理：

1. 建立 200–400 条正式黄金集、provenance、人工复核和 release pointer，重跑
   `make kb-release-evidence-gate`。
2. 在允许的测试租户执行完整 v3→v4 gate→cutover→并发检索→rollback；记录
   断连、续跑和回滚收据。
3. 在 T0 满足后按用户已确认的全局目标执行 `bm25_v2` backfill→gate→cutover→
   并发检索→rollback，并记录全局收据。
4. 完成 T4 真实长 PDF 指标、T7 Helm/Grafana 告警验证和 T9 真实语料灰度。
5. 补充可视且内容有效的 50–200 页扫描/双栏/表格/公式语料；当前
   `/Users/yang/Downloads/PDF文档` 中没有可证明的有效大扫描样本，不能用空白页或
   供应商失败响应替代 T4 质量证据。Qwen-OCR 已作为扫描页默认 OCR 接入，但专项
   表格/公式/定位任务和备用解析器的质量门控仍需真实语料后执行。

## 12. 与 main 的集成方式

本地 `main` 已使用旧值守卫从 `47b7a9b9` 快进 RAG 代码至 `157e5918`，随后线性
提交架构/harness 与最终 review 修复至 `5d43bd6f`；没有改写历史、rebase、amend
或 force push。`worktree-kb-rag-upgrade` 与
`worktree-kb-frontend` 均已由 `git merge-base --is-ancestor` 验证为 `main` 的祖先。

集成时先对除两个重叠文件外的目标分支补丁执行了 `git apply --check --index`，再
执行 `git apply --index`；两个重叠文件仅做三方内容合并，并用
`git diff --cached --quiet worktree-kb-rag-upgrade` 验证索引等于目标树。根 checkout
原有 27 个已修改 tracked 文件和 5 个未跟踪路径的集合前后完全一致；两个重叠文件
保留了根 checkout 的架构/harness 改动，同时加入目标分支的 KB 合同内容：

- `docs/harness/commands.md`
- `harness.yml`

最终 review 已补充真迁移、KB、scripts、前端和备份安全回归；没有重启共享栈或
重复浏览器。未验证发布项仍保持 BLOCKED/CONDITIONAL PASS，不因代码合入改变。
