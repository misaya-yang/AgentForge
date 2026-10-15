# Rust Agent 持久执行与重启恢复验收 — 2026-09-26

本包 DR-01 的本地实现、必需验收与最终独立复核已通过，可以收口。仅关闭 AS-03/TL-03 的 Runtime 同 run 重启恢复缺口，不宣称完整 R1 或整个产品完成。

分支 `codex/runtime-durable-recovery`，基线 `92c98c0b85c7b9b72254e5023e6086fd8d4792df`。验收实现阶段未提交或推送。用户现追加授权快速收尾并本地合入 main；保留既有 PRD 和其他工作。唯一代码写入者为主代理，其余代理只读调查/review。

设计：[恢复合同](../../docs/design/agent-runtime-durable-recovery.md)。程序：[状态](../../deploy/runbooks/r1-assistant-closure-2026-09-23/loop-state.json)、[包收据](../../deploy/runbooks/r1-assistant-closure-2026-09-23/receipts/DR-01.yml)。

## 需求—实现—证据

| 要求 | 实现与验证 |
| --- | --- |
| 同 run/turn 恢复 | 私有 host bridge 接通 Core 的原 turn 恢复；不提交新用户输入。SIGTERM 使用 Core suspend，抑制伪造的用户取消输出。 |
| 唯一所有权 | PostgreSQL renewable owner + monotonic fence；model/rollout/tool reserve/dispatch/cancel 受原身份及 fence 检查；过期接管、并发竞争和旧执行者拒绝有实际 SQL 证据。 |
| 审批续接 | 原 run/call/参数/审批 ID/截止时间保存；复用原决定，进程 waiter 只通知。批准与派发事务内重新检查原权限，重启不延长批准或授权。 |
| 结果复用 | Worker 原 execution/call/attempt/dispatch fence 和 Runtime invocation journal 对账；原 FunctionCallOutput 在冷加载前注入 Core，仅一次。 |
| 未知写操作 | 无可靠回执的已派发写操作标记 side_effect_unknown，不重新派发；实际崩溃注入得到 execution=1、Quiz=0，刷新仍明确未知。 |
| 平台边界 | Rust 执行及恢复，Gateway 保留授权/预算/租户治理。新增空 `:recover` 仅唤醒原任务，不允许替换模型、授权、消息或创建 run。 |
| 历史一致 | 冷取消与终态 receipt 同一事务；恢复 started 不产生重复正文；旧 lifecycle-only unknown 也恢复安全提示、原正文与诊断 run ID。 |

## 最终本地发布单元

- 上游：`279ba894152b2c01c5294cc0723b463b209bdca4`；上游 checkout 保持干净。
- 最终 C4 overlay：`22bf4e1ecad56231aadc5bfc5557037b4bc98bc16837ca53fe4ec8645aa40442`。
- Runtime：`ai-gateway-agent-runtime:local-279ba894152b-22bf4e1ecad5`，OCI digest `sha256:d085575ea6eed245f9f59395ef6651a482d72615f23f7c78aed60dccb8184108`。
- Worker：`ai-gateway-agent-capability-worker:local-279ba894152b-22bf4e1ecad5`，OCI digest `sha256:0c449319307187e052d82bed40bb63b27d619da4a730144475a8e768d655dc0e`。
- 本地 authority epoch 4 已由 `make migrate` 应用；此前先停止旧 Runtime/Worker。新表和函数为增量，不重做数据库。
- Compose owner 为本仓库。Docker 3 GiB；构建/测试串行、jobs=1，仅在 Docker 内运行 Rust。未执行 prune、数据删除或 host Cargo。
- Gateway 已通过 `make hot-update ARGS="--gateway"` 同步 C4 Python，并按 lock 重建 Runtime/Worker 容器。前端本轮无源代码修改；此前 `--all` 已更新已有 R1 bundle。

构建、热更新日志在 [logs](../runtime-recovery/logs/)；最终运行身份另记录在 [release-unit.json](../runtime-recovery/release-unit.json)。

## 自动化与隔离数据库验证（实际执行）

| 检查 | 结果与边界 |
| --- | --- |
| 受影响 Python 契约 suite | **200 passed**，C4 最终重跑；[日志](../runtime-recovery/logs/python-final-c4.log)。包含 fence/model/control/capacity/history/V2/authority/source。 |
| history 定向回归 | **13 passed**；与200重叠，不相加。 |
| 实际 PostgreSQL 两角色矩阵 | **2 passed**：默认 ai_gateway_、自定义 p1ref_；[日志](../runtime-recovery/logs/db-final-history.log)。合成隔离 DB：竞争 claim、过期接管、旧 fence拒绝、immutable response、原call复用、cancel/revoke dispatch拒绝、run-first锁竞争、Quiz Worker实际权限。 |
| 实际 history SQL | 上述两 DB 均覆盖冷取消/重复取消/回执失败回滚/迟到失败/后续成功、同run两次started的fallback与Core正文只一次、different run相同正文保留、旧unknown无compat事件仍明确未知。 |
| baseline/reference | 在隔离新 DB 验证四个冻结 baseline hash，epochs3/4实际 apply+verify；[reference](../runtime-recovery/reference-database.json)。DR-00A 仅修增量 epoch provenance，冻结 baseline SQL/政策未改。 |
| C4 Docker Rust tests | **160 passed，2 ignored**：Runtime78、Worker80、Core原turn/modelmetadata与suspend各1；[日志](../runtime-recovery/logs/rust-tests-c4.log)。两个忽略项是原 opt-in PG fixture，未冒充通过；PG证据来自上述独立矩阵。 |
| 规范检查 | 受影响 Ruff、diff-check、architecture-boundary（607文件零违规）、source-contract（18 passed）、OpenAPI（2 passed）、harness、最终 validate/status 全部通过；日志在 reports/runtime-recovery/logs。pytest 的 Starlette 弃用、OpenAPI 的已有重复operation-ID、validate 的本地bootstrap密码配置警告、harness 旧程序schema/init警告保留；未运行 hosted CI。 |

可复现命令遵守 AGENTS：Python用 `uv run --all-packages --extra test pytest -q --no-cov <报告对应路径>`；Rust用 `AI_PLATFORM_AGENT_RUNTIME_SOURCE=<干净固定上游> make agent-runtime-recovery-tests`，不在 host 运行 Cargo。

## 受控崩溃矩阵（真实 Qwen + 真实 Worker，含明确注入）

以下10种场景实际通过。六个成功窗口均沿用原 run/thread/model/lease/snapshot/context hash；原 call/output各1，完成1、aborted0；同一 execution/attempt且Quiz仅1。拒绝、取消、撤权均零执行。未知写操作 execution仅1、Quiz0，没有未知重试。每个 case 含 before/after/result 安全 JSON；重复 active/terminal `:recover` 后该线程仍只有原 run。

C3 `b9799e5c9478` 验证执行合同；C4 的最后修正仅是未知事实投影与历史去重，已重跑 Rust、实际 SQL、未知崩溃及真实浏览器 SIGTERM。没有把C3全部窗口写成C4重新执行。

| 崩溃/控制窗口 | 镜像 | 原 run | 结果 |
| --- | --- | --- | --- |
| 原调用保存后、审批请求保存前 | C3 | a7359446-1d4e-405f-af9a-6e629d4d3239 | [PASS](../runtime-recovery/cases/DR-CRASH-invocation_saved-1dda8bd2/result.json) |
| 待审批请求保存后 | C3 | e62aa047-8d8b-4912-bb88-821d6d5a5555 | [PASS](../runtime-recovery/cases/DR-CRASH-approval_saved-150336bb/result.json) |
| 批准决定保存后、Worker派发前 | C3 | 20d6c1bc-a741-49fb-b21c-559164949a82 | [PASS](../runtime-recovery/cases/DR-CRASH-before_dispatch-40143e99/result.json) |
| Worker完成并有真实回执、Runtime journal写入前 | C3 | 37eb1d16-3ff9-4673-9650-a1ffd63ae2d7 | [PASS](../runtime-recovery/cases/DR-CRASH-worker_result_observed-4e1f4526/result.json) |
| Runtime结果journal保存后、Core输出写入前 | C3 | 7b212ea1-3d65-4255-965d-98db6007c30b | [PASS](../runtime-recovery/cases/DR-CRASH-invocation_result_saved-1ea88433/result.json) |
| Core原call输出保存后、模型继续前 | C3 | e430bd60-0d36-4d34-beb6-8385e41e79b0 | [PASS](../runtime-recovery/cases/DR-CRASH-core_output_saved-1a193e42/result.json) |
| 重启后拒绝原审批 | C3 | b5941196-7b8e-4cbf-acd3-70d0da3951ed | [PASS](../runtime-recovery/cases/DR-CRASH-reject-c49b7046/result.json) |
| 重启后冷取消、重复取消 | C3 | 3bda6873-bd1f-4a58-aab9-d3e9c8c9b46a | [PASS](../runtime-recovery/cases/DR-CRASH-cancel-94a0e225/result.json) |
| 重启后撤销原snapshot授权 | C3 | c91efcd7-3c8a-4181-a7f6-e11d2e7a6d53 | [PASS](../runtime-recovery/cases/DR-CRASH-revoke-f5a42bfc/result.json) |
| 写动作派发后无回执，同时崩溃Runtime与Worker | C4 | 853fd8e3-4ef2-4f37-8126-4d0fe1964c1a | [PASS](../runtime-recovery/cases/DR-CRASH-worker_unknown-4bf113c4/result.json) |

故障注入：private AGENT_HOME 内的一次性 dev/debug hook，仅命中特定 DR-CRASH-* Quiz title；在 release build 完全无效。各窗口用 SIGKILL（compose restart -t0）；未知窗口用事务表锁阻止 Quiz 写入后同时崩溃 Runtime/Worker。没有模拟 provider 返回或替换 Worker。注入文件已清除，测验、回执及历史保留。

最终代码实际查询10场景 history：各一个 assistant message，取消/失败/未知分类正确；C4未知有且仅一个 compat unknown事件。[对账结果](../runtime-recovery/final-history-reconciliation.json)。

## 内置浏览器真实链路（最终 C4）

- 从新会话发送真实 Qwen3.8-flash generate_quiz 请求，看到实际动作/目标/参数/原600秒期限。
- 未批准时独立 SIGTERM 重启 Runtime；刷新后原审批可继续，剩余期限递减为536秒，没有重新授权或自动工具执行。
- 原 run `ce285f59-1723-4a16-88b0-b88a914f5e44`、thread `01a0dc91-337a-7d82-afc5-e8ea25dff368`、lease `c2a42d9a-d027-48dc-8941-1ec938a70ced`、snapshot `4f83a5b3-6b60-470f-972f-c54b646927ec`、approval `01a0dc91-47a3-7690-a690-9425d45f21d5`保持原值；owner更换、fence1→2。
- 通过原审批后原 run succeeded；execution `01a0dc92-e0a7-77e1-bf1c-06fe95207cec`仅1，真实Quiz仅1。Core started2表示续接，complete1/aborted0/call1/output1。
- 页面看见真实“DR-C4 原任务重启续接”测验、开始作答控件和成功正文。刷新与离开后重新打开历史保持；后台完整facts与完成时逐字段相同，run/执行/Quiz均没有增加。
- 最终C4页面还检查了旧unknown fixture：只一份正文、系统“结果未知”、核实目标系统的指引与安全run ID；冷取消仍“已取消”，旧取消经重复停止补齐receipt后在后续成功轮之前正确保留。

[浏览器前后安全facts与结论](../runtime-recovery/c4-browser-result.json)，同目录 before-restart/rebound/completed/after-refresh-history JSON。

截图仅放tmp：[原审批](../../tmp/runtime-durable-recovery/c4-pending-approval.png)、[重绑定](../../tmp/runtime-durable-recovery/c4-rebound-approval.png)、[完成](../../tmp/runtime-durable-recovery/c4-browser-completed.png)、[重开](../../tmp/runtime-durable-recovery/c4-history-reopened.png)、[未知](../../tmp/runtime-durable-recovery/c4-unknown-history.png)、[取消与后续成功](../../tmp/runtime-durable-recovery/c4-cancel-before-later-success.png)。

## 独立 review、实际发现与修正

独立 reviewer `r1_hard_gate_review` 只读检查最终差异、权限/并发/幂等、Core原身份以及安全证据，不启动DB/Docker/provider。主代理完成所有改动与复测。

已修正：
1. Worker owner检查与reserve/cancel mutation的TOCTOU、revocation/dispatch锁序、已派发写的取消分类。
2. Gateway capacity watcher把Runtime SSE EOF当作取消并撤销lease；现在保留原cursor和单一TTL重连，真正4xx/撤权/用户取消不重试。
3. 原approval重投影新timestamp触发immutable conflict；相同事实返回原sequence，语义变化仍拒绝。
4. 冷取消缺少历史receipt；取消与receipt同一事务，重复停止只修复receipt，不新执行。
5. 最终浏览器发现同run重复started重复正文、unknown被显示“已完成”；修复run边界和Rust/历史未知投影，实际SQL与最终浏览器复测通过。

最终独立复核 **PASS**，无确认剩余 blocker/high/medium；reviewer 逐字段比较浏览器前后事实及 C4 未知结果、核对镜像和日志。见[独立review记录](../runtime-recovery/independent-review.md)。

## 失败记录、未验证项与支持边界

- 第一轮正向approval_saved fixture被Qwen额外加上不允许的 `option` 字段，Worker拒绝生成，无Quiz、无重复工具。记录为 FAILED_POSITIVE_FIXTURE，**不计通过**；保留原批准参数，另用合法原参数任务通过该窗口。
- 第一次worker_unknown脚本误等待已消费的pending approval，人工终止其等待；产品已保持unknown。记录 INTERRUPTED_HARNESS_WAIT，**不计通过**；修脚本后C3/C4分别通过。不是产品自动重试。
- 早期C1/C2真实浏览器曾失败：capacity EOF撤lease、approvalreceipt conflict与取消历史缺失，均保留失败事实，修正后C4重新完成真实同run续接。
- hosted CI、生产、多Runtime的SSE affinity、Gateway与Runtime同时重启、每一种provider/工具的逐一真实崩溃、完整镜像回滚实机没有执行；不据此宣称通过。
- 本次支持有持久上下文的原root动态能力调用。旧run无durable上下文、无法证明安全的native/custom/local-shell调用及活跃descendant恢复失败关闭，不猜授权，不自动重放未知写。
- 原批准/模型lease/snapshot过期或撤销后不会延长恢复权限。取消不宣称外部副作用回滚或资源全部已回收。
- R1其余未验证J05/J06/J07/J29等旅程沿用阶段报告，本轮不扩展知识生命周期、计费或三档权限。

## Git

[验收阶段 Git 快照](../runtime-recovery/git-final-status.txt)保留。用户追加授权本包提交并本地合入main；只提交本包代码、设计、程序状态和验收证据。先前PRD、`docs/README.md`及其他线程架构规划文件保持原工作树内容，收尾结果另记。


## 本地 main 合入收尾

用户追加授权后，恢复包在 `codex/runtime-durable-recovery` 提交为
`91eadb927a8e0fa02eac39fc1657470bcf84ff73`，本地 `main` 已 fast-forward
合入。独立范围审计无新增阻塞。合入前再次运行受影响 Python **200通过**、
architecture/harness、validate/status与运行镜像身份检查；合入后实际 PostgreSQL
两角色矩阵 **2通过**、来源契约 **18通过**。这些是定向合并检查，未冒充再跑完整
Rust/浏览器崩溃矩阵；原 C4 代码和运行镜像一致，没有功能代码追加修改。

本包日志已显式纳入 Git，移除 ANSI 颜色和行尾空白的规则见 logs/README。
11个既有未提交文件逐个哈希对比保持不变：产品PRD、`docs/README.md`、PRD review
报告和其他线程的架构规划继续保留，未顺带提交。无远程推送、历史重写、分支删除或
数据清理。DR-01 收口；完整 R1 的其他未验证旅程仍保留。
[本地合入事实](../runtime-recovery/local-main-integration.json)。
