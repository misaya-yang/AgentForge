# 平台平面化重构 产品需求文档

**文档状态：** Ready for PPR-00 only；后续实现由 PPR-02 的已批准 ADR 解锁
**规划版本：** V1
**基线日期：** 2026-08-26
**目标仓库：** `/Users/yang/projects/AI--Platfform`
**程序代号：** PPR（Platform Plane Restructure）
**分析依据：** [`docs/plans/rust-expansion-and-service-topology-2026-08.md`](../../../docs/plans/rust-expansion-and-service-topology-2026-08.md)

---

## 1. 产品定义

本程序先把平台职责归入 Edge、Control、Data、Index、Governance 五个**逻辑应用 plane**，再用负载证据决定是否改变物理部署。PostgreSQL、Redis、Qdrant 与对象存储是共享基础设施底座，不计作应用 plane。只有在平价预言机、资源收益、运维收益和回滚门同时成立时，才把 Python 实现替换为 Rust 或拆出新服务。

**全过程零公共契约变更**：OpenAPI、`agent-event/v2` 信封、Capability Contract V2、四端 SDK 行为不变。任何用户可见的变化都是缺陷。

---

## 2. 问题陈述

### 2.1 P1 — 性能治理无法收敛：本地与供应商指标混在一个数字里

四次同配置探针（`reports/performance/assistant-ttft-*.json`）：

| 探针 | 首个事件 p50 | 文本 TTFT p50 |
| --- | --- | --- |
| 2026-08-16 | 0.0162 s | 3.925 s |
| 2026-08-17 | 0.0166 s | 4.089 s |
| 2026-08-18 final | 0.0142 s | **9.281 s** |
| 2026-08-16 budget50 | 0.0155 s | 4.431 s |

本地开销四次全部落在 **14–19 ms**；TTFT 跨天摆动 **5.4 s**。**供应商单次抖动是本地全部开销的 360 倍。**

后果：发布门槛 `p50 ≤ 3.41 s` 打在一个本地不可控的量上，四次全部 `passed: False`；任何本地优化的收益都被供应商噪声淹没，无法验收，也无法回归。**这是过去几轮性能工作反复得不出结论的根因，必须先修。**

### 2.2 P2 — 网关一个容器混住四类 SLO

`gateway`（384 m、`--workers 1`）同时承载：

| 职责 | 代码位置 | SLO 类别 |
| --- | --- | --- |
| 公共边缘：鉴权 / 准入 / 限流 / 路由 | `src/core/gateway/`、`src/core/middleware/` | p99 毫秒级，永不阻塞 |
| 模型面：每 token 转发 | `src/services/agent_runtime/model_plane.py`（1,889 行） | 每 token 开销有界 |
| 控制面：线程生命周期 / 能力目录 | `src/services/agent_runtime/control_plane.py`（2,361 行） | p95 亚秒 |
| 治理与业务：Eval / 计费 / Studio | `src/services/eval/`（6,768 行）、`src/services/billing/` | 容忍秒级 |

四者共享一个事件循环、一个内存上限、一个部署单元与一个扩缩策略。任一类抖动传染其余三类。

**空载内存实测（2026-08-26 `docker stats`）：**

| 容器 | 空载 RSS | 上限 | 占用 |
| --- | --- | --- | --- |
| gateway | 129.5 MiB | 384 MiB | 33.7% |
| knowledge-service | 110.9 MiB | 512 MiB | 21.7% |
| knowledge-worker | 106.2 MiB | 512 MiB | 20.8% |
| agent-runtime (Rust) | 52.4 MiB | 192 MiB | 27.3% |
| capability-worker (Rust) | 60.4 MiB | **1 GiB** | 5.9% |

**这张表纠正了一个容易犯的过度主张：空载时没有任何服务处于内存压力下**，网关只用了上限的三分之一。因此本程序的内存论证**不是**「空载占用太高」，而是两条具体的：

1. **并发下的每流增长**才是约束——空载 RSS 说明不了 50 条并发流时会发生什么，而这正是 PPR-00 必须建立的负载画像。
2. **预算未标定**：capability-worker 分了 1 GiB 却只用 60 MiB（16× 过配），而网关要用同一台机器的内存跑四类职责。预算是按容器猜的，不是按 plane 的负载类别定的。

**推论：所有内存门禁必须打在 PPR-00 定义的负载画像上，不得用空载数字验收。**

### 2.3 P3 — Rust 扩面的边际成本过高

| 症状 | 证据 |
| --- | --- |
| 运行镜像 **2.36 GB** | `Dockerfile.runtime:39` 的 runtime 阶段是 `FROM rust:1.95.0-bookworm`，把编译工具链带进了运行镜像 |
| 镜像身份散落 **4 处** | `lock.json`、`.env`、`.env.example`、`docker-compose.yml`（两个服务）；2026-08-26 会话中逐一手改 |
| overlay 哈希是**整棵树**的 | 改 `ai-platform-capability-worker` 一个文件，`ai-platform-agent-runtime` 的镜像 tag 也变，触发无谓重建 |

在这个成本下每加一个 Rust 服务都乘以同一份摩擦。**扩面之前必须先把它压下来。**

### 2.4 P4 — 当前容量与隔离策略没有被负载证据验证

所有 Python 服务 `--workers 1`（compose `command` 实配），但“单 worker”本身不是缺陷：整容器复制、多 worker、独立进程池或拆服务都可能是正确答案。PPR-00 必须先证明实际容量或 noisy-neighbor 问题，PPR-02 再比较 T0 保留现状、T1 只拆 Governance、T2 拆 Edge+Governance 和条件性 T3 Rust model plane；不得从进程数直接跳到微服务结论。

### 2.5 已排除的问题（复核后不成立，不得进入实施范围）

2026-08-17 的 SPO 审查清单已过期。2026-08-26 逐条复核：

| SPO 结论 | 复核 | 证据 |
| --- | --- | --- |
| 准入三次非原子 Redis 往返 | **已修** | `admission.py:413` 单次 `eval_script(CAPACITY_ACQUIRE_PAIR_LUA)` |
| 双滑窗限流重复计数 | **已修** | `rate_limit_counted_dimensions` + `deps.py:124` `skip_dimensions` |
| `security_headers` 缓冲 SSE | **已修** | `_streaming/security_headers.py` 为纯 ASGI |
| JWT 解两次 | **已修** | `verified_jwt_claims` 复用 |
| 每次 retrieve 都 `get_collection` | **已修** | `vector_store.py:179` `_cached_get_collection` |
| 摄入与检索同进程 | **已修** | `KNOWLEDGE_RUNTIME_ROLE=api\|worker`，两个容器 |
| 日记重切块 / deepcopy / MCP 握手 / 串行工具 | **已失效** | Python AgentLoop 已被 FRC 删除 |

**实施者不得因为 SPO 文档提到这些就去"修"它们。** 若认为某条复活，先给出当前树的行号证据。

---

## 3. 目标与成功指标

### 3.1 产品目标

1. 性能可治理：本地与供应商指标分开度量、分开设门禁，本地回归能被单独检出。
2. 每个逻辑 plane 有明确所有权和 SLO；只有实际采用的独立部署单元才拥有独立扩缩与内存预算。
3. 扩大 Rust 面的边际成本降到「一条命令、15 分钟内」。
4. 同样硬件包络下并发承载能力提高，且治理面（Eval）不再挤压边缘。
5. 零公共契约变更。

### 3.2 上线门槛指标

| # | 指标 | V1 门槛 | 测量位置 | 阶段 |
| --- | --- | --- | --- | --- |
| M1 | 可加和 timing SLI | `local_pre_provider_seconds + provider_wait_seconds + local_projection_seconds ≈ ttft_seconds`；`local_overhead_seconds` 明确定义为两个 local 分量之和 | 探针脚本 | PPR-00 |
| M2 | 供应商方差被刻画 | 探索性成功 N≥30，报告中位数/IQR/置信区间与原始试次；任何 p95/release 声明成功 N≥100 或采用预审精度方案 | 探针报告 | PPR-00 |
| M3 | 本地开销 p95 | 用成功 N≥100 的确定性回放建立并在实现前锁定；25 ms 仅是待验证候选值 | 探针报告 | PPR-00 起持续 |
| M4 | Rust 运行镜像 | ≤ 150 MB（当前 2.36 GB） | `docker images` | PPR-01 |
| M5 | 单次 Rust 改动到容器可用 | ≤ 15 min，**单条命令** | 计时脚本 | PPR-01 |
| M6 | crate 级 overlay 标识 | 改 worker 不改变 runtime 的 tag | 构建两次对比 | PPR-01 |
| M7 | 拓扑决策与平面边界 | ADR 比较 T0–T3；越界 import、同步依赖和未映射模块测试失败 | `make plane-boundary-gate` | PPR-02 |
| M8 | Edge 采用门 | 只有 T0/T1 无法满足预先锁定的容量/隔离门才拆；若拆，附加 p99 留在 PPR-00 本地预算内 | 同负载对照 | PPR-03 |
| M9 | Governance 采用门 | 只有可复现 Governance 负载伤害 Edge/Data 且 bounded in-place 方案失败才拆 | 同负载对照 | PPR-05 |
| M10 | 模型面等价 | SSE fixture 回放**字节一致** | `make sdk-sse-contract` 扩展 | PPR-04 |
| M11 | 模型面资源 | warmed 增量 RSS/stream 降 ≥60%，同预算支持流数 ≥1.5×，且 CPU/本地 p99 不回归 | 1/10/25/50 流曲线 | PPR-04 |
| M12 | 摄入不影响检索 | 200 页 PDF 摄入期间检索 p99 抬升 ≤ 10% | 索引面压测 | PPR-06 |
| M13 | 会话存储决策 | 通过隔离副本 parity 后统一 owner，或以有界增长证据明确 defer 并保留 legacy path | schema + 查询审计 | PPR-07 |
| M14 | 供应商侧 TTFT | 随机交错 A/B 支持至少一个方案达到目标，或给出有置信区间、质量、失败率和成本依据的负向结论 | A/B 报告 | PPR-08 |
| M15 | 现有回归 | 真机不少于 141 passed、0 failed，skip 不得超过具名 allowlist；Python 全量 0 failed | 既有套件 | 每阶段 |
| M16 | 回滚 | 新拓扑上 current→frozen→current 演练通过 | `make agent-runtime-rollback-rehearsal` | PPR-09 |

**M14 允许以「门槛不可达」结案。** 如果所有可用 variant 都打不到 3.41 s，正确产出是把门槛改成有依据的数字，而不是继续在本地找 15 ms。

### 3.3 明确不是目标

- 不追求容器数量最小化或最大化。
- 不追求「全 Rust」。领域逻辑留 Python 是刻意选择。
- 不追求网关基准表上的漂亮数字——2026 的同类基准多打在 mock upstream 上。
- 不改检索质量栈（RRF 权重、rerank 策略、召回宽度），那归 `kb-rag-optimization-plan.md`。

---

## 4. 范围

### 4.1 In scope

| 阶段 | 交付物 |
| --- | --- |
| PPR-00 | 本地/供应商 SLI 分离；N≥30 方差刻画；**并发负载画像与各服务 RSS 曲线**；回滚包冻结 |
| PPR-01 | slim 运行镜像；单命令构建链；crate 级 overlay 标识 |
| PPR-02 | ADR-008 平面契约；可执行边界门禁 |
| PPR-03 | 条件性 Edge 部署单元；先证明 T0/T1 失败 |
| PPR-04 | Rust 模型面（影子 → 条件切流） |
| PPR-05 | 条件性 Governance 隔离；先证明 bounded in-place 方案不足 |
| PPR-06 | 独立 Index 容量轨：worker 安全扩展；CPU 内核（条件）；不改检索策略 |
| PPR-07 | 独立会话存储决策与可逆演练；真实数据需批准 |
| PPR-08 | 随机交错的供应商 A/B、prompt-cache SLI、thinking 预算 |
| PPR-09 | 对实际采用拓扑生成串行发布门禁并演练回滚 |

### 4.2 Out of scope

- 鉴权 / RBAC / 配额策略 / 计费算法 / Eval 打分逻辑 / Agent Studio 的**重写**（拆分位置不等于重写实现）
- BM25 / 稀疏检索重实现（已在 PG tsvector 与 Qdrant 原生）
- 嵌入 / 重排客户端（纯远程 HTTP，换语言无收益）
- gRPC、服务网格或临时未版本化 RPC；物理拆分若需要内部 handoff，必须先在 ADR 中冻结并认证、版本化和可回放
- `AgentSpec`、事件信封、能力契约的任何字段变更
- §2.5 已排除的六项

---

## 5. 角色与影响面

| 角色 | 本程序对其意味着什么 |
| --- | --- |
| 终端用户 | 无可见变化 |
| 租户管理员 | 只有 adopted phase 的实测容量收益才会提高并发上限 |
| 自托管部署者 | 运行镜像缩小；同内存并发改善只在资源门通过时承诺 |
| 平台开发者 | 领域逻辑仍在 Python；触碰数据面需要 Rust |
| 运维 | 容器数量由 ADR 和采用门决定；只为实际独立部署单元增加扩缩、告警与回滚责任 |

---

## 6. 贯穿纪律（每阶段适用）

1. **Strangler-fig：** Python 保留 → Rust/新服务影子运行 → 差分对拍 → 切流 → 删除。**不允许先删后补。**
2. **先有预言机才动手。** 无可回放的等价证明 ⇒ 该项不做。
3. **允许子项被取消。** 「测量后发现不值得做」是合格产出，必须写进 `reports/` 证据。
4. **不得沿用未复核的历史结论。** 引用 SPO 或任何 2026-08-17 之前的清单前，先在当前树取证。
5. **每阶段结束跑风险匹配的完整回归**：真机不少于 141 passed、0 failed，skip 不得超过具名 allowlist；Python 全量 0 failed。未运行的 live path 必须标为未验证。
6. **实现者不得自批。** 每个 risk-bearing phase 需要 `loop-state.json` 声明的独立 review；review 未 approved 或具名 waived，不得 done。

---

## 7. 风险登记

| ID | 风险 | 概率 | 影响 | 缓解 | 终止条件 |
| --- | --- | --- | --- | --- | --- |
| R1 | 拆分过程中平面边界漂移 | 中 | 高 | PPR-02 先把边界写成可执行门禁 | 门禁无法表达该边界 → 该拆分不做 |
| R2 | 模型面 Rust 化收益不足 | 中 | 中 | PPR-00 的本地 SLI 先到位；影子对比 | M10/M11 未达标 → 不切流，保留 Python |
| R3 | 切块边界漂移导致整库重嵌入 | 中 | 高 | 逐字节对拍作为门禁 | 对拍不过 → PPR-06 的内核项取消 |
| R4 | 服务边界新增跳数抵消收益 | 中 | 中 | 优先 PyO3 内核而非新服务 | 端到端 p95 变差 → 回退 |
| R5 | 供应商门槛根本不可达 | 高 | 中 | M14 允许以「门槛不可达 + 依据」结案 | — |
| R6 | Rust 面扩大后迭代变慢 | 中 | 高 | PPR-01 先降边际成本 | 单次改动 > 15 min → 停止扩面 |
| R7 | 双跑期运维复杂度翻倍 | 高 | 中 | 影子对比自动化；切流后立即删旧 | — |
| R8 | 实施者按过期清单返工 | **高** | 中 | §2.5 显式排除 + 要求行号取证 | — |
| R9 | 用空载数字验收内存门禁 | **高** | 中 | 所有内存门禁绑定 PPR-00 负载画像 | 报告未标注负载画像 → 该门禁不算通过 |
| R10 | 逻辑 plane 被误写成必须新增容器 | 高 | 高 | PPR-02 比较 T0–T3；PPR-03/05 允许 measured-not-adopted | 无负载证据或简单方案已满足 → 不拆 |
| R11 | 影子模式重复请求供应商 | 中 | 高 | 单请求 frame tee 或离线回放；provider request counter | 每 turn 超过一次 provider 请求 → 停止 |
| R12 | 单个实现者自写、自验、自批高风险边界 | 中 | 高 | 风险触发的独立架构/安全/数据库/产品 review | review 未批准 → 不解锁切流/迁移/发布 |

---

## 8. 决策记录需求

- **ADR-008** — 五个逻辑应用 plane、存储底座、T0–T3 备选、每个选中 handoff 的安全/失败语义（PPR-02，前置于任何拆分）
- **ADR-009** — 模型面独立服务（仅当 PPR-04 切流）
- **ADR-010** — 会话存储统一（PPR-07）

每阶段一份 `reports/` 证据，含：基线、门禁数字、实测结果、**以及被取消的子项与原因**。
