# R1 AI 助手日常使用闭环执行入口

`loop-state.json` 是本轮执行状态权威；本表是开始时的需求—现状—缺口—验收映射，随代码调查和实测更新。基线为 `2ed0345f52a4128ecbecf08e1cbcfe366895f504` 加当前分支未提交成果，**不重置工作树**。既有 [AS-02 验收](../../../reports/product/r1-a-as02-acceptance-2026-09-23.md) 与[模型选择局部验收](../../../reports/product/assistant-model-selection-2026-09-23.md)作为回归基线；其未验证边界仍保留。

| 需求 | 当前实现入口与现状 | 本轮缺口或待判定点 | 验收 |
| --- | --- | --- | --- |
| AS-01 | `ConversationSidebar`、`useChatSession` 有会话列表/切换；`AssistantPage` 只有单一输入状态 | 已确认草稿不按会话隔离；管理动作、重复发送待实测 | J01 |
| AS-02 | 失败/取消/未知/空正文展示与历史投影已有本包修复 | 继承回归；真实 Runtime 成功空正文仍须如实标记证据边界 | J02 |
| AS-03 | `useChatSession` 有 run marker、状态对账和 SSE 重连 | 断网、刷新、Runtime 重启、迟到终态及不重建 run 待实测 | J03 |
| AS-04 | 模型筛选和 Qwen 3.8 默认已局部实现 | 有效参数/不可用原因、不兼容线程及保留草稿的新会话待补 | J04 |
| AS-05 | `useFileHandler`、`ChatInputArea` 有上传和附件预览 | 已确认剩余槽位不足可产生幽灵上传；部分失败/重试/逐项纳入待补 | J05 |
| AS-06 | 助手有 KB/联网选项与引用组件 | 本轮范围可见性、无答案、撤权链路待验证与修复 | J06 |
| AS-07 | `ActivityPanel` 和活动时间线已有 | 已确认流式输出可能强制滚动旧内容；长事件/停止/去重待核验 | J03 |
| AS-08 | 已有记忆入口和会话记忆能力 | 长期记忆真实存储、关闭/修改/删除及跨用户隔离待核验 | J29 |
| AS-09 | `useImageGeneration` 和附件图片入口已有 | 有效模型能力、失败占位、成功图片回看待核验 | J05、J08 |
| AS-10 | `ChatInputArea` 支持常用输入 | 键盘/焦点、390px 与中英文主流程待实测 | J01、J05～J08 |
| TL-01 | 能力目录和连接器入口已有 | 平台目录、租户授权、当前会话集合可能混淆；前提提示待核验 | J07 |
| TL-02 | `ToolApprovalCard` 有审批入口 | 目标/参数/范围/有效期与重复/过期/拒绝合同待核验 | J07 |
| TL-03 | `useChatSession` 有停止与未知事件处理 | Agent停止、外部动作结果和资源回收的证明待核验 | J03 |
| TL-04 | 现有 `safe` 执行配置已发送至 Runtime | UI/V1/V2/Studio 一致性和旧字段拒绝待核验 | J07 |
| AR-01 | `artifactHydration`、`ArtifactsPanel` 有产物列表 | 真实文件/图片/Office/Quiz 归属、部分失败和刷新一致性待核验 | J08 |
| AR-02 | `DocumentPreview` 与下载入口已有 | 类型不支持、过大/过期/撤权后的下载及预览退路待核验 | J08 |
| AR-03 | `ShareDialog`、分享 API/页已有 | 已发现匿名快照可能原样复制历史；访客预览、来源权益、撤销待修与验收 | J08 |
| AR-04 | `QuizCard` 和分享测验页已有 | 提交幂等、答案可见时点、访客归属和恢复待核验 | J08 |

## 顺序与退出条件

1. **R1-0 观察与冻结：**核对现有实现、产品决策默认值、账号/模型/样本与支持范围。每项标为已满足、需修复或待实测，不以源码存在判通过。
2. **R1-1 助手连续使用：**修 AS-01/03～10 的确认缺口，J01～J06/J29 离线与实机分层验收，继承 AS-02 回归。
3. **R1-2 工具审批：**修 TL-01～04，完成 J03/J07 的授权、拒绝、重复和副作用测试。
4. **R1-3 产物分享：**修 AR-01～04，完成 J08 的文件/图片/Office/Quiz、下载、分享和撤销。
5. **R1-4 集成：**最终 Docker 更新、Codex 内置浏览器逐旅程实测、独立只读 review、修复复测；只有 J01～J08/J29 范围内必需用例通过且无阻塞，才标记 R1 完成。

所有浏览器与 Docker 动作遵守[运行和凭据规则](../../../docs/harness/runtime-and-secrets.md)。本地 Playwright 不使用会额外创建/重置 model tester 账号的默认 `global.setup.ts`；沿用禁用该 setup 的 `tmp/` 临时配置或等效安全方式。命令和页面证据写入 `reports/product/`，临时截图留在 `tmp/`。

## 2026-09-23 阶段收口

最终 Runtime/Worker Docker 镜像、真实 Qwen 文本附件读取/刷新、Quiz 审批作答、KB 检索与受限分享等切片的结果，以及逐需求未验证项见[验收记录](../../../reports/product/r1-progress-2026-09-23.md)。独立只读 reviewer 的具体发现、修正和复核结论也在该记录中。**R1 未完成**：J03 待审批 run 在 Runtime 重启后不能安全地以同一 run 续接；其余未验证的 J01～J08/J29 用例仍需按报告补齐。当前分支及未提交 PRD/代码保留，下一步以 `loop-state.json` 的 blocker 和 next_action 为准。


## 2026-09-26 DR-01 收口

Runtime 同run重启恢复缺口已完成本地范围验收；最终 C4 使用 durable owner/fence、原审批与Worker回执，接通 Core 原turn恢复。真实浏览器 SIGTERM 和明确标注的十类受控崩溃/治理矩阵通过。取消历史、恢复正文去重、未知状态与诊断指引已复测。证据见[DR-01报告](../../../reports/product/runtime-durable-recovery-2026-09-26.md)与[收据](receipts/DR-01.yml)。旧阶段失败记录保留为历史事实；R1其它未验证旅程仍保持进行中。
