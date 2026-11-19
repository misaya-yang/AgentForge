# Rust 扩面与微服务生态拓扑计划

> **状态:** superseded — 分析与判据保留；后续实现由
> [`platform-architecture-convergence-prd-2026-08.md`](platform-architecture-convergence-prd-2026-08.md)
> 在 RAG 合入后接管。旧 PPR 后续阶段已 paused，不得继续执行。
> **日期:** 2026-08-26（§2.4 为当日逐条复核）
> **性质:** 架构判断。仓库数据全部可复核；外部数字标注来源，未本地复现。
> **前置:** FRC-06 关闭。
> **不要做:** 为「Rust 更快」而迁移聊天请求路径；重写领域逻辑（计费/权限/Studio/Eval）；重造 BM25；
> 沿用 SPO 2026-08-17 的遗留清单而不复核（其中六条已修、四条已随 Python AgentLoop 删除失效——见 §2.4）。

---

## 0. 一句话结论

**聊天热路径不该为延迟迁 Rust。** 四次同配置探针里，本地首个事件稳定在 **14–19 ms**，而文本 TTFT p50 在 **3.925 s 与 9.281 s 之间摆动**——供应商单次抖动是本地全部开销的 **360 倍**。在这个信噪比下，本地重写的收益连测都测不出来。

Rust 该赢在另外三条轴上：**并发下的资源包络**（Rust 内核实测 52 MiB / 上限 192 MiB，Python 网关 129 MiB / 384 MiB——注意这是**空载**，真正的问题是每流增长，必须在负载画像下验收）、**尾延迟隔离**（所有 Python 服务 `--workers 1`，容器内无水平扩展）、**供应链确定性**（锁 + SBOM + digest 机制目前只为 Rust 存在）。

**并发正确性不再是理由**——SPO-02/04 已经把准入、双限流、SSE 中间件、JWT 双解、collection 缓存全部修掉（§2.4 逐条复核）。

因此目标形态不是「全 Rust」，而是：**稳定机械的数据路径可以使用编译型实现，业务控制与治理保留 Python，CPU 内核优先通过 Python 调原生绑定获得收益**。这是一条语言与所有权原则，不预先承诺新增容器。

---

## 1. 本文与既有资产的关系

| 已有资产 | 角色 | 本文怎么用 |
| --- | --- | --- |
| `agent-runtime-full-rust-cutover`（FRC，FRC-06） | Agent 执行面已完成 Rust 化 | **Wave 0 = 关闭它**，不重开。本文从「执行面之后还有什么」起步。 |
| `sota-performance-optimization-2026-08.md`（SPO） | 热路径性能计划，含实测基线 | 探针数据直接引用。**其遗留清单已过期，必须按 §2.4 复核后的状态执行。** |
| `performance-correctness-hardening`（PCH-07） | 正确性硬门 | SPO-00…04 已落地；其 assistant 相关项随 Python AgentLoop 删除而失效。 |
| `docs/harness/platform-architecture.md` | 产品法（四层 + 准入表） | 本文的拓扑必须落在其分层里，不新增第五层。 |
| ADR-006 / ADR-007 | 单内核 + 模型面/能力面边界 | 迁移不得改变这些边界，只改边界**内**的实现语言。新服务需新 ADR。 |

---

## 2. 现状事实（可复核）

### 2.1 语言与规模

| 面 | 语言 | LOC | 归属 |
| --- | --- | --- | --- |
| Gateway | Python | 83 k (`src/`) | 公共 API、鉴权、限流、准入、配额、计费、模型面、控制面、Eval、Studio |
| 共享内核包 | Python | 56 k (`packages/ai-gateway-core/`) | 持久化、模型、安全、skills、tracing |
| 知识服务 | Python | 64 k (`apps/knowledge-service/`) | 摄入、切块、嵌入、检索、重排、Confluence 同步 |
| Agent Runtime overlay | Rust | 54 k (`rust/agent-runtime-overlay/kernel-rs/`) | Thread/Turn/Item 内核、事件投影、能力执行、Office 生成 |
| 控制台 | TS/TSX | 79 k (`web/src/`) | React 19 |
| SDK | Python / Node / Java / Dart | 3 k + | 四端 SSE 契约 |

Rust 已经拿下的：`ai-platform-agent-runtime`（10.8 k，内核 + V1 投影 + 审批 + PG store）、`ai-platform-capability-worker`（13.7 k，读/写能力、quiz、local node、image/confluence 写代理、python 沙箱拉起）、`ai-platform-office`（2.4 k，docx/pptx/xlsx/pdf 确定性生成）、`ai-platform-capability-contract`（1.7 k）。

### 2.2 实测基线（四次同配置探针，`reports/performance/assistant-ttft-*.json`）

| 探针日期 | 首个事件 p50 | 首个 thinking p50 | 文本 TTFT p50 | 试次 |
| --- | --- | --- | --- | --- |
| 2026-08-16 | 0.0162 s | 3.146 s | 3.925 s | 10 |
| 2026-08-17 | 0.0166 s | 3.246 s | 4.089 s | 10 |
| 2026-08-18（final） | **0.0142 s** | 3.724 s | **9.281 s** | 10 |
| 2026-08-16（budget50） | 0.0155 s | 3.257 s | 4.431 s | 10 |

发布门槛 p50 ≤ 3.41 s，四次全部 `passed: False`。

**这张表是本文最重要的证据，而且比单点数字有力得多：**

- **本地开销纹丝不动**——四次探针 `first_event` 全部落在 **14–19 ms** 区间。
- **同一套代码、同一配置，TTFT p50 在 3.925 s 与 9.281 s 之间摆动，跨度 5.4 s。**

也就是说：**供应商单次抖动（5,400 ms）是本地全部开销（15 ms）的 360 倍。** 在这个信噪比下讨论「把本地路径重写成 Rust 以降低 TTFT」没有意义——它连测都测不出来。

**推论（性能策略的分水岭）：** 本地 SLI 与供应商 SLI 必须**分开度量、分开设门禁**。把 `first_event`（本地权威，可优化、可回归）和 `ttft`（供应商主导，只能靠模型/variant/缓存策略）混在一个数字里，是过去几轮性能工作反复得不出结论的根因。

### 2.3 资源包络（`docker-compose.yml` 实配）

| 容器 | 语言 | `mem_limit` | 镜像体积 |
| --- | --- | --- | --- |
| agent-runtime | Rust | **192 m** | **2.36 GB** ⚠ |
| agent-capability-worker | Rust (+py 沙箱) | 1 g / 1 cpu | 535 MB |
| gateway | Python | 384 m | 1.15 GB |
| knowledge-service | Python | 512 m | 1.15 GB |
| knowledge-worker | Python | 512 m | 1.15 GB |
| frontend | nginx | 96 m | 84.9 MB |
| postgres / redis / qdrant | — | 320 m / 192 m / 352 m | — |

空载实测（同日 `docker stats`）：

| 容器 | 空载 RSS | 上限 | 占用 |
| --- | --- | --- | --- |
| gateway | 129.5 MiB | 384 MiB | 33.7% |
| knowledge-service | 110.9 MiB | 512 MiB | 21.7% |
| knowledge-worker | 106.2 MiB | 512 MiB | 20.8% |
| agent-runtime (Rust) | 52.4 MiB | 192 MiB | 27.3% |
| capability-worker (Rust) | 60.4 MiB | 1 GiB | 5.9% |

三个结论，其中第一条纠正了一个容易犯的过度主张：

1. **「内存已经吃紧」是错的——空载时没有任何服务处于压力下**，网关只用了上限的三分之一。内存论证只有两条站得住：**并发下的每流增长**（空载数字对此毫无预测力），以及**预算未按负载类别标定**（capability-worker 分 1 GiB 用 60 MiB，16× 过配；网关却要用一份预算跑四类职责）。**因此所有内存门禁必须打在并发负载画像上，不得用空载数字验收。**
2. **Rust 内核用 192 MB 的上限干完 Python 网关 384 MB 的活**，实测 52 MiB vs 129 MiB。这是包络优势，不是「Python 撑不住」。
3. **2.36 GB 的 Rust 镜像是打包缺陷，不是 Rust 的代价。** `Dockerfile.runtime:39` 的 runtime 阶段是 `FROM rust:1.95.0-bookworm`——把整套编译工具链带进了运行镜像。改成 slim/distroless 应落到 ~50–100 MB。**这条必须在扩大 Rust 面之前修掉**，否则每加一个 Rust 服务就多背 2 GB。

### 2.4 自检：SPO 遗留清单的当前真实状态

**本节是 2026-08-26 逐条复核的结果，不是转述。** SPO 审查写于 2026-08-17，此后 SPO-00…04 落地、FRC 删除了整个 Python AgentLoop。直接沿用那份清单会让实施者去修已经修好的东西。

| SPO 结论 | 2026-08-26 复核 | 证据 |
| --- | --- | --- |
| 准入三次非原子 Redis 往返，多副本可超卖 | **已修** | `admission.py:413` 走 `CAPACITY_ACQUIRE_PAIR_LUA` 单次 `eval_script`；注释明记替换了 `zremrangebyscore→zcard→zadd` |
| 中间件与路由两套滑窗限流重复计数 | **已修** | 中间件写 `request.state.rate_limit_counted_dimensions`，`deps.py:124` 按 `skip_dimensions` 跳过 |
| `security_headers` 是 HTTP 中间件，缓冲 SSE | **已修** | `src/core/middleware/_streaming/security_headers.py` 是纯 ASGI，只改 `http.response.start` |
| JWT 在中间件与 deps 各解一次 | **已修** | `_streaming/auth.py:146` 写 `verified_jwt_claims`，`deps.py:415` 复用 |
| 每次 retrieve 都 `get_collection` | **已修** | `vector_store.py:179` `_cached_get_collection`（SPO-04 / K1） |
| 摄入与检索同一 uvicorn、同一事件循环 | **已修** | `KNOWLEDGE_RUNTIME_ROLE` 支持 `all\|api\|worker`；实测 `knowledge-service=api`（`DurableEnqueueProxy`）、`knowledge-worker=worker` |
| 日记每轮重切块重嵌入 / 模型边界 deepcopy / MCP 每次握手 / 只读工具串行 | **已失效** | 这些全在 Python AgentLoop 内，`src/services/assistant/` 与 `streaming_tool_execution.py` 已被 FRC 删除 |
| CJK token 估算高估约 15% | **部分成立** | `src/core/utils.py:8` 启发式仍在，但 `dispatcher.py:136` 只在**供应商未回传 usage 时**作为兜底。影响面比 SPO 描述的窄 |
| 会话 `history` 无界 JSONB | **部分成立** | Runtime 路径已规范化到 `assistant_runtime_items`（实测 33,472 行）；`sessions.history JSONB` 仍服务 V1/网关兼容面 |
| 能力工具在 turn 内 3 跳（worker→knowledge→gateway 鉴权） | **修正为 2 跳** | worker 直连 `knowledge-service:8092`；`capability_plane.py` 本地验 HMAC proof，不回调网关 |

### 2.5 复核后仍然成立的问题

只有这些进入实施范围：

| # | 问题 | 证据 | 性质 |
| --- | --- | --- | --- |
| **A** | Rust 运行镜像 **2.36 GB**，runtime 阶段是 `FROM rust:1.95.0-bookworm` | `Dockerfile.runtime:39`；`docker images` | 打包缺陷 |
| **B** | 镜像身份散落 4 处：`lock.json` / `.env` / `.env.example` / `docker-compose.yml`（两个服务） | 本次会话逐一手改 | 流程缺陷 |
| **C** | overlay 哈希是整棵树的——改 capability-worker 会让 agent-runtime 的镜像 tag 一起变 | `manifest.json` 单一 `sha256` | 粒度缺陷 |
| **D** | 网关一个 384 m 容器里混住四类 SLO（边缘 / 模型面 / 控制面 / Eval 与计费） | `src/` 模块清单 + compose | 架构缺陷 |
| **E** | 所有 Python 服务 `--workers 1`，容器内无水平扩展 | compose `command` | 容量缺陷 |
| **F** | 模型面 1,889 行 Python 在每 token 路径上 | `model_plane.py` | 语言选择（非缺陷，是待评估项） |
| **G** | 能力 worker 不继承调用方角色，管理员授权可见的数据集读不到 | `capability_plane.py:_runtime_user` | 产品取舍 |
| **H** | `memory_profile` 无任何客户端下发，记忆写入被钉在 `basic`（仅 semantic） | 本次会话实测 | 产品缺口 |
| **I** | TTFT p50 9.281 s vs 门槛 3.41 s，且同配置跨天摆动 5.4 s | §2.2 | 供应商侧，本地不可解 |

## 3. 判据：什么该迁、什么不该迁

不按「哪块看起来慢」决定，按五维打分。**只有同时满足「(CPU 占比高 或 阻塞爆炸半径大) 且 契约稳定 且 有平价预言机」才迁。**

| 维度 | 问什么 | 高分含义 |
| --- | --- | --- |
| **CPU 占比** | 该组件自身墙钟里，本地 CPU 占多少？ | 高 → Rust 有物理收益 |
| **阻塞爆炸半径** | 它卡住事件循环时，会拖垮几条无关请求？ | 高 → GIL/单 worker 是真风险 |
| **契约稳定性** | 近 90 天该模块的产品语义改了几次？ | 高（少改）→ 可以冻结重写 |
| **领域耦合** | 逻辑是业务规则还是机械变换？ | 低耦合（机械变换）→ 适合迁 |
| **平价预言机** | 有没有可回放的 fixture 能证明字节级等价？ | 有 → 迁移可验收 |

### 3.1 打分结果

| 组件 | CPU | 爆炸半径 | 契约稳定 | 领域耦合 | 预言机 | 裁决 |
| --- | --- | --- | --- | --- | --- | --- |
| 模型面 `model_plane.py` (1.9 k) | 低 | **高** | **高**（ADR-007 冻结） | 低 | **有**（`sdk/fixtures/sse_inner_envelopes.json`） | **Wave 1 迁** |
| 准入 / 限流 `admission.py`+`multi_dimension_rate_limiter.py` (1.3 k) | 低 | **高** | 高 | 低 | 可造（并发 fuzz） | **Wave 1，先 Lua 后 Rust** |
| Tokenizer / token 估算（分散） | **高** | 中 | **高** | 低 | **有**（语料对拍） | **Wave 1，做成共享原生内核** |
| 切块 `chunking.py` (3.4 k) | **高** | **高** | 中 | 中 | **有**（切块边界对拍） | **Wave 2 迁内核** |
| 文档解析 / 图像抽取 (3.5 k) | **高** | **高** | 中 | 中 | 有 | **Wave 2 迁内核** |
| 检索 `retrieval_service.py` (3.6 k) | 中 | 中 | **低**（RRF 权重/profile 在调） | **高** | 有（fixture） | **只抽 fusion 内核，服务不迁** |
| 向量库封装 `vector_store.py` (3.4 k) | 低 | 低 | 中 | 中 | 有 | **不迁**（Qdrant 本身是 Rust） |
| 控制面 `control_plane.py` (2.4 k) | 低 | 低 | 中 | **高** | 部分 | **不迁** |
| 鉴权 / RBAC / 配额 / 计费 | 低 | 低 | 低 | **高** | 弱 | **不迁** |
| Eval / Studio / Admin | 低 | 低 | **低** | **高** | 弱 | **不迁** |
| BM25 / 稀疏检索 | — | — | — | — | — | **不迁**（已在 PG tsvector 与 Qdrant 原生） |
| 嵌入 / 重排客户端 | 低 | 低 | 高 | 低 | 有 | **不迁**（纯远程 HTTP，换语言无收益） |

### 3.2 为什么「不迁」的清单同样重要

`packages/ai-gateway-core/` 是所有 Python 服务的叶子依赖，`platform-architecture.md` §4 明令业务逻辑不得落在这里。把领域逻辑迁进 Rust 只会把同一个耦合问题换个语言重演一遍，还会把团队最需要快速迭代的部分锁进「改一行 → 重建镜像 → 更新 lock → 改三处 pin」的流程里（本次会话的实测成本）。

---

## 4. 业界 SOTA 对照（2025–2026）

### 4.1 最直接的对照：LiteLLM 自己迁了 Rust

LiteLLM——本仓 SPO 文档里被当作 Python 网关参照物的那个项目——在 2026 把数据面迁到了 Rust，并公布了基准：

| 网关 | p99 额外延迟 | 峰值内存 | 每请求开销 |
| --- | --- | --- | --- |
| LiteLLM Rust | **0.7 ms** | **21.8 MB** | ~0.05 ms |
| Portkey | 2.3 ms | 90.4 MB | — |
| Bifrost (Go) | 4.5 ms | 199.1 MB | — |
| LiteLLM Python v1 | **257.7 ms** | 329.5 MB | ~7.5 ms |

来源：[LiteLLM Rust 基准](https://docs.litellm.ai/blog/rust-ai-gateway-benchmarks)、[迁移说明](https://docs.litellm.ai/blog/litellm-rust-launch)。

**但必须配着读这条警告**：2026 的网关基准大多打在 mock upstream 上，测的是代理开销而不是用户体验。「一个写得好的网关加个位数到几十毫秒，而模型 TTFT 是几百毫秒到几秒」，主流供应商的秒级 p99 抖动本身就超过整个网关开销预算（[DeepInspect](https://www.deepinspect.ai/blog/ai-gateway-latency-benchmarks)）。

**这两条合起来正好是本文的论点**：Python 数据面 7.5 ms/请求的开销在 3.9 s TTFT 面前不值一提——**所以不要拿它当迁移理由**；真正值钱的是那一列 **内存**（329 MB → 21.8 MB，约 15×）和它背后的并发密度，而内存正是本仓的硬约束。

### 4.2 Agent 运行时

| 系统 | 内核语言 | 扩展语言 | 启示 |
| --- | --- | --- | --- |
| OpenAI Codex | Rust | — | **我们的内核就是它的 overlay**，方向已被验证 |
| Claude Code | TypeScript | — | 内核不必是 Rust；生态与迭代速度也是一等约束 |
| Zed / Cursor 核心 | Rust | 插件多语言 | 编译型核心 + 多语言扩展 |
| 本仓 | Rust 内核 + Python 沙箱 | Python/MCP | 已经落在同一形状上 |

结论：**内核编译型、工具与扩展随生态**。capability worker 的 runtime 阶段是 `python:3.11-slim` 正是这条原则的体现（`execute_python_code` 需要 Python 在场），不是缺陷。

### 4.3 检索与摄入

搜索核心几乎全是 Rust（Qdrant、Tantivy、Meilisearch、LanceDB），而摄入编排几乎全是 Python（Unstructured、Docling）——**快的部分靠原生绑定**。HuggingFace `tokenizers`（Rust + PyO3）比纯 Python 快约 20×，SQuAD2 子集上有 43× 的报告；批处理场景 10–12× 是常见量级（[Fast Tokenizers](https://medium.com/@mshojaei77/fast-tokenizers-how-rust-is-turbocharging-nlp-dd12a1d13fa9)、[splintr](https://github.com/ml-rust/splintr)）。

**这给出一个比「重写服务」便宜得多的选项：PyO3 原生内核。** 切块、tokenize、哈希、去重、RRF 融合都可以做成一个 Rust crate + Python 绑定，留在现有 Python 服务里调用——没有新服务、没有新网络跳数、没有新契约，却拿到绝大部分 CPU 收益。**Wave 2 默认走这条路，只有当进程隔离本身成为需求时才升级为独立服务。**

### 4.4 平台拓扑

2026 的共识是控制面/数据面分离（Portkey 的混合部署把 Gateway 数据面放进客户 VPC、控制面留在管理侧），且**治理必须在执行循环之外**——控制面不能既是被治理者又是治理者（[vdf.ai 2026 模式](https://vdf.ai/blog/enterprise-ai-agent-platform-architecture-patterns-2026/)、[Atlan](https://atlan.com/know/ai-agent/ai-platform-architecture/)）。

本仓目前的边界是**按语言和历史**切的，不是按负载类型切的——这是下一节要修的。

---

## 5. 迁移分波

**分波编号已被程序取代。** 实施拆解见
[`deploy/runbooks/platform-plane-restructure/`](../../deploy/runbooks/platform-plane-restructure/README.md)（PPR-00…PPR-09），
每阶段一份可执行文件。本节只保留分波背后的排序逻辑。

### 排序逻辑

| 顺序 | 为什么在这个位置 |
| --- | --- |
| **先度量分离** | 本地 SLI 与供应商 SLI 混在一起，导致过去几轮性能工作得不出结论（§2.2）。不分开，后面每一波都无法验收 |
| **再修工程化债** | 2.36 GB 镜像、4 处镜像 pin、整树 overlay 哈希（§2.5 A/B/C）。不修，之后每加一个 Rust 服务都乘以这个成本 |
| **再冻结平面契约** | 边界先写成 ADR + 可执行门禁，再动代码。否则拆分过程中边界会漂 |
| **再决定是否拆边缘/治理** | PPR-00 先证明容量或 noisy-neighbor 问题；PPR-02 比较保留现状、多副本/worker、只拆 Governance 与 Edge+Governance，不能从代码规模直接跳到新服务 |
| **再评估模型面** | §2.5 F 是语言选择而非缺陷。**必须先有 PPR-00 的本地 SLI，否则无法证明收益** |
| **索引面靠后** | 摄入/检索进程隔离**已经完成**（§2.4）。剩下的是 worker 水平扩展与 CPU 内核，收益要先测出来 |
| **供应商侧单独一波** | §2.5 I 是唯一能真正移动 TTFT 的杠杆，与本地重构互不阻塞，可并行 |

### 两条贯穿纪律

1. **允许子项在测量后被取消。** 「测了发现不值得做」是合格产出，必须写进证据。上一版本文把「准入原子化」列为 Wave 1 目标——复核发现 SPO-02 早已用单次 Lua EVAL 修好；**如果不复核就实施，那一整条是纯浪费**。
2. **每一波都是 strangler-fig：** Python 实现保留 → Rust 影子运行 → 差分对拍 → 切流 → 删除。不允许先删后补。

## 6. 目标微服务拓扑

### 6.1 问题：现在按语言切，不按负载切

`gateway` 一个容器里同时住着：公共 HTTP 边缘（要求 p99 毫秒级、永不阻塞）、模型面（每 token 转发）、控制面（线程生命周期、能力目录）、Eval/Studio/计费（重业务、可容忍延迟）。**它们的 SLO 类别完全不同，却共享一个进程、一个内存上限、一个部署单元。**

### 6.2 目标：先按逻辑 plane 分层，再按证据决定部署单元

```
┌─ Edge plane ──────────────────────────────── 无状态 · 水平扩 · 永不阻塞
│  鉴权 · 准入 · 限流 · 路由 · 配额判定
│  SLO: p99 额外延迟 < 10 ms
├─ Control plane (Python) ──────────────────── 业务多变 · 容忍延迟
│  Agent/Thread 生命周期 · 能力目录与指纹 · 策略 · Studio · 计费
│  SLO: p95 < 200 ms，不在 token 路径上
├─ Data plane ──────────────────────────────── 热 · 契约稳定 · 内存有界
│  Rust Agent kernel · 条件性模型面投影 · 能力执行
│  SLO: 每 token 附加开销 < 1 ms；每流 RSS 有界
├─ Index plane ─────────────────────────────── 批量与交互严格分离
│  ingest/chunk (CPU, 批) ⟂ retrieve (IO, 交互)
│  SLO: 摄入不得抬高检索 p99 超过 10%
└─ Governance plane ────────────────────────── 异步 · 不阻塞请求路径
   Eval · 审计 · trace 消费 · 质量门

Shared infrastructure substrate: PG · Redis · Qdrant · 对象存储
```

### 6.3 由此推出的硬规则

1. **数据面在 token 路径上不得同步调用控制或治理面。** 当前路径是 `capability-worker → knowledge-service` 两跳，知识侧本地验证 HMAC proof，不回调 gateway；这个性质必须保持。
2. **每个逻辑 plane 有独立 SLO 和所有权。** 只有实际采用的独立部署单元才有独立内存预算和扩缩策略。
3. **plane 之间只走版本化、认证、可回放的契约。** 优先复用现有信封；物理拆分若需要新的内部 handoff，必须由 ADR 冻结 schema、服务身份、重试/幂等、背压、错误映射和 SSE owner，禁止临时 header 或未版本化 RPC。
4. **治理工作不得在用户请求路径同步执行。** 是否需要独立进程由 PPR-00 interference 证据决定，不把逻辑独立偷换成必拆服务。
5. **新增服务必须写 ADR。** `architecture.md` §6 已有此规定；Wave 1/2 若产出新服务，ADR-008/009 是交付物的一部分。

### 6.4 ADR-008 要评估的候选部署单元

| 服务 | 语言 | plane | 说明 |
| --- | --- | --- | --- |
| `edge`（条件） | Python+Lua；语言另行决策 | Edge | 只有 T0/T1 无法满足采用门才拆 |
| `gateway-control`（条件命名） | Python | Control | 只有相应边界被物理拆出时才由现 gateway 演化而来 |
| `agent-runtime` | Rust | Data | 不变 |
| `capability-worker` | Rust (+py 沙箱) | Data | 不变 |
| `model-plane`（条件） | Rust | Data | 单 provider request 下对拍并通过资源门才新增 |
| `knowledge-retrieve` | Python (+原生内核) | Index | 只读，交互 |
| `knowledge-ingest` | Python (+原生内核) | Index | 批量，CPU |
| `governance`（条件） | Python | Governance | noisy-neighbor 证据成立且 bounded in-place 失败才拆 |
| PG / Redis / Qdrant | — | infrastructure substrate | 不变 |

容器数量不是目标。ADR-008 必须比较 T0 保留现状、T1 只拆 Governance、T2 拆 Edge+Governance，以及在其上条件性增加 Rust model plane 的 T3；若实测收益不足，对应服务不拆。

---

## 7. 风险与终止条件

| 风险 | 后果 | 缓解 | 终止条件 |
| --- | --- | --- | --- |
| 切块边界漂移 | 整库重嵌入，成本与停机 | 逐字节对拍作为门禁 | 对拍不过 → Wave 2.2 取消 |
| Rust 面扩大后迭代变慢 | 团队速度下降 | Wave 0 先降边际成本 | 单次改动端到端 > 15 min → 停止扩面 |
| 供应链锁链成本线性增长 | 每服务一套 lock/SBOM/digest | 按 crate 分标识 + 一条命令重建 | — |
| 新服务边界带来新跳数 | 抵消 CPU 收益 | 优先 PyO3 内核而非新服务 | 端到端 p95 变差 → 回退 |
| 平价预言机缺失 | 无法证明等价，只能靠人肉 | 每波必须先有 fixture 才能动手 | 无预言机 → 该项不迁 |
| 双跑期成本 | 运维复杂度翻倍 | strangler-fig + 影子对比，Python 实现保留至门禁通过 | — |

**统一规则：现 owner 保留，候选实现或部署形态先影子/对照运行，门禁通过才切 owner，切换稳定后再删除旧默认路径。** 不允许「先删后补」。

---

## 8. 度量与门禁

每波必须在动手**之前**写死门禁数字，并在动手前复跑基线。

| 波次 | 门禁 |
| --- | --- |
| Wave 0 | runtime 镜像 ≤ 150 MB；单次 Rust 改动到容器可用 ≤ 15 min（一条命令） |
| Wave 1 | 单 provider request 下 SSE 字节一致；warmed 增量 RSS/stream 降 ≥60%、同预算流数 ≥1.5×；多 worker 准入超卖 = 0；本地 timing 不劣化 |
| Wave 2 | 200 页 PDF 摄入期间检索 p99 抬升 ≤ 10%；切块边界逐字节一致；摄入吞吐 ≥ 基线 3× |
| Wave 3 | 检索 p95 不劣化；RRF 输出在 fixture 上一致 |

**禁止把「代码更干净」「更现代」当验收。** SPO 已经确立了这条纪律，本文继承。

---

## 9. 执行依赖

本节只保留原则，执行权威是 `deploy/runbooks/platform-plane-restructure/loop-state.json`：PPR-00 度量、PPR-01 工程化、PPR-02 ADR/plane gate 是基础链；PPR-03～08 在基础链后按真实依赖独立执行，PPR-09 只对实际采用项收口。阶段编号不构成依赖。

- PPR-03/04/05/06/07 的采用结论可以是 measured-not-adopted 或 deferred，但必须留下证据并保留原 owner。
- 每阶段需要风险触发的独立 review；实现者不能自批。
- 只有 `loop-state.json` 记录进度，本文不记录完成状态。

---

## 10. 一页纸结论

| 问题 | 答案 |
| --- | --- |
| 什么该迁 Rust？ | 已有 Agent kernel；条件性模型投影；经 profiling 证明 CPU 受限且能字节对拍的 tokenizer/切块/解析内核 |
| 什么不该迁？ | 鉴权/配额/计费/Eval/Studio/控制面/检索编排/BM25/嵌入客户端 |
| 主要理由是延迟吗？ | **不是。** 是并发资源包络、故障/尾延迟隔离和供应链确定性 |
| 优先形态是什么？ | **PyO3 原生内核 > 新 Rust 服务**。只有进程隔离本身是需求时才加服务 |
| 微服务怎么布？ | 先按 Edge/Control/Data/Index/Governance 建立逻辑所有权；Storage 是基础设施底座，物理服务仅按证据拆 |
| 第一件事做什么？ | **不是迁移**，是把 2.36 GB 的 Rust 镜像和四处镜像 pin 的改动成本修掉 |

---

## 参考

- [LiteLLM Rust AI Gateway 基准](https://docs.litellm.ai/blog/rust-ai-gateway-benchmarks) · [迁移说明](https://docs.litellm.ai/blog/litellm-rust-launch)
- [AI Gateway 延迟基准的读法（mock upstream 警告）](https://www.deepinspect.ai/blog/ai-gateway-latency-benchmarks)
- [Fast Tokenizers：Rust 如何加速 NLP](https://medium.com/@mshojaei77/fast-tokenizers-how-rust-is-turbocharging-nlp-dd12a1d13fa9) · [splintr](https://github.com/ml-rust/splintr)
- [企业 Agent 平台架构模式 2026](https://vdf.ai/blog/enterprise-ai-agent-platform-architecture-patterns-2026/) · [AI 平台控制面](https://atlan.com/know/ai-agent/ai-platform-architecture/)
- 仓内：[`sota-performance-optimization-2026-08.md`](sota-performance-optimization-2026-08.md)、[`../harness/platform-architecture.md`](../harness/platform-architecture.md)、[`../architecture/ADR-006-agent-runtime-single-kernel.md`](../architecture/ADR-006-agent-runtime-single-kernel.md)、[`../architecture/ADR-007-agent-runtime-data-boundaries.md`](../architecture/ADR-007-agent-runtime-data-boundaries.md)
