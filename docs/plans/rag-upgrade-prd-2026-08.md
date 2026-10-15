# 知识库 RAG 全面升级 PRD（2026-08）

Status: 已审计，可作为实施输入。2026-08-28 经 14-agent 对抗审计工作流
（7 维度找错 + 逐维度独立反驳验证）：30 条原始发现、20 条幸存，其中
13 项已按幸存发现修正、1 项在审计前已修正；幸存发现的代码引用经主会话
抽查复核（附录 C）。本文档是下一轮实施 session 的唯一输入；它合并并取代
`docs/plans/kb-rag-optimization-plan.md`（2026-07-20）的剩余项，与
`deploy/runbooks/platform-plane-restructure/`（PPR）和
`docs/plans/knowledge-bm25-v2-shadow-rollout.md` 划清边界（§9、附录 B）。

证据基础：10 个并行审计 agent 对 `apps/knowledge-service/`（84 个 Python 文件，
约 64k LOC，含 gateway 侧与 web 侧）的逐文件审计；对本地 Dify 1.17.0 全部知识
库子系统的 12 区域拆解；2026-08 业界 SOTA 六主题调研（chunking / retrieval-rerank /
embeddings / parsing / eval / architectures）。所有结论附文件路径或文献依据；
没有依据的判断标记 **[判断]**。

---

## 0. 摘要

我们的知识库微服务在**检索管线主干**上已经处于业界良好水平：真正的混合检索
（dense + BM25 双后端 + RRF/加权融合 + 语言自适应权重）、交互式延迟预算、
revision 感知的缓存、进程级 api/worker 分离、fail-closed 的多租户过滤、以及一个
可用的离线评测台（F1/F4 已交付）。**但它有三类系统性欠账**，构成本次升级的理由：

1. **运营闭环缺失**：没有任何生产查询遥测（`dataset_queries` 只有表没有写入）、
   没有 CI 会跑 KB 测试（`rag-eval-regression-gate` 回放的是预制 fixture，根本不
   导入 `knowledge_service`）、没有任何真实语料上的检索质量基线。所有"质量提升"
   主张当前都不可验证。
2. **生命周期操作不是热更新**：重新摄入会对整份文档 100% 重处理（无位置/内容哈希
   跳过）；前端虽已有单文档重嵌按钮，但无阶段进度/队列可视、无换参数重处理、
   无法编辑/禁用单个 chunk、无法管理嵌入模型迁移；禁用/启用、删除与索引状态
   之间没有统一的状态机合同（agent-kb-eval §4.6 六项正确性债务：#1 大体落地但
   缺统一合同与缓存失效、#2 只做一半、#3/#4 未修、#5/#6 本 PRD 显式延后，见 §10）。
3. **模型代际窗口**：2025-06 至 2026-08，双语嵌入/重排的开源参考点已经换代
   （Qwen3-Embedding / Qwen3-Reranker / jina-reranker-v3），文档解析进入
   端到端小型 VLM 时代（PaddleOCR-VL / MinerU2.5 / GLM-OCR，全部中文厂商、
   中文一等公民）。我们缺少带版本治理的再索引能力，每次模型更替都会变成事故
   而不是脚本。

本 PRD 定义 10 个升级主题（T0–T9）、4 个阶段、以及三条来自用户的硬性验收：
**(H1) 前端优先**——所有知识库管理操作都能在 web 控制台完成（§5 操作覆盖矩阵）；
**(H2) 单文档嵌入更新**——从 UI 对任意单个文档触发重新解析/重新嵌入；
**(H3) upsert 热更新**——一切重建/重嵌语义为稳定 ID 下的 upsert 或
蓝绿切换，**不存在**"先删后建"的可检索性空窗。

---

## 1. 背景与目标

### 1.1 用户硬性要求（本 PRD 的验收锚点）

| 编号 | 要求 | 验收位置 |
| --- | --- | --- |
| H1 | 所有数据库（知识库）管理操作都可以在前端进行 | §5 覆盖矩阵全绿 |
| H2 | 文档可以单独更新嵌入（不必整库重建） | §6.2 单文档重嵌契约 |
| H3 | 重建/重嵌必须是 upsert 热更新（无空窗、无停机、无半成品 revision 暴露） | §6.1/§6.3 稳定 ID + 蓝绿契约 |

### 1.2 为什么是现在

- **评测台已就位但空转**：`retrieval_metrics.py`、`/knowledge/retrieval/presets`、
  `RetrievalEvalWorkbench.tsx`（commit 58e4cd4）都在，但黄金集、真实语料基线、
  CI 门禁都不存在。先补上这一层，后面每个质量改动才有裁判。
- **PPR 已在执行期、但尚未触碰 KB 的契约面**：active_phase=PPR-00，
  PPR-00 的时序门禁 G1–G5 已冻结入 git（2026-08-28，commit ad7777a）。它
  **不会**搬迁或重写 knowledge-service——后者原地成为 "Index plane"。但
  PPR-02 的 plane-boundary gate、PPR-06 的 worker 水平扩容与"交互式检索
  profile"会冻结一批接口。**现在**做升级，还来得及把契约改对；等 PPR-02/06
  落地后再改，成本翻倍（§9 边界清单）。
- **BM25 v2 切换无人认领**：shadow 写入与回填机制已建成且质量很高，但 active
  cutover 被硬禁用，等待一个"跨 PostgreSQL/Qdrant 生命周期协议"，而没有任何在途
  计划负责造它。本 PRD 接手（T6）。
- **模型窗口**：Qwen3-Embedding/Reranker（Apache-2.0）、PaddleOCR-VL-1.6
  （OmniDocBench v1.6 #1，96.34）等在 2025-06 之后才到位；再等一年意味着
  中文检索质量天花板继续压低。

### 1.3 非目标（摘要，完整论证见 §3.3）

默认路径不引入：语义分块、agentic 分块、命题分块、late chunking（BGE-M3 下
实测 3.5× 劣化）、GraphRAG/RAPTOR、HyDE 默认开启、答案级缓存。全部附证据。

**本次升级不将 knowledge-service 迁移到 Rust**（即使核心 agent 已是
Rust）。理由：

1. **程序冲突**：Rust 化是 PPR（platform-plane-restructure）的领地——
   knowledge-service 原地成为 Index plane，PPR-06 已拥有 worker 水平扩容
   与交互式检索 profile。平行再来一个 Rust 迁移 = 与在途程序双重建造、
   双重所有权（§9）。
2. **风险叠加**：功能升级（状态机/蓝绿/评测门禁）与语言迁移各自都是大改；
   同时做意味着迁移期间评测基线失明、回滚路径不可用——恰是 T0 纪律
   禁止的事。
3. **收益错配**：检索主干是 I/O 编排瓶颈（等 Qdrant/PG/重排服务），不是
   CPU 瓶颈，Rust 收益有限；质量差异来自算法杠杆（重排器、词法修复），
   与实现语言无关；重计算（解析/OCR/嵌入推理）本来就交给外部引擎。
4. **本升级改做"Rust-ready"**：PostgreSQL 为唯一事实源、向量索引可重建、
   公开合同严格增量、分块边界字节一致、配置定义的管线——PPR-06 完成后，
   迁移是换执行底座，不是重写语义。

---

## 2. 现状盘点（10-agent 审计结论）

### 2.1 已经强、必须保留（keep-list）

检索主干：

- 真混合检索：Qdrant dense + 两条独立词法腿（PostgreSQL GIN FTS 重打分 /
  Qdrant 原生 sparse），rank 级 RRF(k=60) 或加权融合，语言自适应权重
  （zh 0.75/0.25、en 0.7/0.3、ar 0.55/0.45）——免疫跨尺度分数失真。
- Qdrant 原生混合快路径：单次 `query_batch_points` 完成 dense+sparse prefetch
  + 服务端 RRF（`hybrid_search_multi_native`）。
- 交互式延迟预算是一等机制：ContextVar deadline（默认 3.0s）、重试 ≤2、
  2.0s 熔断健康收据；`stage_timings` 已按阶段埋点。
- fail-closed 多租户：每个点写携带 tenant/dataset/document/segment/revision，
  所有 search/scroll/count/delete 强制租户过滤（调用方无法绕过）；数据集创建
  INSERT-only + 冲突拒绝（migration 082）。
- rerank 供应商韧性：冷却期（300s 永久/30s 瞬态）、BGE→DashScope 自动回退、
  LRU 缓存、指纹化缓存键。
- 确定性检索评测：`/retrieve_evaluate`（hit-rate/precision/recall/MRR/nDCG/MAP@k）
  + 前端 RetrievalEvalWorkbench A/B 对比 + `executionEvidence()` 防伪造。

工程底座：

- api/worker 进程分离（`KNOWLEDGE_RUNTIME_ROLE=all|api|worker`）+
  `DurableEnqueueProxy`——PPR-06 明确"不要重做"。
- PostgreSQL 持久摄入队列：原子 claim/lease（`claim_document_for_enqueue`、
  `document_index_update_lease`）+ 60s 卡死恢复循环；重启不丢活。
- 真正的优雅排水：DRAIN + readiness 503+Retry-After + 有界 join + 取消安全重入队。
- revision 感知检索缓存（键绑 `dataset_revision_fingerprint`，写后不可能读到旧值）。
- 文档版本历史 + 双向 diff + restore（`DocumentVersionHistory.tsx`）——同类
  KB 产品中罕见，是重索引工作流的安全网。
- 上传分块预览（临时 + 按数据集两条路由），10 种分块模式在上传时可配。
- BM25 v2 shadow 状态机：两阶段影子写、授权回填（签名 manifest + sha256
  确认）、收据按 endpoint/client/profile 键控、"schema-ready ≠ 质量证据"的
  证据边界——**任何未来的索引版本化都应复制这个模式**。

已修复、不得再修（PPR R8：重做已验证修复项属计划违规）：

- SPO K1 `_cached_get_collection` 30s TTL（vector_store.py:179）；K2 CPU 抽取
  → `asyncio.to_thread`；K3 interactive profile（vector_k=12 / keyword_k=12 /
  candidate=max(top_k*2,24)）；K4 embedding 超时 + 信号量并发；
  进程分离（2026-08-26 认证）。

### 2.2 关键欠账（按依赖序，全部有文件证据）

**A. 验证层（阻塞一切质量主张）**

| # | 欠账 | 证据 |
| --- | --- | --- |
| A1 | 没有 CI job / Make gate 跑任何 KB 测试（45 个测试文件只能手动跑） | Makefile、`.github/workflows/ci.yml` grep knowledge/ragas 零命中 |
| A2 | `apps/knowledge-service/**` 的声明门禁 `rag-eval-regression-gate` 看不到 KB 代码：只回放 12 条预录 fixture，从不 import `knowledge_service` | 仓库根 `harness.yml`:175-177、scripts/eval_rag.py |
| A3 | 零真实 Qdrant 验证：所有 Qdrant 交互都是 fake；`docker-compose.kbms.yml` 无任何测试引用 | tests/services/knowledge/* |
| A4 | KB SQL 从未在真实 Postgres 上执行；迁移 065/066/082 无测试 | tests/database/* 只覆盖 agent/assistant |
| A5 | 无真实语料检索质量基线；K2 消融（dense→lexical→RRF→rerank→MMR）全未做 | agent-kb-eval-20260802 §6/§7 |
| A6 | RAGAS 判官未校准、未版本化；失败塌缩为 score=0 且被当作"未评分" | ragas_eval_service.py:236-344 |
| A7 | KB e2e 不进 CI（`e2e:opensource` 只跑 6 个 spec），且 knowledge-rag-eval.spec.ts 大量 route-mock | web/package.json:16 |

**B. 生命周期与热更新（H2/H3 的直接阻塞）**

| # | 欠账 | 证据 |
| --- | --- | --- |
| B1 | 增量跳过机制存在但被预清理中和：ingestion_service.py:642-700 已实现（位置, sha256）比对，但 worker.py `_sweep_document_generation`（:438→:641-722，在 ingest 前调用）删除该文档全部点与行 → 哈希表恒空、100% 重嵌、ID 全部轮换、sweep 至重建完成之间检索空窗、失败即零向量；另有哈希类型漂移（迁移 023 回填 MD5 vs 运行时写 sha256），复活跳过前必须先统一重哈希 | worker.py:438/641-722/797；ingestion_service.py:642-700；023_segment_content_hash.sql |
| B2 | 单文档重嵌端点已存在（`POST .../documents/{doc}/reindex`→worker 队列、已排队返回 409），前端也已有入口（文档行 reindex 按钮+确认对话框+批量重索引对话框），但语义是先删后建（见 B1）；真实缺口：无阶段进度可视、无优先队列、无换参数重处理（reprocess）路径；`batch-reindex` 全量模式被 200 条文档列表上限静默截断（document_service.py:563 硬编码 limit=200；persistence 默认 100 无活跃调用者）；前端 `updateSegment` 只发 `{text}` | knowledge.py:978/:2077；web/src/api/knowledge.ts:167-170；DatasetDetail.tsx:734-752/:1537；DocumentRow.tsx:175-227 |
| B3 | 禁用/启用/归档的两套机制各自成立但无统一合同：文档禁用 = DB 标记 + 删除所有 owned collection 的该文档点（保留 segment 行，可重放标记/租约/重试，document_service.py:1154-1205），恢复 = 清行+重新入队（全量重建，:1207-1299）；片段禁用 = DB enabled 标志 + Qdrant payload `{enabled}` 双写（禁用 DB-first、启用 Qdrant-first，点保留、幂等可重试，vector_store.py:2705-2833）；PG FTS 已过滤 enabled/status/archived/生命周期标记（database.py:3424-3443）。缺口：两套语义无统一状态机合同、无检索缓存联动失效、文档恢复是全量重建而非热更新 | agent-kb-eval §4.6-1；document_service.py:1154-1299；vector_store.py:2705-2833 |
| B4 | 修订原子性只做了一半：嵌入失败已"拒绝部分替换"、upsert 失败保留旧向量、DB 写失败有补偿删点（`_persist_segment_batch`）；但子批中途失败仍留下已 upsert 的重复点、无 staging revision 原子翻转、旧向量清理失败仅告警不回滚 | ingestion_service.py:978-1031/1251-1284；agent-kb-eval §4.6-2 |
| B5 | MMR fill-remaining 会把被多样性拒绝的候选重新加回 | agent-kb-eval §4.6-3 |
| B6 | PG 'simple' FTS 与 multilingual tokenizer 不一致，无零结果回退 | agent-kb-eval §4.6-4 |
| B7 | 嵌入模型/维度变更 = 新建集合（`kb_{dataset_id}_{dimension}`），segments 存在即禁止改维度；无迁移机制、无旧集合善后；嵌入身份只存于 dataset 级（documents/segments 无模型出处列）——换模型即冷启动，且无法回答"哪些向量是哪个模型产的" | 集合命名 + PUT config 维度守卫；datasets.collection_name |
| B8 | BM25 v2 active 切换所需的跨存储生命周期协议无人建造 | bm25-v2 doc §Rollout-5；PPR §4.2 明确不管 |

**C. 检索质量细节**

| # | 欠账 | 证据 |
| --- | --- | --- |
| C1 | `dataset_queries` 只有 schema 没有写入——零生产查询遥测 | 全库仅 persistence/datasets.py:1226 的 DELETE 引用 |
| C2 | rerank 失败**已**降级为融合序（stage-4 broad except 记 `meta["rerank_error"]`、不失败请求，retrieval_service.py:2239/:2334-2335），但无显式降级契约/可观测旗标；且 reranker httpx 30s 超时与 3s 交互预算完全脱钩（预算只约束 Qdrant 调用） | retrieval_service.py:2239-2335；text_reranker.py:237 |
| C3 | Python 侧 BM25 的 IDF 只在候选池上计算——默认 `max(keyword_k×3, 50)`（即默认 50）、上限 500（retrieval_service.py:971-981，200 是历史值）——池内相对分，跨查询不可比；召回被池大小硬截（默认 50，比历史值更紧） | retrieval.py:392-432；retrieval_service.py:971-981 |
| C4 | 死代码陷阱：`retrieval_v2.py`（808 行，零生产 import；仅 tests/services/test_multimodal_rag.py:28 依赖其 dataclass；常量与活路径不一致）；多模态表面 ~1700 行不可达但挂在路由上（knowledge.py:1287/:1652 → 无条件 raise） | retrieval_v2.py、retrieval_service.py:2680/3011；tests/services/test_multimodal_rag.py:28 |
| C5 | 检索缓存逐出是 FIFO 不是 LRU；全部进程本地 | cache_manager.py |
| C6 | SPO K3 收窄召回（20→12）上线时未复跑质量评测——一个潜在质量回归躺在性能修复里 | sota-spo-remaining-2026-08.md:260-263 |
| C7 | HierarchicalIndexer 存在但未注入默认 worker（"代码开、生产默认关"），层级路径还带一个失败归因 bug | sota-microservice-review-2026-08-17 §5.1 |

**D. 前端（H1 的差距，详见 §5）**

- 三套互相矛盾的检索默认值（hit-test 0.5/0.5 weighted、settings rrf 0.7/0.3、
  types alpha 0.75、create 向导 threshold 0.2）——违反七月计划 F2/R1。
- `handleSaveRetrievalConfig` 有损：rrf 策略下发 `alpha`、丢弃用户设置的
  BM25 权重、硬编码 `rrf_k=60`（DatasetDetail.tsx:608-648）。
- chunk 级操作面缺失：无 hit_count 显示、无启用开关、无关键词编辑（F10 未做）；
  设置页分块保存只发 `{mode, chunk_size, chunk_overlap}`，其余分块模式参数静默丢失。
- 评测工作台用例是易失的（useState，上限 20），与平台 eval 区（golden JSONL、
  trace 浏览器）零桥接（F6 未闭环）。
- 全库无反馈（thumbs）入口；`DatasetDetail.tsx` 3929 行上帝组件。
- 摄入进度靠 2s 轮询（useKnowledge.ts）；文档列表搜索无防抖（~1582 行）。
- 控制台规模化欠账：文档列表硬上限 200（document_service.py:563 显式
  limit=200，无分页参数）、片段列表硬上限 500；
  `list_datasets` 每数据集做 (1+角色数) 次权限查询（200 数据集 = 数百次
  DB 往返，纯 N+1）；批量操作全部逐项循环（500 段启用 = 500 轮 PG
  advisory lease + 各自一次 Qdrant 调用）。

**E. 运维/供给面**

- 无任何 Prometheus/metrics 端点；`/knowledge/worker/status` 在拆分拓扑下
  报告的是 api 侧代理的空队列，不是真 worker。
- Docker 镜像 pip 构建、不带 uv.lock——不可复现；`python-docx` 未声明，
  容器内 DOCX 摄入直接 ImportError（worker.py:66-72）。
- Helm chart 与 compose 漂移（探针打 /health、无鉴权变量、无 runtime role）；
  幂等存储默认 memory（水平扩容即失效）；上传处理器在事件循环里同步拆分
  大 PDF（>20MB 阻塞单 worker，512m 限额下 OOM 风险）。
- **静默假嵌入**：`embedding_provider` 为空/未知时回落 `local`→`hash-384`
  哈希假向量（embedding_manager.py:180-191/:221），无需凭据、摄入不报错——
  检索面"能用"但语义含量为零。T0 基线与 T3 迁移必须把此类数据集标记为
  质量排除项；生产形态应显式禁用该回落或至少使其可见。
- 迁移供给三套并存且自动应用已断：中央 `database/migrations/`（cli.py
  账本）、`per_service/`（Phase 6 拆分目标、独立账本）、服务本地
  `apps/knowledge-service/migrations/`（无调用者的镜像副本）；服务启动的
  `_auto_apply_*_migration()` 解析到不存在的路径，14 个自动应用全部静默
  空转（persistence/database.py:340-353）。本升级的 schema 增量只走中央账本。

**F. gateway 侧架构债**

- `/api/v1/kb-tools/*` 是冻结在 `sdk/openapi.json` 的旧公开代理面，部分路径仍
  映射到 KS 不存在的路由（/search 等 404）；当前产品 Agent 不依赖它，而是
  通过签名 internal retrieval 使用 `search_knowledge_base` capability。若未来
  有外部消费者，另走公开契约变更。
- gateway 直接 SELECT KS 表：`DatabaseAgentKnowledgeResolver`（每次 agent 运行
  重查 datasets/dataset_permissions）、`presign.py` 重新实现数据集 ACL（无视
  viewer/editor/owner 层级）、ai-gateway-core 里 569 行无调用者的
  `DatabaseKnowledgeRepository`——违反依赖方向与 PPR H4。
- `knowledge:read/write` API key scope 签发但从不校验：read-only key 可删数据集。
- `src/api/schemas/knowledge.py`（722 行）+ `kb_tools.py`（363 行）死副本，
  与 KS 的 1203 行真合同独立漂移。
- KB 流量无配额/计费钩子。

### 2.3 成熟度自评（1-5）

| 维度 | 分 | 说明 |
| --- | --- | --- |
| 检索管线主干 | 4 | 混合+融合+预算+多租户过滤齐全；差在词法统计与降级 |
| 摄入管线 | 3 | 持久队列+排水+补偿收据成熟；增量跳过机制在但被预清理中和（实际 100% 重嵌）；差在稳定点 ID/阶段状态机/热更新语义 |
| 数据模型 | 3.5 | 生命周期+内容哈希+引文溯源字段齐全，缺嵌入模型出处/阶段时间戳/反馈回路 |
| 前端管理面 | 3 | 覆盖面好但保存路径有损、chunk 级操作缺失 |
| 评测与验证 | 2 | 有台无戏：无基线、无 CI、门禁失明 |
| 运维供给 | 2 | 无指标、镜像不可复现、helm 漂移 |
| 模型治理 | 1.5 | 无版本化、无迁移机制、段/文档级无嵌入模型出处、换模型=事故 |

## 3. 业界参考结论

### 3.1 Dify 1.17.0：采什么、改什么、不采什么

Dify 的知识库是"产品级管道 + 工具级质量"的组合：生命周期管道和数据模型值得
直接抄，评测与隔离短板正好是我们反超的空间。

**直接采纳（direct）**：

1. **逐阶段索引状态机 + 每阶段时间戳**：`waiting→parsing→cleaning→splitting→indexing→completed/error`，
   文档行携带每阶段时间戳与原始错误文本，派生 `display_status` 供 UI 过滤。
   我们目前只有粗粒度状态，这是 H2 进度可视化的前提。
2. **`index_node_hash` 内容哈希随每个索引 chunk 存储**：重同步跳过未变块、
   分块器变更后安全重嵌、跨版本去重——B1（100% 重处理）的直接解药。
3. **不可变处理规则 + 每文档规则快照**：`dataset_process_rules` 行不可变，
   文档钉住构建它所用的规则 id。重分块可复现、可审计。
4. **Postgres-as-truth、向量库可重建**：disable=按 node id 删向量但保留
   chunk 行，enable=从 chunk 行重建向量。我们的持久化方向一致（现状：
   文档级已是"DB 标记+删点、行保留"；片段级是 payload 标志双写、点保留，
   见 B3），把它变成统一的显式契约（§6.3）。
5. **按持久状态分支的幂等续跑（recover）**：从 `splitting` 续跑=重做
   抽取+切分+索引；从 `indexing` 续跑=仅从已持久化 chunk 重建向量。
   Qdrant 故障只重放 upsert 阶段，绝不重新解析 PDF。
6. **两级嵌入缓存**：文档嵌入持久缓存（按内容哈希）+ 查询嵌入短 TTL 缓存。
   改进点：批量 `WHERE hash IN (...)` 查询，禁止 Dify 的逐条 N+1；禁止
   pickle 存 Postgres（不安全反序列化面）。
7. **批量状态轮询端点**（`/batch/{batch}/indexing-status`）+ 派生显示状态；
   后续再演进到 SSE（Dify KnowledgeFS 的 `streamProcessingTaskEvents` +
   last-event-id 续传是目标形态）。
8. **慢工作前提交 + 短事务翻转状态**：先提交 `parsing` 再抽取，先提交 chunk
   再嵌入；行锁短持有，API 可随时读取在途文档。

**改造后采纳（adapt）**：

1. **集合绑定间接层（collection binding）**：`(provider, model) → collection`
   的绑定表解耦逻辑 KB 与物理集合，模型迁移=新集合回填+翻指针。**修正
   Dify 的两个错误**：不做"一个模型全租户共享一个集合"（孤儿向量 + 跨库
   HNSW 串扰），保留每数据集集合 + 绑定表；换模型时旧集合必须有显式回收策略。
2. **租户隔离队列**：TenantIsolatedTaskQueue 的 flag+list+drain 思路可用，
   但要修它的缝：flag 的 check-and-set 用 `SET NX`/Lua 原子化；排队的批次
   持久化在 Postgres 而不是只存 Redis（worker 硬死会丢批）。
3. **双队列优先级（改造，非照抄）**：Dify 有 `dataset`/`priority_dataset`
   双队列，但路由只看计费档位（Sandbox→普通队列，付费→优先队列，自托管→
   优先队列且无租户隔离），与操作类型/批大小无关。我们的改造：按操作类型
   与批大小在入队时路由——单文档重嵌/重处理（H2）→优先队列，批量导入→
   批量队列，租户档位作辅助路由。H2（单文档重嵌）的延迟保障。
4. **类型化元数据注册表 + 过滤 DSL**：每 KB 的字段定义（string/number/time）+
   条件 DSL（name/operator/value，and/or）。实现为 Qdrant 原生 payload
   Filter（而非 Dify 的 document-id 预过滤），同一 API 契约、更好的扩展性。
5. **摘要索引作为兄弟向量**（`is_summary` + `original_chunk_id`）：对长/密
   chunk 可选生成摘要、独立嵌入、命中后回溯原块。对中文长文档（查询措辞 ≠
   文档措辞）价值高。作为每数据集可选层，带生成状态表（generating/completed/error）。
6. **重排后置阈值规则**：Dify 用 bug #35233 换来的教训——混合检索中向量腿
   传 `threshold=0`，租户阈值只在 rerank/融合**之后**施加（那里分数才可比）。
7. **外部知识库联邦契约**：`POST /retrieval {retrieval_setting, query,
   knowledge_id, metadata_condition} → {records:[{title,content,score,metadata}]}`
   ——刻意最小化，与我们的 Qdrant+元数据模型贴合；补上 Dify 缺的异步/分页/
   密钥库凭据。P2 再做。
8. **图片作为 chunk 附件**的数据模型（SegmentAttachmentBinding）：存资源
   绑定而不是 Dify 的 markdown 链接 + 正则回读。多模态解锁时采用。

**明确不采（Dify 的坑）**：

- 按**字符**而非 token 计长（`max_tokens=500` 实为 500 字符）；我们已用
  tiktoken，保持。
- `unicode_escape` 处理用户分隔符——CJK 分隔符被 mojibake，`。` 永远匹配
  不上。我们的 CJK 分隔符阶梯必须走显式字符串处理。
- 全局 `ETL_TYPE` 开关（解析后端不可按租户/数据集选择）。
- PDF 仅文本层解析（pypdfium2，无 OCR/版面/表格）——扫描件直接空页。
  我们已有 VLM/OCR 路径，方向领先，按 §3.2-parsing 升级引擎即可。
- 数据集级嵌入切换产生的**孤儿向量**（换模型不删旧点）、无在途锁。
- jieba 关键词表整表 JSON 在 600s 锁下重写（O(表大小) 写放大）。
- Qdrant 全文腿是无排名 `MatchText` scroll + 空格分词（对中文查询是错的）。
- 租户级数据集 API key（全库同权、以属主身份执行）；我们要每数据集 + 读写
  scope。
- 检索路径内联 `hit_count` 写（热路径写放大）；我们走异步缓冲。
- 无离线评测、无软删、无 FK、hit-testing 结果不落库——这些正是我们的机会。

### 3.2 SOTA 2026：分主题证据结论

**Chunking**（三个独立基准互相印证：Vectara NAACL 2025、FloTorch 2026、
arXiv:2606.00881）：

- 递归/定长 400–512 token 仍是基线冠军，**保留为主干**，不换。
- **按文档类型路由的结构感知分块**是近零成本的最大免费升级：
  markdown/HTML→标题优先、PDF/扫描件→页/版面感知、代码→AST。标题面包屑
  无条件加入每个 chunk 并存入 payload（BM25 多关键词、调试可见、跨翻译边界
  稳定）——"上下文检索的免费 20%"。
- **父子分块（parent-child）是本次升级的检索头牌**：检索索引子块
  （200–500 token）、返回父段（1000–2000 token）给 LLM；Qdrant payload
  parent_id 即可实现，无需新模型依赖。生产标准（Dify v0.15.0、LlamaIndex、
  LangChain）。已知运维皱褶：child→parent fan-out 需要抬高有效 top-k。
- **Contextual Retrieval 只做可选层**：Anthropic 报告 35–67% 失败率下降，
  但独立复现只有 +0.008 nDCG@5（ECIR 2025）与 ~+2–3pp Recall@5
  （arXiv:2604.01733）。中文文档必须用中文能力 LLM 生成上下文；对中文
  价值最大的是 contextual BM25（去上下文化的 CJK 块丢实体名）。**注意本仓库
  现状**：`contextual_retrieval.py` 只剩 .pyc、`contextual_prefix` 列无人写、
  SPO 明令禁止回到默认摄入——本 PRD 将其定位为"评测基线建立后的可选
  实验层"，不是 P0（同时消解七月计划 C2 与 SPO 的三方冲突，见 §9）。
- **late chunking 在 BGE-M3 下禁止**：实测 0.246 → 0.070 nDCG@5（~3.5×
  劣化，ECIR 2025 + 火山引擎中文复现）；仅当嵌入迁移到 Jina v4/v5 时作为
  实验。
- 分块配置是**一等、版本化的评测维度**：chunk 元数据存 chunker name+version
  +parent 链接；任何默认值变更先在评测面跑 2–3 组配置 × 50–100 文档 ×
  每语料类型 20–30 真实查询。**与 PPR-06 的字节一致性约束联动**：分块算法
  变更 = 潜在全库重嵌 + 与未来 Rust 内核字节对比冲突，默认不动算法、只动
  路由/富集（§9）。

**Retrieval/Rerank**（主导杠杆结论：arXiv:2606.28367——强交叉编码器重排
承载了管线大部分质量；一旦有强重排，图扩展/层级摘要/路由/学习融合等高级
附加件无可靠收益）：

- **双语重排器 bake-off 是 P0**：在我们的 zh+en 评测集上比
  Qwen3-Reranker-4B（与 0.6B 延迟底线，vLLM 服务）、jina-reranker-v3
  （listwise、长上下文、BEIR nDCG@10 61.94）、现任 BGE-reranker-v2-m3；
  可选 Cohere Rerank 4 当 API 标尺。目标看**中文切片**的净胜。指令感知
  模型可注入租户/领域指令，要用上。
- **修中文关键词腿**：fastembed 的 BM25 无中文分词（qdrant/fastembed#610
  仍开放），空格分词正在静默杀掉中文关键词召回。两条路：(a) jieba 预分词
  + 每租户/领域自定义词典（Milvus 2.5 jieba analyzer 参考）；(b) 用
  BGE-M3/DashScope-v4 的学习型 sparse 替代 BM25 腿。我们的现状是 PG FTS
  + Qdrant sparse，与 bm25_v2 指纹冻结联动决策（§9）。中文实践基准：
  hybrid+sparse+rerank 把 recall@5 从 ~68% 提到 ~81%（需在自有语料验证）。
- **融合移入 Qdrant 原生 Query API**：prefetch+RRF（1.16 起 k 可调、1.17
  起加权 RRF）、DBSF、formula query（1.14+，业务加权：时新性衰减、文档类型
  权重）。注意：prefetch 内部融合是 shard-local，最终融合必须顶层。
- 查询侧：多查询扩展 + 多轮会话改写作评测门控开关；**HyDE 默认关**
  （实践对比测得其脆弱）。
- Agentic 深搜（有界 2–4 轮 + CRAG 置信门控）= P2；GraphRAG/RAPTOR 不进
  通用 KB（arXiv:2506.05690：图方法在真实任务上常输给 vanilla RAG）。
- LLM-as-reranker（RankGPT 类）不做交互重排，改作离线评测判官。

**Embeddings**：

- **默认稠密嵌入升级到 Qwen3-Embedding**（4B 质量档 / 0.6B 成本档，
  Apache-2.0，CMTEB ~70.6，32K 上下文，指令感知）。注意它**不是**
  Matryoshka 训练的：维度策略=全维 + Qdrant scalar 量化，不做截断。
  若允许国内云 API：DashScope text-embedding-v4（同血统、带 sparse +
  弹性维度）或 Seed1.6-Embedding（CMTEB 75.62，闭源）——数据驻留评审先行。
- **先建迁移能力，再换模型**（这是 H3 的模型侧）：model+version+dim 钉进
  集合元数据/命名；回填期双写或影子集合；scroll+re-embed 可断点续跑；
  alias/绑定表切换；评测门控放行。嵌入模型 6–12 个月必换一次（gte→Qwen3
  只用了 18 个月），能力建成后每次换模型=自动化再索引而不是事故。
- 每文档嵌入溯源：`embedding_model+version+dims` 存 Postgres 文档元数据；
  拒绝混模型查询；租户级分阶段升级可行。
- 指令模板标准化：KB chunk 一套指令、查询一套指令，写进钉住的模型配置。
- 双语内部基准是模型选择的唯一门禁；MTEB/MMTEB 只做候选筛选（法律 RAG
  实例：MTEB top-3 在内部评测排 5/7/2）。
- 微调缓行：等基础模型升级触顶，再用合成查询对比学习（5–25% recall 收益
  是有条件的）。

**Parsing**（2026 范式：端到端小型文档 VLM 取代多阶段 CV 管线；
OmniDocBench v1.6 前列全部是中国厂商模型，中文一等公民）：

- **默认解析器候选**：MinerU 3.x 混合引擎（最完整 RAG 平台：API server、
  页级路由、公式→LaTeX、表格→HTML；许可证为 Apache-2.0 变体 + 权重附加
  条件，**商用多租户嵌入前必须法务确认**）或 PaddleOCR 级联
  （PP-StructureV3 确定性快档 + PaddleOCR-VL-1.6 精度档，纯 Apache-2.0，
  许可故事更干净）。无论选谁，解析器藏在接口后、后端可插拔。
- **无损中间表示（IR）先行于分块改造**：Postgres 存文档→页→块的结构化
  JSON（类型、阅读序、表格、公式、图、页码、bbox、parser 版本），markdown
  由它渲染。**重分块不再需要重解析**；引用可回溯到页/区域；Qdrant payload
  从它派生。这是"解析一次、分块多次"的架构地基，直接服务 H2。
- **表格入 RAG 策略**：≤20 行表格整块保留 markdown；20–100 行按重复表头
  分段带重叠；超大表出行级块或 QA 对；生成表摘要+列语义作检索上下文
  （RAGFlow 0.23 Table Context Window 验证过）。表格双存 markdown+HTML；
  解析质量用租户风格表格黄金集测 TEDS（合并单元格是 markdown 退化点）。
- 摄入异步化、页级并行、可续跑、版本键控：每页一个任务、
  （内容哈希+parser 版本）缓存键、断点续传、parser 升级金丝雀重摄入。
- 自建双语解析评测集（50–200 页，扫描件/表格密集/公式/多栏），指标：
  文本编辑距离、TEDS、公式 CDM、阅读序；OmniDocBench 已饱和、各版本
  快照互掐，不能只看公榜。
- **本轮 OCR 现行合同（用户 2026-08-29 明确确认）**：DashScope
  `qwen-vl-ocr` 是扫描页/硬页的默认 OCR。调用原生多模态 API 的
  `document_parsing` 任务，保留 `advanced_recognition`、`table_parsing`、
  `formula_recognition` 作为页级专项路由；文本层完整的数字 PDF 仍先走
  本地文本层解析，不为所有 PDF 强制烧 VLM token。OCR 请求必须携带区域端点、
  像素边界、自动转正和可审计的模型/任务版本。
- **复杂版面路由**：页级信号先分流（文本层覆盖率、图像占比、表格/公式/多栏
  风险），再按 `text_layer → Qwen-OCR document_parsing → 专项任务/备用解析器`
  逐页升级；双栏按版面阅读序输出，不把两栏原始坐标简单按全页横向排序。大规模
  扫描件按页流式提交、限并发、断点续跑并缓存页结果，不能一次性把整本 PDF
  或整本 OCR 文本放进内存。所有升级结果落 IR，保留页码/bbox/parser/版本，
  低置信或结构异常页可单独重跑。
- 硬页回退层=通用 VLM（Qwen3-VL，中文能力强）；商用解析 API 只做
  评测 oracle（Qwen-OCR 是本轮用户明确批准的 OCR 例外；其他商用解析 API
  仍不得未经数据驻留和质量评审进入默认路径）。

**Eval**（先评测后重构，是本次升级的纪律）：

- **钉住双语黄金集**：200–400 QA 对，约半真实租户问题、半合成
  （Ragas TestsetGenerator / DataMorgana 式），全部人工复核；显式包含
  跨语对（zh 查询→en 文档及反向）、多跳、错误前提、不可回答题。版本钉进
  Postgres：冻结回归集（重构期不变）+ 增长集（生产反馈喂养）。
- **RAGAS 0.4.x + claim 分解判官**：Faithfulness（claim 级）/Response
  Relevancy/Context Precision/Recall 为核心；中文内容用中文能力判官；
  judge≠generator；结构化 JSON（reason+score），temperature 0。
- **EN/ZH 判官分别校准**：每语言×维度 20–50 条人工标注，测一致性并要求
  记录阈值；判官模型/提示词/版本变化即重校。未校准的中文判官会**静默
  作废一半测量**——双语部署最常被忽视的一条。
- **pytest 式 CI 评测门禁，检索与生成分离**：检索指标（hit-rate/recall/
  nDCG@k，对已标注查询→chunk 对）门控 Qdrant/嵌入/重排变更；生成侧
  （faithfulness/relevancy）门控提示词/模型变更。红线告诉你是哪边坏了；
  诊断升级用 RAGChecker 式 claim 归因。
- **影子回放再金丝雀**：租户分层抽样的生产查询并行打新旧管线，pointwise
  判分（pairwise 有位置偏差），影子持平或净胜后 5–10% 金丝雀，
  faithfulness/零结果率阈值自动回滚。
- **反馈飞轮在产品面第一天就有**：web 加 👍/👎+理由码、绑 trace id；
  采样轨迹在线判官评分；负例进标注队列、每周提升进增长黄金集；多租户
  路由反馈（聚合指标会掩盖单租户知识缺口）。
- **检索健康 SLO**：每租户零结果率、查询类别延迟 SLO、ANN-vs-exact 召回
  漂移、查询分布漂移、摄入滞后（文档更新→可检索）。

**Architectures**（2026 产品收敛形态：可配置摄入管线 ⊥ 查询时检索路径）：

- **配置定义的摄入管线**：stage DAG（parse→split/QA→enrich→embed→index）
  按 KB 存 Postgres，官方模板（通用、QA 对、父子分块），阶段注册表留在
  代码里（Haystack 教训：序列化管线引用可 import 的类，注册表纪律必须）。
  对标 Dify Knowledge Pipeline 的模板+草稿/发布+一键迁移，不引入工作流引擎。
- **队列化异步 worker + 每阶段可观测**：所有解析/嵌入移出请求路径；
  最小/最大/扩容环境参数；流式进度到 React UI；每阶段日志。重活
  （摘要、图抽取）单独批队列。
- **Qdrant 多租户加固**（官方指南）：共享集合 + `tenant_id` payload keyword
  索引（`is_tenant=true`，**在批量摄入前创建**，让 filter-aware HNSW 边在
  索引期建好）；租户过滤由服务端从认证身份注入（Qdrant 不把过滤器绑定到
  调用身份，issue #8015——永不信任客户端租户 id）；噪声大租户用 1.16
  Tiered Multitenancy 提升到专属 shard。
- **集合=ACL 原语**（R2R 模型）+ 权限在**检索时**执行（Onyx 规则），
  不在 UI 层。
- **chunk 检视 + 版面引用 UX**：管理端"索引看到什么"浏览器
  （RAGFlow 模式）+ 用户侧带分数的引用高亮（Kotaemon 模式）。我们已有
  VLM/OCR 知道页/bbox，成本低、企业信任收益高。
- **分层缓存**：先嵌入缓存 + 检索结果缓存（键=租户+规范化查询+索引/嵌入
  版本，文档更新即失效）；语义查询缓存只在严格阈值（0.85–0.95 余弦）+
  每租户键+TTL+命中率监控下考虑；答案级缓存不做。
- **对外检索面**：把检索暴露成干净的认证 API/tool（search+cite+filter），
  未来 agent/MCP 消费者零改造接入——这是"RAG→context"方向的具体低风险
  兑现。

### 3.3 明确非目标（附证据，防止未来被重新提议）

| 非目标 | 证据 |
| --- | --- |
| 语义分块（嵌入相似度断点） | FloTorch 2026：recall 升但答案准确率 69%→54%（~43 token 碎片）；Vectara NAACL 2025：算力不值 |
| 命题分块 / DenseX | 2026 最大规模调研中**最慢且最差**的方法 |
| Agentic/LLM 分块进主管线 | 成本与基准都不支持；只留"低量高价值语料"小生境 |
| late chunking（现模型下） | BGE-M3 实测 0.246→0.070 nDCG@5；仅 Jina 系模型有效 |
| GraphRAG / LightRAG / HippoRAG 进通用 KB | arXiv:2506.05690：真实任务常输 vanilla RAG；索引成本高；仅多跳/全局综述租户再议 |
| HyDE 默认开启 | 实践 9 技术对比：脆弱、延迟不值；留旗标 |
| 学习融合（TRF 等）替代 RRF | 论文赢、产品未成标准；RRF 是零调参默认（可选 DBSF A/B） |
| 重建 BM25/sparse | PPR-06 明令禁止（Postgres tsvector + Qdrant 已原生） |
| 全局答案缓存 | 失效与投毒风险未解决；先做检索结果缓存 |
| 未经审查的商用解析 API 进默认路径 | 中国企业数据驻留与供应商锁定风险；Qwen-OCR 仅按本轮明确批准的 OCR 合同进入扫描页路径，其余商用解析 API 仍只留评测 oracle/薄适配 |

## 4. 升级主题（T0–T9）

依赖总图：`T0 →（T1 ∥ T2 ∥ T5）`（T5 后端端点先行）；`T1 → T3`；
`T1 →（T4 ∥ T6）`（T6 另依赖 T0）；`（T0+T1）→ T9`；`T7` 无硬依赖、
`T8` 依赖 T1 期间顺手建成的 KS 授权端点，两者穿插。冲突时以各主题
"依赖："行与 §7 阶段计划为准。

### T0 评测基础与验证门禁 —— P0，一切质量工作的前提

**目标**：让"变好了/变坏了"成为可判定事实（A1–A7 全清）。

范围与交付：

1. `make kb-unit-gate`：跑 `tests/services/knowledge` + `tests/knowledge` +
   KB 相关 eval 测试（`--no-cov`），接入 `ci.yml`；仓库根 `harness.yml` 的
   `apps/knowledge-service/**` 触发改指真实套件，`docs/harness/commands.md` §7
   补 KB 行。
2. 双语黄金集：200–400 QA 对（半真实半合成、人工复核、含跨语/多跳/错误
   前提/不可回答），版本钉进 Postgres，分冻结回归集与增长集；提供种子脚本。
3. 真实语料检索基线：用现成 `/retrieve_evaluate` + Workbench 记录
   hit-rate/MRR/nDCG/recall@k 分布并**冻结**（agent-kb-eval 纪律：第一轮
   只记分布不定阈值）；同时复验 SPO K3 收窄后的默认召回（C6）。
4. Qdrant 集成冒烟（`docker-compose.kbms.yml`，`@pytest.mark.integration`，
   栈未起时跳过）：建集合、dense+sparse upsert、原生混合+RRF、按过滤删除。
5. KB 迁移真库测试：065/066/082 应用 + `get_kb_ragas_summary` 窗口 SQL
   真跑（沿用 `tests/database` 模式，CI 加 postgres service）。
6. RAGAS 判官版本化 + 金夹具测试（判官 JSON→指标数学的固定断言）。
7. 观测回放程序：`scripts/eval_rag.py` 伴侣脚本，从活检索栈重新生成
   observations fixture，让 rag-eval-regression-gate 从"盲回放"变成
   "可更新证据"。

Done-when：CI 对 KB 代码变更红绿敏感；黄金集+基线冻结文件入库；任何
后续主题的质量主张必须引用基线编号。　工作量：M。依赖：无。

### T1 摄入生命周期状态机与增量 upsert 引擎 —— P0（H2/H3 后端）

**目标**：任何重建/重嵌操作都是"按稳定 ID 的 upsert + 阶段状态机 +
可续跑"：复活被预清理中和的增量跳过（移除 worker 预 sweep、先统一 MD5/sha256
哈希类型），补上稳定点 ID（现为随机 uuid4），消灭文档级"先删后建"的
检索空窗与变更块的 ID 轮换。

范围与交付：

1. **逐阶段状态机**（抄 Dify 契约、落到我们表）：文档行携带
   `waiting→parsing→splitting→indexing→completed/error` + 每阶段时间戳 +
   原始错误文本；chunk 行携带 `waiting/indexing/completed/error/paused`；
   API 派生 `display_status`（queuing/indexing/paused/error/available/
   disabled/archived），永不泄漏内部态。
2. **内容哈希 + 稳定 chunk 身份**：segments 已有 `content_hash`（迁移 023，
   运行时写 sha256、回填为 MD5），dedupe 端点仅按 `content_hash` 分组去重；
   `index_node_id/index_node_hash` 两列由迁移 002 建出但**全库无写入方**
   （线上恒为 NULL，唯一引用是 update_segment_fields 白名单）。**[判断]
   本升级为这两列补确定性写入方**（摄入时写入：`index_node_id`=位置谱系、
   `index_node_hash`=内容哈希），点 ID 由（文档, 位置/哈希谱系）确定性派生
   （现为随机 uuid4，见 §6.1）；备选是放弃该两列、直接由（位置,
   content_hash 谱系）派生点 ID——二选一在实施期定，二者对检索面等价。
   注意哈希类型漂移：复活跳过前必须先做一遍统一重哈希；重摄入对未变块
   跳过、对同位变更块原地 upsert；worker 的 `_sweep_document_generation`
   预清理由此退役。这是 H3 的 ID 层。
3. **单文档重处理端点**（H2）：现有 `POST .../documents/{doc}/reindex`
   （→worker 队列）升格为 `/reembed` 契约的落点；新增 `.../reprocess`
   （重解析+重切分+重嵌，按哈希跳过未变块）。仅重嵌不重解析（用于模型/参数
   不变下的向量修复）。走优先队列（§3.1 双队列），带批次/阶段进度查询；
   顺带修复 `batch-reindex` 全量模式被 200 条文档列表上限截断的问题
   （document_service.py:563；分页/游标对应 T5-#8）。
4. **幂等续跑（recover）**：按持久状态分支——`splitting` 续跑重做抽取+
   切分+索引；`indexing` 续跑只从已持久化 chunk 重建向量；worker 崩溃/
   重启后自动 recover（现有卡死恢复循环扩展），不需要人工重试按钮兜底。
5. **修订原子性**（§4.6-2 债务）：一次摄入批次在 staging revision 上构建，
   Qdrant/DB/旧向量处置通过 manifest 原子翻转；子批失败不再"继续并留洞"。
6. **禁用/启用/归档统一索引状态机**（§4.6-1 债务剩余缺口）：**保留现有
   两套机制、不推倒重做**——片段级维持 payload 标志翻转（O(1) 可逆，保留
   现有禁用 DB-first/启用 Qdrant-first 次序，绝不退化为删点+重嵌），
   文档级维持"DB 标记+删点、行保留"；统一的是状态迁移合同 + 检索缓存
   联动失效 + 文档恢复路径（现状=清行全量重建→改为走本主题的增量跳过
   引擎重建，不整文重嵌）。**[判断]** 归档带 reason/by/at 审计。
7. **处理规则快照**：每文档钉住不可变规则 id（重分块可复现、可审计）。
8. 修复 B5（MMR fill-remaining 回填被拒候选，`strict_diversity`/`fill_policy`
   契约）、B6（PG FTS simple vs multilingual 不一致 + 零结果回退）。
   同一代码路径顺带消灭 perf-review 的 MMR O(n²)（retrieval_service.py:2116
   全量载荷滚动 + Python 余弦）——B5 重写该路径时一并做，不单独立项。

约束：**不改分块算法输出**（PPR-06 字节一致性），只做身份/跳过/状态机；
不改持久队列所有权语义（PPR-06 stop 条件）。
Done-when：单文档重嵌端点上线且被前端调用；同一文档二次摄入的未变块
0 重嵌；worker 崩溃后自动续跑通过故障注入测试；§4.6 债务 1–4 的剩余缺口
关闭（#1 大体已落地、剩余缺口见 B3；#5/#6 不在本主题，见附录 B）。
工作量：XL。依赖：T0（基线）。

### T2 检索质量：中文词法、重排换代、降级与预算 —— P0

**目标**：把检索主干从"好"推到"双语 SOTA"，并消除两个可用性悬崖。

范围与交付：

1. **中文关键词腿修复**：jieba 预分词 + 租户/领域自定义词典路径（对齐
   bm25_v2 指纹约束：若触及 `bm25_v2` tokenizer 参数 → 新版本字段或全量
   重建，绝不写入现有字段，见 §9）；或评估 DashScope-v4/BGE-M3 学习型
   sparse 替代腿。**决策前跑 T0 基线对比**。
2. **双语重排器 bake-off**：Qwen3-Reranker-4B/0.6B（vLLM）、
   jina-reranker-v3、BGE-reranker-v2-m3（现任），中文切片净胜为门禁；
   利用指令感知注入租户/领域指令。记忆预算 512m/384m 下自托管推理的
   部署位置是设计问题（独立推理容器，不占 KB 限额）。
3. **重排预算耦合 + 降级契约显式化**（C2）：降级到融合序**已存在**
   （broad except + `meta["rerank_error"]`），缺的是预算与显式契约：
   rerank 包 `asyncio.wait_for(剩余预算)`（现状 30s httpx 超时在预算之外）；
   降级旗标显式化为 `meta.rerank_degraded=reason`（在自由文本
   `rerank_error` 之上提供机器可读信号）；3s 交互预算从此覆盖全管线。
4. **后置阈值规则**：混合模式向量腿恒传 `threshold=0`，租户阈值只在
   融合/重排后施加（Dify #35233 教训 + §4.6-6 分数边界债务一并处理）。
5. **查询遥测**（C1）：`retrieve()` 末尾异步缓冲 INSERT `dataset_queries`
   （dataset_id、查询指纹、mode、top_k、命中数、stage_timings）；供给
   零结果分析与权重调优。独立事务，永不阻塞检索。
6. **死代码清除**（C4）：删/隔离 `retrieval_v2.py`（连同其唯一依赖方
   tests/services/test_multimodal_rag.py 一并处置）、`retrieve_with_images`
   v1/v2 不可达体、孤儿多模态调用点（~2500 行），在重构前拆雷。
7. 缓存修正（C5）：逐出改真 LRU；检索结果缓存键含索引/嵌入版本号。
8. 多轮会话改写 + 多查询扩展为评测门控开关（预设层暴露）；HyDE 仅旗标。

Done-when：bake-off 报告（含中文切片）入库且获胜配置可复现；重排故障
注入下 `rerank_degraded` 旗标正确置位且检索 p95 延迟不破交互预算
（注意：降级本身已存在，本项验收的是预算耦合与旗标，不是降级行为）；
`dataset_queries` 有生产数据流入。
工作量：L。依赖：T0。

### T3 嵌入版本化与蓝绿迁移 —— P0/P1（H3 的模型侧）

**目标**：把"换嵌入模型"从事故变成脚本；支撑默认升级到
Qwen3-Embedding。

范围与交付：

1. **版本元数据**：文档/数据集行存 `embedding_model+model_version+dims`；
   集合命名/绑定表含模型与维度；拒绝混模型查询；Qdrant payload 携带
   `embedding_model` 供审计。
2. **蓝绿迁移机器**：集合绑定间接层（逻辑 KB→物理集合）；新集合影子
   回填（scroll+re-embed，断点续跑，按内容哈希跳过，走 T1 的 upsert 引擎）；
   评测门控（T0 门禁通过才翻指针）；alias/绑定行原子切换；旧集合保留期
   +显式回收——**零孤儿、零空窗**。回填期间旧集合持续可读可查。
3. **默认模型升级**：BGE-M3/现任 → Qwen3-Embedding（4B 或 0.6B，按部署
   资源定），vLLM/TEI 服务；指令模板标准化（文档侧/查询侧各一）。
   切换本身是迁移机器的一次演习。
4. 嵌入缓存批量化（`WHERE hash IN`），内容哈希键，禁 pickle。
5. 租户级分阶段推进：按数据集逐个迁移、逐个评测放行。

Done-when：一次真实模型切换在 staging 全程演练（含中断续跑、回滚演练）；
切换期间检索可用性 100%（影子期旧集合服务、切换后新集合服务）。
工作量：L。依赖：T1（upsert 引擎）、T0（门禁）。

### T4 解析引擎升级与无损中间表示 —— P1

**目标**：解析质量对齐 2026 端到端 VLM 水准；建立"解析一次、分块多次"
的 IR 地基（H2 的解析侧）。

范围与交付：

1. **IR 先行**：Postgres 存文档→页→块结构化 JSON（块类型、阅读序、表格、
   公式、图、页码、bbox、parser+版本）；markdown 渲染自 IR；Qdrant payload
   从 IR 派生；引用回溯到页/区域。
2. **可插拔解析器级联**：接口 + 后端注册（现有 VLM/OCR 作为后端之一，当前
   扫描页后端为 DashScope `qwen-vl-ocr` 原生 OCR）；
   新增候选默认：PaddleOCR 级联（PP-StructureV3 快档 + PaddleOCR-VL-1.6
   精度档）或 MinerU 3.x（许可证先行，§10）；通用 VLM（Qwen3-VL）作硬页
   回退；每租户/数据集可配级联。
3. **页级并行 + 版本键控**：每页独立任务，（内容哈希+parser 版本）缓存，
   断点续传；parser 升级走金丝雀重摄入（按租户抽样）。
4. **表格策略**：按 §3.2 的尺寸分层 + 双存（markdown+HTML）+ 表摘要
   作检索上下文；租户风格表格黄金集测 TEDS。
5. 双语解析评测集（50–200 页真实形状）：编辑距离、TEDS、公式 CDM、阅读序。
6. **Qwen-OCR 原生任务与复杂页路由**：扫描页默认 `document_parsing`；
   表格/公式/需定位页按信号切换专项任务；响应、模型、任务、地域和页级
   失败/重试收据进入解析 provenance。数字 PDF 不因存在 OCR 配置而强制走
   远程 OCR。

约束：解析产物→分块的边界变化会改变块内容；任何**分块边界**变化按 §9
走"重嵌影响评估 + 评测门控"，默认保持当前边界策略不变。
Done-when：扫描件/表格密集语料的解析指标显著优于现行（评测集证据），且
Qwen-OCR 的专项任务/失败重试/页级 provenance 可复现；
重分块不触发重解析。　工作量：L。依赖：T1。

### T5 前端优先管理面 —— P0（H1）

**目标**：§5 覆盖矩阵全绿；所有后端新操作在 UI 有对应面。

范围与交付：

1. **默认值统一**：检索默认收敛到 `DEFAULT_RETRIEVAL_CONFIG` 单一来源
   （hit-test/settings/create 向导/preset 夹具同值），并按基线证据更新
   推荐值；修 `handleSaveRetrievalConfig` 有损保存（rrf 不带 alpha、
   权重全传、`rrf_k` 可配）。
2. **chunk 级操作面**（H1 核心）：hit_count 徽章、启用/禁用开关、关键词
   编辑、单文档重嵌/重解析按钮（调 T1 端点）+ 阶段进度显示、错误文档
   一键重试；`updateSegment` 扩展为 {text, keywords, enabled}。
3. **分块配置完整保存**：设置页下发全模式参数（分隔符、正则、标题层级、
   层级父子尺寸、QA 前缀），不再静默丢失。
4. **进度升级**：条件轮询（有非终态行才轮询）→ P1 SSE（网关
   ServiceProxy 已支持 SSE 直通；对齐 Dify KnowledgeFS 的
   last-event-id 续传 + 版本号丢弃旧事件）。
5. **评测闭环**（F6）：Workbench 用例持久化（每数据集存储 + JSONL 导入，
   复用平台 `goldenImport.ts` 解析器）；QA/hit-test 结果一键"送评测黄金集"。
6. **反馈飞轮**：检索结果卡与 QA 气泡加 👍/👎+理由码（后端新表 + 端点），
   绑 trace/查询指纹；负例面板入标注队列。
7. **元数据面**（P1）：类型化字段注册表管理 + 每文档元数据编辑 + 检索
   过滤器（T1/T2 后端 DSL）。
8. **结构债与规模化**：拆分 `DatasetDetail.tsx`（3929 行）为按 tab 组件，
   所有上述改动的先决条件；消除 2s 无脑轮询与无防抖搜索；文档/片段列表
   加游标分页（现状硬上限 200/500 无分页参数）；`list_datasets` 权限改
   批量查询（现状每数据集 1+角色数次，纯 N+1）；批量操作改真批量（现状
   逐项循环 + 逐项 advisory lease）。
9. 多模态类型广告与 `text_only` 现实对齐（要么解锁、要么隐藏滤镜项）。

目录权限批读实现边界（2026-10-12）：Python 与 Native 的分页目录读取，保留
每页最多 200 个扫描行、完整游标遍历、ACL 后空页继续、全目录过滤/计数及
统计/脱敏逻辑。仅对当前页需要显式权限判断的 ID 发出一次 grant SQL；匿名、
创建者、同租户管理员及归档拒绝等原有无需查询分支不访问 grant 表。同租户
角色 ID 子集与 actor 角色数组分开绑定，不展开角色×数据集，不缓存跨请求权限。
传统 Python 非分页内部方法保持原行为，实际资源访问端点继续即时授权复查。

明确的观察边界变更：当前页所有显式 grant 由一个 SQL statement snapshot
读取，取代逐数据集/逐角色独立语句的观察时刻。无新增事务或跨页快照；不声称
并发 grant/revoke 的观察时序与旧实现一致。保留各实现原静态权限规则，包括
历史非法/空/大小写 grant 的现存差异，不借性能优化统一授权策略。

源码/DB-double 示例（非线上耗时）：200 个可见、同租户、非 owner 数据集，
一个角色时，Python 由 402 条 SQL（1 scan + 400 grants + 1 stats）降为 3；
Native 由 202（已有逐数据集 user+roles 合并）降为 3。全 fast-path 页仍为零
grant 查询；失败不退化成空权限。HTTP 请求数不变，目录仍完整加载后展示
全量统计。需独立静态决策对照、查询计数与游标/归档回归；真实 PostgreSQL
并发、浏览器和生产延迟/负载验收仍待完成，测试夹具不能作为线上延迟收益。


Done-when：§5 矩阵全绿；从 UI 完成"上传→编辑→单文档重嵌→禁用→恢复→
删除"全生命周期无需任何 API 手工调用；e2e 覆盖新操作面并进 CI。
工作量：L。依赖：T1/T2 端点（可并行开发，后端先行）。

### T6 BM25 v2 跨存储生命周期协议 —— P1（认领无主阻塞）

**目标**：建造 bm25-v2 文档要求但无人认领的跨存储生命周期机制，使
`active_version=bm25_v2` 可安全放行。

范围与交付：

1. **写入排除协议**：切换生命周期内排除所有 PostgreSQL 与 Qdrant 写入者
   （摄入/更新/删除/回填）的机制——数据集级切换锁 + 队列静默 +
   双阶段确认（对齐现有"失效哨兵"模式：写前先把完成收据换成
   `status=invalidated`）。
2. **并发集成测试**：切换期间的真实并发写测试（不是单测模拟）。
3. **放行与回滚**：切换后 `active_version=bm25_v2` 生效（词法腿=原生
   qdrant/bm25 + IDF）；回滚=翻回 `lexical_v1`，v2 字段与数据保留
   （现有回滚语义不变）。
4. 词法统计真实化联动评估：native BM25 的服务端 IDF 是语料级统计，
   顺带消灭 C3（候选池内 IDF）问题。

约束：指纹冻结（§9）；shadow-only 语义在放行前不变；
`KNOWLEDGE_QDRANT__BM25_V2_ENABLED`（bm25-v2 文档中缩写为
`KB_BM25_V2_ENABLED`）门闩语义不变。
Done-when：一个测试租户在并发摄入负载下完成 v1→v2 切换与回滚，检索
无错误、无数据丢失，T0 基线净胜。　工作量：L。依赖：T0、T1（写入路径
受状态机控制后排除才可证明）。

### T7 可观测性与供给可靠性 —— P1/P2

1. `/metrics`（prometheus_client）：队列深度、认领/竞争、按模式摄入时长、
   检索延迟、嵌入调用错误、缓存命中、重排降级率；`DrainMiddleware` 已
   预留路径。
2. `/knowledge/worker/status` 改从 Postgres 持久队列读真实状态。
3. **可复现镜像**：Dockerfile 引入 uv.lock；声明 `python-docx`；删除
   死依赖 `sqlalchemy`。
4. 上传路径大 PDF 拆分移出事件循环（`asyncio.to_thread` 或下沉 worker）。
5. 配置收敛：~15 个裸 `os.getenv` 并入 Settings；修 `.env.example`
   （S3 嵌套键、运行时角色、鉴权变量）。
6. Helm：对齐 compose（`/health/ready`、鉴权变量、运行时角色、镜像钉版本）
   或显式标注不支持。
7. 幂等后端默认切 redis（为多副本做准备，配合 PPR-06）。

Done-when：摄入与检索的关键指标在 grafana 可见；镜像构建两次字节级
依赖一致。　工作量：M。依赖：无硬依赖。

### T8 gateway 契约清理与权限统一 —— P1

1. **scope 强制**：`knowledge:read/write` 在代理层校验（复用
   `agent_runtime.py` 的 `required_scopes` 模式）。
2. **ACL 单一权威**：`presign` 与 `DatabaseAgentKnowledgeResolver` 改走
   KS 内部授权端点（HMAC 签名，类 `/internal/eval/ragas` 模式）；
   删除 ai-gateway-core 的 KB SQL/仓储与 schema 死副本（先确认
   openapi.json 无引用）。保留 `AGENT_KNOWLEDGE_UNAVAILABLE` fail-closed。
3. Agent/RAG 桥接边界：已确定由 Agent/边界层把 `search_knowledge_base`
   封装为 tool，KS 只提供受权限保护的 HTTP/internal retrieval；当前计划不在
   KS 新增 LangChain/LangGraph tool，也不改动旧 `/api/v1/kb-tools/*` 公开代理。
4. 契约漂移栅栏：harness 门禁"gateway 不得定义 KB 请求 schema"
   （`docs/harness/workflow.md` §5 的规则化路径）。

Done-when：read-scope key 无法写；gateway 零 KB 表直读。
工作量：M。依赖：KS 授权端点（T1 期间顺手建）。

### T9 父子检索激活与结构化路由 —— P1/P2

**目标**：把休眠的层级能力变成生产默认（C7），落地 SOTA 头牌（§3.2）。

范围与交付：

1. HierarchicalIndexer 注入默认 worker 的路径修复（含"扫描+层级失败仍标
   completed"的失败归因 bug），先对存量数据集灰度。
2. 子块索引、父段返回：child 点带 `parent_segment_id` payload；rerank 后
   按父聚合（父分 = max(child)）；fan-out 的有效 top-k 上限显式配置。
3. 结构路由（不改边界算法的前提下）：markdown→标题优先路由、标题面包屑
   进 payload；代码语料 AST 路由作为模板实验。
4. 摘要索引（可选层）：`is_summary`+`original_chunk_id` 兄弟向量 +
   生成状态表，每数据集开关。
5. 评测面：层级开/关作为 Workbench 一等对比维度。

约束：任何边界算法变化触发 §9 重嵌评估；默认父子参数变化先在黄金集过门禁。
Done-when：层级路径在真实数据集灰度净胜且失败归因正确；
`make rag-eval-regression-gate`（更新后）全绿。
工作量：M。依赖：T0、T1。

## 5. H1 验收：前端操作覆盖矩阵

验收规则：**矩阵全绿 = H1 达成**。"目标"列全部必须存在于 web 控制台；
后端端点缺失的，按标注的主题补建。标注 ✓=已有 / ✗=缺失 / ◐=部分。
**H1 覆盖 1–29 行**（既有管理操作）；第 30 行（外部知识库联邦）是 P2
新功能，不进 H1 验收，按其自身阶段验收。

| # | 管理操作 | 后端 | 前端 | 目标 | 归属 |
| --- | --- | --- | --- | --- | --- |
| 1 | 数据集创建（含模型/分块/检索配置） | ✓ | ✓ | 保持 | — |
| 2 | 数据集删除（密码门控） | ✓ | ✓ | 保持 | — |
| 3 | 文件/URL/文本上传、批量上传(≤50) | ✓ | ✓ | 保持 | — |
| 4 | 文档列表 + 阶段进度（每阶段时间戳） | ◐ 粗状态 | ✓ 2s 轮询 | 阶段时间戳 + 条件轮询→SSE | T1/T5 |
| 5 | **单文档重解析+重切分+重嵌（reprocess）** | ✗ | ✗ | 一键按钮+进度 | **T1/T5（H2）** |
| 6 | **单文档仅重嵌（reembed）** | ✓ `/reindex` 已存在 | ◐ 按钮已有、缺进度/队列可视 | 阶段进度+优先队列可视+409/已排队处理 | **T1/T5（H2）** |
| 7 | 批量重索引 / 批量删除 | ✓ | ✓ | 修 >200 文档截断 + 真批量 | T1/T5 |
| 8 | 失败文档一键重试（从持久状态续跑） | ◐ | ◐ | 对齐 recover | T1/T5 |
| 9 | 文档启用/禁用（含索引同步） | ◐ 状态机分裂 | ✗ | 统一状态机+开关 | T1/T5 |
| 10 | 文档归档/解档（审计） | ✓ 带 reason/by/at | ✗ | 前端暴露 | T5 |
| 11 | 文档元数据编辑（类型化字段） | ◐ 4 字段白名单 | ✗ | 字段注册表+批量编辑 | T1/T5 |
| 12 | 文档版本历史/对比/恢复 | ✓ | ✓ | 保持 | — |
| 13 | chunk 列表/树形浏览/内容编辑 | ◐ PUT 丢 answer/keywords | ✓ 仅 text | 全字段保存+编辑 | T5 |
| 14 | **chunk 启用/禁用** | ✓ 单条+批量 | ✗ | 前端开关+索引同步验证 | **T1/T5（H1）** |
| 15 | chunk 关键词编辑 | ◐ schema 收、路由丢 | ✗ | 修路由+编辑框 | T5 |
| 16 | chunk hit_count 显示 | ◐ 列存在但从不写 | ✗ | 接线遥测+徽章 | T2/T5（F10） |
| 17 | 分块预览（上传前+按数据集） | ✓ | ✓ | 保持 | — |
| 18 | 分块配置编辑（全模式参数） | ◐ | ◐ 丢字段 | 全参数下发 | T5 |
| 19 | 检索配置编辑（融合/权重/阈值/重排） | ✓ | ◐ 有损保存 | 修保存 | T5 |
| 20 | 检索预设管理 | ✓ | ✓ | 默认一致性修复 | T5 |
| 21 | hit-testing（原始分/阶段分） | ✓ | ✓ | 保持 | — |
| 22 | 评测工作台（A/B + IR 指标 + 门槛） | ✓ | ◐ 用例易失 | 持久化+导入 | T5（F6） |
| 23 | 检索结果送黄金评测集 | ✗ | ✗ | 一键桥接 | T5（F6） |
| 24 | QA 问答（SSE + 引用 + 阶段计时） | ✓ | ✓ | 保持 | — |
| 25 | **反馈 👍/👎 + 理由码** | ✗ | ✗ | 结果卡+QA 气泡 | **T5** |
| 26 | 查询日志/零结果分析 | ✗（表空） | ✗ | 遥测+面板 | T2/T5 |
| 27 | **嵌入模型/版本切换（蓝绿）** | ✗ | ✗ | 向导+进度+回滚 | **T3/T5（H3）** |
| 28 | 集合/索引健康收据查看 | ◐ 内部 | ✗ | 管理面板 | T6/T5 |
| 29 | 词法版本配置（lexical_v1/bm25_v2） | ✗ active 硬禁 | ✗ | 协议放行后暴露 | T6 |
| 30 | 外部知识库联邦管理 | ✗ | ✗ | P2 | P2 |

## 6. H2/H3 验收：单文档重嵌与 upsert 热更新契约

### 6.1 稳定身份（H3 的 ID 层）

- **chunk 身份**：segments 已携带 `content_hash`（迁移 023，运行时写
  sha256、回填为 MD5），dedupe 端点仅按 `content_hash` 分组去重；
  `index_node_id/index_node_hash` 两列由迁移 002 建出但**全库无写入方**
  （线上恒为 NULL，唯一引用是 update_segment_fields 白名单）。升级方向：
  **[判断]** 为这两列补确定性写入方（`index_node_id`=位置谱系、
  `index_node_hash`=内容哈希）并以其派生点 ID；备选是放弃该两列、直接由
  （位置, content_hash 谱系）派生——实施期二选一，对检索面等价。另需补齐
  文档级内容哈希。目标态：Qdrant 点 ID 由（数据集, 文档, 块谱系）确定性派生。
  **现状已核实**：点 ID = `vector_id` = segment 行 ID = 入库时随机
  `uuid4`（ingestion_service.py:871/911/925；层级路径同为
  uuid4，hierarchical_indexer.py:371）——不存在确定性派生，重摄入变更块
  必然换 ID。全库唯一复用 ID 的写路径是段级编辑（document_service.py:843
  `pid = seg.vector_id or segment_id`），它正是热更新写路径的模板。
- **不变式**：同一位置、同一内容的块，在任何重摄入/重嵌之后点 ID 不变
  → 写路径恒为 upsert，检索面**不存在**空窗时刻。
- **可见性纪律**：任何触及多块/多集合的写（文档级 reprocess、模型迁移、
  词法切换）必须经 staging revision 或影子集合整体切换，检索面不得见到
  半成品 revision（现状"子批失败留洞继续"模式作废，§4.6-2）；单块写由
  上一条不变式保证天然原子。
- **变化检测**：重摄入按（位置, 内容哈希）比对：未变→跳过；内容变→
  原地 upsert（新向量覆盖旧向量，同 ID）；删除→显式按 ID 删点（仅真删除
  路径允许删点）。
- 元数据携带 `chunker_name+version` 与 `embedding_model+version+dims`
  （今天嵌入身份只存于 dataset 级——出处列是本升级的 schema 增量，走
  中央迁移账本），使"哪些块需要重嵌"成为可查询事实（支撑 §6.3 按版本扫描）。

### 6.2 单文档重嵌（H2）

- 端点：现有 `POST .../documents/{doc}/reindex`（→worker 队列、已排队 409）
  升格为 `/reembed` 契约的落点；新增 `/reprocess`（换参数全链路）。
  幂等键复用现有中间件。
- 走**优先队列**（单文档/交互触发）与批量导入队列分离；租户公平队列防
  大租户饿死小操作（原子化的 flag+list 模式）。现状：worker 并发仅 3 个
  文档槽且与上传共用（ProcessingSettings；worker.py:324-350）——不分离则
  重嵌饿死上传、上传阻塞重嵌。
- 阶段进度落库并暴露（§5-#4），前端按钮直达（§5-#5/#6）。
- 重嵌期间文档**旧向量持续可检索**（新向量 upsert 覆盖，无删除步骤）；
  失败文档保留旧向量 + 错误状态，一键续跑。**现状恰相反**：文档级重嵌先
  `_sweep_document_generation`（worker.py:641-722）删光全部点再重建——
  sweep 至完成之间检索空窗，失败即零向量；图片路径亦先删后嵌。全库唯一的
  热更新原语是段级 `update_segment`（同点 ID、fail-closed 状态机，
  document_service.py:733-883），文档级 upsert 以它为模板；单次运行内的
  "先建后删"排序已有先例（ingestion_service.py:1004-1032）。
- 与修订守卫联动：重嵌完成前检索缓存按 `dataset_revision_fingerprint`
  自然失效，不读半新半旧。

### 6.3 模型/维度级迁移（H3 的集合层）

- **同模型同维**：§6.1/6.2 的原地 upsert 即是热更新。
- **换模型/换维**：今天集合绑定是 `datasets.collection_name` 1:1 列、全局
  唯一约束（082，含软删预留）——蓝绿把它升级为 1:N 绑定表（活动别名），
  预留语义原样保留。流程：绑定表指向旧集合继续服务 → 新集合影子回填
  （可续跑、按哈希跳过、并发受租户公平队列约束）→ T0 评测门禁净胜 →
  绑定行/alias 原子翻转 → 旧集合保留期后回收。**任一步失败回退到
  "继续服务旧集合"，永不删除在服集合**。
- 该机制与 bm25_v2 回填共享同一份"权威源 = PostgreSQL enabled chunk 行"
  的纪律（Qdrant 不能自证完整）。
- 现有守卫已在、蓝绿是它的落地路径：`_update_dataset_locked` 在文档存在时
  拒绝变更嵌入身份（"create a reindexed generation"，dataset_service.py:930-931），
  集合名编码维度（vector_store.py:340-344）——跨维度换模型必然走新集合 +
  绑定翻转，无法原地变更。
- 禁用/启用语义（§4.6-1 剩余缺口）：**保留现存两套机制并收敛为一份
  合同**——片段级 = payload `{enabled}` 标志翻转（O(1) 可逆，点保留；
  禁用 DB-first、启用 Qdrant-first 的现有次序不动，**绝不重做成删点+
  重嵌**）；文档级 = DB 标记 + 按稳定 ID 删点、保留 chunk 行与绑定，
  enable = 按行经增量引擎 upsert 重建（替代现状的清行全量重摄入）——
  恢复同样是热更新。统一合同还需覆盖检索缓存联动失效（现状两套机制
  均不触缓存）。

### 6.4 验收场景（全部需自动化测试覆盖）

1. 上传文档 → 修改一个段落 → reprocess：仅该段落对应块重嵌（点 ID 不变，
   断言新旧点集对称差 = 变化块集合）。
2. reprocess 进行中的并发检索：始终返回（旧或新的）完整结果，无 404/空腿。
3. worker 在 indexing 阶段被 SIGKILL：重启后自动续跑，不重解析。
4. 嵌入模型切换演练：切换全程检索可用；中途中断可续跑；回滚=翻指针。
5. 禁用→启用文档：向量删除→恢复，检索结果一致（含词法腿与缓存）。
6. 并发双文档重嵌 + 批量导入：租户公平队列下互不饿死、无双重认领。
7. 重嵌中途失败：文档不零向量——旧向量持续服务（对照现状：失败 = 全删），
   错误状态 + 一键续跑。

## 7. 分阶段计划

**Phase 0 —— 裁判先行（T0 + 快赢）**
- T0 全套：CI 门禁、黄金集、真实语料基线冻结、集成冒烟、判官版本化。
- 独立快赢（与主题解耦、小步可并）：声明 `python-docx`；合并裸
  `os.getenv`；修 `.env.example`；`dataset_queries` 遥测接线；重排预算
  耦合+降级旗标显式化；死代码清除（`retrieval_v2.py` 等）；stage_timings
  统一进 `/retrieve` meta；`/metrics` 首版；片段 PUT 丢 answer/keywords
  修复（路由一行 + 服务支持）；`batch-reindex` 全量模式 >200 文档截断修复。
- **出口准则**：CI 红绿敏感 + 基线冻结编号可引用。此前不做任何默认值
  变更。

**Phase 1 —— 生命周期与前端（T1 ∥ T5 ∥ T2-bake-off）**
- T1：状态机、内容哈希、稳定身份、单文档 reprocess/reembed、recover、
  §4.6 债务 1–4、修订原子性。
- T5：DatasetDetail 拆分 → 默认值统一 → chunk 级操作面 → 进度升级。
  （后端端点先行，前端随后接通；矩阵 §5 逐项变绿。）
- T2：中文词法方案对比 + 重排器 bake-off（只跑评测，不切默认）。
- **出口准则**：§6.4 场景 1/2/3/5/6/7 全过（六项均为 T1 所有；场景 4
  属 T3 蓝绿机器，归 Phase 2 出口）；§5 矩阵 #5/#6/#9/#14/#15/#16/#25 绿。

**Phase 2 —— 模型代际与词法放行（T3 ∥ T4 ∥ T6 ∥ T8）**
- T3：版本元数据 + 蓝绿机器 + Qwen3-Embedding 升级（真实切换演练）。
- T2 收尾：bake-off 获胜配置经门禁后切默认；后置阈值规则上线。
- T6：生命周期协议 + 并发集成测试 + 测试租户放行 `bm25_v2`。
- T4：IR 表结构 + 解析器接口 + 评测集（解析引擎替换本身放 Phase 3 灰度）。
- T8：scope 强制 + ACL 收归 KS + 死 schema 清理。
- **出口准则**：§6.4 场景 4（嵌入模型切换演练：全程检索可用、中断可续跑、
  回滚=翻指针）通过——即"一次完整模型切换演练通过"；一个租户在并发负载
  下完成 v2 切换与回滚。

**Phase 3 —— 结构升级与长尾（T9 ∥ T7 收尾 ∥ P2）**
- T9：层级灰度 → 默认；结构路由（面包屑/标题优先）；摘要索引可选层。
- T4 收尾：新解析引擎按租户金丝雀。
- T7 收尾：helm 处置、幂等 redis 化、上传拆分下沉。
- P2 池：外部联邦、深搜模式、语义查询缓存、多模态解锁、嵌入微调。
- **出口准则**：T0 基线对比报告覆盖所有已切默认；未通过门禁的项留在
  旗标后。

与 PPR 的并行关系：Phase 0–2 不触碰 PPR-06 的领地（worker 扩容、队列
所有权、交互 profile 的召回宽度）；Phase 3 的 T9 若涉及默认召回参数，
必须与 PPR-06 的 profile 认证协调（§9）。若 PPR-02 的 plane-boundary
gate 先落地，T8 的清理按其规则复核一次。

## 8. 质量纪律（所有主题适用）

1. **先基线后阈值**：任何门禁阈值在真实语料基线冻结前不得设定
   （agent-kb-eval 既有纪律，升级为全计划纪律）。
2. **评测分离**：检索指标门控索引/嵌入/重排变更；生成指标门控提示词/
   LLM 变更；红门禁必须能回答"哪边坏了"。
3. **默认值变更三件套**：基线对比报告 + 评测门禁通过 + 变更日志记录
   （架构 §4：合同变更是有意行为）。
4. **影子先行**：影响检索行为的变更先影子回放（租户分层抽样、
   pointwise 判分），再金丝雀，阈值自动回滚。
5. **不重做已修复项**：引用 §2.1 keep-list 与 SPO/PPR 的已验证清单；
   行动前用当前行号复核（PPR R8）。
6. **证据边界**：schema-ready ≠ 质量证据、fixture 通过 ≠ 线上质量——
   沿用 bm25-v2 文档的证据措辞。

## 9. 风险与硬边界（违反即停）

**合同冻结（架构 §4 + PPR H6）**：

- `sdk/openapi.json` 冻结期内，KB API 只能**严格增量**；现有路由行为、
  KB 工具边界（≤4096 字符、≤8 数据集、top_k 1–20、threshold 0–1）不放宽。
  删除/改名公开路由 = 合同变更流程（源真更新 + 门禁重跑 + CHANGELOG）。
- DB schema 变更的合法轨道只有两条：中央 `database/migrations/`（cli.py
  账本）与 `per_service/knowledge`（schema 限定，随部署态定
  public/knowledge）——这是一般合同；**本升级的 schema 增量（阶段时间戳、
  绑定表、嵌入出处列等）只落中央账本**（§2.2-E），不用 per_service 轨道；
  服务本地 `apps/knowledge-service/migrations/` 是无调用者镜像、启动自动
  应用已断（静默空转），**禁止**作为落点；`make migrate-status` 验证。
- 评测金夹具与 RAG fixture 的更新必须走重录程序（T0-#7）+ 证据记录。
- `.env.example` 新增环境面同步 `make validate-example-config`。

**PPR 边界**：

- 不动 worker 持久队列的所有权语义；不做进程再合并（api/worker 分离是
  PPR 的已验证起点）。
- **不做 knowledge-service 的 Rust 迁移**：Index plane 的 Rust 归属由
  PPR 拥有；本计划的合同变更严格增量、语义全部落在"可重建的事实源 +
  稳定身份"上（§1.3 理由 4），使未来迁移只需换执行底座。
- **分块边界字节一致性**：任何改变分块算法输出（字节级）的提案，先做
  全库重嵌影响评估；若与未来 Rust 内核的字节对比冲突，按 PPR-06
  "宁可取消内核工作也不接受边界漂移"处理——默认策略：只加路由/富集，
  不动边界算法。
- 交互 profile（12/12/24）的召回宽度属于 PPR-06 认证面；T2 若调整需
  同步质量复验并记录（C6 的前车之鉴）。
- PPR-02 plane-boundary gate 落地后，apps/* 的新增依赖方向按其复检。

**BM25 v2 边界**：

- 指纹（字段/模型、k、b、avg_len、tokenizer、language、lowercase、
  ascii_folding、token 长度预处理）冻结；变更 = 新版本字段或全量重建，
  绝不写入现有 `bm25_v2` 字段。
- `KNOWLEDGE_QDRANT__BM25_V2_ENABLED` 门闩与生产硬禁用不动；T6 交付的是协议与测试，
  放行决策单独走评审。
- `scripts/migrate_sparse_vectors.py` 永不用于 v2。
- 一切新增/重建的写点路径必须经 `vector_store.upsert`（scope 校验 +
  数据集租约 + lexical_v1/bm25_v2 shadow 注入，vector_store.py:2144-2190/
  :2354）——绕过即造成 shadow 索引失同步。

**多租户与数据边界**：

- 所有点写/查询继续强制租户+数据集过滤（经 VectorStore 适配器，不得绕过）；
  租户过滤参数来自认证身份，不接受客户端指定。
- 共享 Postgres（knowledge,gateway,assistant,public）：KB 迁移不得触碰
  其他 schema 对象；gateway 直读 KB 表的清理（T8）完成前，不在这些表上
  做破坏性重塑。

**运行环境**：

- 记忆预算（Qdrant 384m / KB 512m / worker 1 / ~4GiB 宿主）是合同的一部分；
  性能主张必须绑 PPR-00 负载剖面，不接受空载数字。自托管重排/嵌入推理
  容器独立于该预算（新增部署位，不挤占）。
- 常规验证用 `make hot-update` + `make status`；不做 `docker compose up
  --build`/`build`/`prune`/`down -v`；Docker 动作串行。

**三方文档冲突的显式裁决**（Contextual Retrieval）：

- 七月计划 C2（暴露前端开关）×SPO（禁止回默认摄入）×agent-kb-eval
  （仅当持久化+版本化+评测过才允许 contextual_v1）→ 本 PRD 裁决：
  **非默认、非 P0；作为 T0 基线之后的可选实验层**，带持久化列与版本，
  评测净胜才谈暴露（T5 矩阵不为它留位，改在分块配置里留旗标）。

## 10. 开放问题（需用户/评审决策）

1. **kb-tools 处置（已决策）**：当前产品 Agent 已通过
   `search_knowledge_base` tool → Capability Worker → Knowledge Service 的
   签名 HTTP/internal retrieval 链路接入 RAG，并由 Agent 负责多步/重写查询。
   Knowledge Service 只提供受权限保护的 HTTP/internal retrieval，不新增
   LangChain/LangGraph tool 工厂；后续 LangChain 等接入在 Agent/边界层封装。
   `/api/v1/kb-tools/*` 不属于当前 Agent 路径，本计划不新增其 search 面，也
   不擅自删除既有公开代理；若未来有外部消费者，另走公开契约变更。
2. **T6 全局升级（已决策）**：BM25 v2 生命周期协议纳入本计划，目标是对全局
   租户完成安全的 `lexical_v1 -> bm25_v2` 放行与可回滚；实际切换仍必须满足
   T0 门禁并完成 backfill、并发检索和 rollback 收据，不能只翻环境开关。
3. **解析引擎选型**：MinerU 3.x（能力最全，许可证附加条件需法务）vs
   PaddleOCR 级联（许可干净，中文同级）。建议：接口先行、两者都挂后端，
   默认给许可干净者。
4. **嵌入供给方式**：自托管 vLLM（数据不出域）是否唯一选项？DashScope
   text-embedding-v4 / Seed1.6 API 是否可接受（数据驻留评审）？
5. **重排/嵌入推理部署位**：512m/384m 预算外新增独立推理容器（建议），
   还是复用现有模型服务？
6. **SSE 进度**：Phase 1 末尾还是 Phase 2？（网关 ServiceProxy 已支持
   SSE 直通，成本主要在 KS 事件总线。）
7. **多模态**：本计划维持 `text_only`（与现状一致），附件数据模型（T4/
   3.1）只做地基。确认不在本轮解锁。
8. **集合策略**：确认保持"每数据集集合 + 绑定表"（推荐），不采 Dify 的
   每模型共享集合。
9. **KB 直接迁 Rust？**（用户 2026-08-28 提出）：本 PRD 的建议是**本次
   不迁**（§1.3 完整理由：程序冲突/风险叠加/收益错配）。若最终决定要走
   Rust，正确次序也是先在 Python 侧落地 T0–T3 的语义与评测门禁，再由
   PPR-06 的 Index plane 工作承接迁移，而不是两条线并行。待用户拍板。
10. **agent-kb-eval §4.6 债务 5 与债务 6 剩余子项的归属**：债务 5
    （HNSW/量化参数 sweep + 影子评测门控的 alias 切换）与债务 6 的版本
    路由次序、restore MAX+1 事务两个子项，在本 PRD 各主题中**无认领**，
    已显式延后（附录 B）。建议：债务 5 在 T3 蓝绿机器落地后作为后续
    索引调优立项（可复用影子/别名机制）；债务 6 两子项可归入 T1 后续
    小主题或单独立项。若评审认为必须本轮完成，需相应扩大 T1/T3 范围。

## 附录 A：预期主要文件清单（实施 session 的地标）

后端（apps/knowledge-service/src/knowledge_service/）：
`services/knowledge/worker.py`（状态机+哈希跳过+recover）、
`document_service.py`/`ingestion_service.py`（reprocess/reembed 端点服务层）、
`api/routes/knowledge.py`（新端点、批量操作、查询遥测）、
`persistence/datasets.py`（阶段时间戳/哈希/绑定表）、
`services/knowledge/retrieval_service.py`（遥测、降级、后置阈值）、
`text_reranker.py`（预算耦合）、`cache_manager.py`（LRU+版本键）、
`lexical_config.py` + 新增迁移（`database/migrations/`）。

前端（web/）：`src/pages/knowledge/DatasetDetail.tsx`（拆分）、
`detail/`（chunk 操作组件）、`src/api/knowledge.ts`（新端点客户端）、
`src/hooks/useKnowledge.ts`（条件轮询→SSE）、
`src/pages/knowledge/detail/RetrievalEvalWorkbench.tsx`（持久化）。

门禁：`Makefile`（kb-unit-gate）、`.github/workflows/ci.yml`、
仓库根 `harness.yml`（触发映射）、`docs/harness/commands.md` §7。

## 附录 B：与在途文档的对齐表

| 文档 | 关系 |
| --- | --- |
| `docs/plans/kb-rag-optimization-plan.md`（2026-07-20） | 合并并取代：F1/F4 已交付确认；F2 默认一致性→T5-#1；F3 分块暴露→T5-#3（Contextual 部分按 §9 裁决）；F5 rerank top_n 规范化→并入 T2-#4（rerank 后置参数语义统一），"模型推荐"不单做、由 bake-off 基线证据替代；F6→T5-#5；F7 配置/实验历史+趋势图→**明确延后**（P2，依赖 T2-#5 遥测先落库）；F8 可观测看板：后端遥测=T2-#5（查询遥测）+T7（/metrics），KB 详情页"洞察"子 tab UI→**明确延后**（P2，前端带宽保留给 §5 矩阵）；F9 HyDE/late-chunking→§3.3 非目标（留旗标）；F10→T5-#2。实施后该文档标注"superseded by 本 PRD"。 |
| `docs/plans/knowledge-bm25-v2-shadow-rollout.md` | 遵守其证据边界与指纹冻结；T6 建造其 §Rollout-5 要求的协议；shadow 语义在放行前不变。 |
| `deploy/runbooks/platform-plane-restructure/`（PPR） | 不搬不重写 KB；遵守 §9 边界清单；PPR-06 的交互 profile 与 worker 扩容与本计划互不侵入；检索质量变更的管辖权由本 PRD（承继七月计划）承担。 |
| `deploy/runbooks/agent-kb-eval-optimization-20260802/` | §4.6 六项债务的 1–4 剩余缺口由 T1 关闭（#1 已于 2026-08-02 commit 58e4cd4 大体落地，剩余=统一合同+缓存失效+文档恢复热更新，见 B3；#2→T1-#5、#3→T1-#8、#4→T1-#8）；#6 的分数边界子项由 T2-#4 承接；**#5（HNSW/量化 shadow sweep）与 #6 的版本路由次序、restore MAX+1 子项无主题认领，显式延后**（理由与后续路径见 §10-10）。K1/K2 的真实语料工作由 T0 基线 + T2 bake-off 承接。 |
| `reports/code-review/perf-review-2026-08-16.md` | 未修项映射：串行 LLM 摘要与 100% 重处理→T1；事件循环上的分词→T2；MMR O(n²)→T1-#8（与 B5 正确性修复同址，retrieval_service.py:2116）；轮询/防抖→T5；C5 缓存（FIFO→LRU、版本键）→T2-#7。"检索缓存禁用"一项**不移植**：源报告 §M12 已判为误报（缓存实际启用、ttl=300s）。 |

## 附录 C：本 PRD 的证据链索引

- 自身审计：10-agent 工作流（ingestion/api-data/retrieval/api-data/ops/
  gateway/web/plans/tests-eval），结构化发现存档于会话工作流目录
  （`wf_4e8c5079-e7e`）。
- Dify 1.17.0 本地检出（/Users/yang/projects/opensource-harness/dify）：
  12 区域拆解（数据模型/摄入/抽取/分块/嵌入/向量库/检索/管线插件/
  API 面/web UX/运维/质量评测）。
- SOTA 调研（2026-08）：chunking（Vectara NAACL 2025、FloTorch 2026、
  arXiv:2606.00881、ECIR 2025 late-chunking、arXiv:2604.01733）、
  retrieval-rerank（arXiv:2606.28367、qdrant/fastembed#610）、
  embeddings（MTEB v2/MMTEB、Qwen3 家族）、parsing（OmniDocBench v1.6）、
  eval（RAGAS 0.4.x、MTEB 污染研究、RGB/CRAG）、architectures
  （Qdrant 多租户指南 1.16+、Dify Knowledge Pipeline、RAGFlow 0.21+）。
- 终稿对抗审计（2026-08-28，工作流 `wf_4532cdba-eb6`）：lifecycle/
  retrieval/dataops/frontend/constraints/dify/consistency 七个找错维度，
  每个维度的发现由独立验证 agent 逐条反驳；30 条原始发现、20 条幸存、
  10 条被驳回（误引文档原文或措辞级挑剔）。幸存发现全部修正或确认已被
  先前轮次覆盖；high 级发现的代码引用（document_service.py:563/:1154-1299、
  vector_store.py:2705-2833、retrieval_service.py:2239-2335/:971-981、
  index_node_hash 无写入方）由主会话独立复核。
