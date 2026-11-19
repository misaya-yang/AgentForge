# KB / RAG 企业级优化计划

> **状态:** superseded — 当前合同是 `kb-rag-upgrade` worktree 的 2026-08 RAG PRD/增补；本文只保留
> 2026-07 背景，不得指导当前实现。

> 目标：将现有知识库（KB）RAG 模块打造为**企业级、前端可配置 / 可管控 / 可测评**的工作平台。
> 对标：**Dify 知识库** 与 **阿里云百炼（Model Studio）知识库** 的产品形态。
> 方法：代码精读（现状盘点）＋ 联网对抗式调研（参数/方案/陷阱，带引用）＋ 分阶段落地 ＋ Playwright 真人式回归。
> 日期：2026-07-20 · 版本：v1.0

---

## 0. 执行摘要（TL;DR）

**结论一：后端已接近"Advanced/Modular RAG"成熟度，功能几乎齐全。** 分块 9 策略（含父子 small-to-big、Contextual Retrieval）、混合检索（dense+BM25）、RRF(k=60)+加权融合、MMR、语言自适应权重、多查询改写、4 种 reranker（带缓存/熔断）、多模态重排、Self-RAG 式自适应检索，以及一整套 **eval 平台后端**（评测数据集 / 评估器(含 ragas) / 实验+运行+基线晋升+运行对比(A/B)+门禁）。

**结论二：真正的缺口集中在"前端可配置/可管控/可测评"层，尤其是把已有后端能力串成 KB 专用工作台。**
1. 缺 **KB 检索评测工作台**（现有 eval 平台面向 agent/assistant trace，未接入"检索/分块配置 A/B → 评测集 → 检索质量+RAGAS → 门禁"闭环）。
2. 缺 **检索质量硬指标**（hit-rate / MRR / nDCG / recall@k）——后端 RAGAS 服务目前只有 LLM 指标。
3. **配置 UX 与 Dify/百炼 有差距**：检索测试 tab 默认用 *weighted* 融合、rerank/mmr 默认关（与后端 RRF+rerank 最佳默认不一致）；缺预设下拉、参数推荐值 tooltip；父子/语义/contextual 分块前端未充分暴露。

**最高优先级（P0）**：补齐检索硬指标（后端小改）＋ KB 检索评测工作台（前端核心新增）＋ 检索测试 tab 预设化与默认一致性修复（前端）。

---

## 1. 现状评估（代码盘点）

### 1.1 后端 RAG 管线（`apps/knowledge-service/`）

| 能力 | 模块 | 现状 | 成熟度 |
|---|---|---|---|
| 分块 | `services/knowledge/chunking.py` (~3000行) | 9 策略：automatic/fixed/paragraph/page/heading/regex/separator/recursive/**hierarchical(父子)**/qa；token 级精确切分(tiktoken cl100k)；多语言校准(ar/zh)；`merge_small_chunks`/`enforce_token_limits` 后处理 | ★★★★ |
| 结构感知分块 | `structured_document_parser.py` | 按文档结构生成语义 chunk | ★★★ |
| Contextual Retrieval | `contextual_retrieval.py` | Anthropic 方案：chunk 前置 50-100 token 上下文（模板/LLM 两策略） | ★★★★ |
| 嵌入 | `embedding.py` + `config` | gemini-embedding-001(默认)/dashscope/siliconflow；多模态 tongyi-embedding-vision | ★★★★ |
| 向量库 | Qdrant (`vector_store.py`) | dense + 稀疏；metadata 过滤 | ★★★★ |
| 检索管线 | `retrieval_v2.py` | RetrievalPipeline：dense+BM25+hybrid；**RRF(k=60)**+加权；**语言自适应权重**(ar 提升 BM25)；**stage 级分数追踪**(可解释) | ★★★★★ |
| 检索编排 | `retrieval_service.py` | **多查询扩展/改写**(`max_query_expansions=3`,`MULTI_QUERY_TOP_K`)；over-retrieve(`top_k*4,≥20`)；全局去重 | ★★★★ |
| 融合 | `retrieval_v2.py` | `rrf_fusion`(minmax 归一)、`weighted_fusion` | ★★★★ |
| MMR | `retrieval_v2.py::apply_mmr` | lambda 可调，cosine 多样性惩罚 | ★★★★ |
| Reranker | `text_reranker.py` | DashScope gte-rerank / **BGE bge-reranker-v2-m3** / Cohere / 本地 cross-encoder；LRU 缓存+熔断/健康探测 | ★★★★★ |
| 多模态重排 | `multimodal_reranker.py` | VLM 跨模态打分 | ★★★★ |
| 预设 | `retrieval_config.py::DEFAULT_CONFIGS` | fast/balanced/accurate/diverse/**sota** | ★★★★ |
| 自适应检索 | `assistant-service/.../query_intent_analyzer.py` | Self-RAG 式：决定是否检索；`scenario_aware_retriever.py` 场景化多查询 | ★★★★ |
| 配置持久化 | `persistence/database.py` | `datasets.index_config`(jsonb)、embedding_provider/model/dimension、needs_reindex | ★★★★ |

### 1.2 评测后端（`/api/v1/eval/*` + `services/eval/`）

- **KB RAGAS**（`ragas_eval_service.py`）：context_relevancy / context_precision / context_recall / faithfulness / response_relevancy（LLM-as-judge，对齐 RAGAS）。
- **eval 平台**（`api/eval.ts` 揭示的完整后端）：
  - 评测数据集：`/eval/datasets`（CRUD、examples、`:import`/`:export`/`:from-trace`）
  - 评估器：`/eval/evaluators`（rule/llm/llm_judge/trajectory/composite/**ragas**）
  - **实验**：`/eval/experiments` + `:run`（run_mode=rescore_trace/live_candidate，repetitions，baseline）
  - **运行对比(A/B)**：`/eval/experiment-runs:compare`（baseline vs candidate，deltas，regression_summary，gate）
  - **基线晋升**：`:promote-baseline`
  - **门禁**：`/eval/gates:dry-run`（pass/fail/warning + thresholds）
  - **KB 批评分**：`/eval/knowledge/{dataset_id}/batch-score`、`/eval/knowledge/score-retrieval`、`/eval/knowledge/summary`
  - trace 看板：`/eval/summary`、`/eval/dashboard`、`/eval/traces`

### 1.3 前端（`web/src/pages/knowledge/` + `web/src/pages/eval/`）

- `DatasetDetail.tsx`(4635行)：文档管理、分段列表(ChunkCard/HierarchicalSegmentCard)、**检索测试 tab**（mode/top_k/fusion/dense-bm25 权重/score_threshold/rerank 模型/mmr）、RAGAS 单查询评分(`scoreKbRagasRetrieval`)、设置 tab（chunk_size/overlap、retrieval mode/top_k/fusion/权重/rerank/mmr-lambda、embedding 模型）、版本历史、Confluence 同步、**chunk 预览**(`previewChunking`)。
- `Datasets.tsx`：知识库列表/创建（含 embedding 模型选择）。
- `pages/eval/index.tsx`：**通用 eval 平台 UI**（数据集/实验/运行/对比/门禁/基线晋升），`ExperimentRunComparison.tsx`/`ExperimentRunResults.tsx`——**但面向 agent/assistant trace，非 KB 检索配置**。
- Playwright：`playwright.config.ts` + `e2e/global.setup.ts`（bootstrap 自动注入鉴权 storageState）+ `knowledge-workflow.spec.ts`、`eval-trace.spec.ts` 等 21 个 spec。

### 1.4 成熟度定位

按调研中的 **Modular-RAG 成熟度分类（Naive → Advanced → Modular）**（Gao et al., arXiv:2312.10997）：
- **后端 ≈ Advanced→Modular**：可配置管线、多路召回、融合、重排、自适应、评测闭环均已具备。
- **前端 ≈ Advanced**：可配置但**缺测评闭环的 KB 化、缺预设化 UX、缺检索硬指标可视化**。

---

## 2. 联网调研结论（经 3 票对抗验证，24/25 confirmed）

### 2.1 检索架构

| 结论 | 证据（已验证） | 对我们的含义 |
|---|---|---|
| **混合检索(BM25+dense)应作为默认**，优于单路 | TREC DL19 mAP：BM25 30.13 → 监督 dense 44.66 → hybrid 47.14 → HyDE+hybrid 52.13（arXiv:2407.01219, EMNLP'24） | 我们默认 `hybrid` ✔；继续 |
| **稀疏权重 α≈0.3 最优**（该基准），但**数据集相关** | 同上，融合式 S_h=α·S_sparse+S_dense；Dynamic Alpha Tuning(arXiv:2503.23013) | 我们 dense 0.6/bm25 0.4 接近；**前端应允许按库调 α 并 A/B 验证** |
| **RRF**：score=Σ 1/(k+rank)，rank 从 1 起，**k 默认 60，≥1** | Elastic 官方文档（primary） | 我们 `rrf_k=60` ✔ 完全对齐 |
| **交叉编码重排大幅提升**：MS MARCO MRR@10 BM25 11.65 → monoT5 31.78；**~50 个一阶段候选送入 reranker** | arXiv:2407.01219 Table 9 | 我们 over-retrieve `top_k*4(≥20)`；**建议 rerank 候选提升到 50-100，final top_k=5-10** |
| **生产模式**：over-retrieve top-20（或 top-k×5）→ rerank → 仅 top 3-5 送 LLM | opcito/dev.to（多源佐证） | 与我们 sota 预设(top_n=10)一致；可在 UI 暴露"召回深度 vs 送入 LLM" |
| **HyDE 是 opt-in 的延迟/质量权衡**：1 个伪文档足够（mAP 50.87 vs 8 个 51.64，但延迟翻倍 7.2→14.1s）；纯 Hybrid 0.477@1.45s ≈ HyDE 0.478@11.7s | arXiv:2407.01219 | **HyDE 做成可选开关，默认关**；我们尚无 HyDE → P2 增强 |

### 2.2 分块

| 结论 | 证据 | 含义 |
|---|---|---|
| **无普适最优 chunk size**；按答案类型调：**简短事实 64-128 tokens，分散/技术 512-1024 tokens** | Bhat et al. arXiv:2505.21700（COLING'25 佐证） | 我们 token_limit=400-500 合理；**前端按"答案类型"给推荐档位** |
| **生产默认**：~1000 字符 + ~200 字符(10-20%)overlap + **前置上下文/元数据头** | orkes + Anthropic Contextual Retrieval（primary 佐证） | 我们 char 默认 2000/overlap 300 ≈ 对齐；**Contextual 前端要暴露开关** |
| **语义分块**：按相邻句嵌入相似度跌破 breakpoint 阈值切分（LlamaIndex `breakpoint_percentile_threshold`） | LlamaIndex 文档 | 我们 `structured_document_parser` 有结构语义；**可补 embedding-based 语义分块（P2）** |
| **Late chunking**：全文长上下文 transformer 后再 mean-pool，chunk 向量保留跨块上下文；BeIR nDCG@10 +1.5~1.9 | Jina arXiv:2409.04701（ICLR'25） | **P2 增强**：需长上下文 embedding 模型 |
| **父子(small-to-big)**：小块检索精度 + 大块上下文 | 行业共识 | 我们 hierarchical 已有；**前端暴露 parent/child token 配置** |

### 2.3 评测

| 结论 | 证据 | 含义 |
|---|---|---|
| **RAGAS 三元组**（Context Relevance / Faithfulness / Answer Relevance）+ 组件级指标（context precision/recall、faithfulness=原子声明支持率、answer relevancy） | RAGAS paper arXiv:2309.15217 + 官方文档 | 我们 RAGAS 服务已覆盖 ✔ |
| **检索必须与生成分开评测**（答案 95% 正确但检索精度仅 60% 的案例）；报告**顺序敏感指标 MRR/nDCG** + 顺序无关 Precision/Recall/**HitRate**@K | swept.ai + arXiv:2504.20119 | **我们最大缺口**：补 hit-rate/MRR/nDCG/recall@k |
| **检索目标参考值**：Precision@5 ≥0.7(窄域)/0.5(宽域)，Recall@10 ≥0.8(@k=20)，nDCG@10 ≥0.8 | futureagi（实践） | 作为门禁默认阈值参考 |
| **LLM-as-judge 需谨慎**：63 篇综述仅 6 篇与人工对比；对 prompt 高度敏感 | arXiv:2504.20119 + ACL'25 Findings | **门禁不应只看 LLM 分**；固定 judge prompt/模型版本；关键决策抽样人工复核 |
| **CI/CD 质量门禁**：faithfulness/recall/latency 偏离阈值则阻断 | dextralabs | 我们 `/eval/gates:dry-run` 已有基础 |

### 2.4 调研未能验证的开放问题（用领域知识补足，落地时 A/B 验证）

- **MMR lambda**：无统一最优；**实践默认 0.5-0.7**（偏向相关性），多样性场景降到 0.3-0.5。我们 sota 预设 lambda=0.7 合理。建议：**默认关 MMR，仅在"多文档摘要/浏览类"查询开启**。
- **命名 reranker 对比**：BGE bge-reranker-v2-m3（多语言、可本地、1024 tok）、Cohere rerank-multilingual-v3.0、Jina reranker v2 均为一梯队；monoT5 是性价比 sweet spot。**top_n（rerank 深度）经验：一阶段召回 50-100，rerank 后留 5-10**。
- **企业 A/B 机制**：核心是"配置即实验"——每个检索预设作为一个 experiment run，对比 baseline 的指标 delta + 门禁。我们后端已具备，前端需 KB 化。

---

## 3. 分主题优化方案

### 3.1 Chunking 分块

**保留**：9 策略、token 级切分、多语言、父子块、Contextual Retrieval。

**优化项**：
- **C1（前端暴露）**：设置面板完整暴露 `mode`（含 hierarchical/heading/qa）、`token_limit`、`min/max_chunk_tokens`、`parent_token_limit/child_token_limit`、`parent_mode`、`strict_section_traceability`，并按"答案类型"给推荐档位（事实型 128 / 通用 400 / 技术长答 800）。
- **C2（Contextual 开关）**：前端暴露 Contextual Retrieval 开关（模板/LLM 两模式 + 成本提示）。后端已有。
- **C3（实时预览）**：`previewChunking` 已有 → 前端在设置面板嵌入"输入样本文本→实时看 chunk 数/平均 token/分布直方图"（Dify/百炼 均有）。
- **C4（P2：语义分块）**：新增 embedding-based semantic chunker（breakpoint 阈值），作为 `mode=semantic`。
- **C5（P2：late chunking）**：长上下文 embedding 模型下的 late chunking 选项。

**推荐默认**（写入 UI tooltip）：通用 `token_limit=400, overlap=50(≈12%)`；父子 `child=400 / parent=1500`；事实密集型 `token_limit=128`。

### 3.2 检索 / 融合 / 多样性

**保留**：hybrid 默认、RRF k=60、语言自适应、多查询改写、over-retrieve。

**优化项**：
- **R1（默认一致性修复）**：前端检索测试 tab 与设置默认统一为 **RRF + rerank 开（对齐后端 balanced/sota）**，消除当前"前端默认 weighted、rerank/mmr 关"的不一致。
- **R2（预设下拉）**：前端引入后端 `DEFAULT_CONFIGS`（fast/balanced/accurate/diverse/sota）作为一键预设，选预设自动填充 mode/top_k/fusion/权重/rerank/mmr。
- **R3（召回深度参数）**：暴露 `vector_top_k / keyword_top_k / candidate_pool_size`，并给出"rerank 候选 50-100、final top_k 5-10"的推荐。
- **R4（P2：HyDE）**：新增 HyDE 开关（默认关，提示延迟权衡）。
- **R5（MMR 使用指引）**：UI 标注"MMR 适合多文档浏览/摘要类查询；精确问答建议关闭"，lambda 默认 0.7。

### 3.3 Reranker 重排

**保留**：4 provider、缓存、熔断。

**优化项**：
- **K1（默认模型）**：多语言库默认 **bge-reranker-v2-m3**（可本地、AR-EN 佳）；云托管可选 gte-rerank-v2 / cohere。前端按"是否多语言/是否允许本地推理"推荐。
- **K2（top_n 规范化）**：rerank `top_n` 默认 = final top_k（如 5-10），一阶段召回提升到 50-100。避免当前 `top_n=None`（全量）带来的延迟。
- **K3（可观测）**：rerank 命中缓存率、provider 熔断状态接入看板。

### 3.4 评测（核心）

**保留**：RAGAS LLM 指标、eval 平台后端。

**优化项**：
- **E1（检索硬指标，后端 P0）**：在 KB eval 中新增 **hit-rate@k / MRR / nDCG@k / recall@k / precision@k**，基于评测集中标注的"正确 segment_id / 正确上下文"。计算在检索结果列表上，顺序敏感。
- **E2（评测集管理，前端 P0）**：KB 专用评测集 UI——golden Q&A + 标注正确分段；支持手工录入、CSV 导入、**从真实 trace 一键生成**（`examples:from-trace`）、LLM 合成问题。
- **E3（KB 检索评测工作台，前端 P0 核心）**：选评测集 + 选 1~2 个检索/分块预设 → 批量跑（`batch-score`/experiment run）→ **并排对比**检索硬指标 + RAGAS + 延迟/成本 → **门禁判定**（阈值可配）→ 晋升为新基线。复用 `ExperimentRunComparison.tsx`。
- **E4（门禁与回归）**：门禁阈值默认参考——nDCG@10≥0.8、Recall@10≥0.8、faithfulness≥0.8、context_precision≥0.7；接入 `gates:dry-run`，回归趋势图。
- **E5（LLM-judge 治理）**：固定 judge 模型/prompt 版本；关键门禁抽样人工复核；展示 judge 置信度。

### 3.5 平台化 / 可观测

- **P-1（配置即实验）**：每个 KB 的 `index_config` 版本化；改动配置自动生成 experiment 候选。
- **P-2（可观测看板）**：检索各 stage 分数（已有 pipeline_log）、rerank 缓存/熔断、RAGAS 滚动均值（`/eval/knowledge/summary`）上前端看板。
- **P-3（管控）**：按库的配置变更审计、权限、危险操作（删除需密码，已有）。

---

## 4. 前端补齐清单（对标 Dify / 百炼）

### 4.0 对标 Dify / 百炼 前端功能规格（源码/官方文档已验证）

**Dify 知识库（`langgenius/dify` 源码 + docs.dify.ai 已验证）**

| 要素 | Dify 规格（已验证） | 我们对照 |
|---|---|---|
| 检索默认 | **Top K=3，Score Threshold=0.5**（`score_threshold_enabled` 开关） | 我们 top_k=5、阈值 0.3；UI 需暴露 threshold 开关 |
| 权重预设 | `WeightedScoreEnum`：semantic_first=**1.0/0**、keyword_first=**0/1.0**、customized=**0.7/0.3**（`DEFAULT_WEIGHTED_SCORE`） | 采用同款三预设 + 自定义 |
| 分块模式 | **General / Parent-child / Q&A** 三 tab；**模式创建后锁定**，仅 delimiter + max length 可调 | 我们 9 策略更强；但需同样"创建后锁定模式"的明确提示 |
| 父块 | Full-Doc 父块**截断 10,000 tokens**；父块创建后不可编辑 | 我们 parent_token_limit 可配；UI 标注上限 |
| 关键依赖 | **TopK 与 Score Threshold"仅在 Rerank 阶段生效"**（docs 原文）→ UI 状态需联动（开 rerank 才启用这两项） | **前端必须复刻此联动**，否则用户困惑 |
| 召回测试 | 输入框 **200 字符上限**（后端 250）；"{{num}} Retrieved Chunks"；推荐"短陈述句" | 采用 |
| 经济模式 | 每 chunk 抽 **10 个关键词**、不消耗 token | 可作为低成本索引选项 |
| 分段编辑 | 关键词最长 20 字符 | 采用 |
| `RetrievalConfig` 类型 | `top_k, score_threshold_enabled, score_threshold, reranking_mode, weights{weight_type, vector_setting{vector_weight}, keyword_setting{keyword_weight}}` | 我们 `retrieval_config.py` 已覆盖且更全 |

**阿里云百炼知识库（help.aliyun.com 官方文档已验证）**

| 要素 | 百炼规格（已验证） | 我们对照 |
|---|---|---|
| 分块默认 | chunk_size=**500**，overlap=**50**（推荐 10-25%、须 < chunk_size）；分隔符默认 `,\|，\|。\|？\|！\|\n`；分段编辑 10-6000 字符；**切分策略创建后不可变** | 我们默认接近；UI 加 overlap≤chunk_size 校验 |
| 相似度阈值 | 默认 **0.2**，范围 **0.01-1.0**（过滤重排后分段） | 采用 0.2 默认 + 0.01-1.0 滑杆 |
| KB 权重(应用侧) | 默认 **1**，范围 **0.5-2**（同分时跨库决胜） | 多库场景可引入 |
| Embedding | text-embedding-v3/v4，**默认 1024 维**（v4 支持 2048/1536/…/64） | 我们 1024 对齐 ✔ |
| Rerank 模型 | **qwen3-rerank（hybrid=语义+BM25，推荐）**、qwen3-rerank（纯语义）、qwen3-vl-rerank（多模态）、"不使用模型"；模式 问答/相似/自定义 | 我们 4 provider 对齐；UI 加"hybrid/semantic/无"语义化标签 |
| KB 类型 | 4 类（文档搜索/数据查询/图片问答/音视频搜索），**创建后不可变**；文档搜索 4 场景（基础问答/图文并茂/视觉理解/极速问答），**相似度阈值仅基础+图文并茂支持** | 类型/场景化是产品亮点，P2 可借鉴 |
| 配额 | 知识检索服务 ≤15 库；初步 TopK 1-100 | 参考 |

**评测平台 UI 标杆（已验证）**

| 平台 | 可借鉴的 UI 模式（已验证） |
|---|---|
| **Arize Phoenix / AX** | **"Mark as baseline"**（每数据集一个持久基线）；对比视图展示**聚合指标 + 分数分布箱线图**；**Diff Output Mode**（相对基线高亮增删）——**唯一在产品 UI 展示 nDCG/MRR/hit-rate 等 IR 指标者** |
| **Langfuse** | 从 trace/observations **批量加入数据集**（字段映射）；**实验为一等公民**（2026-04 重构）；**版本化数据集上跑实验**（2026-02） |
| **CozeLoop（扣子）** | 评测集 CSV 导入：**≤5000 行、≤200MB、UTF-8、≤50 自定义列**；"提交新版本"版本化 |
| **RAGFlow** | 召回测试默认：相似度阈值 0.2、向量权重 0.3（hybrid = keyword×0.7 + vector×0.3） |
| **RAGAS（重要澄清）** | **RAGAS 从不内置 IR 排序指标（nDCG/MRR/MAP/hit-rate）**；其检索级指标始终是 LLM 判定的 context precision/recall。→ **我们的检索硬指标必须自研（E1/B1），不能指望 RAGAS** |

> 共同产品要素（参照系）：索引设置（分块方式/size/overlap/embedding/索引模式）、召回测试（query→命中分段+分数，可切模式/rerank/top_k/阈值）、分段管理（列表/编辑/启停/新增/命中次数）、检索配置（模式/rerank 下拉/top_k/阈值/权重）、评测（百炼内置、Dify 靠外部）。

### 我们相对 Dify/百炼 的**前端缺口与补齐项**（优先级排序）

| # | 缺口 | 补齐 | 优先级 |
|---|---|---|---|
| F1 | 无 KB 检索评测工作台 | 新增「评测」tab：评测集管理 + 预设 A/B + 指标对比 + 门禁 | **P0** |
| F2 | 检索测试 tab 默认与后端不一致、无预设 | 预设下拉 + 默认对齐 RRF+rerank + 参数 tooltip + 召回深度 | **P0** |
| F3 | 分块设置暴露不全、无实时预览面板 | 暴露全部 chunking 字段 + 答案类型档位 + 实时预览直方图 + Contextual 开关 | **P0** |
| F4 | 检索硬指标无展示 | 召回结果卡展示 rank/score；评测工作台展示 hit-rate/MRR/nDCG | **P0** |
| F5 | rerank top_n/模型推荐缺失 | rerank 模型按多语言/本地推荐 + top_n 规范化 | P1 |
| F6 | 评测集从 trace 生成、LLM 合成问题无入口 | 「从对话 trace 导入 golden」「LLM 生成测试问题」按钮 | P1 |
| F7 | 配置版本/审计/实验历史无 KB 视图 | 配置变更历史 + 实验运行历史 + 趋势图 | P1 |
| F8 | 可观测看板（stage 分数/缓存/熔断/RAGAS 趋势） | KB 详情页「洞察」子 tab | P2 |
| F9 | HyDE / 语义分块 / late chunking 开关 | 高级选项区（默认关，带权衡说明） | P2 |
| F10 | 分段命中次数、批量评测单条 | 分段卡展示 hit_count；评测集逐条诊断 | P2 |

---

## 5. 后端配套（最小必要改动）

| # | 改动 | 模块 | 优先级 |
|---|---|---|---|
| B1 | 检索硬指标计算（hit-rate/MRR/nDCG/recall@k/precision@k），输入=检索结果+标注正确 segment 集合 | `services/eval/` 新增 `retrieval_metrics.py`；`score-retrieval`/`batch-score` 扩展 | **P0** |
| B2 | 评测集"正确分段"标注字段（example.expected_output.relevant_segment_ids） | eval dataset schema | **P0** |
| B3 | 检索预设 API：`GET /knowledge/presets` 返回 fast/balanced/accurate/diverse/sota（含推荐文案） | `api/routes/knowledge.py` | **P0** |
| B4 | rerank top_n 规范化（默认 final top_k；召回深度 50-100） | `retrieval_service.py` | P1 |
| B5 | HyDE 查询扩展（可选，默认关） | `retrieval_service.py` | P2 |
| B6 | semantic chunker（embedding breakpoint）+ late chunking 选项 | `chunking.py`/`embedding.py` | P2 |
| B7 | 配置版本化（index_config 变更生成 experiment 候选 + 审计） | `persistence/database.py` | P1 |

---

## 6. 推荐默认参数总表（写入 UI 预设与 tooltip）

| 参数 | 推荐默认 | 范围/档位 | 依据 |
|---|---|---|---|
| 检索模式 | hybrid | vector/keyword/hybrid | hybrid > 单路（DL19 mAP +17）|
| 融合策略 | **RRF** | rrf / weighted | RRF 稳定、无需调权；k=60 |
| RRF k | 60（Elastic/Cormack'09 近优且不敏感）| ≥1 | ⚠️ **后端差异需归一**：Qdrant 默认 k=2 且零基 rank；UI 须标注/统一口径 |
| dense/bm25 权重(weighted 时) | 0.6 / 0.4（α≈0.3 稀疏） | 按语料 A/B | arXiv:2407.01219 |
| 语言自适应 | 开（ar→0.5/0.5） | — | 已实现 |
| vector_top_k / keyword_top_k | 各 20-50 | 召回深度 | over-retrieve 供 rerank |
| rerank 候选深度 | 50-100 | — | ~50 典型 |
| final top_k | 5（问答）/ 10（浏览） | 3-10 | 送 LLM 3-5 |
| rerank 模型 | bge-reranker-v2-m3（多语言/本地）；gte-rerank-v2（云） | — | 一梯队 |
| rerank top_n | = final top_k | — | 延迟控制 |
| score_threshold | 0.3（hybrid）/ 0.2（vector） | 0.2-0.35 | 预设 |
| MMR | 默认关；浏览/摘要类开 | lambda **0.5**（LangChain 默认，0=最多多样/1=最相关）、k=4；偏相关可 0.7 | LangChain API 已验证 |
| Multi-query 变体数 | 3 | — | LangChain MultiQueryRetriever 默认 3 |
| chunk token_limit | 400（通用）| 128 事实 / 400 通用 / 800 技术 | arXiv:2505.21700 |
| chunk overlap | 50（≈12%）| 10-20% | LangChain 默认 |
| 父子 child/parent | 400 / 1500 tokens | — | small-to-big |
| Contextual Retrieval | 开（模板优先，LLM 可选） | — | Anthropic |
| HyDE | 关（opt-in） | — | 延迟权衡 |
| 门禁阈值 | nDCG@10≥0.8, Recall@10≥0.8, faithfulness≥0.8, ctx_precision≥0.7 | 可调 | futureagi 实践 |

---

## 7. 分阶段路线图

**P0（本次核心，1 个迭代）**
- 后端 B1/B2/B3：检索硬指标 + 正确分段标注 + 预设 API。
- 前端 F1/F2/F3/F4：KB 检索评测工作台（评测集→预设 A/B→指标对比→门禁）、检索测试 tab 预设化与默认修复、分块设置全暴露+实时预览+Contextual 开关、检索硬指标展示。
- Playwright 回归覆盖上述新 UI。

**P1（紧随）**
- 后端 B4/B7：rerank top_n 规范化、配置版本化。
- 前端 F5/F6/F7：rerank 推荐、trace→golden/LLM 合成问题、配置/实验历史与趋势。

**P2（增强）**
- 后端 B5/B6：HyDE、语义分块、late chunking。
- 前端 F8/F9/F10：可观测看板、高级选项、分段命中诊断。

---

## 8. Playwright 真人式回归方案（Docker `localhost:8081`）

**环境**：前端 `http://localhost:8081`（容器 `ai-gateway-frontend`），网关 `:8080`，knowledge `:8092`，Qdrant/Redis/PG 已运行。复用 `e2e/global.setup.ts`（bootstrap 登录 → 注入 storageState）。

**运行**：`E2E_BASE_URL=http://localhost:8081 E2E_API_URL=http://localhost:8080 E2E_REUSE_SERVER=1 pnpm -C web exec playwright test e2e/knowledge-*.spec.ts`（`reuseExistingServer` 复用已启动容器，不重启 stack）。

**新增/扩展 spec（像真人一样点击走查）**：
- `knowledge-config.spec.ts`：进入知识库 → 设置 tab → 切换分块 mode/token_limit → 输入样本文本看实时预览 → 切 embedding 模型 → 保存 → 断言持久化。
- `knowledge-retrieval-test.spec.ts`：检索测试 tab → 选预设(sota) → 切 mode(hybrid/dense/bm25) → 调 top_k/权重滑块 → 开 rerank/mmr → 输入 query → 断言召回卡含分数/rank → RAGAS 评分按钮。
- `knowledge-eval-workbench.spec.ts`（核心）：建评测集 → 录入/导入 golden（含正确分段标注）→ 选两个预设跑 A/B → 断言对比表含 hit-rate/MRR/nDCG/faithfulness → 门禁判定 → 晋升基线。
- `knowledge-segment.spec.ts`：分段列表 → 编辑/启用禁用/搜索 → 命中展示。

**真人化要点**：用 `page.getByRole`/`getByLabel`/`getByText`（贴近用户语义，非脆弱选择器）；真实鼠标 `click`/`fill`/`selectOption`/拖拽滑块；等待网络 idle 与 toast；失败留 trace/screenshot/video（config 已配）。**并发纪律**：spec 顺序执行（`fullyParallel:false` 或单 worker），不 fan-out agent。

---

## 9. 风险与陷阱（来自调研）

1. **参数不可迁移**：α、chunk size、阈值均**语料相关**，默认值只是起点，必须用本库评测集 A/B 验证（Dynamic Alpha Tuning）。
2. **LLM-judge 不可全信**：prompt 敏感、仅 6/63 研究对齐人工 → 门禁固定 judge 版本 + 抽样人工复核。
3. **答案分会掩盖检索失败**：必须**分开评测检索与生成**（这正是 E1/F4 的动机）。
4. **HyDE/MMR 是权衡非银弹**：默认关，按需开。
5. **基准数字勿当 SOTA**：调研 MRR 等来自单一论文特定 harness，取其**相对结论**（hybrid+rerank+RRF 模式）而非绝对值。
6. **rerank 延迟**：top_n=None 全量重排会拖慢；规范到 50-100 候选 → top 5-10。
7. **配置一致性**：前端默认必须与后端预设对齐（R1），否则"所见非所得"。

---

## 附录 A：关键源码索引

- 分块：`apps/knowledge-service/src/knowledge_service/services/knowledge/chunking.py`
- Contextual：`.../contextual_retrieval.py`
- 检索管线：`.../retrieval_v2.py`（RRF/加权/MMR/stage 分数）
- 检索编排：`.../retrieval_service.py`（多查询/over-retrieve）
- 检索配置/预设：`.../retrieval_config.py`（`DEFAULT_CONFIGS`）
- Reranker：`.../text_reranker.py`、`multimodal_reranker.py`
- RAGAS eval：`.../services/eval/ragas_eval_service.py`、`api/routes/eval.py`
- 配置持久化：`.../persistence/database.py`（`datasets.index_config`）
- 前端知识库：`web/src/pages/knowledge/DatasetDetail.tsx`
- 前端 eval 平台：`web/src/pages/eval/index.tsx`、`components/ExperimentRunComparison.tsx`
- API 客户端：`web/src/api/knowledge.ts`、`web/src/api/eval.ts`
- Playwright：`web/playwright.config.ts`、`web/e2e/global.setup.ts`

## 附录 B：调研引用（已验证，primary 优先）

- arXiv:2407.01219 — Searching for Best Practices in RAG（hybrid/HyDE/rerank 消融，EMNLP'24）
- arXiv:2312.10997 — RAG 三阶段综述（Naive/Advanced/Modular）
- arXiv:2505.21700 — Rethinking Chunk Size（64-128 vs 512-1024 tokens）
- arXiv:2409.04701 — Late Chunking（Jina，ICLR'25，BeIR +1.5-1.9 nDCG）
- Elastic RRF docs — score=Σ1/(k+rank)，k 默认 60
- arXiv:2309.15217 + docs.ragas.io — RAGAS 三元组与组件指标
- arXiv:2504.20119 — RAG eval 系统综述（MRR/nDCG；LLM-judge 仅 6/63 对齐人工）
- ACL'25 Findings (2025.findings-acl.301) — EM/ROUGE 局限、judge prompt 敏感
- Anthropic Contextual Retrieval — 上下文头
- 实践：futureagi（检索目标值）、opcito/dev.to（over-retrieve+rerank）、dextralabs（CI/CD 门禁）、swept.ai（分开评测检索与生成）

**Dify / 百炼 / 评测平台 UI / 参数默认（并行调研工作流 `wf_62d9a7f9`，源码/官方文档对抗验证）：**
- Dify 源码（`langgenius/dify` `web/types/app.ts`、`web/models/datasets.ts`、i18n）+ docs.dify.ai：Top K=3/阈值0.5、权重预设 1/0·0/1·0.7/0.3、分块模式 General/Parent-child/Q&A 创建后锁定、Full-Doc 父块 10000 token 截断、TopK/阈值仅 Rerank 阶段生效
- 阿里云百炼 help.aliyun.com：chunk 500/overlap 50、阈值 0.2(0.01-1.0)、KB 权重 1(0.5-2)、embedding 默认 1024 维、qwen3-rerank hybrid 推荐、4 类 KB 创建后不可变
- 评测平台：Arize Phoenix/AX（Mark-as-baseline、分数分布箱线图、Diff Mode、IR 指标）、Langfuse（trace→数据集、实验一等公民、版本化数据集实验）、CozeLoop（评测集 5000 行/200MB/UTF-8）、RAGFlow（阈值0.2/向量权重0.3）
- 参数默认：Cormack'09 RRF k=60（Qdrant k=2 零基）、LlamaIndex chunk 1024/overlap 200、Weaviate alpha 0.75、LangChain MMR lambda 0.5/k=4、MultiQuery 3 变体
- 重要澄清：RAGAS 从不内置 nDCG/MRR/MAP/hit-rate → 检索硬指标须自研（E1/B1）
