# Agent Runtime 上游 Codex Harness 升级 PRD

- **状态：** queued — `platform-architecture-convergence` 验收并合入 `main` 后启动
- **domain_id：** `agent-runtime-upstream-sync`
- **owner：** 下一轮 Agent Runtime 主实现 Session
- **last_verified：** 2026-08-30
- **当前 Runtime pin：** `93c54bca38996b56d344a2ca65f01627b1953b27`
- **本轮 selected upstream：** `/Users/yang/projects/opensource-harness/codex-harness@94cbbddafc1776d5e377bca1b05932c697e82238`（2026-08-30 session 启动时 fetch `upstream/main` 后固定）

> 这是实现合同，不是完成证据。任务只做一件事：把平台使用的 Codex Runtime 源码整体更新到固定
> upstream 快照，修好平台兼容性，并用编译后的系统、真实接口和 UI 点击完成验收。

---

## 0. 结果定义

本任务只认两个可见状态：

### `RUNNING_CANDIDATE`

- 新 upstream 已成为 composed source 的基础，不是旧源码或旧镜像；
- Runtime 与 Capability Worker 编译并构建成 candidate 镜像；
- candidate 在当前 Compose 中启动并通过健康检查；
- Gateway 可以真实调用 candidate。

### `MERGE_READY`

- 实际后端接口全部通过；
- 编译后的前端可以通过真实点击完成 Agent Runtime 核心旅程；
- DashScope/Qwen 实际模型链路通过；
- 当前平台兼容性没有回归，旧 Runtime/Worker 可以回切；
- 实测发现的问题已修复，分支提交清楚并可合并。

评价标准不是修改文件数、review 次数、finding 数量、门禁数量或文档完整度。只看编译后的系统是否可用，
接口是否正确，UI 点击功能是否正常。

---

## 1. 升级方式：整体同步，不逐提交搬运

### 1.1 固定一个 upstream 快照

Session 启动时核对本地 upstream HEAD，固定 selected SHA；之后不再追逐 upstream `main`。本轮直接以
selected SHA 的 `codex-rs` 源码及 Runtime/Worker 所需 workspace closure 为新基础。

“同步全部更新”的含义是：

1. 使用 selected SHA 的完整上游源码快照，不 cherry-pick 372 个提交；
2. 不逐文件手抄上游实现，不重新审计每个历史 commit；
3. 上游新增 crate、资源和协议依赖只要属于 Runtime/Worker 构建闭包，就随快照一起进入 composed source；
4. 平台未启用的上游功能可以保留在源码中，但不得自动暴露到 Gateway、工具目录或 UI；
5. 平台差异只留在 overlay/extension seam，优先适配上游，不重写一套等价内核。

本地 upstream 仓库是只读来源。不得在 `/Users/yang/projects/opensource-harness/codex-harness` 中提交平台修改。

### 1.2 冲突处理优先级

遇到平台 overlay 与 upstream 冲突时，按以下顺序处理：

1. upstream 已有等价能力：删除平台重复实现，使用 upstream；
2. upstream 已提供 extension seam：把平台逻辑接到 seam；
3. upstream 尚无 seam：在新快照上最小重放平台 patch；
4. 只有实际接口或 UI 行为需要时，才增加兼容 adapter。

不得因为一个冲突重新设计 Gateway、数据库、Capability 系统或整个仓库架构。

---

## 2. 必须保持的平台兼容性

upstream 内核可以更新，但以下平台边界不变：

- Rust Runtime 仍是唯一 Agent loop，不恢复 Python Agent loop；
- Gateway 仍负责用户、租户、模型配置、凭据、配额、计费和政策；
- PostgreSQL ThreadStore 仍是 Thread/Turn/Item 持久化权威；
- Capability Worker 仍负责工具执行、lease、proof 和 approval；
- 现有 Assistant、Responses、Agent Runtime 的公开 OpenAPI、SSE、错误形状和 SDK 行为不漂移；
- 当前聊天、stream、工具调用、审批/拒绝、取消、恢复、历史记录和 Knowledge 绑定继续工作；
- Runtime 容器不读取用户主机的 `CODEX_HOME`、凭据、plugins、hooks 或任意 shell 环境；
- 不因为 upstream 新增能力而新增常驻服务或改变 UI 产品范围。

上游新增 history、settings、tool-output、multi-agent、Guardian、browser、terminal、hooks、plugins、SQLite
等实现可以随源码存在；平台没有现成产品入口时保持未暴露，不为它们另开开发任务。

---

## 3. 单一主 Session 实现

默认由一个主 Session 在当前分支、当前 checkout 顺序完成 runtime-core、app-server、
capability-worker、gateway-compat 和 web-acceptance 五个实现面。不开新 worktree，不把收尾拆成多轮
调查/review，也不让多个 writer 同时修改共享树。

主 Session 统一拥有 workspace/Cargo lock、source lock/receipt、overlay manifest、Compose、程序状态、
Git staging/commit，以及 Docker-contained Rust image build、DB、E2E/provider 的全局执行。只有用户再次
明确要求、且存在真正互斥的独立代码面时，才临时使用少量子代理；最终集成与实机验收始终由主 Session
完成。

---

## 4. 实施顺序

### ARU-1：整体更新 upstream source

1. 核对当前 overlay manifest/source receipt；如有不一致，只做一次窄修复。
2. 固定 selected SHA，更新 source lock/receipt。
3. 以新 upstream 快照物化 composed source。
4. 纳入 Runtime/Worker 所需的新 crate、workspace 配置和 bundled assets。
5. 删除已被 upstream 覆盖的重复 patch；其余平台 patch 最小重放。

本阶段完成物必须是实际 composed source，不是 inventory 或审查报告。

### ARU-2：托管 CI 验证并在 Docker 内编译

按以下顺序运行并修复：

1. 托管 CI 运行 Cargo metadata、fmt、check 和 changed-crate tests；
2. 修复 CI 暴露的 workspace、protocol 和 platform contract 问题；
3. 使用 canonical Docker 多阶段 builder 编译 Runtime/Worker release binaries；
4. 核对 Docker builder 没有向仓库 bind-mount `target`；
5. 产出带 source/image identity 的 candidate images。

宿主机禁止直接运行 Cargo/rustc/rustfmt/clippy 或 Rust check/test/build。`CARGO_BUILD_JOBS=1` 只用于
托管 CI 和 Docker builder 内部。Docker BuildKit cache 可以保留，不自动执行清理。

编译错误按共同根因成批修复。不得围绕一个文件反复 review；同一失败修复后只重跑受影响测试，再继续下一步。

### ARU-3：接回平台并启动 candidate

1. 将 candidate Runtime/Worker 接入当前 Compose；
2. 核对 Compose ownership、source/image identity 和实际运行二进制；
3. 启动 Gateway、Runtime、Worker 及现有依赖；
4. 通过真实 HTTP/SSE 请求确认 Gateway 可以调用新 Runtime；
5. 达到 `RUNNING_CANDIDATE`。

在 `RUNNING_CANDIDATE` 前禁止进行新的全仓 review、仓库清理、文档清理、性能工程或供应链扩建。

### ARU-4：实际接口与 UI 验收

按第 5 节矩阵运行后端、浏览器和 Qwen 实机。发现失败就直接修复，重建受影响服务并重跑失败旅程。

所有必测旅程通过后，只做一次 integrated diff 兼容性复核。复核发现的问题只有满足第 6 节“真实 blocker”
定义时才继续修；其余记录为后续优化，不得重新打开全仓审查。

### ARU-5：回切与交付

1. candidate → frozen Runtime/Worker 回切 smoke；
2. 确认已有 thread/session 仍可读取，工具副作用未重复；
3. 切回 candidate 并重跑一条聊天旅程；
4. 整理实际运行命令和结果，形成清晰提交；
5. 达到 `MERGE_READY` 后停止，不再主动搜索新优化项。

---

## 5. 唯一验收矩阵

Docker、E2E 或 provider 操作前必须先读 `docs/harness/runtime-and-secrets.md`。命令使用实施时
`docs/harness/commands.md` 与 `harness.yml` 已有 canonical gate，不复制第二套命令系统。

| 层级 | 必须通过的真实结果 |
| --- | --- |
| Source | selected SHA、composed source、overlay manifest、source receipt 与 image identity 一致 |
| Rust CI | 托管 CI 的 Runtime/Worker metadata、fmt、check 和 changed-crate tests 通过 |
| Docker build | Runtime/Worker 只在 Docker 多阶段 builder 内编译；仓库没有宿主机 `target` 产物 |
| Container | 从当前分支 Docker 构建的 Runtime/Worker 镜像启动；健康、依赖和 Gateway 联通正常 |
| Public API | 现有 OpenAPI/SSE/SDK fixtures 无非预期漂移；真实 Assistant/Responses 请求成功 |
| Agent API | 创建 thread、发送 turn、stream、读取历史、取消、刷新后恢复均成功 |
| Tools | 普通工具调用、需要审批的 allow/deny、失败工具终态和重复提交保护正确 |
| Provider | 使用当前有效 DashScope/Qwen 配置完成至少一条真实流式回复和一条工具旅程 |
| UI 点击 | 登录、创建会话、发送消息、查看流式输出、审批/拒绝、取消、刷新恢复和历史切换可点击完成 |
| Knowledge | 从 Assistant 选择/使用现有知识库并得到可验证回答；不修改 RAG 算法 |
| Negative | 未认证、越权租户、未批准工具和无效 capability 仍被现有接口拒绝 |
| Browser health | 必测旅程无阻断性 console error、白屏、卡死或异常失败请求 |
| Rollback | candidate → frozen → candidate smoke 通过，已有 session 可读且副作用不重复 |
| Repository | `make harness-check` 及本次触及路径的现有 gate 通过；没有把未跑项写成 PASS |

API 单测或 mock 不能替代真实 HTTP/SSE；静态前端 build 不能替代编译后页面的真实点击；健康接口不能替代
Qwen 实际回复。反过来，矩阵全部通过后也不再为了理论完整性增加新门禁。

---

## 6. 修复循环与停止条件

### 6.1 只修真实 blocker

以下任一项实际发生时必须修复：

- Runtime/Worker 无法编译、链接、构建镜像或启动；
- 当前 Gateway API、SSE 或 SDK 合同出现非预期变化；
- 第 5 节必测接口、UI 点击或 Qwen 旅程失败；
- 数据丢失、跨租户、凭据泄露、授权/审批绕过；
- 工具出现无法解释或重复的副作用；
- candidate identity 不明，或 frozen 回切不可用。

“可能更优”“代码还能更漂亮”“reviewer 认为风险较高”“缺少新的通用 gate”都不是 blocker，除非能通过上述
编译、接口、UI、数据或权限路径复现。

### 6.2 每个失败只走一个闭环

```text
复现真实失败 → 定位根因 → 最小修复 → 受影响测试 → 重跑失败接口/UI → 继续矩阵
```

- 不因一次失败重新审查整个仓库；
- 不对同一成功路径反复启动 reviewer；
- 不在每次修复后重跑全部矩阵；全部局部失败关闭后只跑一次最终矩阵；
- 不使用 blocker/high/medium 这类可无限扩张的主观清单；
- 理论问题和新能力统一记为 follow-up，不阻止 `MERGE_READY`。

### 6.3 明确停止

当第 5 节所有必测项通过、真实 blocker 为零、回切通过并完成最终提交整理时，任务完成。主 Session 必须停止，
不得再进行“最后再审一轮”“顺手清理”“继续完善证据”或重新扫描 upstream。

---

## 7. 明确不做

- 不逐个分析或 cherry-pick upstream 的 372 个提交；
- 不重新设计平台架构、数据库迁移、知识库或前端产品；
- 不把 upstream 新能力全部接入平台 UI/API；
- 不新增常驻服务、第二 Agent loop 或 Python fallback；
- 不建设新的通用审计、receipt、门禁或测试框架；
- 不做全仓死代码、死文档、死测试清理；
- 不要求在本任务中完成多架构发布、SBOM、美化文档或理论最小 fork；
- 不创建额外 worktree；
- 不在宿主机运行任何 Cargo/Rust 编译或测试命令；
- 未经用户明确授权不 push、不合并 `main`、不发布生产。

如果现有 release gate 明确要求某项制品，则完成现有 gate；不得借此扩建一套新体系。

---

## 8. 主 Session 最小执行合同

```text
把固定 upstream 快照整体同步为新的 Runtime 基础，由一个主 Session 顺序完成代码、托管 CI 验证、
Docker 内整合编译和提交。先让 Runtime/Worker Docker 镜像启动，再跑真实 Gateway API、UI 点击和 Qwen
旅程；宿主机绝不运行 Cargo/Rust 构建或测试。
只修编译、接口、UI、数据/权限或回切中实际复现的问题。全部验收通过后整理提交并停止，不再扩展范围。
```

进度只允许报告：

```text
upstream_synced
source_green
candidate_running
api_ui_testing
merge_ready
```

“正在 review”“继续完善门禁”“发现更多优化项”不是有效进度。

---

## 9. 依据

- `rust/agent-runtime-overlay/manifest.json`
- `deploy/agent-runtime-source/lock.json`
- `deploy/agent-runtime-source/source-receipt.json`
- `docs/architecture/ADR-006-agent-runtime-single-kernel.md`
- `docs/architecture/ADR-007-agent-runtime-data-boundaries.md`
- `docs/harness/runtime-and-secrets.md`
- `docs/harness/commands.md`
- upstream range：`93c54bca38996b56d344a2ca65f01627b1953b27..94cbbddafc1776d5e377bca1b05932c697e82238`
