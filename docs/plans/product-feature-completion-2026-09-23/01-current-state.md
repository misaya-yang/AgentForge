# 01｜现状、证据与模块判断

本轮为源码和历史记录分析，没有启动 Docker、调用模型或重新进行浏览器实测。基线为 `main@2ed0345f`。下列“验证”均附日期和范围；9月7日旧候选结果只能作为参考，不能转写为9月22日目标内核的全量通过记录。

## 1. 证据索引

| 编号 | 依据 | 本次可支持的结论 |
| --- | --- | --- |
| E01 | [Web 路由](../../../web/src/router.tsx) | 当前主要入口、公开页、路由权限和 Studio 开关 |
| E02 | [9月22日本地验收](../../../reports/architecture/codex-harness-refresh-2026-09-22/implementation-verification.md)、[IAB回执](../../../reports/architecture/codex-harness-refresh-2026-09-22/iab-acceptance.json) | 对话/历史、Python审批、run取消、KB检索/引用、修正后的冷恢复等有限主流程通过 |
| E03 | [9月7日本地验收](../../../reports/architecture/phase1-local-acceptance-2026-09-07/README.md) | 旧候选的入库、隔离、Eval等较广证据，需按改动影响复核 |
| E04 | [权限研究](../../../reports/architecture/codex-harness-refresh-2026-09-22/permission-controls-research.md) | 当前固定 safe、人工逐次审批；三档统一权限和自动审核未接通 |
| E05 | [助手主页面](../../../web/src/pages/assistant/index.tsx)、[会话 Hook](../../../web/src/pages/assistant/hooks/useChatSession.ts)、[消息渲染](../../../web/src/pages/assistant/components/ChatMessage.tsx) | 配置/恢复/输入/展示已有；无正文终态只分 cancelled 与 emptyResponse，失败可能显示“无回复” |
| E06 | [工具目录](../../../src/services/agent_runtime/capability_catalog.py)、[目录API](../../../src/api/v1/tool_inventory.py)、[验收诊断](../../../reports/architecture/codex-harness-refresh-2026-09-22/api-acceptance-diagnosis.json) | 租户可见目录与当前会话有效工具集合存在区别，todo工具在通用助手默认隐藏 |
| E07 | [知识库页面](../../../web/src/pages/knowledge)、[知识服务](../../../apps/knowledge-service/src/knowledge_service) | 创建、上传、文档/切片、版本、配置、召回、QA、评测、权限等已有实现入口 |
| E08 | [Agent Studio](../../../web/src/pages/agents/AgentStudioPage.tsx)、[预览](../../../web/src/pages/agents/AgentPreviewPanel.tsx)、[发布](../../../web/src/pages/agents/AgentReleasePanel.tsx)、[已发布页](../../../web/src/pages/agent-public/AgentHostedPage.tsx) | 草稿、模型/工具/KB/记忆配置、预览和发布产品面已存在；当前候选未完整实测全部流程 |
| E09 | [Eval 页面](../../../web/src/pages/eval/index.tsx)、[对比页](../../../web/src/pages/eval/components/ExperimentRunComparison.tsx) | 数据集、运行、Trace、评分、候选比较等实现；E03有单例真实运行和如实显示失败gate的证据 |
| E10 | [服务页](../../../web/src/pages/Services.tsx)、[模型API](../../../src/api/v1/models.py)、[厂商API](../../../src/api/v1/providers.py)、[首次配置](../../../src/api/v1/setup.py) | 服务/厂商/模型管理、能力配置、探测和配置状态接口存在 |
| E11 | [调试页](../../../web/src/pages/playground/index.tsx)、[调试流](../../../web/src/pages/playground/hooks/usePlaygroundStream.ts) | 服务选择、会话与流式调试实现存在，本轮未重跑其组合场景 |
| E12 | [任务页面](../../../web/src/pages/tasks/index.tsx)、[任务API](../../../src/api/v1/tasks.py)、[定时占位](../../../web/src/pages/tasks/components/ScheduledTasksTab.tsx) | 当前实际挂载收件箱/ID查询；定时任务组件是占位且未接入该页面；任务详情对admin有例外分支，须明确角色作用域 |
| E13 | [登录页](../../../web/src/pages/Login.tsx)、[认证](../../../src/api/v1/auth.py)、[用户管理](../../../src/api/v1/users.py)、[角色](../../../src/api/v1/roles.py) | 登录、改密、管理账号/角色存在；“忘记密码”为联系管理员提示，不能称自助找回已实现 |
| E14 | [设置页](../../../web/src/pages/Settings.tsx)、[配额服务](../../../src/services/billing/quota_service.py)、[API Key](../../../src/api/v1/api_keys.py) | 认证/限流/容量/负载均衡/Key设置、用量配额存在；不等于订阅支付 |
| E15 | [仪表盘](../../../web/src/pages/dashboard/index.tsx)、[指标模块](../../../src/services/metrics) | 总览、运营、可靠性、治理、追踪及观测基础存在；不直接证明长期SLO |
| E16 | [连接器设置](../../../web/src/pages/settings/ConnectorsSettings.tsx)、[MCP](../../../src/api/v1/mcp.py)、[Skills](../../../src/api/v1/skills.py)、[Local Node](../../../src/api/v1/local_nodes.py) | 集成、工具发现、技能管理、本地节点基础已有；每项就绪/授权/运行须分别核对 |
| E17 | [会话分享](../../../src/api/v1/conversation_shares.py)、[产物接口](../../../src/api/v1/_assistant_routes/artifacts.py)、[Quiz](../../../web/src/pages/QuizPage.tsx) | 分享创建/读取/撤销、文件产物和交互测验已有实现，不是新增一个链接按钮即可完成 |
| E18 | [CLI 产品合同](../../../sdk/cli/README.md)、[Python SDK](../../../sdk/python/README.md)、[SDK目录](../../../sdk) | 独立本地CLI与Gateway兼容客户端并存；不同语言/OS的支持程度必须按实际证据声明 |
| E19 | [单实例约束](../../../scripts/harness/single_instance_guard.py)、[运行环境约束](../../harness/runtime-and-secrets.md) | Gateway/Runtime仍按单实例约束运行；本次完善功能不解除该约束 |

## 2. 逐模块产品判断

| 模块 | 已有价值，应保留 | 本轮发现或待验证点 | 产品处理 |
| --- | --- | --- | --- |
| M01 助手 | 多轮、历史、模型、附件、知识选择、工具活动；主流程有E02证据 | 已观察失败消息刷新为“无回复”；长任务、模型切换、附件及记忆的完整组合证据不足 | 先完善异常/恢复和输入配置的可理解性，再提升连续任务效率 |
| M02 工具/审批 | 实际人工审批、拒绝零执行、工具上限与Worker执行 | E04三档未接通；E06目录可见不等于本会话可用；Local Node参数前提容易误解 | 先做有效能力和审批范围展示，三档另设受控增量 |
| M03 产物/分享 | 文件产物、分享页、Quiz实现 | 预览/下载失败、过期和撤销、部分产物成功、跨端回看需要补验收 | 用“拿到并使用结果”作为完成标准 |
| M04 知识库 | 生命周期入口丰富；混合召回/引用已实测 | 文件类型支持、批量部分失败、版本切换、撤权、重处理恢复并未在新候选全部证明 | 先覆盖每一步实际状态和维护动作，保留旧有效结果 |
| M05 Studio | 已保存草稿、独立预览、版本/发布能力 | 当前候选主要验证了页面渲染，不能宣称预览到发布所有行为已通过 | 优先完成一个绑定KB/工具的Agent从预览到已发布使用 |
| M06 Eval | 用例、Trace、实际候选、比较和gate | 旧证据已区分任务完成/样本通过/gate通过；缺完整当前版本闭环与可理解配置 | 从一条真实失败生成回归用例，不先增加评估算法 |
| M07 服务/模型 | 接入、模板、同步、能力配置 | 配置存在/连接可达/某项能力验证通过/用户有权限容易混淆 | 统一有效配置与能力验证说明，不以模型数量为目标 |
| M08 调试 | 模型/服务选择、流式输出、历史 | 参数生效、模型能力差异、失败复现、导出可执行请求需核对 | 明确它是诊断工具，与Assistant任务分工清楚 |
| M09 任务 | 收件箱、筛选、任务查询/结果/取消接口 | 当前不能据页面名宣称覆盖所有Agent、KB、Eval任务；定时任务未交付 | 明确覆盖范围，接通来源详情，逐步联邦查询已有任务源 |
| M10 用户/访问 | 账号、权限、停用、登录审计 | 角色作用域、停用后的已有会话、找回入口的完成路径要明确 | 完善管理员管理模式，不引入自助注册作为前置 |
| M11 配额/设置 | tokens/成本限额、策略、Key管理 | 预算动作与收费、估算与实际、保存与生效需清楚；完整Key生命周期待验证 | 让管理员理解影响范围并能撤销配置错误 |
| M12 仪表盘 | 多视图和诊断基础 | 指标窗口/来源/缺失/样本分母、任务跳转和用户可见性需系统验收 | 用可解释数据支持操作，不添展示性图表 |
| M13 API/SDK/CLI | 多种协议/客户端已有；CLI独立执行边界明确 | 真实平台支持、示例、错误/取消/恢复矩阵证据不齐 | 先验证已承诺范围，不承诺未打包平台或完整桌面应用 |

上述“需核对”是验收缺口，不是声称代码没有实现；后续需求允许用有效证据直接关闭，不要求重复开发。

## 3. 值得优先处理的具体问题

1. **失败历史表达丢失（E05）**：用户看见“无回复”，无法区分模型连接失败、取消和成功无文本。规划为 `AS-02`，属于小范围产品修正。
2. **能力目录与实际可用范围不一致（E06）**：平台目录有某工具，却未进入当前会话。规划为 `TL-01`，向用户说明原因，不能为消除差异而放开权限。
3. **权限参数的产品语义不统一（E04）**：UI固定safe；V1声明的档位与V2拒绝行为不同。规划为 `TL-04`，先统一支持声明，再决定新模式。
4. **“已取消”不保证所有外部动作已停止（E02）**：当前目标版本有已dispatch执行记录为unknown。规划为 `TL-03`，区分任务停止、执行结果未知和资源回收。
5. **页面/文档中的未来能力容易误读（E12/E18）**：定时任务、完整跨平台CLI不能因为有组件或目录而计为已完成。规划为 `TK-01`、`CL-01`。

## 4. 与旧规划的关系

以[既有模块需求](../agent-platform-vnext-2026-09/03-module-requirements.md)作为技术约束与历史需求来源；本次按用户可完成的工作重排。继承身份、单内核、知识ACL、不可变版本、明确失败及副作用限制，不重复重写内核，也不重新执行已结束程序。

已知LOC门禁、全量hosted Rust、完整回滚和发行矩阵仍保留在原报告。产品完善包应证明自己的发布路径和数据保护，不能以本文取代全平台发行认证。
