# 平台架构收敛与运行边界优化 PRD

**状态：** active closeout — 核心候选 `9aaa9cab` 已通过当前栈实机；托管 Rust CI、多架构、
fresh-machine 与冻结镜像回切证据仍 blocked/not-run，见程序 `loop-state.json`
**版本：** 1.2（2026-08-30 事实状态更新；完成定义不降低）
**日期：** 2026-08-30
**产品目标：** 在不改变现有用户能力的前提下，把 Gateway、Rust Agent 执行面、Knowledge
的数据与部署边界做实，并降低 coding agent 后续修改、评审、测试和发布的认知成本。
**执行者：** 一个主 coding session、一个 writer worktree，顺序完成全部工作包；并行 Agent 只读探索或
独立 review，不并行写这一大功能。
**文档定位：** 这是产品与工程合同；实际完成状态和运行证据以
`deploy/runbooks/platform-architecture-convergence/` 为准。

**实施启动输入：** Ultra 主 Session 启动前必须在 program baseline 中冻结：

```yaml
builder_mode: hosted_ci | authorized_staging_registry | proven_local_buildx
builder_ref: <workflow or registry/buildx profile, no credential>
push_authorized: true | false
```

三种 builder 都不可用时，制品/远端发布状态为 `BLOCKED_EXTERNAL_RELEASE_INFRA`；Session 可完成本地代码
与合同，但不能承诺一次完成 `RELEASE_CANDIDATE_PASS`。不要等到 ARC-08 才发现没有另一架构制品来源。

---

## 1. 执行摘要

本轮不做“全面 Rust 化”，也不继续按 Edge、Control、Data、Index、Governance 的抽象名称
增加容器。目标架构收敛为三个有明确数据所有权的业务边界：

1. **Gateway Control（Python）**：公共 API、鉴权、租户、模型路由、配额、计费和管理面。
2. **Agent Execution（Rust）**：Agent Runtime 与 Capability Worker；一个版本单元、两个隔离进程。
3. **Knowledge（Python）**：Knowledge API 与 Knowledge Worker；一个代码单元、两种运行角色。

Frontend 是产品 Surface，PostgreSQL、Redis、Qdrant 和对象存储是基础设施。逻辑模块不自动
成为新容器；本 PRD 不新增常驻服务。

本轮要解决的不是“Python 还在不在”，而是以下已确认问题：

- Assistant 的 Python Agent loop 已删除，但 Gateway 的 Assistant API、Runtime control plane、
  model plane 仍聚集在多个 1,800～2,300 行文件中，职责难以审查。
- Gateway 的 control plane 会通过 HTTP 调用同一 Gateway 进程的 capability catalog 路由，
  形成无必要的 self-HTTP。
- 服务共享同一个 PostgreSQL 用户和宽 `search_path`；schema 已分名，但数据权限尚未分权。
- `ai-gateway-core` 同时包含协议、基础设施和 quiz/skills/image/memory/knowledge 等领域实现，
  已成为跨服务依赖磁铁。
- Capability Worker、Runtime 与 Gateway 的健康状态不能准确表达“基础聊天可用、部分能力降级”。
- 固定 `container_name` 让 Compose 无法直接扩多个 worker；用户也无法从产品界面理解各服务职责。
- 多份历史 runbook/计划描述相近目标，coding agent 容易从过期阶段继续开发。
- 当前多个“绿色门禁”没有验证其声称的实现：前端 type-check 实际检查 0 个文件，OpenAPI 在 Gateway
  不可达时可 skip 后仍绿色，`harness.yml` 的 trigger 不会驱动 CI，常规 Gateway/Knowledge/DB/Rust
  测试也没有形成完整 CI 闭环。
- 默认 Assistant、Responses 与 Studio/Published Agent 还没有先解析为同一个 launch 合同；Capability
  Descriptor V2 在 Worker→Gateway→Runtime 链路中被 V2→V1→V2 重复投影。
- 长时间 embedding backfill/gate 仍占用同步 HTTP 生命周期，容易越过 Gateway 30 秒超时且无法清晰处理
  客户端断开、重试和取消。
- 旧代码、旧 Python Agent 文档、空/自证测试、无主依赖、超大文件和被 `.gitignore` 忽略的“永久截图”
  已经成为仓库维护问题；2026-08-13 的死代码数字本身已经过时，必须重做事实基线。

本 PRD 是一个完整功能，由同一主 Session 从事实冻结做到实机与回滚。工作包是提交、review 和停止边界，
不是把结果拆给多个并行 writer 的组织方式。

---

## 2. 当前事实：Assistant 已是 Rust 内核，Python 还负责什么

### 2.1 已删除的 Python 执行面

以下内容已经退出当前运行路径：

- Python `AgentLoop` 和工具选择循环；
- `apps/assistant-service` 应用与容器；
- Python docgen 执行服务；
- Assistant 请求在 Rust 失败后回退 Python loop 的路径。

`scripts/harness/runtime_dependency_gate.py` 永久阻止这些模块、容器名和依赖重新进入运行树。
`src/services/assistant_runtime_assignment.py` 只接受 `agent_runtime` owner。

### 2.2 `src/api/v1/assistant.py` 仍然存在的原因

`assistant.py` 是 **Gateway public API 聚合文件**，不是 Agent loop。它当前包含：

| 区域 | 当前责任 | 目标所有者 |
| --- | --- | --- |
| models/datasets/config | UI 所需目录与配置读取 | Python Gateway |
| tools/policies | 租户能力与策略展示 | Python Gateway |
| approvals/runs | 公共审批和 Run 状态适配 | Python Gateway facade → Rust Runtime |
| chat/chat-stream | 鉴权、限流、模型权限、会话绑定、SSE 转发 | Python Gateway facade → Rust Runtime |
| sessions/history | 用户会话 CRUD 与 V1 历史投影 | Python Gateway facade → Runtime ThreadStore |
| task cancellation | 公共取消入口 | Python Gateway facade → Rust Runtime |
| artifacts | 用户制品元数据与下载边界 | Python Gateway/Capability ownership |
| context metrics | 管理与观测 API | Python Gateway |

因此本轮要**拆模块、收窄 facade、删除兼容残留**，但不能把鉴权、模型目录、计费、租户策略
等 Gateway 责任迁入 Rust。

---

## 3. 用户问题与产品机会

### 3.1 主要用户

| 用户 | 当前问题 | 本轮结果 |
| --- | --- | --- |
| 平台使用者 | 系统升级不能引入聊天、工具、KB、历史恢复回归 | 所有现有旅程和公共合同保持兼容 |
| 自托管管理员 | 看到十余个 Compose 条目，不知道哪些是业务服务、依赖或一次性任务 | 管理面按三个业务边界展示状态、职责和降级原因 |
| coding agent | 需要读取超大文件和多份相互重叠的计划，修改范围容易失控 | 每个工作包有明确 owned/forbidden paths、门禁和提交边界 |
| reviewer agent | 很难判断重构是否改变行为或跨越数据边界 | 合同快照、负向权限测试和逐包 review 形成可重复证据 |
| 发布负责人 | 多镜像、多 schema、多内部协议缺少一个统一兼容矩阵 | 一份发布兼容 manifest 锁定代码、镜像、schema 与协议版本 |

### 3.2 产品假设

1. 当前 Agent 本地路径已经足够快；继续全面 Rust 化不会显著改善供应商主导的 TTFT。
2. 三个业务边界足以覆盖当前规模；增加更多服务会先增加故障与发布复杂度。
3. 数据权限、内部模块化和可观测性比物理拆库、服务网格或新语言迁移更能降低下一年的维护成本。
4. coding agent 在路径互斥、测试明确的小工作包中可靠性更高；同一主 Session 可以顺序完成 API、DB、
   Compose 和 core，但不得在同一个 active package/提交里同时改这些层，也不得拆给多个并行 writer。

---

## 4. 目标与成功指标

### 4.1 产品目标

1. 终端用户现有 Assistant、Agent、Knowledge、Artifacts 和管理功能无行为回归。
2. 一个新 coding agent 在只读入口文档后，可以在 15 分钟内指出某个功能的代码、服务和数据 owner。
3. Assistant Gateway 路由和 Runtime adapter 按 use case 拆分，修改单个能力不再要求理解 2,000 行文件。
4. 一个服务默认不能写另一个服务的 schema；应用进程不再拥有生产 DDL 权限。
5. 基础聊天、知识、读工具、写工具、连接器可以分别报告 healthy/degraded，不互相伪装成全局健康。
6. 评估 compact 拓扑，只有资源/p99 采用门通过才向用户提供；扩展拓扑可启动多个已证明安全的 worker。
7. 发布前可证明所有实际运行镜像、内部协议和数据库 revision 属于同一个兼容版本单元。

### 4.2 可量化验收

| ID | 指标 | 完成条件 |
| --- | --- | --- |
| AC-M01 | 单 Agent 内核 | `make runtime-dependency-gate` 通过；无 Python loop、Assistant Service 或 fallback |
| AC-M02 | Assistant API 可定位 | `assistant.py` 保留稳定 facade/兼容 re-export，路由与跨入口 service 分离；`agent_runtime.py` 同步拆分；新 Python 文件 `<800` 行且 import/owner gate 通过 |
| AC-M03 | Runtime adapter 可定位 | `control_plane.py`、`model_plane.py` 保留稳定 facade，内部按 use case 拆分；Runtime/Worker HTTP 热点完成 owner 分类与必要拆分 |
| AC-M04 | 公共兼容 | 既有 OpenAPI 路由、status code、SSE event、SDK fixtures 零非预期漂移 |
| AC-M05 | self-HTTP | Gateway 进程内获取 capability catalog 不产生发往 `gateway:8080` 的 HTTP 请求 |
| AC-M06 | DB 最小权限 | Gateway、Runtime、Capability Worker、Knowledge API/Worker 的 table/function/sequence 正负权限矩阵通过；租户越权测试仍独立通过 |
| AC-M07 | 新 schema 基线 | KB 升级后的验收状态生成不可变 baseline；新库从 baseline 初始化，旧库可验证并认领同一 fingerprint |
| AC-M08 | 单一迁移 authority | 生产模式只有 migrator 可执行 DDL；应用启动不自动修改 schema |
| AC-M09 | Core 边界 | 跨服务协议只来自 successor ADR 选定的无 I/O contracts package，或 versioned JSON Schema/fixture/Rust authority；无第二手写权威 |
| AC-M10 | 健康语义 | 基础聊天依赖失败使 core not-ready；可选能力失败只标记对应 capability degraded |
| AC-M11 | Trace 连贯 | Gateway → Runtime → Worker → Knowledge/Gateway broker 全链路保留 trace/request/run/execution identity |
| AC-M12 | Compose 模式 | compact 只有在 RSS 明显下降且摄入中查询 p99 不劣化时采用；scale 可启动至少 2 个 Knowledge/Capability Worker；Gateway/Runtime 不虚报可扩 |
| AC-M13 | 性能非劣 | 同机同负载 candidate p95 `≤25 ms`，且 `candidate_p95 - baseline_p95 ≤ max(baseline_p95 × 20%, 2 ms)` |
| AC-M14 | 真实产品回归 | Rust Assistant、工具、审批、取消、恢复、KB 检索、Knowledge 摄入和管理状态页面全部通过实机 |
| AC-M15 | 发布一致性 | compatibility manifest 被各 `/version`/管理面实际消费，覆盖所有镜像、协议、DB、Qdrant/embedding/BM25 和 Compose profile |
| AC-M16 | 门禁可信 | TypeScript app/node 检查非零文件；离线 OpenAPI 不依赖活栈；每个变更路径映射真实 CI 结果；发布 gate 零意外 skip |
| AC-M17 | 统一启动合同 | Assistant、Responses、Studio Preview、Published Agent 都先解析为 `ResolvedAgentLaunchV1`；ControlPlane 不再为某入口临时发明 AgentSpec |
| AC-M18 | 长任务耐久 | 既有 durable queue API保持兼容并 additive 暴露 execution/job；新 backfill/gate/rebuild 使用 versioned `202 + job_id`；断连可续、可查、可取消、幂等重试 |
| AC-M19 | 构建经济性 | Runtime/Worker 独立 artifact identity、同 revision 多架构镜像、单命令 Docker 更新；Cargo 只在 Docker/CI 内以 1 job 运行，记录冷/暖构建时间与峰值内存 |
| AC-M20 | 仓库质量 | post-RAG manifest 中 `confirmed_dead` 全处置；changed paths 不新增空/自证测试、无主依赖或死文档；动态候选具名 owner/backlog |
| AC-M21 | 证据治理 | scratch、durable、restricted raw 三类分流；所有永久截图/receipt 可由 manifest 定位并校验，不再被 ignore 后虚称永久 |
| AC-M22 | 大文件治理 | post-RAG LOC baseline 建立；超阈值文件 no-growth；例外具名 owner/原因/过期；拆分后 owner/import graph 而非只看行数 |

### 4.3 非目标

- 不全面重写 Gateway 或 Knowledge 为 Rust。
- 不创建独立 Edge、Governance 或 Rust Model Plane 服务。
- 不拆成多个物理 PostgreSQL 实例；本轮只做同库 schema/role 隔离。
- 不修改 KB 检索算法、RRF/rerank/BM25 阈值；这些属于本轮之前的 KB 升级。
- 不重新设计 Agent UI、AgentSpec、工具协议或产品导航。
- 不以删除代码行数、容器数量或 Rust 占比作为成功指标。
- 不把所有超大文件、所有历史测试或所有旧文档一次性重写；只处理经过 post-RAG 基线确认且与本轮
  owner/门禁相关的批次。
- 不把清理本机 `tmp/`、Docker cache、worktree 或浏览器凭据当作仓库优化；清理必须是窄 allowlist、
  dry-run 优先，并单独获得破坏性操作授权。

---

## 5. 目标架构

```text
Browser / SDK
      |
      v
Frontend -----> Gateway Control (Python)
                    |  public auth / tenant / model / quota / billing
                    |
                    +---- start Turn + SSE ----> Agent Runtime (Rust)
                    |                                |
                    |<--- private model stream ------+
                    |                                |
                    |                          Capability Worker (Rust)
                    |                             |              |
                    +---- KB public proxy ------> |              +--> Gateway-owned brokers
                                                  v
                                           Knowledge API (Python)
                                                  |
                                           Knowledge Worker (Python)

Shared infrastructure: PostgreSQL with per-service roles, Redis, Qdrant, object storage
```

Qdrant/Redis/object storage are shared physical infrastructure with logical namespaces, not ownerless
common buckets. Dataset vectors、Agent memory vectors、durable job keys 和 artifacts 分别声明唯一 owner；
Gateway/Knowledge 的跨 namespace 读写必须有具名 API/function 和负向测试。

### 5.1 部署单元与逻辑所有权

| 部署单元 | 语言 | 可独立扩展 | 数据 owner | 允许同步调用 |
| --- | --- | --- | --- | --- |
| `frontend` | TS/nginx | 是 | 无持久数据 | Gateway public API |
| `gateway` | Python | **当前否** | Gateway 数据、Assistant 公共 facade、策略/launch 签发 | Runtime、Knowledge、provider |
| `agent-runtime` | Rust | **当前否** | Thread/Turn/Item、snapshot、lease | Gateway model plane、Capability Worker |
| `agent-capability-worker` | Rust | 有条件 | capability execution/event ledger | Knowledge、Gateway broker、sandbox/local node |
| `knowledge-service` | Python | 是 | `knowledge` schema、Qdrant collection contract | Provider embedding/rerank、基础设施 |
| `knowledge-worker` | Python | 有条件 | 与 Knowledge API 同一领域，不另建 schema | PostgreSQL、Qdrant、对象存储 |

`agent-runtime` 与 `agent-capability-worker` 是一个 **Agent Execution 版本单元**，但保留两个进程：
前者不应获得工具工作区和业务 provider 凭据；后者的故障和高资源任务不应拖垮 token/event kernel。

“当前否”是事实而不是永远禁止：Gateway 有无 leader lease 的后台 scheduler，Runtime 的 broadcast、
approval/cancel 与实时 SSE 仍含进程内 ownership；在这些问题解决前复制实例会产生重复任务或丢失实时事件。
Capability/Knowledge Worker 的多副本能力也必须通过 durable claim、恢复、取消和 side-effect-unknown 测试，
不能只凭 `docker compose --scale` 启动成功。

Qdrant 是共享基础设施而非 Knowledge 整库独占：Knowledge 拥有 dataset collection namespace，Gateway/
Agent 数据治理拥有 memory-vector namespace。目标态必须为 collection/alias/prefix 建唯一 owner 与负向删除测试。

### 5.2 新服务准入规则

本 PRD 期间禁止新增常驻服务。以后新增服务必须同时满足：

1. 有明确且可测试的数据/契约 owner；以及
2. 至少一个真实需求：独立扩容、独立权限、故障隔离或独立发布周期；以及
3. 原进程内模块、worker 或资源池无法满足已经测得的 SLO；以及
4. 有兼容、可观测、部署和回滚责任人。

仅仅因为文件很大、语言不同或“微服务更先进”不构成新增服务理由。

---

## 6. 工作包

### ARC-00：KB 合入后的事实基线与架构决策冻结

**结果：** 后续所有变更从同一个干净 SHA、可信门禁、服务/数据 owner 和合同快照工作；本机有一条
不会拖垮机器的 Rust 更新路径。

**前置：** `kb-rag-upgrade` 已拆分提交、完成 KB 门禁、rebase/merge 到 `main`，主工作树干净。

#### ARC-00A：执行真相与事实冻结

1. 重新统计服务、路由、大文件、跨 package imports、实际 SQL/表/函数/sequence owner、Qdrant/Redis/
   object-store namespace、运行依赖和可扩条件；不得复制本 PRD 的行数快照当成当前事实。
2. 建立机器可读 `service-topology`、`data-access-inventory`、LOC/dependency/skip/artifact baseline；每项记录
   owner、读者、写者、健康、扩缩容、合同和证据来源。
3. 新增 successor/conformance ADR，确认“三个 bounded context、polyglot、无新增常驻服务、同库分权”；
   保留 ADR-006/007 的单 Rust kernel、lease/proof、append-only 不变量，逐条 supersede 已失效的 Python
   fallback、snapshot/model-plane owner 与 retry 描述。
   用户批准本 PRD 并启动实施，即视为接受本文冻结的三 bounded-context、polyglot、无新增常驻服务、
   同库分权决定。ARC-00 successor ADR 是 conformance record：post-RAG事实仍符合时主 Session可记为
   Accepted；只有事实要求改变这些决定时才停止并请求新批准，不能为重复确认卡住 Ultra Session。
4. 原 `platform-plane-restructure` 只保留 PPR-00 已冻结性能证据；PPR-01 的构建经济性并入 ARC-00C，
   PPR-08 作为独立 provider 实验 backlog，PPR-02～07/09 被新 ADR supersede。实施时只能有一个
   authoritative architecture program。
5. 关闭/归类仍显示 active 但已被 Rust cutover 取代的 FRC/PCH/ACU/CHR 等执行账；历史报告不改写。
6. 冻结 OpenAPI、SSE、Runtime HTTP/model metadata、Capability V2、DB 函数、Compose resolution、
   服务/image revision 与 PPR-00 性能基线。

#### ARC-00B：先修可信门禁

在移动任何业务模块前：

1. 修复 Web type-check，使 app 与 node/config 项目都检查非零文件；运行 Node 单测、lint、build、i18n。
2. 把 OpenAPI 拆为 in-process offline contract 与 live contract；不可达时不允许 offline gate skip。
3. 增加真实静态 import boundary；`apps/* ↔ src`、跨 app、core 反向 import 必须 fail closed。
4. `harness.yml` gate 增加 tier、trigger、required_on、resource、skip、timeout 和 evidence；diff 的每个路径
   必须匹配 gate，CI 最终 job 验证 required result 存在。
5. 将 RAG fixture replay、Knowledge 单测/隔离集成、live quality 分成不同名字与证据等级。
6. CI 补 Gateway/Knowledge/DB/Rust/Web Node 真实门禁；release-required gate 零意外 skip。
7. `harness-check` 明确区分 structural lint 与 semantic program/gate validation，程序状态与 docs index 不再
   手工维护两份真相。
8. 让最小 `hygiene-check`、evidence policy、LOC no-growth、dependency owner 和 shim ledger 从 ARC-01 前
   生效；ARC-07 只清历史 backlog/复核规则，不等到末尾才开始维护质量。

#### ARC-00C：Rust 构建与全局资源经济性

1. 吸收 PPR-01：Runtime/Worker 分别计算真实 source closure 和 artifact identity，但作为同 revision 的 Agent
   Execution release unit 构建、发布、验证。
2. 运行镜像使用最小受支持 base；默认 quickstart 拉取真实 versioned multi-arch 镜像，不依赖开发机
   `local-*` tag。
3. 托管 CI 是 Rust fmt/check/changed-crate 测试 authority；宿主机禁止直接运行 Cargo/rustc/rustfmt。
   本机候选制品只能由 Docker 多阶段 BuildKit builder 编译，或从远端 builder 拉取；随后核对 identity
   并 smoke。
4. 资源受限更新命令只调用 Docker builder：Cargo jobs=1 仅作为容器 build arg，构建前检查内存/磁盘；
   需要时先停应用服务降低峰值，保留基础设施、BuildKit cache 和可回滚镜像。
5. 在 Git common dir 实现 `integration-runtime` 与 `rust-build` 两类共享 lock；前者统一覆盖 Docker/DB/
   E2E/browser/provider，低内存机上二者互斥。原子获取、heartbeat、signal cleanup、stale/force-release
   规则遵循 harness integration contract。
6. 明确继承 PPR-01 数字门：Runtime image `≤150 MB`、warm one-line edit→healthy `≤15 min`；Worker
   image size 与峰值内存先冻结基线再预声明门槛。Docker/CI 内构建要求 jobs=1、无失控 swap/guard
   abort、Runtime/Worker identity 不串扰，并记录冷/暖时间、cache、SBOM/许可证。

**验收：** ADR approved；事实/合同基线可机器读取；假绿探针会失败；所有 changed paths 有真实 gate；
托管 builder 产出同 revision multi-arch 候选；本机 Docker builder 可产出单机候选并完成 identity/smoke；
宿主机无 Cargo/target 产物；
不存在两个 active 架构计划。

**禁止：** ARC-00A/B 不改业务语义或数据库；ARC-00C 只允许构建/分发/资源锁相关文件，不改变 Runtime/
Worker 协议、执行语义或 Compose 业务拓扑。

### ARC-01：Assistant Gateway API 模块化

**结果：** 保留相同 `/api/v1/assistant/**`、Responses 和 Agent Runtime 公共行为；路由不再被其他路由
当作 service library，单个 use case 有明确 owner。

**目标布局：**

```text
src/api/v1/assistant.py                 # 稳定 router facade + 有期限的兼容 re-export
src/api/v1/_assistant_routes/
  catalog.py           # models / datasets / config / tools / policies
  chat.py              # chat / chat-stream 与 Gateway edge checks
  runs.py              # approvals / runs / resume / cancellation
  sessions.py          # session CRUD / history
  artifacts.py         # artifact CRUD / download
  metrics.py           # context / tenant metrics
  schemas.py           # 仅该 API 私有的响应模型
src/services/assistant_entry/
  model_access.py      # Assistant / Responses / published routes 共用
  session_binding.py
  run_queries.py
```

保留 `assistant.py` 文件而不是立刻换成同名 package：`responses.py`、`agent_runtime.py` 与现有测试仍直接
导入其私有 helper。先把 helper 提取到 service，再用兼容 re-export 迁移消费者，最后删除 shim。

**要求：**

- `src/api/router.py` 的 import 与 prefix 行为保持兼容。
- Chat 路由只能校验 Gateway 责任并调用 `AgentRuntimeControlPlane`；不得出现模型/工具 while loop。
- Session history 继续从 Runtime projection 读取；不得恢复 `sessions.history` 为新写入权威。
- 公共异常 status/detail、SSE headers、auth/rate-limit 顺序和 OpenAPI operation 保持一致。
- 能复用 `src/api/schemas/assistant.py` 的公共 schema，不复制一份。
- 纯移动提交与行为修复提交分开；现有把 `str(exception)` 暴露给 500 响应的问题单独做错误脱敏并测试。
- `_thread_locks` 等按 session 增长的进程内 map 必须有清理/有界合同，不能只换文件位置。

#### ARC-01B：相邻 Agent API surface

`src/api/v1/agent_runtime.py` 同样超过 1,900 行并承担 Studio/发布、snapshot、附件、限流和流式启动。
在 Assistant facade 稳定后按 use case 拆分，但保持外部 router、operation id、权限、SSE 与内部 import
兼容；不得在拆分中重做 Agent Studio 产品。

**直接门禁：** 真正的 Assistant/Responses/Agent API、auth、model access、session、artifact、cancel/restore、
OpenAPI/SSE、runtime dependency、Ruff 与 import owner gate；零意外 skip。

**禁止：** 不改 Rust、不改 DB schema、不顺手重写 UI、不删除 V1 路由。

### ARC-02：Runtime Control/Model Plane 模块化并消除 self-HTTP

**结果：** Python 仍持有 Gateway policy/model 责任，但默认 Assistant 与其他 Agent 入口统一成一个 launch
合同；内部模块可独立测试；Capability V2 只投影一次；同进程 catalog 不再走网络。

**目标布局：**

```text
src/services/agent_runtime/control_plane.py  # 稳定 AgentRuntimeControlPlane facade
src/services/agent_runtime/control/
  thread_lifecycle.py
  snapshot_builder.py
  capability_catalog.py
  memory_context.py
  event_stream.py
  approvals.py
  run_ledger.py
  types.py

src/services/agent_runtime/model_plane.py    # 稳定 AgentModelPlane facade
src/services/agent_runtime/model/
  authorization.py
  request_builder.py
  native_responses.py
  chat_completions.py
  stream_projection.py
  accounting.py
  timing.py                 # 可复用现有 timing primitives，不复制定义
```

现有测试和脚本导入 facade 的私有 helper/logger，因此先保持 `.py` facade，内部移动完成且消费者迁移后才
评估 package 化；不得为满足目录外观制造 import 回归。

**self-HTTP 修改：**

1. 把 catalog 的租户/RBAC/策略过滤提取为进程内 `CapabilityCatalogService`。
2. Gateway internal route 与 `AgentRuntimeControlPlane` 调用同一个 service interface。
3. Service 内部仍可通过受限 HTTP client 读取 Rust Worker catalog；只删除 Gateway → Gateway 的回环。
4. 为未来物理拆分保留 `LocalCapabilityCatalogClient` / `HttpCapabilityCatalogClient` adapter，默认必须是 local。

**统一 launch：**

新增 versioned `ResolvedAgentLaunchV1`，至少携带 identity、resolved AgentSpec、model/profile、capability/
knowledge bindings、memory policy 和 channel policy。Assistant、Responses、Studio Preview 与 Published Agent
先由 Gateway policy 层解析成该合同；ControlPlane 只负责持久化、签 snapshot/model lease 并启动 Thread/
Turn，不再为默认 Assistant 临时合成另一套 AgentSpec。

**Capability 合同收敛：**

1. `CapabilityDescriptorV2` 是 snapshot 内部权威；仅在安装 Codex `dynamicTools` 的边界投影一次。
2. 统一重复的 permission helper；保留 tenant/RBAC/resource/version/proof 的 fail-closed 复核。
3. 在合同测试证明 Worker create 会重新校验后，删除每次工具执行前冗余 catalog HTTP preflight。
4. 删除无实际 Gateway 路由承接的 Worker-disabled fallback，改成具名 capability degraded；不得静默旁路。
5. `capability_leases.py` 等疑似死代码先跑 consumer/dynamic-entry gate，再删除或迁移。

**要求：** facade 的构造参数和主要公共方法通过 adapter 保持兼容；不要让 FastAPI Request 渗入
domain/service 模块；不得改变 lease、snapshot、timing、计费和 provider wire 语义。Gateway 允许拥有
身份/策略/不可变 launch 签发，不得拥有 loop、tool selection、compaction、cancellation state 或 event order。

#### ARC-02B：Rust HTTP 边界可维护性

在跨语言 fixture 稳定后，按 protocol/use case 拆分 Runtime `http_service.rs` 与 Worker `http_service.rs`、
`read_capabilities.rs` 中的路由/解析/状态机适配；上游 kernel 大文件不因本地 LOC 目标做无收益 churn。
仅拆 launch/V2 变更实际触及的模块，其他大文件进入 no-growth ledger。拆分前后必须由托管 CI 运行
changed-crate，并运行跨语言 contract、restart/cancel/recovery 和 side-effect-unknown 门禁。

**直接门禁：** control/model/launch/capability catalog、timing/accounting、SDK SSE、托管 CI 的
Runtime/Worker changed-crate、release/restart/recovery gate；Docker 内 Rust 镜像构建在最终串行窗口运行。

**采用门：** Rust Model Plane 不在本包实现。只有后续独立实验同时证明正确性收益或显著资源收益时才能新立项。

### ARC-03：单一迁移 Authority 与 PostgreSQL 分角色

**结果：** 仍使用一个 PostgreSQL，但服务的错误 SQL 不能跨域写数据。

#### 3A. KB 升级后定档新的 `init` baseline

该构想合理，但**不能在 migration 105 后直接拍快照**。当前真实初始化先加载 `schema.sql`，随后有
filename、numeric、per-service 多种 runner/ledger；根迁移从 002 起且存在重复版本号。RAG 100–105 的
新表使用未限定名称，最终 schema 又依赖 runner 的 `search_path`。必须先完成对象归属收敛和 runner
统一，再区分“新安装入口”和“已有安装历史”：

- **新安装：** 不再依次执行历史 `001～099`；直接应用 KB 升级完成并验收后的不可变 baseline，
  然后执行该 baseline 之后的新 migrations。
- **已有安装：** 绝不删除、重写或伪造已经执行过的历史 migrations。先验证实际 schema 与新 baseline
  等价，再写入 baseline marker；不重放 `init`，不丢失业务数据。
- **编号：** 可以重新从 `001`、`002` 开始，但必须位于新的 migration epoch 目录中，migration id 使用
  `baseline_id + sequence`，避免和旧全局 `001` 冲突。
- **收敛点：** post-KB 新增一次具名 schema convergence change，按 ownership inventory 检查/移动**全部
  持久对象**；100–105 的 8 张新表是强制回归子集，不是全部范围。目标与来源同名对象同时存在时 fail
  closed，不自动合并或删除。完成后才能冻结 baseline。

推荐目录：

```text
database/
  bootstrap/
    roles.sql                       # 创建/轮换 LOGIN/NOLOGIN roles，不放业务表
    extensions.sql                  # allowlist extension，owner/version 可验证
  baselines/
    2026_08_post_kb_v1/
      init.sql                      # 新安装唯一 schema 起点
      reference_data.sql            # 必需默认角色/策略等，不含用户数据与凭据
      grants.sql                     # 由 data-access inventory 生成的最小权限
      manifest.json                 # baseline id、Git SHA、schema fingerprint、对象清单
      verify.sql                    # 只读验证，不修复、不写业务数据
  migrations/
    legacy/                         # 现有 001～KB 定档点；不可变，只服务旧库升级
    2026_08_post_kb_v1/
      manifest.yml                  # owner/mode/checksum/rollback/pre-postconditions
      001_<change>.sql
      002_<change>.sql
  schema.sql                        # 由 baseline + current migrations 校验生成，不再手工成为第二真相
```

目录迁移必须分两步：先让 runner 同时识别当前路径与新路径并通过升级测试，再移动 legacy 文件；不得在
同一提交里移动全部 SQL、重写 runner 和改变 schema。

#### 3B. Baseline 生成规则

`init.sql` 不允许人工把历史 SQL 简单拼接。定档流程必须可重复：

1. 从每个受支持的真实来源构建临时数据库：现行 fresh `schema.sql` 路径、public-layout、split-layout、
   filename ledger、numeric ledger、per-service ledger 和部分迁移状态；不能声称 legacy SQL单独可重建新库。
2. 用兼容 runner 补齐 RAG 迁移与 schema convergence change；对 ownership inventory **每一行**断言
   relnamespace/owner/ACL/default privileges，100–105 对象是具名强制子集。
3. 每个旧来源执行与 fresh baseline 相同的 object-owner/default-privilege/grants cutover change 后再计算
   fingerprint/adopt；不能拿旧 PUBLIC ACL 与新最小 grants 直接对拍。
4. 运行 schema、KB、Runtime/Capability 真 PostgreSQL 门禁，并生成唯一 data-access inventory。
5. 从收敛数据库生成稳定 DDL 与必需 reference data；去除环境 login owner、随机 ACL、时间和宿主路径。
6. 分别计算 structural、owner/ACL/default privilege、required extensions、reference-data 四类 fingerprint；
   不把用户数据或密钥混入 baseline。
7. 在第二个空数据库应用 `roles/bootstrap → extensions → init.sql → reference_data.sql → grants → marker`，
   证明四类 fingerprint 与所有升级来源一致。
8. 分别以 Gateway、Runtime、Capability Worker、Knowledge API/Worker 角色启动，证明 baseline 不是“DDL
   相等但应用不可用”。
9. 将 baseline id、源 Git SHA、最后 legacy change、四类 fingerprint、生成工具/镜像版本和兼容 manifest
   写入 `manifest.json`；文件定档后不可修改，只能新建下一 baseline。

`reference_data.sql` 与 exact hash 只包含应用用户不可修改、以具名 natural key 标识的 system-owned
不可变目录行，并逐行声明不可变列。租户角色绑定、策略、provider/model、rate-limit 和任何管理员可编辑
行都是业务数据，不进入 exact hash；只验证 schema、必需 key 和语义不变量，升级必须保留本地修改。
Adoption 不覆盖用户数据：缺失 system row 通过具名 change 补齐，受保护值冲突则 fail closed 并报告。

ACL fingerprint 使用 manifest 中逻辑 principal id。NOLOGIN object-owner 名应稳定；可配置的是 LOGIN
principal/部署映射，不能让环境角色前缀造成等价权限 hash 不同。

#### 3C. Migration ledger 与旧库认领

使用一个由 migrator 独占写权限、且不与现有表名冲突的新账本：

```text
platform_schema_baselines(
  baseline_id, manifest_sha256, structural_sha256, acl_sha256,
  extensions_sha256, reference_data_sha256, source_git_sha, adopted_at
)

platform_schema_changes(
  baseline_id, sequence, name, checksum_sha256, applied_at, duration_ms, runner_digest
)

platform_schema_change_attempts(
  attempt_id, baseline_id, sequence, checksum_sha256, runner_digest, phase, checkpoint,
  lease_owner, fence_generation, state, started_at, finished_at, error_code
)
```

`platform_schema_changes` 只记录成功事实，主键为 `(baseline_id, sequence)`，完整 SHA-256 永久不可变。
事务型 change 的 DDL 与成功 ledger 同事务提交，因此不承诺持久 `started`。只有显式 non-transactional
change 使用独立 attempts/lease/recovery 表；它不能伪装成原子事务。

旧库升级规则：

1. 如果旧 ledger 完整到定档 migration，先只读生成实际 fingerprint。
2. fingerprint 与 baseline 完全一致时，只写一条 baseline adoption marker；不得执行 `init.sql`。
3. 旧库只完成了部分 migrations 时，先通过 compatibility runner 补齐到定档点，再执行验证和认领。
   numeric ledger 遇到重复 016/031 等歧义时，必须以对象/约束/checksum 证据生成 reconciliation receipt；
   无法证明具体文件已执行则 `BLOCKED`，不能猜测补跑或直接认领。
4. 对象、约束、函数、grant 或 checksum 任一不一致时 fail closed，并生成 drift report；不得自动猜测修复。
5. 认领 baseline 后只读取新 epoch 的 `001/002/...`；同一 `(baseline_id, sequence)` 的 checksum 永久不可变。
6. Runner 的重复调用必须幂等；每个 change 本身不可变、有前/后置条件，并对“对象已存在但定义错误”
   fail closed。不得用大量 `IF NOT EXISTS` 掩盖漂移。无法单事务执行的操作必须显式分类并采用
   expand/backfill/contract。
7. 现有 `schema_migrations`、numeric ledger 和 `schema_migrations_meta` 只能作为一次性 adoption 输入；
   验证完成后冻结为历史证据，不再写入，不能长期维护多套 authority。
8. 新 runner 使用 PostgreSQL session advisory lock；runner 拥有事务，migration 文件不自带
   `BEGIN/COMMIT`。非事务 change 必须显式声明、记录状态并有恢复程序。
9. 事务型 change 的 SQL 与成功 ledger 原子提交。非事务 change 不具备该原子性：attempt/lease/
   checkpoint/postcondition 必须让 crash 后进入可判定的 resumable/failed 状态，只有全部后置条件通过才
   写成功 ledger。并发 runner、checksum 修改、错序和部分对象均 fail closed；`status/verify` 绝对只读。
10. 每个 epoch `manifest.yml` 是机器载体，逐 change 声明完整 SHA-256、owner、transaction mode、
    rollback class、前后置条件、timeout/lock budget 和 resume/repair handler；runner/CI 不从报告文字猜测。
11. 所有 `SECURITY DEFINER` function 必须由 NOLOGIN owner 持有、固定安全 `search_path`、撤销 PUBLIC
    EXECUTE，并仅授权 allowlist roles；静态 SQL/真 PG门禁任一不满足即 no-go。

统一完成时逐个封死旁路：`scripts/new/migrate.sh`、`database/cli.py`、
`database/migrate_per_service.py`、`database/run_migration.py`、Gateway `AUTO_INIT`、Compose migrate image
和 Helm hook 都只能调用同一 authority/ledger；不存在的 Helm Python entrypoint 必须修复或删除，不能留
一个未测试的“备用 runner”。

每次新 baseline 只为降低新安装成本和整理历史；不能把“定档”当成删除线上升级路径的理由。legacy migrations
至少保留到所有受支持版本都已越过该 baseline，并保留在仓库 archive 中供审计和灾难恢复。

#### 3D. Baseline 与数据库角色的执行顺序

**角色：**

| DB role | 默认权限 |
| --- | --- |
| `ai_gateway_migrator` | 唯一 DDL 执行身份；可 `SET ROLE` 到 NOLOGIN object owners |
| `ai_gateway_gateway` | 具名 Gateway table/function/sequence 权限，不是整个 schema DML |
| `ai_gateway_runtime` | 具名 Thread/Turn/Item/snapshot/lease 对象权限 |
| `ai_gateway_capability_worker` | 具名 capability execution/event 权限；只读获准 snapshot/lease view/function |
| `ai_gateway_knowledge_api` | Knowledge API 所需对象权限；无 worker-only claim 写入 |
| `ai_gateway_knowledge_worker` | Knowledge durable job/ingestion/indexing 所需对象权限 |

该表只描述进程角色，不是可直接执行的 grants 清单。当前 Worker/Runtime/Gateway 仍跨写 memory、quiz、
approvals/runs 等对象；最终权限只能由 post-RAG `data-access-inventory` 逐 table/function/sequence 生成。

**要求：**

1. Cluster-role bootstrap 与 schema migrator 分离。Local Compose 可由一次性 PostgreSQL admin 创建带可配置
   前缀的 NOLOGIN owner/LOGIN app roles；托管 PostgreSQL 由 DBA 预创建并提供无秘密 receipt。Migrator
   永无 `CREATEROLE`/cluster-admin；随后应用 baseline/changes，最后才启动应用。密码不能写进仓库/证据。
2. `init.sql` 和后续 change 明确 object owner；migrator 本身不永久拥有对象，只能按需 `SET ROLE`。末尾
   执行 `REVOKE PUBLIC`、撤销 function 默认 EXECUTE 并授最小 grants。
3. 去掉应用角色对业务 schema 的 `CREATE` 和 `PUBLIC` 宽授权。
4. Role-level `search_path` 以 `pg_catalog` 开头、`public` 最后且不可 CREATE；关键 Python/Rust SQL仍显式
   schema，不依赖 runner 或数据库全局顺序。
5. Gateway 跨 assistant 的必要操作通过具名 function/view，禁止直接获得整个 schema 写权限。
6. quickstart 仍是一条命令：初始化脚本生成各角色凭据，migrator 在应用启动前完成。
7. 生产配置应用进程 `AUTO_INIT=false`；开发/测试 escape hatch 必须具名、显式且不能进入生产默认。
8. 应用启动只检查支持的 baseline/epoch revision 与 required objects；完整 fingerprint 由 migrator/status
   只读执行。兼容 revision 不满足时应用不启动。
9. 保留同库事务与现有外键，暂不拆物理数据库。
10. 角色切换分阶段：创建 roles/双 grants → 新版本切换凭据 → 观察与正负测试 → 撤销旧共享 superuser。
    角色强制收紧必须晚于 data-access inventory 和 ARC-04 的跨域 persistence 清理。
11. Baseline install 先验证数据库为空；任何非空数据库只能走 reconciliation/adoption，绝不尝试把
    `init.sql` 当作“幂等修复脚本”执行。

Migration 101 会改写状态/哈希、替换唯一约束并扫描/锁定大表，不得默认标为 additive。该 finding 已发送
给当前 RAG 验收任务，优先作为合入门。若分支已合入但证据缺失，baseline freeze 前必须用保存的 pre-101
数据库快照/真实规模副本与 N-1 binary 补做；证据未闭合不得冻结 immutable legacy。执行前先声明锁预算，
超过预算必须拆成 batch backfill / concurrent index / 延后 contract；无法证明旧 binary 可运行时标记
`restore-required`，回退靠匹配备份而不是简单部署旧镜像。

**测试：** 所有受支持 init/ledger/layout 来源收敛、baseline 四 fingerprint 对拍、旧库认领、部分旧库补齐、
concurrent/kill/checksum/错序拒绝、fresh install、只读 status、每个角色的 table/function/sequence 正负矩阵、
租户越权、Runtime/Worker/Knowledge 真 PostgreSQL、migration 101 兼容/restore、备份恢复与 Compose/Helm顺序。

每个来源必须作为一行端到端矩阵执行，而不是把组件测试拼成结论：

| 来源 | 必须完成 |
| --- | --- |
| 空库 baseline | empty preflight → bootstrap/extensions/init/reference/grants → 四 fingerprint → 五应用角色启动 |
| `schema.sql` / Gateway AUTO_INIT + absent ledger | 对象证据 reconciliation → convergence/grants → adoption；不得伪造已执行 filename |
| public + filename ledger | legacy reconcile → convergence → adoption → 四 fingerprint → rollback verdict |
| split + filename ledger | RAG 100–105 relnamespace/owner → convergence → adoption → 五角色启动 |
| split + filename + `schema_migrations_meta` mixed | 双 ledger reconciliation receipt → 单 authority import/freeze → convergence/adoption |
| numeric ledger | duplicate-version reconciliation receipt → convergence/adoption；歧义即 BLOCKED |
| per-service meta ledger | import evidence → convergence/adoption → 原 ledger 只读冻结 |
| 声明的 partial/crash states | resume/repair decision → 无 ghost ledger/object → fingerprint 或 fail-closed report |

**高风险 review：** 独立数据库 reviewer 必须检查 grants、search_path、SECURITY DEFINER、函数 owner 和回滚。

### ARC-04：收缩 `ai-gateway-core`，建立真正的 contracts 层

**结果：** 跨服务共享的是协议和基础设施原语，不是领域业务实现。

**目标：**

1. 先生成 import/data-access inventory，列出 Knowledge 实际使用的 core 模块；只有至少两个 owner 真实消费
   且跨边界稳定的无 I/O 协议才进入轻量 `packages/ai-gateway-contracts`。若清单过小，可保留独立 fixture/
   schema 而不为了目录美观新增 package。contracts 只允许：
   - Pydantic/dataclass 协议模型；
   - version/schema constants；
   - 纯验证与签名 payload 规范；
   - 无 I/O 的序列化工具。
2. contracts 禁止数据库、Redis、HTTP client、FastAPI、provider SDK 和服务配置依赖。
3. `ai-gateway-core` 暂时保留跨 Python 服务的 auth/comm/tracing 等基础设施，但建立明确 allowlist。
4. quiz、skills、image、memory、sharing、knowledge、eval 等具体实现迁回 owner；不能为了减少重复把业务逻辑继续塞进 core。
5. 每移动一组先保留薄兼容 import，迁移所有消费者后删除 shim；shim 必须有删除条件和测试。
6. Rust/Python 共享协议以现有 JSON schema/fixture 或 Rust contract crate 为权威；Python package 是生成物/
   薄模型，不成为第二个手写权威。
7. `DatabaseStorage`、AgentRepository 和 Knowledge persistence 按 table writer owner 分离，先消除跨域 SQL，
   再让 ARC-03 收紧角色；不把 55k 行 core 全量搬空作为完成条件。

**第一批优先级：**

- capability proof、runtime lease、event envelope → contracts；
- knowledge HTTP proxy → Gateway client owner；
- quiz/skills/image/memory/sharing concrete services → Gateway/对应领域；
- persistence god classes 按 schema owner 分离，不复制到新 contracts。

ARC-00 的 import/data-access inventory 把第一批冻结为具名文件/消费者清单；只有该清单和本轮触及路径
阻塞本 PRD，其他领域候选进入 owner/no-growth backlog，不把全仓 core 清空当作隐含范围。

**门禁：** 机械 allowlist、Knowledge→core 依赖数、最小 wheel 安装、import graph、包构建、应用隔离、
无循环依赖、data-access owner 与原 API 测试。

**禁止：** 不做一次性全仓移动；每个提交只迁一个领域，并提供消费者清单。

### ARC-05：长任务、依赖健康、服务身份和端到端 Trace

**结果：** 系统能准确回答“什么坏了、影响哪些能力、哪一次 Run 在哪个 hop 失败”。

**健康模型：**

| 层级 | 语义 | 示例 |
| --- | --- | --- |
| live | 进程和事件循环活着 | 不探测远端依赖 |
| core-ready | 该服务的核心产品路径可接流量 | Gateway auth+DB+Runtime+model plane；Runtime kernel+store |
| capability | 可选能力独立 healthy/degraded | KB、write tools、image、connector、local node |
| dependency detail | 管理员可见、用户不可见的失败原因 | timeout、auth、schema mismatch、queue unavailable |

**要求：**

1. Capability Worker readiness 检查 execution store；其下游按 capability 单独报告，不全局阻断。
2. Runtime readiness 不因可选工具失败而下线，但必须暴露 Worker/catalog/model-plane dependency 状态。
   Worker 不可用时，仅无显式工具/KB绑定的纯文本 Turn 可使用空 catalog；存在显式 binding 时必须 fail
   closed，不能悄悄丢能力。
3. Gateway readiness 区分 core 与 degraded；不得用一个可选 connector 让所有聊天退出负载均衡。
4. 所有内部 HTTP 传播 W3C `traceparent`/`tracestate`、`x-request-id`，并关联 `run_id`、`turn_id`、
   `execution_id`；日志不包含 prompt、token、密钥或原始敏感参数。
5. 扩展而非重造现有 `GatewaySecret v2` / `INTERNAL_AUTH_KEYS`：key id 绑定 caller service、audience/path；
   dual-read/single-write 轮换后撤销 shared token。lease/proof 继续绑定 tenant/user/session/run/execution。
   mTLS/service mesh 不在本轮范围。
6. 增加故障注入测试：Worker down、Knowledge down、provider timeout、DB 权限拒绝、schema mismatch、
   SSE client disconnect。
7. 已有 reembed/reprocess/recover/retry durable queue API保持当前 status/response 兼容，并 additive 返回
   `execution_id`/`job_url`。尚无耐久语义的 embedding backfill/gate、BM25 rebuild 新增 versioned job
   endpoint（202/job_id、持久 claim、GET receipt、cancel、断连不取消、幂等）；旧同步入口在具名兼容
   窗口中作为 adapter，OpenAPI/SDK/UI 同步迁移。预期变化写 approved contract-delta manifest，不计入
   AC-M04 的非预期漂移。
8. Gateway/Runtime 的后台 scheduler、thread/SSE owner 在本轮保持单实例；若未来扩 Gateway，先给每个
   scheduler 数据库 leader lease；若扩 Runtime，先解决 thread affinity/跨实例事件通知。

**验收：** 每个故障产生唯一、可追踪且产品语义正确的终态；无卡住的 running/approval/execution。

### ARC-06：Compact/Scale 部署模式与管理员架构状态页

**结果：** 单机用户减少无必要进程，生产/压测用户可扩 worker；管理员不需要打开 Docker Desktop 猜服务。

**Compact：**

- `knowledge-service` 使用已有 `KNOWLEDGE_RUNTIME_ROLE=all`，同时提供 API 和单实例摄入 worker；
- 不启动独立 `knowledge-worker`；
- Agent Runtime 与 Capability Worker 仍保持隔离；
- 用于本地开发、演示和低并发自托管。
- 只有实测总 RSS 明显下降、摄入负载下 Knowledge 查询 p99 不劣化、event loop 不被 worker 阻塞时才设为
  默认；“少一行容器”不是采用理由，未过门则保留双进程。

**Scale：**

- Knowledge API 与 Worker 分离；
- 去掉所有长期服务/网络的固定全局 identity 依赖，脚本使用 Compose project/service labels 发现实例；
- `docker compose up --scale knowledge-worker=2` 可用；
- durable claim、lease、recovery 和幂等保证多副本不重复写向量或抢占 generation；
- Compose 只承诺单机扩展；跨主机编排另立项目。
- Runtime/Gateway 保持单实例并在 topology manifest 中如实标记；Capability Worker 另跑两副本
  recovery/cancel/side-effect-unknown，不能因为进程能启动就宣称 scalable。

**管理员产品面：**

- 扩展现有 Services/health 管理区域，不新增重复导航页面；
- 按 Gateway Control、Agent Execution、Knowledge、Infrastructure 分组；
- 显示职责、逻辑状态、版本、必要/可选依赖、degraded 原因和最后检查时间；
- `init`/`migrate` 明确显示为一次性 job，不显示为长期业务服务；
- 不暴露容器 IP、内部 URL、用户名、DSN、token、宿主机路径或原始异常堆栈；
- 后端使用一个显式、仅管理员可见的 additive API；更新 OpenAPI 与权限测试。

**机器拓扑合同：** 新增 `service-topology` manifest，记录 `service_id / bounded_context / process_role /
exposure / required_deps / optional_deps / state_owner / scale_support / health_contract / image_artifact`。
Compose、status、deploy wait、ownership guard、管理面和 compatibility manifest 从同一清单生成/校验，不能
再次漏掉 Capability Worker 或把 `init/migrate` 当长期服务。

**验收：** compact/full/scale resolution、资源与 p99 adoption、Knowledge/Capability 2-worker 实机竞争/
恢复、Gateway/Runtime single-instance guard、管理面桌面/窄屏/无权限/degraded、fresh-machine quickstart。

### ARC-07：仓库质量与证据治理

**结果：** 仓库不再靠过时扫描、假绿测试、无主依赖和不可移植截图维持“看起来有证据”；死亡资产可
证明地删除，历史证据可追溯，活文档只有一个执行真相。

**规则来源：** [`docs/harness/repository-quality.md`](../harness/repository-quality.md)。RAG 合入后重做
baseline，2026-08-13 的“240 处”只作为线索，不能复制为当前数字。

#### 7A. 复核门禁与处理测试信任 backlog

- ARC-00 独占首次 type-check/OpenAPI/import/RAG gate 修复；ARC-07 只复核未回归假绿。
- 处理 post-RAG manifest 中具名空 `pass`、自证 mock、过期 skip/xfail 和本轮 changed-path 候选；冻结其
  failure mode 与 replacement executable test。
- 其余 `needs_dynamic_proof` 测试具名 owner、所需证据和 backlog，不阻塞本轮完成。

#### 7B. 死代码、依赖与兼容 shim

- 高置信候选需同时通过全仓引用、entrypoint/plugin/route/Compose/CLI、合同和前后定向门禁才删除。
- 动态 discovery、FastAPI 隐式依赖、provider/plugin extra 不能只凭 `rg` 删除。
- 每次只移除一个直接依赖并更新 lockfile；dependency ledger 记录 owner、用途、入口和删除条件。
- shim 必须记录消费者、owner、截止 release/date 和 absence gate；无期限 shim 判为架构债务。

#### 7C. 文档生命周期

- 机器状态统一为 `active / queued / superseded / archived`；`blocked` 只属于程序/package 执行状态，
  Current/Queued/Blocked/Historical 只是 docs index 展示分组。每个 domain 最多一个 active program。
- Current/Queued 记录 domain_id、owner、last_verified、successor；queued 另记 prerequisite。
- 已删除 Python AgentLoop 的设计/计划移出 active 阅读路径；旧 prompt handoff 不再作为计划。
- ADR、migration、loop-state、失败 critic、security/release/rollback evidence 保留；由 successor/归档而不是
  改写历史。
- 修复发布文档与实际分发服务冲突；active 文档引用不存在源码路径时 gate 失败。

#### 7D. Playwright 与证据

- artifact 按内容分类而非目录：截图/HTML可为 scratch，trace/video/HAR/console/network 与未脱敏截图默认
  restricted raw；`web/.playwright` auth 永非普通 scratch。
- durable evidence 经脱敏后进入 manifest（SHA、source SHA、命令、场景、viewport、URI）；不能一边被
  `.gitignore` 忽略，一边由 feature oracle 宣称永久。
- trace/HAR/video/raw receipts 若含敏感元数据绝不提交，使用密封包/留存期；清理前先解除 oracle 引用。
- 提供窄 allowlist、dry-run 默认的 artifact status/cleanup；动态读取全部 Git worktree，canonicalize 目标，
  拒绝 repo/common-dir/worktree/symlink/外部挂载/env/auth/committed/referenced evidence；只清本次产生或用户
  授权的本地文件，不用清用户 ignored 数据换 gate 绿色。

#### 7E. 大文件与 module/bounded-context ownership

- 生成 post-RAG LOC/complexity/import baseline；已超阈值 no-growth，新 Python <800、TS/TSX <500。
- 例外必须有 owner、原因、过期和 removal condition；测试文件按 scenario 拆，但与生产重构分提交。
- 优先治理本轮触及的 Assistant/Agent/control/model/core/persistence/Knowledge 热点；pinned upstream Rust 不为
  行数做无收益 churn。
- fresh-agent locating task 验证“15 分钟定位 owner”，记录起点、答案和误导文档，而不是主观打勾。

**已核实的首批候选（合入后重验）：** `FileStorage` barrel-only export、未调用 `get_langgraph_proxy`、
`tests/proxy/test_streaming.py` 的空/自证测试、错误发布文档；Adapter discovery 和若干 Python/Web 依赖仍
属于动态验证候选，不得直接批删。

**验收：** `hygiene-check` 进入轻量 CI；post-RAG manifest 的 confirmed_dead 批次前后门禁等价；active
文档无死路径/双重 owner；永久 evidence 可解析；changed paths 不新增空/自证测试、无主依赖或意外
only/fixme；动态候选有 owner/backlog；size/dependency/shim ledger 有效。

### ARC-08：统一发布兼容矩阵、最终回归与文档退役

**结果：** 测试通过的就是实际发布的完整版本单元，历史计划不会继续误导 coding agent。

本地终态为 `RELEASE_CANDIDATE_PASS`：托管 builder 或临时 registry 已证明 OCI multi-arch index、digest、
`/version` 与 fresh isolated-environment quickstart；本机可 pull/smoke。向 GHCR/生产 registry push 与真正
远端 fresh-machine pull 属于用户明确授权后的 `RELEASED`，不阻塞代码候选完成。若执行前授权 staging
registry，则远端 pull receipt 并入候选验收。

**Compatibility manifest 至少包含：**

- Git SHA；
- Gateway、Frontend、Knowledge、migrator 镜像 digest；
- Agent Runtime 与 Capability Worker fork/overlay/image/schema digest；
- 数据库 migration revision 与 grants revision；
- OpenAPI、SSE fixture、Capability Contract、Agent event schema digest；
- compact/scale Compose profile revision；
- Node/Python/Rust toolchain major version。
- release id 与各服务 `/version` 实际上报值；
- Qdrant dataset/memory namespace revision、collection/alias、embedding provider/model/dimension、BM25 revision；
- service-topology、data-access、quality baseline 与证据 policy revision。

**最终目标门禁分层：**

```bash
# L0/L1: offline and domain
make harness-check
make affected-gates BASE_SHA=<post-rag-base>
make architecture-boundary-gate
make verify-openapi-contract
make validate-example-config
make runtime-dependency-gate
make verify-assistant-runtime-dev
SDK_SSE_CONTRACT_REQUIRE_ALL=1 make sdk-sse-contract
make rust-workspace-gate
AI_PLATFORM_AGENT_RUNTIME_SOURCE=<controlled-fork> make agent-runtime-write-gate
make kb-unit-gate
make rag-eval-fixture-contract
make hygiene-check
make web-quality-gate           # real app+node typecheck, Node units, lint, build, i18n

# L2: hosted/disposable integration
make platform-db-convergence-gate
make agent-execution-integration-gate
make knowledge-integration-gate

# L3: singleton live release window
make integration-preflight
make validate
make status
AI_PLATFORM_AGENT_RUNTIME_IMAGE=<manifest-runtime-digest> make agent-runtime-smoke
AGENT_CAPABILITY_WORKER_IMAGE=<manifest-worker-digest> make agent-capability-worker-smoke
AI_PLATFORM_AGENT_RUNTIME_IMAGE=<manifest-runtime-digest> \
  AGENT_CAPABILITY_WORKER_IMAGE=<manifest-worker-digest> make capability-live-write-gate
make rag-live-quality-gate
make e2e-release-gate           # frozen scenario ids/counts, zero unexpected skip
make fresh-install-gate
make platform-rollback-rehearsal
```

上面的新增/重命名门禁是本程序的交付目标，不是当前已存在/已通过的命令。ARC-00 实现 gate schema、
diff selector、CI result enforcement，以及当时能真实执行的 boundary/OpenAPI/Web/fixture/minimal hygiene gate；
每个后续 ARC 在其实现存在后登记自己的 DB、runtime、Knowledge、deployment/live gate。禁止提前创建
placeholder PASS。ARC-08 只消费全部已实现门禁。现有 `agent-runtime-release-gate`、静态 RAG fixture、
可选语言 SDK 和 route smoke 只能作为子证据，不能冒充全平台 release gate。

Docker、迁移、E2E 与真实模型测试前必须先读 `docs/harness/runtime-and-secrets.md`。最终实机至少覆盖：

- Assistant 短答、长输出、多轮、刷新恢复、取消后继续；
- 读工具、写工具审批拒绝/批准、Worker 重启恢复；
- Assistant → Knowledge 检索与引用；
- Knowledge 上传、摄入、查询、worker 竞争和失败恢复；
- 管理员架构页的 healthy/degraded；
- compact 与 scale 拓扑；
- current → frozen → current 回滚。
- fresh isolated environment 从临时/授权 registry 拉取 Runtime + Capability Worker 同 revision multi-arch
  制品并完成 quickstart；真正公开 registry/fresh-machine receipt 仅在发布授权后要求。

完成后更新 `docs/README.md`、harness、successor ADR 和相关 runbook 状态；历史报告只归档，不得改写为
新证据。最终产物是 merge/push-ready candidate；只有用户明确授权后才 push 或部署。

---

## 7. Coding-agent 执行合同

### 7.1 每个工作包的最小输入

实现 Agent 只需读取：

1. 根 `AGENTS.md`；
2. 本 PRD；
3. `docs/harness/work-packages.md` 与 active ARC package；
4. 所属 ARC 工作包列出的目标文件；
5. `docs/harness/commands.md` 中对应门禁；
6. 若触碰 Docker/DB/E2E/provider，再读 `runtime-and-secrets.md` 与 `integration-and-rollback.md`；
7. 清理代码/文档/测试/截图时读 `repository-quality.md`。

不要要求每个 Agent 阅读所有历史 PRD、研究报告或已 superseded runbook。需要历史证据时由集成 Agent 给出
精确文件和段落。

### 7.2 标准任务描述

给 coding agent 的任务必须包含以下六项，不写角色扮演和工作目录废话：

```text
结果：最终必须达成什么用户/系统行为。
Owned paths：本 Agent 可修改的目录或文件。
Forbidden paths：不得修改的共享/其他工作包路径。
必须保持：公共合同、数据 owner、性能或安全不变量。
验证：必须实际运行的精确命令和实机场景。
完成输出：提交列表、测试结果、未验证项、风险和后续集成说明。
```

### 7.3 Worktree 与并行规则

- 整个架构升级由一个主 Session 在一个 writer worktree 顺序完成；同一大功能不拆给多个 writer。
- 并行 Agent 只做只读探索、review、测试批判或证据核对；发现项交回主 Session应用。
- 每个 ARC 是独立 checkpoint commit/review 边界，但主 Session 对最终集成和实机负责。
- `src/main.py`、`src/api/router.py`、`docker-compose*.yml`、`Makefile`、`harness.yml`、`.env.example`、
  `.github/workflows/**`、`docs/README.md`、`database/schema.sql` 仅在各 ARC 的 integration commit 修改。
- ARC-04 在 ARC-01/02 facade/launch 稳定后执行；data-access inventory 先于 DB grants 收紧。
- ARC-05/06 在接口与角色稳定后进行；Docker 内 Rust 镜像构建、migration、E2E/provider 全局串行；
  Rust fmt/check/test 只在托管 CI 运行。
- 每个提交只对应一个可 review 主题；禁止把格式化、API 拆分、DB grants 和 Compose 混进同一提交。

### 7.4 Review 规则

1. 主实现 Session 先运行直接测试并提交候选。
2. reviewer Agent 只读检查 diff、调用链、合同和失败路径；不得与主 Session并发写或边 review 边重写。
3. finding 按 blocker/high/medium 分类；修复后只重跑直接门禁和受影响汇总门禁。
4. ARC-03、内部身份、证据清理和回滚必须独立 review；主 Session不得自批。
5. 同一主 Session 最后集成、跑全门禁和实机；Git 合并成功不等于架构任务完成。

### 7.5 停止条件

coding agent 遇到以下情况必须停止扩张范围并报告：

- 需要改变现有公共 endpoint、SSE event 或 SDK 行为，而工作包未明确授权；
- 需要新增常驻服务、物理数据库或跨服务同步依赖；
- 发现 KB worktree 尚未完成或基线 SHA 不干净；
- 数据迁移无法证明幂等或回滚；
- 为拆文件必须重写业务逻辑；
- 当前代码与本 PRD 快照不一致且会改变架构决定。

---

## 8. 交付顺序与依赖

| Wave | 主 Session 顺序工作 | 合入条件 |
| --- | --- | --- |
| 0 | RAG 验收、提交、合入；建立唯一 post-RAG base | main clean；RAG real UI/DB/data gates accepted |
| 1 | ARC-00A/B/C 事实、可信 gate、successor ADR、构建经济性 | 假绿探针失败；基线/owner可机器读；单一程序 |
| 2 | ARC-01 Assistant/Agent API facade 与共享 entry services | OpenAPI/SSE/import/错误合同 parity + review |
| 3 | ARC-02 control/model/ResolvedLaunch/Capability V2/Rust HTTP | 跨入口/跨语言/性能/recovery parity + review |
| 4 | ARC-04A persistence/data-access owner 清理 | 跨域 SQL 已归 owner；inventory 可生成 grants |
| 5 | ARC-03A runner/ledger/schema convergence | 单 authority；runner crash/checksum/多来源 gates |
| 6 | ARC-03B roles/grants/adoption/baseline freeze | 所有来源 fingerprint 一致；正负权限/rollback通过 |
| 7 | ARC-04B 其余具名 core/contracts 批次 | allowlist/最小 wheel/import parity |
| 8 | ARC-05 durable jobs、health/auth/trace | 断连/重启/故障注入终态正确 |
| 9 | ARC-06 topology/compact/scale/Services | adoption资源门；worker scale；single-instance guard |
| 10 | ARC-07 repository quality/evidence backlog | hygiene gate复核；active truth唯一；证据可移植 |
| 11 | ARC-08 manifest、全量回归、rollback、fresh environment | 全部 required gates/真实旅程零意外 skip |

不启动并行 Wave。任何 Wave 失败时回滚或 forward-fix 该 Wave 的提交，不恢复 Python Agent loop，也不让
旧新数据 owner 在无兼容窗口/账本的情况下同时接写流量。

---

## 9. 风险与缓解

| 风险 | 影响 | 缓解 |
| --- | --- | --- |
| 把拆文件误做成重写 | 高 | facade-first；同一提交先移动再最小整理；行为测试先于重构 |
| 绿色门禁实际检查 0 文件或意外 skip | 高 | ARC-00 先修 gate truth；记录文件/用例/skip 数；release 零意外 skip |
| 公共 API/SSE 漂移 | 高 | 冻结 snapshot/fixture；每个相关提交执行合同测试 |
| DB role 导致隐式查询失败 | 高 | 显式 schema；角色矩阵；fresh/upgrade 真 PG；分阶段收紧 grants |
| 重置 migration 编号破坏旧库升级 | 高 | 新 epoch 内重置；legacy 不可变；fingerprint 等价后只写 adoption marker |
| 手工拼接 `init.sql` 与真实升级结果漂移 | 高 | 从完整迁移后的空库稳定导出；第二空库 fingerprint 对拍 |
| migration 101 破坏 N-1 binary rollback | 高 | 真实规模锁/回填测试；声明 rollback class；不通过则 restore-required |
| runner/search_path 让同一对象落入不同 schema | 高 | schema convergence inventory；SQL显式 owner；多来源 relnamespace 对拍 |
| core 移动形成循环依赖 | 高 | 先生成 import graph；一个领域一批；contracts 保持无 I/O |
| Capability Worker 故障拖垮基础聊天 | 高 | core/capability readiness 分离；Runtime fail closed 到具名工具结果 |
| 单一大 Session 范围过大导致失控 | 高 | 顺序 package、checkpoint、loop-state、独立只读 review；一次只 active 一个包 |
| 并行 reviewer 误写主 worktree | 高 | reviewer 只读；所有补丁由主 Session审核并应用 |
| compact 模式掩盖 worker 竞争问题 | 中 | scale 模式仍是发布门；多副本 durable claim 故障注入 |
| 长任务绑在 HTTP 请求上超时/断连 | 高 | 202/job_id durable state；断连/重启/取消/幂等 gate |
| Gateway/Runtime 盲目 scale | 高 | 当前 single-instance manifest；scheduler lease/thread affinity 先于扩容 |
| 清理误删动态入口或证据 | 高 | 三类候选；consumer gate；oracle/manifest 检查；dry-run窄清理 |
| 默认镜像只在开发机 arm64/local 可用 | 高 | Runtime+Worker 同 revision 多架构发布；fresh-machine quickstart |
| 为减少跳数复制 provider/策略逻辑到 Rust | 高 | 明确非目标；保留 Gateway model owner；收益实验另立项目 |
| 历史 runbook 被误当当前指令 | 中 | ARC-00/07 统一 active 状态；索引只保留一个下一步入口 |
| 内部状态页泄露部署信息 | 高 | admin-only；响应 allowlist；禁止路径、IP、DSN、token、原始堆栈 |

---

## 10. 完成定义

本 PRD 只有在以下事实同时成立时完成：

1. Rust 仍是唯一 Agent kernel，Python 只承担明确的 Gateway/Knowledge 责任。
2. Assistant/Responses/Studio/Published 使用统一 `ResolvedAgentLaunchV1`；Assistant、Agent API、control/
   model 与必要 Rust HTTP 热点按 owner/use case 拆分，公共行为未变。
3. Gateway self-HTTP catalog、Capability V2重复投影和无效 fallback/preflight 已按合同收敛，没有新增服务跳数。
4. 真 TypeScript/OpenAPI/import/changed-path/CI gate 会对假绿、零文件和意外 skip 失败。
5. KB 后 baseline、legacy 旧库认领、单一 migrator 与 PostgreSQL 分角色已经在
   fresh/upgrade/fingerprint/backup-restore 中证明。
6. contracts 与领域实现边界有机器 allowlist，`ai-gateway-core` 不再是无边界共享业务包。
7. durable job、core/capability 健康、现有 v2 服务身份与 trace 可在断连/重启/故障注入中解释真实失败。
8. compact 只有过资源/p99采用门才启用；worker scale 可用，Gateway/Runtime 保持诚实 single-instance；
   管理员从现有 Services 面理解职责和降级。
9. Post-RAG manifest 中 `confirmed_dead` 全处置；changed paths 不新增空/自证测试、无主依赖或死文档；
   其余动态候选有 owner/证据需求/backlog；永久证据可移植，超大文件 no-growth。
10. compatibility manifest 对应真实多架构发布制品与各服务 `/version`；全量门禁、fresh quickstart、真实
    UI/模型/Knowledge 和分层回滚均通过。
11. 历史架构计划已明确 superseded/archived，不再存在两个可执行真相。
12. 所有 package 提交经过独立只读 review，候选 worktree 干净且 merge/push-ready；只有用户明确授权后
    push。未执行项没有被写成通过。
