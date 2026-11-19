# 权限档位研究：当前实现与原生 Codex 对应

2026-09-22；只读代码研究和内置浏览器观察，未开放新权限。上游固定为 `279ba894152b2c01c5294cc0723b463b209bdca4`。

结论：平台已经具备工具授权、人工审批、Worker 隔离和租户执行边界，但没有接通截图中的三档统一权限。当前 Web 固定 `safe`；“自定义”弹窗是回复风格，与权限无关。

| 用户选项 | 原生 Codex 组合 | 平台目前状态 |
| --- | --- | --- |
| 请求批准 | OnRequest + User + workspace permission profile | 有逐次人工审批，但平台固定 read-only；write/unknown 工具逐次申请，不等同于桌面默认工作区可写 |
| 帮我批准 | OnRequest + AutoReview + workspace permission profile | 上游已有 Guardian 风险与用户授权审核；平台 managed profile 关闭 Guardian，动态工具 broker 尚未连接 |
| 完全访问 | Never + disabled Codex sandbox | 平台未提供。Web 产品应定义为租户已授权资源范围内免逐次人工确认，不能借此扩张租户资源或本机授权 |

上游依据（路径相对 `opensource-harness/codex-harness`）：`codex-rs/utils/approval-presets/src/lib.rs:40`、`codex-rs/tui/src/chatwidget/permissions_menu.rs:89`、`codex-rs/protocol/src/config_types.rs:175`、`codex-rs/ext/guardian-reviewer/src/assessment.rs:11`。这是目标源码的机制映射，不声称截图所用桌面客户端的具体版本已经核验。`Never` 单独只表示不请求批准，不会自动关闭沙箱。

当前平台证据：

- `web/src/pages/assistant/index.tsx:762` 固定发送 `execution_profile: safe`。
- `src/api/v2/agent.py:119` 明确拒绝非 safe；V1 `src/api/schemas/assistant.py:218` 仍声明 safe/balanced/power，但 `src/api/v1/_assistant_routes/chat.py:103` 不把该字段传入 Runtime，因此并未切换权限。这是两个入口的未对齐合同。
- `src/services/agent_runtime/control/thread_lifecycle.py:273` 固定 on-request/read-only。
- `rust/agent-runtime-overlay/kernel-rs/ai-platform-agent-runtime/src/tool_policy.rs:206` 关闭 Guardian；ToolPolicy 控制工具可用上限，独立于审批频率。
- 同 crate `src/http_service/capability_dispatch.rs:340`、`src/approval_control.rs:106` 为写操作创建独立审批并绑定参数、run、call 与10分钟有效期；`database/migrations/096_agent_capability_executions.sql:418` 在最终执行处校验并原子消费。
- 本候选 IAB 已观察 Python print(6*7) 审批通过后返回42、单独拒绝按钮和长任务取消；数据库执行证据在本次验收报告中单列，不把界面点击直接当成全部边界证明。

建议复用原生权限配置和 AutoReview，不另写一套风险分类器。增加一个会话级配置，由 Gateway 认证后解析、持久化并冻结到每轮快照；原生工具和平台动态工具都消费同一有效配置；执行账本保留人工、自动审核或策略批准的来源。界面可保持截图的三档交互，但“完全访问”的说明必须明确作用于当前授权工作区与工具。切换档位不能扩大 ToolPolicy、租户权限或 Local Node grants。

最小落地需同时打通 Web → V1/V2 → Runtime → broker/Worker，并统一现有 safe/balanced/power 的明确映射或拒绝行为。仅增加下拉框、修改 approvalPolicy 或打开 Guardian feature flag 都不能完成端到端功能。当前只完成研究，未实施上述新产品能力。
