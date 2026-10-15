# 知识库 RAG 升级 PRD 增补：Dify 2026 深读差量（2026-08）

Status: 已核查，与主 PRD 配合使用。本文档是
`docs/plans/rag-upgrade-prd-2026-08.md`（commit `691f01a`，下称主 PRD）
的唯一增补：**不新增主题、不改 §7 阶段计划、不动 §9 硬边界**，只向既有
主题注入合同细节、补拒绝清单、并提出待决的 §5 矩阵候选行。实施 session
的读法：主 PRD 对应主题 + 本文档对应节。本文档引入的 schema 增量
（摘要表、附件绑定表、处理输入快照等）遵守主 PRD §9——只落中央迁移账本。

证据基础：2026-08-28 深读工作流 `wf_07d8fa90-edb`（8 agent，256,696
tokens，522 次工具调用）对本地 Dify 检出（`/Users/yang/projects/
opensource-harness/dify`，HEAD `f7d6cd1a` = 1.17.0 发布后 3 天的 main；
shallow 克隆，全部结论直接读码、附文件路径）逐子系统对照主 PRD §3.1
采纳清单；本文档的代码引用另经对抗核查（附录 B）。

---

## 0. 摘要

**主题级结论：无遗漏。** 主 PRD 已把 Dify 结合完整——Dify 2026 年的
五个 KB 新能力（Summary Index 1.12、多模态 1.11、Knowledge Pipeline、
父子分块、TiDB/Hologres 混合后端）全部落在主 PRD §3.1/§3.2 的采纳、
改造或延后决策之内。三处我们的设计经深读确认对 Dify **严格占优**，
保持不学：

1. **父子块解析**：我们 payload `parent_segment_id` + rerank 后库内聚合
   + 显式 fan-out top-k；Dify 查询时 Postgres join 解析父块
   （retrieval_service.py:575-584，多一次往返、无法库内聚合），且在
   **子块级截断**后才折叠到父块——返回父块数可能远少于 top_k。
2. **队列路由**：我们按操作类型/批大小（主 PRD adapt-3）；Dify 双队列
   只看计费档位（task_proxy.py:89-99），与操作类型无关。
3. **逐阶段状态机**：Dify 的 pipeline 模式把文档状态塌缩为
   waiting→completed|error，多分钟进度藏进节点执行表——我们的逐阶段
   合同（主 PRD direct-1）对 SSE 进度与 recover 分支都更好。

细节级有真增量，分三类：

- **§1 按主题吸收的合同细节**——不改范围，实施期直接生效；
- **§2 不抄的坑**——补入主 PRD §3.1 拒绝清单；
- **§3 §5 矩阵候选新行**——改变 H1 验收范围，**需用户决策**；未决策前
  实施 session 不动主 PRD §5 矩阵。

## 1. 按主题吸收的合同细节

### T1（生命周期 / recover / 重处理）

1. **recover/retry 双动词合同**：recover=增量续跑（只重放已持久化且
   status≠completed 的块，同 chunk/向量 ID → 幂等 upsert）；retry=清理
   重建。API 层显式区分两个动词。
2. **块先以 enabled=false 落库**、按 worker 组提交翻转——进度信号与
   检索可见性来自同一行状态。
3. **每文档重放快照**：提交时落处理输入记录（数据源信息+生效规则/配置+
   触发点，Dify 合同为 DocumentPipelineExecutionLog），reprocess/recover
   按**快照版本**重放（Dify 按"当前已发布版本"重放，在途文档会静默
   漂移——不学）。
4. **发布期配置晋升 + 不可变守卫**：发布是唯一可设置"嵌入模型+集合
   绑定+分块结构"的时机；首次发布后冻结分块结构——这是主 PRD §3.1
   direct-3（规则快照）与 adapt-1（绑定表）之间缺失的胶合。
5. **预发布校验清单**（终态配置校验通过后才快照）。
6. **嵌入批处理结构**：外层 1000 + 内层 provider MAX_CHUNKS；并行块写
   用 content-hash % workers 分片避免死锁。
7. **多模态地基当日接线**：SegmentAttachmentBinding 清理必须进单文档
   reprocess 路径（Dify 的 recover/reprocess 从不清理绑定行与图片点，
   无 FK，孤儿静默累积）。表形：(tenant_id, dataset_id, document_id,
   segment_id, attachment_id, created_at) + 四列复合索引 + attachment_id
   索引；补 Dify 缺的 FK 或延迟清理钩子与唯一约束。
8. **稳定 ID 设计内预留两个约定**：点 ID 可用附件文件 ID、payload 携带
   doc_type——未来图片向量与文本同集合共存，免 schema 手术。

### T2（检索 / 重排）

1. **threshold=0 纪律扩到所有检索腿**（Dify #35233 只修了 hybrid，
   semantic/full-text 腿仍在 rerank 前对原始引擎分预筛，
   retrieval_service.py:328-339——半截修复，不学）。
2. **融合并集去重**：O(n)、按稳定块 ID、留最高分、保首见序。
3. **重排返回后防御性重施**：客户端重施 threshold+top_n 并按返回分
   重排——bake-off 硬性要求（provider 插件过滤行为不一致）。
4. **RRF 默认地位加强**：Dify 加权融合无归一化（ts_rank ~0.1 与 cosine
   ~0.8 混进同一加权和，`_calculate_cosine` 盲目复用已有
   metadata['score']，weight_rerank.py:181-182）——若保留加权选项，必须
   每请求每腿 min-max 归一化 + 服务端权重和校验（Dify 两者皆无）。
5. **混合查询合同（供 T6）**：同后端两腿、同一 kwargs 契约；命中携带
   分腿分数字段（vector_score、lexical_score）+ 独立 fused/rerank 分
   ——禁止 Dify 的单一 score 字段混装。
6. **无重排回退场景显式定义**：阈值是否施加于融合分（Dify 此场景下无
   融合、原始分过滤会把无分数的词法命中全部杀掉，
   retrieval_service.py:910-913）。
7. **中文安全的块搜索配方**：escape_like_pattern + JSONB 关键词。

### T3（嵌入迁移）

1. **摘要侧两段式再生**：摘要 LLM 模型变更=重生内容；嵌入模型变更=仅
   重嵌现有 summary_content（regenerate_summary_index_task.py:23-42）
   ——蓝绿迁移合同必须覆盖摘要向量。反面教材：Dify 嵌入模型变更走
   PATCH 静默改 collection_binding_id、块不重嵌（记得摘要、忘了块）。
2. **绑定表行携带能力标志**（如 vision-capable），随蓝绿迁移走。

### T5（前端）

1. **SSE 事件形状钉死**：id=task:seq、progress {percent, stage, state}、
   终态事件、last-event-id 续传（KnowledgeFS 合同证明这是正确接口）。
2. **进度端点发 completed/total 块计数**（廉价 count 查询）；**批次戳
   轮询模式**：创建返回 batch id → 轮询 /batch/{batch}/indexing-status
   （比文档 ID 枚举更贴合主 PRD direct-7）。
3. **批量生命周期单端点**（enable|disable|archive|un_archive）：两遍式
   先全量校验后单事务提交（dataset_service.py:3190-3250）；重试在任何
   写之前先锁整批（:2113-2153）。
4. **hit-testing 父块卡片下折叠展示命中子块**（含逐子块分数与 edited
   标记）。
5. **能力感知引导卡**：所选嵌入模型缺能力时在创建/设置页显示可关闭
   提示（1.17.0 新增的廉价 UX；同样适用于重排器/未来模型缺口）。
6. **运维读端点族**（廉价高信任价值）：/error-docs 快捷、删除守卫
   （use-check + related-apps 前置检查）、系统发起禁用的审计行。
7. 注：Dify 经典知识库**没有**版本历史、**没有**反馈面板——我们的
   版本历史+diff 与 👍/👎 飞轮没有 Dify 参照，按自研设计。

### T6（bm25_v2 协议）

1. **切换分类学二选一**：原地幂等 ensure-index（catalog check 后
   create，TiDB 模式）或重建 + 查询时缺失检测（field_exists，
   Milvus/OceanBase 模式）——没有第三条路；缺失时禁止静默返回 []，
   必须响亮回退或显式报错（Dify 的能力探测缺失会静默降级：OceanBase
   返回 []、Qdrant 返回无排名 scroll、tidb-on-qdrant 直接忽略查询
   ——比失败更糟）。
2. **tokenizer/analyzer 作为集合/部署级配置** + 允许名单 + 切换时引擎
   版本门禁（OceanBase 模式）；禁止按语言 fork 后端（ES-ja 反模式）。
3. **生命周期状态进持久表**——Redis TTL 旗标/锁（TTL 3600s、20s 创建
   锁）是反模式。

### T9（父子 / 摘要）

1. **保持我们的机制**（见 §0 占优 1）：payload parent_segment_id +
   rerank 后按父聚合 + 显式 fan-out top-k，不学 Dify 的 join 与子块级
   截断。
2. **吸收**：parent_mode 配置维度（段落父块 vs 全文父块）+ 子块独立
   切分规则（分隔符/尺寸/重叠）进不可变规则快照；单文档重处理支持只
   重切一个父段的子块；父块编辑带 regenerate_child_chunks 标志。
3. **摘要合并语义钉死**：摘要命中→返回原块、摘要前置于 LLM 上下文、
   分数 = **max(块分, 摘要分)**（Dify 用摘要分覆盖块自身分数是 bug，
   retrieval_service.py:698-706）；摘要表加 UNIQUE(dataset_id, chunk_id)
   （Dify 无唯一约束、并发路径产生重复行、靠 ~100 行合并/补偿代码
   兜底）；禁用=删向量留行、与块对称；index_node_hash 跳过扩展到摘要
   （按**源内容**哈希——Dify 存了 summary_hash 却从不用作跳过条件）；
   LLM 重的摘要生成走专用队列（Dify 以单测强制 dataset_summary 队列
   隔离）；SUMMARIZING 派生徽章 + 每段摘要状态端点 + 可编辑摘要文本框
   + hit-testing 摘要标注。

### 验收增补（各主题 Done-when 追加项）

| 主题 | 追加验收 |
| --- | --- |
| T1 | recover/retry 双动词均有端点且语义分别测试；在途文档按重放快照重处理、不漂移到新配置（并发测试）；附件绑定清理进 reprocess 路径（故障注入后孤儿绑定/点计数为零）；绑定表唯一约束 + FK 或延迟清理钩子存在 |
| T2 | 所有腿预筛阈值恒 0 有测试；重排返回后防御性重施 threshold/top_n 并重排有测试；若保留加权融合：每请求每腿归一化 + 服务端权重和校验有测试 |
| T3 | 嵌入切换演练覆盖摘要向量两段式再生（换嵌入只重嵌、不换内容）；绑定表行携带能力标志 |
| T5 | SSE 断线 last-event-id 续传测试；批量操作两遍式提交测试（校验失败零副作用） |
| T6 | 索引缺失响亮失败测试（禁止静默 []）；生命周期状态在持久表（无 Redis TTL 旗标） |
| T9 | 摘要合并 max() 测试（块同时直接命中与经摘要命中时保留两者最大值——Dify 此处是覆盖 bug）；摘要表 UNIQUE(dataset_id, chunk_id)；父块折叠后返回父块数 ≥ 请求 top_k（语料不足除外） |

## 2. 明确不抄的坑（补入主 PRD §3.1 拒绝清单）

- 嵌入结果 NaN 跳过后续跑（文本↔向量对位损坏，cached_embedding.py:
  72-89）——批失败或显式占位。
- 租约用裸 key delete 释放（可跨请求误删他人锁，
  retry_document_indexing_task.py:119）——带 token 校验释放。
- 段 PATCH 请求内同步 LLM 生摘要（请求延迟耦合 LLM）——入队。
- 重摄入孤儿摘要：批量删块只清块向量、不清摘要行/摘要向量（无 FK
  级联）——清理必须对称；过期摘要向量会持续占用 top_k 预算。
- 摘要/图片腿无每查询退出机制，污染所有向量检索的 top_k 预算——提供
  退出开关或额外 k 预算。
- 索引预估（预览）阶段同步烧真 LLM token——只展示提示词。
- 加权融合静默丢图片命中（weight_rerank.py:50-53）、非视觉重排器透传
  图片不排序——任何融合后阶段必须对每种腿类型有定义行为，禁止静默
  丢弃。
- 批量导入任务态只存 Redis（无租户范围、任务 ID 可猜、状态轮询无属主
  校验）——任务进 Postgres 带租户范围。
- 暂停/恢复存在两套语义分裂的表面——只留一对规范端点。
- 除锁失败 `except LockNotOwnedError: pass` 静默丢弃文档写入——锁丢失
  必须显式失败重入队。

## 3. §5 矩阵候选新行（**改变 H1 验收范围，需用户决策**）

| 候选行 | 说明 | 建议 |
| --- | --- | --- |
| KB 级 ACL/共享 | 成员部分共享 + 13 键 RBAC（含 document_download / retrieval_recall scope） | 与 T8 范围联动决策 |
| 源文件下载/批量 ZIP 导出 | 签名 URL 单文件 + 流式 ZIP | [判断] 建议纳入（低成本高信任） |
| 删除守卫 | use-check + related-apps 前置检查 | [判断] 建议纳入（廉价） |
| 标签 CRUD + 绑定 | KB 组织/过滤 | P2 |
| CSV 批量段导入 | 批量创作路径（任务持久化进 Postgres） | P2 |
| tsne 命中散点 | 评测台可选可视 | P2 |
| 系统禁用审计 | 可被 #28 健康收据面板吸收 | 并入 #28 行 |

**[判断]** 默认只把"删除守卫 + 源文件下载"两行纳入 H1（主 PRD §5 追加
#31/#32 行），其余候选行留待用户决策；未决策前实施 session 不动 §5
矩阵。

## 附录 A：与主 PRD 的关系

- 读序：主 PRD 全文 → 本文档对应节；冲突时以主 PRD §9 硬边界为准。
- 本文档不改变主 PRD 的任何既有验收；§1 各条是对应主题的**追加**合同
  细节；验收增补表是各主题 Done-when 的追加项。
- §3 未决策前，主 PRD §5 矩阵保持原样（H1 覆盖 1–29 行 + P2 第 30 行）。
- 本文档引入的新表/新约束（摘要表、附件绑定表、处理输入快照等）全部
  走中央 `database/migrations/` 账本（主 PRD §9）。

## 附录 B：证据链与核查记录

- 深读工作流 `wf_07d8fa90-edb`（2026-08-28，8 agent，256,696 tokens，
  522 次工具调用）：summary_index / knowledge pipeline / 多模态 /
  ChildChunk / 检索重排 / 新向量后端 / 摄入运行时 / API 面，8 个子系统
  逐码深读 × 对照主 PRD §3.1。
- 对抗核查工作流（2026-08-28）：见文末更新——本文档每条代码引用由
  独立验证 agent 逐条反驳，"已覆盖/占优"断言对照主 PRD 原文复核，
  §9 硬边界相容性单独核查。
