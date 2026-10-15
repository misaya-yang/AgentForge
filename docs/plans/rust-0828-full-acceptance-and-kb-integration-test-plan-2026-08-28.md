# rust-0828 全面验收与知识库 worktree 集成测试计划（2026-08-28）

> **状态:** archived — 验收已完成；本文是历史执行计划与证据索引，不得重新执行。

## 1. 目标与边界

本计划先证明当前 `main` 上的 `rust-0828` 结果可发布，再给仍在开发中的
`worktree-kb-rag-upgrade` 固定后续审查与合并顺序。

- `rust-0828` 的本轮增量是 PPR-00：模型面时序拆分、TTFT 基准驱动、并发资源
  驱动及其证据；它不是 PPR-01～PPR-09 的后续拓扑实现。
- 为防止 PPR-00 破坏已经完成的 Rust Assistant cutover，验收同时覆盖单内核、
  V1/V2 投影、只读能力、隔离边界、SDK SSE 和真实浏览器主流程。
- 真实 UI 不能只测单点问答；必须覆盖流式、多轮、刷新恢复、中断恢复、长输出、
  模型选择和至少一条能力/工具链路。
- 知识库 worktree 尚未完成，本轮不编辑、不提交、不变基；只记录合并前必须满足的
  门禁和提交切片。

## 2. 当前基线

| 项目 | 当前事实 | 本轮处理 |
| --- | --- | --- |
| Rust 验收基线 | `main == rust-0828 == 73b1c27`；相对 `origin/main` 前进 3 个提交、37 个文件 | 在该 SHA 的工作树上验收 |
| 邻接修复 | `audit_event_writer.py` 与对应测试有 2 个未提交文件 | 独立验证；不得混写进知识库提交 |
| 知识库 worktree | 基于 `7a7fc3a`；41 个 tracked 修改、30 个 untracked 文件，仍被 Claude session 锁定 | 只读审查，不干预 |
| 路径冲突快照 | Rust 37 个提交文件与知识库当前 71 个脏文件交集为 0 | 机械冲突风险低，语义/迁移风险仍高 |

## 3. rust-0828 完成标准

以下条件全部满足才判定通过：

1. PPR-00 四组核心测试全绿：timing、model-plane、TTFT driver、resource driver。
2. G1～G5 仍成立；尤其是三段时序可加和、延迟归因不串位、取消流不留伪证据、
   报告遇到缺失身份/样本/内存限制/容器重建时 fail closed。
3. Rust release gate、Assistant runtime、隔离、SDK SSE、harness 全绿；公共 API、
   DB schema 和事件字节没有因本轮增量漂移。
4. Docker Compose 所有权属于仓库根目录；容器内源码与当前工作树一致；9 个服务
   健康且无异常重启。
5. 使用真实 `qwen3.8-flash` 完成多场景 UI 回归；响应可见、流式终态唯一、刷新后
   会话可恢复、浏览器控制台和服务日志无新增错误。
6. 性能结论只按冻结口径判断：本地 pre-provider p95 不超过 25 ms；单次 UI
   观测只作 smoke，不用一次供应商延迟推翻或重写 107-trial 基线。
7. 若发现缺陷，最小修复后重跑直接测试、所属门禁和最终运行时 smoke。

## 4. 执行矩阵

### A. 工作树与静态完整性

| 检查 | 方法 | 通过条件 |
| --- | --- | --- |
| 范围 | 审查 `origin/main...main` 和未提交 diff | 无无关重构；邻接修复保持独立 |
| Patch 健康 | `git diff --check` | 无空白错误 |
| Python 静态检查 | 对 6 个 PPR/审计实现与测试文件运行 Ruff check、format check | 全绿 |
| 公共合同 | 检查本轮未改 `sdk/openapi.json`、公共 event schema、数据库迁移 | 零漂移 |
| 证据可追溯 | 解析 4 份 TTFT、resource report 与 samples | JSON 可读、身份/样本/配置指纹齐全、无凭据字段 |

### B. PPR-00 定向测试

运行：

```bash
uv run --all-packages --extra test pytest -q --no-cov \
  tests/services/agent_runtime/test_timing.py \
  tests/services/agent_runtime/test_model_plane.py \
  tests/scripts/test_assistant_ttft_benchmark.py \
  tests/scripts/test_ppr00_resource_profile.py
```

重点不是用例数，而是下列失败面均被命中：

- Responses V1 与 Chat Completions 两条 wire 的 pre/wait/projection 归因。
- fake clock 精确恒等式、真实时钟舍入边界、G3/v2 p99/max/lower-bound。
- tool-only、refusal、函数参数事件、取消流、重复日志、错模型/错 run identity。
- warm-up 排除、缺失/畸形 receipt、existing-output 拒绝、统计法确定性、报告脱敏。
- 9 容器 owner、memory limit、Docker CLI timeout、容器 ID/restart 漂移、零样本、
  wall-clock/max-call cap、429 不重试、错误分类与报告落盘。

### C. Rust 全栈非回归门禁

按从快到慢顺序执行：

```bash
make harness-check
make verify-assistant-runtime-dev
make test-isolation
make sdk-sse-contract
make agent-runtime-release-gate
```

`agent-runtime-release-gate` 是最终离线汇总门禁；前面的单项命令用于保留清晰失败
归因。Java/Dart 工具链若机器缺失必须记为 skipped，不能写成 passed。

### D. 容器新代码与健康

1. 先运行 `make doctor`，再检查 `docker ps`、Compose 状态和
   `com.docker.compose.project.working_dir`。
2. 源码/前端变更使用 `make hot-update ARGS="--all"`，不用无必要的全镜像重建。
3. 运行 `make validate` 和 `make status`。
4. 对 gateway 中本轮 Python 文件计算 host/container SHA-256；前端以构建时间和
   实际静态资源为准；确认 agent-runtime 使用 Compose 指定且 pin 校验通过的镜像。
5. 记录服务 health、容器 ID、restart count；测试结束再次比较，任何非预期重建或
   restart 都失败。

### E. 真实模型与 UI 多场景回归

模型固定选 `qwen3.8-flash`，测试账号只在执行时读取。测试会话使用新建线程，避免
污染既有业务数据。

| 场景 | 操作 | 核心断言 |
| --- | --- | --- |
| E1 模型/流式 | UI 选择 Qwen 3.8 Flash，发送确定性短答 | 实际 dispatch 模型和 wire 正确；首 token 可见；终态一次 |
| E2 长输出 | 请求结构化长答，观察多 chunk 流式渲染 | 非一次性假流式；页面不冻结；Markdown/滚动正常 |
| E3 多轮上下文 | 连续两轮，第二轮引用第一轮事实 | 上下文正确；同一 thread/run 关系一致 |
| E4 刷新恢复 | 刷新并重新进入刚才线程后继续提问 | 历史、模型选择、终态恢复；无重复消息 |
| E5 中断再用 | 长输出中点击停止，再发新问题 | 取消只终止当前 turn；无幽灵 running；后续 turn 正常 |
| E6 能力链路 | 执行一个安全、可验证、无破坏性的只读能力/工具调用 | 工具事件、结果投影、活动面板一致；无越权或手工 API 代替 |
| E7 视觉输入 | 上传小型测试图片并询问可客观核对的内容 | 附件可见；模型收到视觉输入；结果正确且刷新后仍可查看 |
| E8 错误恢复 | 触发一个客户端可恢复的校验错误或取消态 | UI 有明确状态；输入不丢；不产生 5xx/卡死线程 |

每个场景同时检查：网络请求状态、浏览器 console、活动/步骤面板、gateway 与
agent-runtime 日志、唯一终态、会话持久化。性能记录分三层：UI 首 token、服务端
model-plane TTFT、三段 timing；供应商 wait 单独列出，不归因给 Rust 本地路径。

### F. 运行时性能与资源 smoke

- 不重复花费 107+82 次调用来“重新证明”已经冻结的基线，除非本轮修复改变时序
  计算或负载驱动。
- 运行小样本顺序 smoke 检查 receipt 完整性与 G1/G3 边界；小样本不产出新的 p95
  结论。
- UI E1～E8 期间观察 `docker stats`、restart count、5xx、超时与内存 guard。
- 如果本地 pre-provider 明显超过 25 ms，先区分冷启动、供应商 wait 和本地残差，
  再决定是否扩大样本；不凭单次总耗时判性能回归。

## 5. 知识库 worktree 后续合并方式

知识库代码完成前不 rebase、不 merge、不要求当前全绿。Claude session 结束后按以下
顺序处理：

1. 先把当前主工作树的审计序列化修复单独提交，得到干净 `main`；不得与知识库
   改动混成一个提交。
2. 在知识库 worktree 先按可审查主题拆提交，每个提交带自己的测试：
   - migrations/schema + 真 PostgreSQL 测试；
   - 配置/依赖/metrics；
   - lifecycle/persistence/worker + recover/retry/并发 fence；
   - retrieval/rerank/telemetry + 质量与预算测试；
   - API 契约；
   - CI/Make/harness/fixtures/docs；
   - frontend 单独一组（当前快照没有 `web/` 改动，不能宣称 H1 UI 已完成）。
3. 每个提交执行对应 KB 门禁；全分支至少执行：

```bash
make kb-unit-gate
make kb-migration-gate
make rag-eval-regression-gate
make validate-example-config
make harness-check
```

4. 增补真实 PostgreSQL + Qdrant + worker 故障注入：二次摄入未变块不重嵌、稳定
   ID、旧向量持续可读、recover/retry 语义分离、双动词竞争无死锁、批量 >200 不
   截断、禁用/恢复/归档/删除无孤儿点或行。
5. 黄金集当前只有 18 条且标记为 `human-review-pending`；它可以验证合同，不能支撑
   PRD 所写的 200～400 条质量结论。先补人工复核与真实 segment 绑定，再冻结质量
   基线；不得先拍阈值。
6. 分支仍未发布且无 upstream 时，在所有改动提交、工作树干净后 rebase 到最新
   `main`；若届时已经共享/发布，则改为 merge `main`，不改写共享历史。
7. rebase/merge 后先重跑 KB 门禁，再补跑 `make agent-runtime-readonly-gate` 和一次
   Assistant→Knowledge UI 链路，证明知识库变化没有破坏 Rust 能力桥。

当前路径交集为 0，因此预期不会出现大规模文本冲突；真正要审的是迁移顺序、队列
所有权、检索语义，以及 PPR-06 与知识库 worker 改动的边界，不能用“Git 合并成功”
代替系统验收。

## 6. 证据记录

执行完成后在最终验收中逐项记录：命令、退出码、通过/失败/跳过数量、容器身份、
UI 场景结果、真实模型、TTFT 分解、日志异常、修复 diff 与未验证项。任何未实际
运行的检查不得标为通过。

## 7. 本次执行结果（2026-08-28）

结论：`rust-0828` 的 PPR-00 增量、既有 Rust 单内核合同和真实 UI 主链路通过。
实机发现并修复了“首个文本 token 前取消后，刷新丢失取消状态”的恢复缺陷。

### 7.1 已通过

- PPR-00 定向：109/109；审计序列化相关：43/43。
- `make harness-check`、`make verify-assistant-runtime-dev`（5/5 groups）、
  `make test-isolation`（27/27）、`make sdk-sse-contract`（Python 3/3、CLI
  10/10）和 `make agent-runtime-release-gate` 均退出 0；修复后串行 release
  gate 再跑一次仍退出 0。
- Python Ruff check/format check、`git diff --check`、frontend type-check/lint/build
  和新增 frontend 状态单测 2/2 全绿。
- Compose owner 全部为当前仓库根；host/container 的 model-plane、timing、
  thread-store、audit writer、frontend index/manifest SHA-256 相等；9/9 healthy，
  restart count 全为 0，memory limit 全部非空。
- 冻结证据复核：run4 为 107/107 success、`recordable: true`、G3/v2 通过；
  resource profile 为 82/82 stream、197 个 raw samples、exit 0；凭据键扫描为空。

### 7.2 真实 UI 与性能归因

使用 `qwen3.8-flash` / Responses V1 完成：确定性短答、12 项长输出、多轮验证码
保持、刷新后继续、写工具审批拒绝、生成中断、中断后继续、`todo_read` 只读工具
成功执行。再次刷新后，成功回答、模型选择、工具结果与取消状态均恢复；浏览器
console warning/error 为 0。

本轮共有 9 次真实模型 dispatch、8 条完整 timing；第 9 次是主动取消，按合同不
产生 completed-call timing。8 条记录的范围：

| 指标 | 观察范围 | 判断 |
| --- | --- | --- |
| local pre-provider | 1.592–4.005 ms | 全部远低于冻结的 25 ms gate |
| provider wait | 4.955–31.824 s | 慢请求主要来自供应商/思考阶段 |
| local projection | 60.8–564.7 ms | 含输出节奏，不作为 PPR-00 本地 gate |
| 短答 UI 首 token | 约 5.5–6.7 s | 页面健康、可交互；受 provider wait 主导 |

工具场景总耗时 61.14 s，包含两次供应商调用（provider wait 31.824 s 与
27.536 s）；不是 Rust 本地 pre-provider 阻塞。运行期 gateway/runtime/capability
worker 的 ERROR/Traceback/panic/5xx 扫描为空。

### 7.3 本次修复

1. 将 PPR-00 已披露的 4 个 Ruff format 遗留机械整理，并在整理后重跑 109 项。
2. 保留并验证模型配置审计 payload 对 Decimal 等 Pydantic 值的 JSON 归一化修复。
3. 修复取消 turn 恢复：thread-store 现在把无 `text_delta` 的 cancelled/run_error
   terminal 事件投影为空 assistant turn，frontend 再从持久化 process summary
   恢复 cancelled/failed/running 状态。新增后端回归和前端状态映射单测，并用真实
   已取消 turn 刷新验证“已取消”恢复。

### 7.4 明确未验证/环境提示

- Java、Dart SSE 因本机未安装 Maven/Dart，明确 skipped；Python/CLI 已通过。
- 视觉上传会把文件传给真实模型，本次请求未单独授权该传输，因此未执行；不影响
  Rust 文本、工具、审批、取消、持久化和 timing 核心结论。
- 主机 Node 为 24，仓库声明 Node 22；type-check/lint/build 实际通过，但仍应在
  CI 的 Node 22 环境复跑。
- Docker Desktop 分配 3 GiB，低于项目建议约 4 GiB；本轮 9 服务健康且无重启，
  但不据此扩展新的高并发容量结论。
