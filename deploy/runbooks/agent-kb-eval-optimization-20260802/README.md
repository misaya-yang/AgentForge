# 通用 Agent、KB/RAG 与 Eval 一体化瘦身计划

**日期：** 2026-08-02
**状态：** Assistant 单一 Turn Engine、Responses API 双层兼容与 Eval V2 离线合同已实现并完成本地验收；KB BM25 v2 shadow 与索引生命周期 fence 正在最终复审。真实语料消融、HNSW Pareto 调参、live Eval/nightly gate 和不可变镜像发布仍未完成。
**范围：** Assistant runtime、Knowledge Service、Agent Eval、RAG Eval
**非目标：** 不引入第四套 agent framework；不直接删除兼容入口；不无评测调参；不部署或破坏性重建现有索引。

## 1. 直接结论

当前系统的问题不是能力不足，而是同一职责存在多套执行中心。Assistant 已有 `TurnKernel`、持久化审批、command ledger、checkpoint、Context V2、Memory V2、工具权限和 trace；KB 已有 dense、稀疏检索、Qdrant native RRF、rerank、MMR 和 IR 指标。继续复制 Grok Build、OpenClaw 或 Hermes 会扩大状态空间和回归面。

目标架构收敛为：

```text
Chat API / SSE / AG-UI ─┐
POST /v1/responses ─────┴─> AssistantService._iter_turn_events
                           -> one AgentLoop
                           -> canonical TurnEvent stream
                           -> SSE passthrough | TurnEventCollector

ModelRegistry
  -> chat_completions (default)
  -> responses_v1 (opt-in; OpenAI/DashScope only)

AgentLoop
  -> TurnKernel + append-only TurnEvent
  -> ContextProjector / MemoryPort / RunBudget
  -> ToolCatalog -> Authorizer -> Executor -> Approval ledger
  -> optional bounded Orchestrator

Knowledge request
  -> immutable RetrievalProfile
  -> dense + versioned lexical/BM25 recall
  -> weighted RRF -> rerank -> optional MMR
  -> citations + complete run fingerprint

Both paths
  -> versioned dataset -> repeated trials -> outcome gate
  -> trajectory/IR/RAG diagnostics -> latency/cost/safety guardrails
```

原则是保留已验证的可靠性能力，把来源项目的优点变成 policy 或 adapter，而不是保留三套循环。

## 2. 已核实基线

### Assistant

- `agent_loop.py` 约 8,955 行，`execution_gateway.py` 约 5,240 行，`assistant_service.py` 约 2,579 行。
- `chat_stream()` 与 `chat()` 现在都消费 `_iter_turn_events()`；non-stream 由 `TurnEventCollector` 归约同一条 canonical event stream。
- `_chat_stream_legacy`、`_chat_legacy` 与 `_execute_with_planning` 已退出生产代码；执行控制路径从 2 条收敛为 1 条。
- AgentLoop 仍按请求创建；Runtime Adapter、ToolInvoker 等重依赖改为进程级 composition root 注入。
- Context、Memory、工具授权和文档生成均存在 legacy、新 runtime 或复制实现。
- live 默认开启 Context V2/Memory V2；Gateway、Subagent、Tool Policy V2、Scheduler、Failover 默认关闭。因此库存代码不能被当成 live 能力。
- 工具 schema 已有相关性选择和约 2,000 token 预算，后续应升级为可观测的 deferred catalog，而不是新增 ToolManager。

本轮实现的是控制面瘦身，不是总代码量下降。严格 RunBudget、event collector、Responses 协议和故障合同增加了代码；后续仍需按模块提取并删除复制实现，不能把“单一执行路径”误写成“整体 LOC 已完成瘦身”。

### KB/RAG

- Qdrant collection 已有 dense HNSW、稀疏向量、IDF modifier、payload index 和 native batch RRF。
- retrieval pipeline 已有 dense/keyword fallback、rerank、MMR、阶段 timing 和多 query。
- 当前名为 `bm25` 的自定义 sparse 实际是 hashed term presence 与 Qdrant IDF：tokenizer 会去重，且没有 BM25 文档长度归一与 TF saturation。它必须先作为 `lexical_v1` 对待，不能拿名字冒充算法证据。本地 PostgreSQL fallback 已恢复 document TF，但这不改变 Qdrant 字段的算法身份。
- `rrf_weights` 原先只进入配置，没有进入 native Qdrant RRF 或 Python RRF；本轮已开始修正。
- chunking 策略很多，但缺少由版本化 golden set 驱动的选择；应收敛为少量受评估 profile。
- dataset/collection 创建链存在 ID 复用覆盖和 collection 复用的 tenant isolation 风险，安全性优先于召回调参。

### Eval

- Agent Eval 已有 immutable run case、live candidate、重复试验、baseline promotion、fingerprint compatibility 和 paired bootstrap。
- 现有 checked-in `18/18` 报告是手工 observation 的离线合同回放，不是 live 模型、记忆、HITL 或多代理质量证明。
- RAG Eval 已有 Hit/Precision/Recall/MRR/nDCG/MAP 和 RAGAS-aligned judge，但缺版本化 golden dataset、服务端 gate、索引消融 manifest 和完整回答/citation 证据。
- retrieval-only trace 曾把 `"N retrieved documents"` 当成生成答案参与 faithfulness/answer relevance；本轮已改为 semantic review。

### 本地上游源码对比（只读）

本轮直接核对了三份本地 checkout，而不是只依赖旧笔记：

| 上游 | 本地路径 | 只吸收 | 明确不吸收 |
| --- | --- | --- | --- |
| Grok Build | `/Users/yang/projects/grok-build` | tool progress/terminal 配对、dangling tool repair、取消与并发回收、catalog search、`deny > ask > allow` shell policy、边界安全 compaction | Rust actor/session host、computer hub、JSON-RPC runtime 与多套 compaction engine |
| OpenClaw | `/Users/yang/projects/open claw/openclaw` | memory plugin boundary、source-of-truth 与派生索引分离、分层 tool policy、default-deny exec approval、tool-result repair | channel/plugin/qmd/subagent registry 整个平台和永久后台代理状态机 |
| Hermes Agent | `/Users/yang/projects/Hermes_agent` | capability toolsets、prompt-cache stable prefix、compression lock/anti-thrashing、completed-turn memory sync、并发 approval queue、显式 fan-out/depth/budget | ThreadPool 子代理循环、cron/Kanban/curator/provider 周边和无 durable resume 的路径 |

对应决策不是继续复制代码，而是把上述能力压成当前 `TurnKernel`、`ToolInvoker`、`ContextPacket`、`MemoryPort` 和 `RunBudget` 的合同测试。多代理也必须复用主 kernel；现有 `subagent_manager.py` 的第二套模型循环在 replay 对齐后退出。

## 3. Assistant retain / merge / retire 决策

| 决策 | 模块 | 理由与退出条件 |
| --- | --- | --- |
| Retain | `TurnKernel`、attempt/terminal contract | 唯一确定性状态机；任何外部路径都投影到它。 |
| Retain | DB-authoritative approval、checkpoint、command ledger | HITL、恢复和 exactly-once 的承重结构。 |
| Retain | `RegistryToolInvoker` + capability allowlist | 执行前最终权限边界；policy outage 必须 fail closed。 |
| Retain | `ContextAssemblerV2`、packet receipt、trace | 作为唯一模型上下文投影和评测证据。 |
| Merge | Runtime Adapter、ToolInvoker 生命周期 | 由进程 composition root 创建一次并注入所有请求。 |
| Merge | working/session/durable memory | 统一为 `MemoryPort`；长期记忆必须带 provenance、TTL、scope 和删除语义。 |
| Merge | context manager/compressor/RAG context | 一个 projector；摘要永远不是事实的唯一副本。 |
| Merge | Registry/tenant policy/middleware/gateway 决策 | 固定为 catalog -> authorize -> approval/dispatch -> canonical result event。 |
| Extract | AgentLoop model turn、tool step、compaction、checkpoint、trace sink | 纯提取，先不改行为；每步以 typed input/output 测试。 |
| Retired | non-stream 独立 pipeline | `chat()` 已消费同一事件流并由 `TurnEventCollector` 归约；旧 legacy pipeline 已删除。 |
| Retire | 无调用的 eager analyzers/managers | 构造计数与调用 telemetry 归零后删除或懒加载。 |
| Retire | Assistant 内复制的 docgen/skill bundle | MCP docgen 与 shared skill runtime 成为唯一实现且 fallback 测试通过后再删。 |
| Version | MCP 2025 session adapter | 最新 MCP 已变为 stateless；新增 2026 adapter，不能原地硬改旧服务器兼容路径。 |

### 单一 RunBudget

当前 `RunBudget v1` 是不可扩权预算对象，覆盖 model turns、tool calls、parallel tool calls、wall time、累计 tool-result bytes，以及 resume 时的严格恢复。恢复值一旦缺失、放宽或不一致就 fail closed。

input/output tokens、cost、child count、fan-out、tree budget、context/tool-schema budget 和 retry allowance 仍是后续目标。子代理预算最终必须从父预算扣减；多代理默认关闭，只对已由 eval 证明可独立分解、工具过载或开放式并行研究的任务开启。

### 安全并行工具

并行不按“多个 tool call”自动开启，只允许：

1. tool metadata 明确 `read_only` 或 `parallel_safe`；
2. 参数标准化后资源/path lock 不冲突；
3. 结果按模型原始 call order 回填；
4. cancel、timeout 和异常都有占位终态；
5. `side_effect_unknown` 会阻断后续冲突写操作。

### 严重无输出事故与修复

严重无输出由两层合同不兼容共同触发：DashScope/Qwen 的 OpenAI-compatible tool continuation frame 可能在同一 index 上携带空 `id`/空 `name`，旧 validator 把合法 continuation 当成损坏流；前端又假定每个临时 tool event 都已经有名称和成功状态。

后端现在只在该 index 已建立非空身份后忽略空 continuation identity，同时继续拒绝 orphan、identity rebinding 和跨 index 重复 ID；前端保留先前真实工具名、允许临时无名事件，并从显式 `status/success/error` 推导结果。复杂 ORION-27 多轮浏览器回归完成 todo 写读、48,600 预算恢复、两组 Recall 约束计算和 release gate 判断，修复后未再出现“无回复”。

### Responses 风格 API

公开兼容入口 `POST /v1/responses` 与 provider outbound `wire_protocol=responses_v1` 都复用现有 AgentLoop；没有新增第二套 agent/tool loop。默认 provider 协议仍是 Chat Completions。

| 层次 | 已支持 | 明确不支持 |
| --- | --- | --- |
| Provider outbound | OpenAI/DashScope；严格 sequence/terminal/usage；streaming function call；DashScope native web search | 其他 provider；non-stream function-call 返回；隐式改写默认协议 |
| Public ingress | JWT/API key；model permission/rate limit；文本与有序历史；instructions、temperature、max tokens；`store:false`；non-stream/SSE；stateless completed function-result replay；non-stream idempotency | `store:true`、`previous_response_id`、client tools、built-in tools、媒体输入、后台任务、持久会话、Memory/KB、任意未知字段 |

`store:false` 不是 zero-data-retention 承诺：平台仍可按租户策略保留运行/trace receipt。详细支持矩阵见上表；入口实现见 [`src/api/v1/responses.py`](../../../src/api/v1/responses.py)。

Assistant 的 KB tool contract 同时收口为当前可发布的 text-only 子集：查询最多 4,096 字、一次最多 8 个唯一 dataset、`top_k` 为 1–20、阈值为 0–1。JSON schema、公共 Gateway schema、已签名 Agent snapshot、自动 RAG 和 executor 都执行相同边界；非法值在任何检索调用前失败。尚未达到统一索引与删除合同的 `find_image` / `include_images` 不再向模型广告，也不能由旧签名 snapshot 绕开。

## 4. KB/RAG 目标架构

### 4.1 索引与 tenant isolation

- Dataset 创建必须 INSERT-only；现有 dataset ID 或 collection name 冲突直接拒绝，不能 upsert 改 owner。
- 每个 point 写入 `tenant_id`、`dataset_id`、`document_id`、`segment_id`、document/chunk revision。
- search、scroll、count、delete 与 native prefetch 由 VectorStore adapter 强制 tenant+dataset filter；业务调用方不能选择省略。
- 旧 collection 用审计 -> shadow backfill -> count/hash/sample compare -> alias/cutover；不原地猜测或破坏性回填。
- disable/re-enable document/segment 必须同时维护 DB、dense、lexical 与 cache 状态，并用状态机测试。

### 4.2 稀疏检索

保留当前字段作为兼容的 `lexical_v1`，新增 versioned shadow path：

```text
lexical_v1 = multilingual token hash + legacy weights + Qdrant IDF
bm25_v2    = verified BM25 tokenizer/model + explicit model/version fingerprint
```

`bm25_v2` 只有在以下条件全部满足后才能替代默认：

- TF saturation、document-length normalization、IDF 语义有单测或官方 model receipt；
- 中文、英文、阿拉伯文及 exact-ID/code slice 在 held-out 集上不回退；
- shadow collection 完成 backfill/readiness；
- latency、index size、ingest cost 有记录；
- 切换可由配置一键回滚。

Qdrant 官方支持 dense+sparse prefetch、RRF/DBSF 和 native BM25 `Document` inference，但具体收益必须由本库数据证明：[Hybrid queries](https://qdrant.tech/documentation/search/hybrid-queries/)、[Indexing](https://qdrant.tech/documentation/concepts/indexing/)、[BM25 inference](https://qdrant.tech/documentation/inference/inference-bm25/)。

2026-08-02 再核验后的官方边界更明确：native BM25 同时依赖 TF、IDF 与文档长度，且非英语语料必须显式选择 tokenizer/stemmer/stopword 配置；`Document` 写入和 query 端必须分别使用文档/查询语义。当前 `lexical_v1` 去重 term-presence 不满足这些条件，所以本轮只能把 `bm25_v2` 做成 fail-closed shadow，不能直接冒充 active BM25：[Qdrant full-text/BM25](https://qdrant.tech/documentation/search/text-search/full-text-search/)。

### 4.3 HNSW 与量化

不设置一个“行业最佳”常数。将下列参数写入 immutable index profile 与 eval fingerprint：

- `m`、`ef_construct`、query `hnsw_ef`、`full_scan_threshold`；
- payload index、`payload_m`、on-disk 与 indexed-only；
- scalar/product/binary quantization、oversampling、rescore；
- collection revision、Qdrant/client version。

同一 embedding/chunk corpus 上做 recall-latency-memory Pareto 扫描；HNSW、embedding 或 chunk 变化必须用 shadow collection，不能和 rerank/RRF 在同一实验 arm 同时改变。

payload index 必须在首批向量写入前创建，才能让 filter-aware edges 进入 HNSW；strict mode 用来拒绝未索引过滤，而不是把意外全扫留到线上才发现。Qdrant 1.16+ 的 ACORN 只作为多字段严格过滤或软删除很多时的独立实验 arm，收益要与额外搜索成本一起测，不能默认打开：[Qdrant indexing](https://qdrant.tech/documentation/manage-data/indexing/)。

### 4.4 Chunking

默认只暴露三条受评估路径：

1. `structure_v1`：标题/段落/页结构优先，约 400–500 token child 和受控 overlap；
2. `parent_child_v1`：child 召回、parent 上下文返回；
3. `contextual_v1`：只在 LLM 生成的 chunk context 被真实持久化、版本化和评测时启用。

其余策略保留实验入口但不进入默认 UI。Anthropic 的 contextual retrieval 实验显示 dense+BM25、context prepend 和 rerank 在其语料上互补，但该数字不能直接外推到本项目：[Contextual Retrieval](https://www.anthropic.com/engineering/contextual-retrieval)。Late chunking 作为独立 embedding 实验，而不是与其他变量一起打开：[Late Chunking](https://arxiv.org/abs/2409.04701)。

### 4.5 Fusion、rerank 与 MMR

- weighted RRF 的 dense/sparse weights 必须同时进入 Qdrant native 和 Python fallback，并出现在 trace fingerprint。
- 有标注 train/validation set 才调 weighted RRF；没有标注集时使用普通 RRF，只有在能信任并校准原始 score 分布时才考虑 DBSF。禁止直接对 cosine 与 BM25 原始分数做固定 alpha 线性相加。
- reranker adapter 按 provider/model 绑定请求 schema；Qwen3 与旧 GTE schema 不能只替换模型名。
- rerank 默认作用于融合候选，MMR 在 rerank 后可选执行；两个阶段分别记录 candidate count、p50/p95、fallback 和 error。
- ColBERT/multivector 只作为二阶段 shadow arm；没有 nDCG/latency 证据不加入默认主链。

### 4.6 审计确认的下一批正确性债务

以下问题已有代码证据，但不在本轮 P0 补丁中伪装成“已经解决”：

1. document disable 只改 DB，segment disable 只删 vector，PostgreSQL FTS 又未统一过滤 enabled/archive；必须收敛为 DB+dense+lexical+cache 的索引状态机，并覆盖 re-enable reindex。
2. ingestion 允许子批失败后继续，且 Qdrant/DB/旧向量删除不是一个 revision 提交；应以 staging revision + manifest 完成双存储后再原子切 active revision。
3. MMR threshold 后的 fill-remaining 会重新塞回被多样性阈值淘汰的候选；需要显式 `strict_diversity` 与 `fill_policy` 合同。
4. PostgreSQL `simple` FTS 与 Python/Qdrant multilingual tokenizer 语义不同，零结果也不回退；需按中文、英文、阿拉伯文和 exact-ID slice 做 tokenizer/FTS 消融。
5. HNSW 目前是固定常数，缺 query `hnsw_ef`、exact、quantization、on-disk 和 schema signature；只能经 shadow profile 的 recall@k / p95 / 内存 sweep 后切 alias。
6. 动态 version route 与静态 compare route 顺序、restore 的 MAX+1 事务、raw/calibrated/display score 边界需分别修复，不能与检索调参混在同一变更里。

### 4.7 Confluence 激活边界

当前 `knowledge-service` 的 composition root 没有初始化或挂载 Confluence sync service/scheduler/router，gateway 也把对应 state 置空并 fail-closed 返回 503。因此，本轮对 generation、page receipt 和附件 manifest 的修改只是 dormant-path hardening，不能称为已上线或已由浏览器验证的能力。

启用前还必须完成独立安全门禁：Confluence base URL 只允许规范化后的受信 HTTPS origin；DNS 解析结果拒绝 loopback、private、link-local、metadata 等地址；attachment `downloadLink` 及每一次 redirect 都必须保持同源；Basic/Bearer 凭据不得发送到第二 origin。随后再做 scheduler crash/restart convergence、分页/限额/oversize attachment、token 泄露和真实 Confluence sandbox E2E。未满足这些条件前保持 503 比“先接活再观察”更安全。

## 5. 统一 Eval 合同

### Agent Eval

每条 case 包含 task、initial state、allowed tools、budget、expected outcome、安全不变量和 slice。每个 trial 保存：

- final environment state；
- 完整 TurnEvent/parent-child lineage；
- model/provider/prompt/tool/policy/context/memory/MCP fingerprint；
- approval/cancel/retry/compaction 事件；
- tokens、cost、p50/p95 latency、tool calls、fan-out。

Gate 分四层：

1. Outcome：最终状态、任务成功、安全规则；
2. Reliability：`pass@1` 与 `pass^k`，至少重复 3 次的能力集；
3. Trajectory：重复工具、循环、非法审批、handoff、预算，仅作诊断和硬安全检查；
4. Efficiency：context/tool schema tokens、cost、latency、子代理开销。

deterministic grader 优先；LLM judge 必须版本化、盲化身份、交换候选顺序、允许 `Unknown` 并以人工标注校准。Anthropic 的 agent eval 指南同样区分 task/trial/grader/transcript，并建议能力集与回归集分开：[Demystifying evals](https://www.anthropic.com/engineering/demystifying-evals-for-ai-agents)。涉及真实环境状态与双向用户交互时，参考 [tau-bench](https://arxiv.org/abs/2406.12045) 的 stateful success / pass^k 和 [tau2-bench](https://arxiv.org/abs/2506.07982) 的 dual-control 任务设计，但不直接照搬其阈值。

### RAG Eval

`rag-eval-v1` manifest 至少包含：

- query、reference answer、unanswerable；
- source document hash、稳定原文 span、relevant document/chunk graded labels；
- language/domain/length/exact-ID 等 slice；
- KB revision、chunk/index/embedding/lexical/fusion/rerank/MMR/judge fingerprints。

离线 IR：Recall@k、Precision@k、MRR、nDCG、MAP。生成链：context precision/recall、citation coverage/correctness、faithfulness、answer relevance 与 abstention。judge 指标与人工标签先做一致性校准，review/unknown 不能混入有效均值。

RAGAS、ARES、RAGChecker 作为指标和诊断参考，不替代本项目 golden provenance：[RAGAS](https://aclanthology.org/2024.eacl-demo.16/)、[ARES](https://arxiv.org/abs/2311.09476)、[RAGChecker](https://arxiv.org/abs/2408.08067)。

### 执行频率

- PR：20–50 条 deterministic/offline smoke，固定 seed，禁止网络依赖；
- nightly：200+ held-out、多语言、真实 Qdrant、重复 trials；
- provider candidate：显式运行并保存成本/region/model receipt；
- 发布：只比较兼容 fingerprint；安全或关键 slice 回退立即阻断。

基线不足时不写拍脑袋阈值：第一轮只记录分布并冻结 baseline；第二轮才设置服务端 hard floor 与相对 non-regression gate。

## 6. 分期执行与退出门槛

### S0：正确性与安全止血

- [x] Redis password 不再重复出现在 Healthcheck/Test/helper argv/Config.Cmd。
- [x] DashScope region endpoint 跨 gateway/assistant/knowledge/docgen 一致透传。
- [x] retrieval-only trace 不参与生成答案 faithfulness/relevance 伪评分。
- [x] 重复检索 ID 不再压缩真实 rank。
- [x] weighted RRF 同时进入 native 与 fallback。
- [x] Agent Eval candidate/statistics/evaluator/subagent 核心测试进入 CI。
- [x] dataset/collection 冲突改为 fail closed，并完成 tenant regression；`082_kb_dataset_collection_identity.sql` 已于 2026-08-02 14:02:47 UTC 在本地应用，pending migration 为 none。
- [x] embedding cache key 纳入 provider/model/dimension/text type/endpoint/account 的非秘密 profile fingerprint。

退出门槛：目标测试、Ruff、compose render、secret scan、dirty-tree 审计全部通过；不把历史数据称为已修复。

### S1：Assistant 单一 composition root

- [x] Runtime Adapter 与 ToolInvoker 每进程各一个，由 composition root 注入。
- [x] builtin memory、AgentLoop、Gateway 共用 runtime/invoker；治理删除 adapter 因更严格的 Qdrant receipt 保持显式独立。
- [x] 删除或按需构造已识别的请求级重复重依赖；AgentLoop 本身保持 request scope。
- [x] 增加实例 identity/构造计数与跨 tenant/user/scope cache 隔离回归；并发 shutdown/cleanup 故障注入留在 S2。

退出门槛：公共 API/SSE 无变化；连续请求构造计数为 1；Assistant 目标 gate 全绿。

### S2：单一 Turn Engine 与模块提取

- [x] non-stream 改为事件流 collector；与 streaming 共用 `_iter_turn_events()` 和一个 AgentLoop。
- [x] 引入 `RunBudget v1`，覆盖 model/tool/parallel/wall-time/tool-result bytes，并在 resume 时严格恢复。
- [x] 增加 attempt/terminal、cancel/resume identity、tool lifecycle 和 collector fail-closed 合同。
- [ ] 从 AgentLoop 纯提取 model turn、tool step、context projection、compaction、checkpoint、trace sink。
- [ ] 将 token/cost/child/tree/context schema 预算纳入同一 budget，并继续降低核心单体体积。

退出门槛：一个 turn 恰好一个 terminal；approval/resume/cancel/retry/unknown-side-effect 故障注入通过；核心单体行数持续下降且无新控制面。

### K1：tenant-safe shadow retrieval

- [ ] 所有 point 与 query 强制 tenant+dataset identity。
- [ ] 历史 collection 审计与 shadow backfill 工具，仅生成 plan/receipt，不默认切换。
- [ ] `lexical_v1` 诚实命名，`bm25_v2` 版本化 shadow。
- [x] Qwen3 reranker legacy/flat schema adapter、Singapore endpoint、cache/circuit profile 与真实 provider smoke。

退出门槛：跨租户读写/delete 测试为零泄漏；backfill count/hash/sample receipt 完整；旧索引可回滚。

### K2：检索消融与默认 profile

- [ ] 逐变量实验 dense -> lexical/BM25 -> RRF -> rerank -> MMR。
- [ ] chunk/embedding/HNSW 各自独立 shadow arm。
- [ ] 按语言、exact ID、长文、表格、无答案 slice 选 Pareto profile。

退出门槛：held-out IR 与 latency/cost 报告可复现；默认值由结果而非论文数字决定。

### E1：stateful Agent/RAG release gate

- [x] `eval-gate-metrics/v2` 与 stateful plan/tool pairing/budget/HITL/compaction/security 合同。
- [x] 关键 case hard block、single-runtime fingerprint、complete-trial receipt 与 baseline promotion CAS。
- [x] 版本化 RAG ranked-list golden、retrieval/judge artifact-policy binding、failure attribution 与 server-side gate。
- [x] trace/message/citation/chunk 级反馈归因合同。
- [ ] live provider、重复 trial 与 nightly 报告进入 immutable artifact，不覆盖手工 dirty report。

当前证据仍是 recorded offline fixture：Agent 25/25，其中 critical 20、stateful 7；每 case 仅 1 次，trace receipt 未记录，local-live/real-provider 均未运行。RAG 为 12/12 离线 ranked-list fixture，retrieval gate 通过，但 answer quality 为 `not_run`，provider execution 明确未执行。这证明合同和回归门禁，不证明 live Agent/RAG 质量提升。

退出门槛：发布 job 依赖 compatible candidate gate；离线报告不再被描述为 live quality。

## 7. 当前轮验证与运行策略

- 所有本地测试输出写临时目录，不能覆盖 `reports/eval-regression/latest.json` 的用户改动。
- 先跑目标单测/Ruff，再跑 Assistant/KB/Eval 聚合门禁。
- Docker 更新前重新检查 `com.docker.compose.project.working_dir`。
- Python source 变更优先 hot copy + restart；Compose/env 变化需要受控 recreate，不能用 `up --build`。
- provider smoke 只发送最小请求，日志只记录 region/model/status/latency，不记录 key、authorization header 或完整敏感 payload。
- 不 commit、不 push、不 deploy；不自动切换或删除历史 Qdrant collection。

### 本轮实际 receipt

- Responses：当前树 public gateway、internal ingress、provider adapter 与 model boundary 聚焦回归 190/190；Singapore provider canary 覆盖文本、streaming function call 和 native web search completed；公开 ingress non-stream 返回精确 `PUBLIC_RESPONSES_OK`，SSE sequence 连续且仅一个 terminal，function-call/output replay 成功；unauthenticated、`store:true`、未知字段和 client tools 均按 401/400 fail closed。
- Assistant：当前树全量 1,934 passed / 1 skipped；唯一 skip 是 opt-in PostgreSQL memory integration，随后用本地真实 PostgreSQL DSN 单独运行 1/1 passed。process-scoped runtime/invoker、外部 gateway identity、跨 tenant/user/scope cache 隔离、single-loop/collector/RunBudget 与 bounded text-only KB tool 均包含在该全量回归中。
- Eval：`make verify-eval-dev` 通过 46 + 129 + 200 + 35 + 17 条测试；离线 Agent 25/25、critical 20、stateful 7；RAG 12/12，answer-quality `not_run`。前端 lint 0 error/17 个既有 warning，typecheck 通过。
- KB：等待当前最终树完成 lifecycle/identity fence、完整 KB suite、独立 `0 blocker / 0 high` 终审与真实浏览器生命周期后填写；旧的 188/188 已删除，不能复用早期数字。
- Provider：Singapore `text-embedding-v4` 返回 1024 维，`qwen3-rerank` legacy schema 返回排序结果，`qwen3.7-plus` Chat Completions 与 Responses 文本均成功；容器内 embedding/rerank 再次通过。
- Migration：`082_kb_dataset_collection_identity.sql` 已在本地应用，applied at 2026-08-02 14:02:47 UTC，pending none。
- Browser：ORION-27 复杂多轮任务完成 todo 写读、预算/约束恢复与 release gate，修复后无 silent output；Eval Overview/Gates/Assets/Trace 已实机加载。
- Runtime：8/8 容器健康且 compose working directory 匹配本 checkout；Redis 稳态 UID 999、配置 0600、密码值不在 Config.Cmd/healthcheck/argv；前端 production build 7,385 modules 通过。
- 安全：`.env` 为 0600 且被 Git ignore；tracked diff 不含 provider key；用户预存的 eval report 与 Playwright 文件哈希保持不变。

独立审查在首版补丁中实际发现并推动关闭了这些高风险问题：共享 ToolInvoker 的跨 tenant/user cache key、Qdrant collection 并发 claim/False 返回、Redis shell 注入/root/旧密码兼容、weighted `alpha` 被默认 RRF 权重覆盖、Qdrant native 与 Python fallback 的 weighted-RRF 公式不一致，以及 embedding endpoint/dimension 的全局状态与缓存别名。终审不是形式签字，而是用可改变排名、跨身份复用和恶意配置字符串的反例重新验证。

后续终审还关闭了模型工具和 HTTP 资源放大面：KB tool/LangGraph dataset fan-out 设总量上限；Gateway 请求体在不信任 `Content-Length` 的前提下增量计数，JSON 默认 4 MiB、单文件 16 MiB、批量 32 MiB、配置硬上限 48 MiB，并用 2 并发/128 MiB 在途预算 fail-fast；非幂等请求没有非空 Idempotency-Key 时不再自动重试；非 retrieve 流式响应不再被 trace 全量物化，retrieve trace 只捕获有界且完整的结果。provider-paid QA/eval 当前限定 admin，RAGAS 默认关闭，直到调用者身份和计费预算可被端到端签名与计量。

`082` 唯一索引迁移已在本地数据库应用。真实 KB corpus、BM25 v2 active serving、HNSW `m/ef_construct/hnsw_ef` Pareto、chunk profile、MMR/rerank 消融及生成式 faithfulness/citation 质量均未完成验证；离线 fixture 的满分不能外推为真实语料收益。最终 KB 浏览器 E2E 清理后还需重新核对 documents/segments/Qdrant collection 基线。

当前验收使用现有容器上的 source hot-copy/restart。backend、frontend、knowledge 仍以已发布 `2.0.0` 镜像为基底，assistant 使用本地 review 镜像；这证明当前本地进程加载了修改，不是不可变镜像构建、发布或可重现部署。容器 recreate 可能丢失 overlay；不能把本次 live 状态误当成已发布 release。

## 8. 研究依据边界

核心设计采用“小而稳定的 agent loop、最小高信号上下文、deferred tools、阶段 compaction、outcome-first eval”的共同方向：[OpenAI agent loop](https://developers.openai.com/api/docs/guides/agents/running-agents)、[Anthropic context engineering](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents)、[OpenAI harness engineering](https://openai.com/index/harness-engineering/)。

这里的 harness engineering 不是再造一个 framework，而是让仓库知识、架构边界、测试、日志、浏览器和质量门禁都对 agent 可读且可机械执行，并持续清理熵；这也是本轮选择“单一 AgentLoop + versioned contracts + 独立审查”，而不是继续叠加 Grok/OpenClaw/Hermes 控制面的依据。

MCP 最新规范已经改为 stateless 并移除旧 initialize/session/Last-Event-ID 语义，因此只能增加 versioned adapter，不能未经兼容测试硬迁移：[MCP 2026-07-28 changelog](https://modelcontextprotocol.io/specification/2026-07-28/changelog)。

这些资料用于提出候选设计，不是本项目性能结论。只有本仓库的测试、trace、held-out eval、运行 receipt 与人工校准能把候选提升为默认或发布门槛。
