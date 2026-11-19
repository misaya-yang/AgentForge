# 一期 Gateway / Eval 实现合同

本文件记录 P1-04/05/06 的代码合同。完成状态仍以 `deploy/runbooks/agent-platform-vnext/loop-state.json` 为准；单元测试不代替 Docker、PostgreSQL 迁移和真实 Runtime 验收。

## 模型和价格

`llm_models` 的 tenant/provider/model 组合行拥有租户价格。模型创建、编辑和启动同步钩子不再把租户价格复制到全局 `model_pricing`。省略 provider 只允许唯一模型，歧义必须先选择 provider。编辑和删除也先确定唯一 provider，再用完整组合键写入。

价格快照保存租户、provider、model、USD、每千 token 单位、精确 Decimal 字符串和内容 SHA256。Runtime 使用启动时的价格快照；usage recorder 校验身份和版本，并在累积用量结算时保留首次价格。缺少租户模型时只能显式退回内置目录，不能读历史全局租户镜像。

Eval 入队冻结 candidate/judge 的 ModelRef（组合身份、capability revision、price version）。启动时版本变化返回 409；不会静默换 provider。judge 使用 `/v1/responses`，固定 `tools=[]`、`tool_choice=none`、`parallel_tool_calls=false`、温度 0、最多 512 输出 token。judge 不可用或结果无效属于 infrastructure review；composite 也不能把它变成普通语义分数。

## 容量和重试

Dispatcher 同步与流式调用和透明代理复用 CapacityResolver/CapacityAdmissionController。注册服务的显式旧并发限制仍被读取。run、provider、capability、job、SSE 使用不同资源键；父 run 等待 callback 时，callback 不会申请同一父 run 配额。

Redis 租约在 TTL 内续期，失去所有权会取消执行 owner。取消中途申请或释放时，已取得的局部、租户和共享配额必须完成清理；SSE 交接到消费 task 时重新绑定 owner。共享资源不再因为请求使用 stream/sync 而复制预算。run watcher 从本 turn 的起始 cursor 回放，只有真实终态或已确认中断才能释放；无终态 EOF、未确认中断或 turn POST 响应丢失时保留待核对的配额，后续真实终态可以回收。私有模型入口在发送 200 响应头前完成 admission，拒绝返回实际 429/503；未 dispatch 的 reservation 进入 failed，已 dispatch 的不确定结果进入 unknown。上线必须停止旧格式容量键的 Gateway 实例后再启动新版本。

只有显式配置的上游去重合同与可重放请求体同时存在，Idempotency-Key 才允许 mutation 重试。写后超时或断链属于 `UPSTREAM_OUTCOME_UNKNOWN`，不自动重放。普通 GET/HEAD/OPTIONS 保留有限重试。Runtime 的模型重试仍由 Runtime 申请新 reservation。

## Eval 恢复与取消

SQL epoch 002 增加 outbox owner、claim token、lease、heartbeat 及 case Runtime handle/dispatch state。每个可用 worker 只 claim 一项；过期项可恢复，超过尝试上限的过期项进入终态。每次域写入与 claim 验证处于同一事务并锁住 outbox 行，覆盖 trace、score、case、run 和终态。ContextVar 隔离并发 worker 的 claim。

发起 turn 前先保存 dispatch 标记，返回 turn 后立即保存句柄，然后才订阅 SSE。重启后重连原 turn；完整事件回放重建轨迹，持久 cursor 作为已观察水位。若 POST 结果丢失，尝试用已有 Runtime snapshot 找回原 run；无法确定时要求 reconciliation，不盲目再次执行。旧版 running case 缺少可靠句柄时迁移为 `reconcile_required`。评分沿用现有稳定 score identity/upsert。

`POST /api/v1/eval/experiment-runs/{run_id}:cancel` 要求既有 Eval run 权限且限制在认证租户。先原子取消 job/run，阻断旧 owner 写入，再按保存的 candidate 身份中断 Runtime。未确认的中断计入 `runtime_interrupt_pending`，接口可重试。为兼容既有 case 状态约束，被取消的未完成 case 存为 skipped，同时记录 `observed_metrics.execution_outcome=cancelled`。

应用 epoch 002 前必须停止旧 Eval worker；不得用旧版本 reader/writer 继续运行。数据库指纹和 rollback floor 由 epoch authority 的真实验收产生，不从代码推测。

## 验证入口

使用 `uv run --all-packages --extra test pytest -q --no-cov` 运行以下有界测试组：

- `tests/services/llm/test_model_service_pricing.py`、`tests/services/test_scoped_price_snapshot.py`、`tests/services/assistant/test_launch_resolution.py`。
- `tests/core/test_dispatcher_usage_recording.py`、`tests/proxy/test_capacity_lease_lifecycle.py`、`tests/packages/ai_gateway_core/test_retry_policy.py`。
- `tests/services/eval/test_evaluator_executor.py`、`test_outbox_worker.py`、`test_eval_lease_fencing.py`、`test_candidate_handle_recovery.py`、`test_eval_llm_client.py`。
- `tests/api/test_eval_run_cancel.py`、`tests/api/test_responses_ingress.py`。

新增反例覆盖跨租户/跨 provider 同名价格、运行中改价、ModelRef 漂移、取消部分申请、取消清理、跨 TTL 续租和失租、流消费者关闭、未知 mutation 不重放、旧 Eval owner 写入拒绝、运行句柄恢复、同租户凭证换主体以及取消确认状态。PostgreSQL 真实并发锁、kill/reclaim 和 Runtime 端到端中断仍由整栈验收确认。
