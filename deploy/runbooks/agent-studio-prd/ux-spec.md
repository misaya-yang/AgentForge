# Agent Studio 信息架构与交互规范

## 1. 设计原则

1. **先让 Agent 可用，再暴露高级参数。** 创建流程只要求名称、Instructions、模型和至少一次预览；MCP/OAuth、评测门槛和渠道策略在 Studio 内逐步完成。
2. **配置结果可见。** 右侧 Preview 始终显示当前运行的是 Draft revision 还是 Version、实际模型、能力数和 Trace 状态。
3. **来源与风险透明。** Platform、MCP、Skill、Connector、Knowledge 视觉分组，展示风险与设置状态。
4. **发布是受控动作。** 发布面板展示 Draft 与生产版本差异、阻断项、Eval 结果和回滚目标，不能用普通“保存”按钮替代。
5. **延续现有产品语言。** 复用现有导航、Ant Design/Radix/Tailwind 组件、间距、色板和 i18n，不做独立营销站风格的重设计。

## 2. 路由与页面矩阵

| 路由 | 页面 | 主要用户 | 核心状态 |
| --- | --- | --- | --- |
| `/agents` | Agent 目录 | Owner/Editor/Viewer | loading、empty、filtered-empty、error、permission denied |
| `/agents/new` | 创建向导 | Owner | blank、template、validating、created、failure |
| `/agents/:agentId` | Studio 默认页 | Owner/Editor/Viewer | draft、saved、conflict、archived、degraded |
| `/agents/:agentId/evals` | 评测 | Owner/Editor | no dataset、queued、running、passed、failed、cancelled |
| `/agents/:agentId/versions` | 版本与 Diff | Owner/Editor/Viewer | no version、selected diff、rollback confirm |
| `/agents/:agentId/channels` | 渠道 | Owner | disabled、private、public、degraded、rate-limited |
| `/agents/:agentId/analytics` | 分析与 Trace | Owner/Editor/Viewer | no data、loading、partial、retention-limited |
| `/a/:publicId` | 托管 Agent | End User | welcome、chatting、auth required、disabled、quota exceeded |
| `/embed/agents/:publicId` | 嵌入 Agent | End User | origin rejected、token expired、ready、offline/retry |

`/share/:shareId` 继续表示只读会话快照，不能复用为交互式 Agent 路由。

## 3. Agent 目录

### 顶部区域

- 标题、Agent 总数和“创建 Agent”主按钮。
- 搜索输入；Owner、状态、渠道筛选；排序为最近更新/最近运行/名称。
- 当前权限不足时隐藏创建动作并显示原因，不显示无效按钮。

### 卡片/表格内容

- 图标、名称、描述、Owner。
- Draft 状态和 Production Version，例如“草稿有 3 项未发布改动”。
- 渠道徽标：Hosted、Embed、API。
- 最近运行、最近更新、健康状态。
- 行操作：打开、复制、归档；发布/权限等高风险操作进入详情页。

空状态提供“从空白创建”和 2–3 个受控模板，不自动创建资源。

## 4. 创建向导

V1 使用三步轻量向导：

1. **身份：** 名称、描述、图标，可选模板。
2. **行为：** Agent Instructions、默认模型。
3. **开始：** 创建 Draft 并进入 Studio，右侧自动准备空 Preview。

模板复制 Prompt 和非敏感配置，不复制 MCP OAuth、API Token、用户 ACL、会话、长期记忆或不可访问的知识库。若模板资源不可访问，创建前显示排除列表。

## 5. Studio 框架

### 桌面布局（宽度不小于 1180px）

```text
┌──────────────────────────────────────────────────────────────────┐
│ Breadcrumb / Agent name / Draft status / Preview / Publish       │
├──────────────┬───────────────────────────┬───────────────────────┤
│ Section nav  │ Config editor             │ Live preview          │
│              │                           │ revision + trace      │
│ Overview     │ contextual form           │ conversation          │
│ Instructions │ validation summary        │ effective capabilities│
│ Model        │                           │                       │
│ Capabilities │                           │                       │
│ Knowledge    │                           │                       │
│ Memory       │                           │                       │
│ Eval/Publish │                           │                       │
└──────────────┴───────────────────────────┴───────────────────────┘
```

### 窄屏/移动端（390x844 为必测视口）

- 顶部保留名称、保存状态和更多菜单。
- “配置 / 预览”用标签切换；切换不丢未保存表单或会话。
- Section nav 使用可访问抽屉。
- Publish 流程使用全屏 Sheet，阻断项和确认按钮始终可达。
- 不允许水平滚动；代码/schema 区域自身可滚动并有复制动作。

## 6. 配置分区

### Overview

名称、描述、图标、主题、欢迎语、建议问题。右侧即时预览品牌，但只有保存后的 revision 可运行。

### Instructions

- 支持纯文本/Markdown 编辑，不执行其中 HTML。
- 顶部解释平台安全层不可覆盖。
- 提供字符/Token 估算、变量/能力引用提示和冲突警告。
- V1 不提供“AI 自动改写并直接覆盖”；若提供辅助生成，必须显示 diff 并由用户应用。

### Model

模型、temperature、max tokens、思考/搜索能力说明。选择不兼容模型时显示具体受影响能力，并阻止保存非法组合。

### Capabilities

四个标签：Platform Tools、MCP、Skills、Connectors。

每行包含：启用开关、来源、风险、设置/健康、版本/schema、权限、凭证主体（service account 或当前用户 delegated）、渠道可用性和测试按钮。选择高风险工具或租户服务凭证时即时显示渠道限制；不能等发布后才提示。

MCP 资源缺失时 Editor 可发起“请求管理员配置”，但只有 Admin 能输入 URL/OAuth/Secret。工具 schema 更新显示 Added/Removed/Changed 参数 diff。

### Knowledge

Dataset 多选、检索模式、Top K、阈值、图片开关。显示更新时间、文档/片段数、索引状态和访问异常。提供一条测试查询并展示引用，不把测试结果写入正式会话。

### Memory & Safety

记忆模式、附件、内容保留、工具审批、公开渠道限制。选择 public/embed 时动态展示强制覆盖：session-only memory、Origin、限流和写工具策略。

### Eval & Publish

选择 Eval Dataset、运行记录、阻断阈值、Draft/Production diff、资源检查和发布/回滚。

### Channels & Analytics

Hosted、Embed、API 独立卡片；每张卡显示状态、URL/代码、认证、配额、当前版本和最近健康。Analytics 深链到同 Agent/Version/Channel 过滤视图。

## 7. Preview 契约

- Preview 顶部固定显示 `Draft rN` 或 `Version N`，以及“未保存改动不会进入预览”。
- 保存成功后用户显式“应用到预览”或自动重建 Preview 会话；不得在已有会话中热换配置。
- 切换版本前提示将创建新会话；旧预览保留可返回的历史记录。
- 工具/KB 调用以现有流式事件呈现，显示来源、状态和审批，不泄露内部参数或 Secret。
- 错误区分配置错误、资源未设置、权限不足、Provider 故障和 Runtime 故障，并提供可操作入口。
- Viewer 可查看已有 Preview/Trace，但默认不能触发可能产生外部副作用的运行。

## 8. 保存、冲突与恢复

| 状态 | UI 行为 |
| --- | --- |
| Clean | 显示“已保存 rN”和时间 |
| Dirty | 显示未保存标记；离开时提醒 |
| Saving | 禁止重复保存；非阻塞编辑需形成下一批 revision |
| Saved | 更新 revision；配置摘要和预览应用按钮同步 |
| Conflict | 展示服务端 revision、本人改动与冲突字段；允许刷新/复制/重新应用 |
| Validation error | 就地字段错误 + 页面级阻断摘要，焦点移动到首个错误 |
| Network error | 保留本地编辑内容，允许重试或导出非敏感草稿 JSON |

浏览器本地恢复不得保存 Secret、OAuth Token 或服务端 API Key。

## 9. 发布与回滚交互

发布 Sheet 顺序固定：

1. Draft revision 与目标渠道。
2. 与当前 Production Version 的结构化 diff。
3. 资源/权限/Secret/Schema/KB 健康检查。
4. Eval Gate 结果和 Trace 链接。
5. 渠道影响：新会话版本、已有会话、公开访问、配额。
6. 版本说明和明确确认。

存在阻断项时不渲染可点击的“强制发布”。用户明确批准的非阻断 waiver 必须填写理由并进入审计。回滚采用同一流程，但目标是历史 Version，仍执行资源撤权和安全复检。

## 10. Hosted 与 Embed

### Hosted

- 使用独立、克制的聊天界面，延续 Agent 的图标/主题色；平台品牌与隐私入口按租户策略显示。
- 未开始对话时展示欢迎语和建议问题；开始后保持聊天主任务，不堆叠营销模块。
- 引用、工具进度、附件和错误沿用 Assistant 既有成熟组件。

### Embed

- Widget 支持 launcher、inline 和 iframe 三种呈现配置；V1 底层都使用受控 iframe 隔离，并指向专用 `/embed/agents/:publicId` 文档，不把普通 `/a/:publicId` Hosted 页面直接嵌入。
- 安装代码只包含 `publicationId`、公开样式配置和可选短期用户签名，不含服务端 Token。
- 父页面与 iframe 的 `postMessage` 只接受固定 Origin 和版本化事件：`ready`、`resize`、`open`、`close`、`new_message`、`error`。
- 提供暗色/亮色、位置、尺寸、初始打开状态和语言；业务方不能通过 CSS 注入 iframe 内部。
- Hosted 页面继续返回 SAMEORIGIN/`frame-ancestors 'self'`；专用 Embed 文档由服务端按 Publication allowed origins 生成 `frame-ancestors` 且不返回 SAMEORIGIN XFO，Nginx 与 Helm 路由必须保持该差异。

## 11. 可访问性、视觉与浏览器验收

每个 UI Phase 必须至少验证：

| 路径 | 视口 | 必查项 |
| --- | --- | --- |
| `/agents` | 1440x900、390x844 | 空/列表/筛选、键盘、无横向溢出 |
| `/agents/:id` | 1440x900、1024x768、390x844 | Section、编辑、保存状态、Preview 切换 |
| Publish Sheet | 1440x900、390x844 | 阻断项、焦点陷阱、确认/取消、Esc |
| `/a/:publicId` | 1440x900、390x844 | 欢迎、流式回复、引用、错误、认证 |
| Embed fixture | 1280x800、390x844 | allowed/rejected Origin、resize、键盘焦点 |

验收包括：axe 无严重/高等级问题；可见焦点；错误与控件通过 `aria-describedby` 关联；对比度达到 AA；减少动画偏好生效；加载/空/错误/权限/降级状态均有截图证据。

## 12. 文案原则

- 使用“Agent”“能力”“MCP Server”“Skill”“知识库”“版本”“渠道”等稳定术语。
- 不使用“魔法”“一键全自动”等不可验证表达。
- 工具不可用时说明是“未配置”“无权限”“健康检查失败”还是“版本不兼容”。
- 发布按钮用“发布 Version N 到 Hosted”，回滚用“将 Hosted 回滚到 Version N”，避免含糊的“应用”。
