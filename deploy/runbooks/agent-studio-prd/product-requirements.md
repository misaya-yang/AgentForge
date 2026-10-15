# Agent Studio 产品需求文档

**文档状态：** Ready for implementation planning  
**规划版本：** V1  
**基线日期：** 2026-07-17  
**目标仓库：** `/Users/yang/projects/AI--Platfform`

## 1. 产品定义

Agent Studio 将当前通用 AI Assistant 作为执行底座，允许租户成员通过前端创建、配置、预览、评测、发布和运营多个独立 Agent。每个 Agent 可以拥有自己的身份、系统指令、模型参数、平台工具、MCP 工具、Skills、知识库、记忆策略和发布渠道，并可作为托管聊天页、新窗口、嵌入组件或服务端 API 使用。

V1 不是通用工作流/DAG 编排器，也不是 Dify 的完整复制品。它首先解决“同一个可靠 Assistant Runtime 如何安全地承载多个可配置、可版本化、可发布 Agent”这一核心问题。

## 2. 问题与机会

当前产品已经具备聊天、知识库、模型、工具、Skills、会话、运行轨迹和评测基础，但它们仍围绕保留的 `__builtin_assistant__` 会话身份和单次请求配置组织：

- 用户只能在 Assistant 页面按会话选择模型、知识库、联网和写作风格，不能保存成可复用 Agent。
- `system_prompt` 已存在于 API/Runtime，但前端只把“写作风格”转换为临时系统提示词。
- 工具并非统一 MCP：平台原生工具、模型原生联网、Skills 桥接、知识库代理、Connector 工具和未完整接通的 MCP 并存。
- MCP 管理路由、租户配置和 Assistant Runtime 的组合根尚未形成可用闭环。
- Skills 有上传/版本表，但运行时未可靠绑定完整、不可变的 Skill 内容版本。
- 当前分享页是冻结的会话快照，不是可持续交互的独立 Agent 应用。

因此，直接加一个“Agent 配置表单”会产生租户泄漏、配置漂移、发布不可回滚、工具越权和评测无法复现等问题。V1 必须先建立 Agent 领域模型和运行时隔离契约。

## 3. 目标与成功指标

### 3.1 产品目标

1. 非开发用户能在一个工作台内创建并预览可用 Agent。
2. Agent 所引用的模型、工具、MCP、Skill 和知识库都经过租户权限与可用性校验。
3. 每次发布生成不可变版本；发布渠道可安全升级或回滚。
4. 托管页、嵌入组件和 API 共享同一版本化运行契约，但拥有独立渠道策略。
5. 现有 `/assistant` 和 `__builtin_assistant__` 在迁移期间保持兼容。

### 3.2 上线门槛指标

| 指标 | V1 门槛 | 测量位置 |
| --- | --- | --- |
| 首次创建到成功预览 | 中位数不超过 10 分钟 | Studio 漏斗事件 |
| 发布成功率 | 最近 7 天不低于 99%，配置校验失败不计平台故障 | Publication 审计 |
| 版本回滚恢复 | 所有正式渠道 2 分钟内恢复到上一版本 | 发布事件与探针 |
| 租户/Agent 越权 | 自动化隔离用例 0 个失败 | 安全与集成测试 |
| 运行可追溯性 | 100% Agent 运行写入 `agent_id`、`agent_version_id`、渠道和配置指纹 | Run/Trace 查询 |
| 嵌入安全 | 非允许 Origin 的初始化和请求 100% 被拒绝 | Embed 安全测试 |
| 现有 Assistant 回归 | 关键聊天、会话、知识库、工具和评测门禁全部通过 | 现有回归套件 |

指标不是承诺流量规模；容量阈值必须在 AS-08 根据真实部署基线补齐。

## 4. 用户与权限角色

| 角色 | 核心诉求 | 默认权限 |
| --- | --- | --- |
| Agent Owner | 创建、配置、评测、发布和回滚自己的 Agent | 全部 Agent 管理权限，受租户策略约束 |
| Agent Editor | 协作编辑 Draft、运行预览和评测 | 不可修改成员权限或生产渠道密钥 |
| Agent Viewer | 查看配置、版本、评测和分析 | 只读，不可执行高风险预览 |
| Tenant Admin | 管理模型、MCP、Skills、知识库、配额和策略 | 工作区资源与审计管理 |
| End User | 通过托管页、嵌入或 API 使用已发布 Agent | 仅获得 Publication 暴露的能力 |
| API Client | 以服务端凭证调用指定 Publication | 固定作用域、速率和版本解析规则 |

权限必须由服务端校验；前端隐藏按钮不能替代授权。

## 5. 核心概念与生命周期

```text
Agent（稳定身份）
  ├─ Draft（唯一可变工作副本，带 revision/ETag）
  ├─ Version 1..N（不可变、可复现配置快照）
  └─ Publication 1..N（渠道入口，指向一个 Version）
       ├─ hosted
       ├─ embed
       └─ api
```

生命周期：`draft -> validated -> previewed -> evaluated -> published -> superseded`，Agent 可被 `archived`。发布不是覆盖 Draft，而是创建不可变 Version 并让 Publication 原子地指向该 Version；回滚只改变指针并留下审计记录。

## 6. 功能需求

### 6.1 Agent 目录、身份与协作

- **AS-AG-001** 用户可按名称、状态、Owner、更新时间和发布渠道筛选 Agent 列表。
- **AS-AG-002** Owner 可从空白模板或现有 Agent 复制创建 Agent；复制不得复制密钥值、API Token 或历史会话。
- **AS-AG-003** Agent 必须包含租户内稳定唯一的 ID 和 slug；名称可重复但 UI 必须显示 Owner 和状态避免误选。
- **AS-AG-004** Draft 保存必须使用 revision/ETag 乐观锁；冲突返回 `409` 和当前 revision，不得静默覆盖他人修改。
- **AS-AG-005** Owner 可授予 Viewer、Editor、Owner 角色；最后一个 Owner 不得被移除。
- **AS-AG-006** 归档 Agent 后禁止新预览和发布；已发布渠道按 Owner 选择继续固定版本或同时下线，操作需二次确认并审计。
- **AS-AG-007** 删除采用软删除与保留期；存在 Publication、审计保留或法务保留时不得物理删除。

### 6.2 身份、指令与模型

- **AS-CFG-001** Draft 支持名称、描述、图标、主题色、欢迎语和建议问题。
- **AS-CFG-002** Owner 可编辑完整 Agent Instructions，并查看平台不可编辑层与自己的指令层次关系。
- **AS-CFG-003** 模型选择器只展示租户可用且满足 Agent 所需工具/视觉/上下文能力的模型。
- **AS-CFG-004** 支持 temperature、max tokens、思考模式等当前 Runtime 已支持的参数；越界值在保存时返回字段级错误。
- **AS-CFG-005** 发布版本保存最终解析后的模型 ID、Provider 和参数，不允许运行时自动漂移到“最新模型”。模型失效时渠道进入 degraded，不得静默换模型。
- **AS-CFG-006** V1 不提供任意工作流输入表单；交互入口是聊天消息、允许的附件和固定欢迎/建议问题。

### 6.3 能力目录与绑定

- **AS-CAP-001** Studio 统一展示四类能力：Platform Tools、MCP Tools、Skills、Connectors；来源类型必须可见，不得把所有工具伪装成 MCP。
- **AS-CAP-002** 每项能力显示描述、风险等级、所需权限、审批行为、配置状态、健康状态和最近 schema/version 变化。
- **AS-CAP-003** Draft 可绑定能力；Version 必须保存精确能力 ID、版本或 schema hash 与参数覆盖。
- **AS-CAP-004** 运行时有效能力集合必须是 `租户策略 ∩ Agent Version 绑定 ∩ 渠道策略 ∩ 当前用户权限 ∩ 资源就绪状态`，任一缺失均默认拒绝。
- **AS-CAP-005** 高风险写工具保留现有审批语义；托管匿名/嵌入渠道默认不暴露写工具、文件系统写入和持久记忆写入。
- **AS-CAP-006** `AssistantConfig.tools_enabled` 或其替代字段必须真正控制工具可见性和调用；不能只用于日志。

### 6.4 MCP 服务与工具

- **AS-MCP-001** Tenant Admin 可创建、测试、禁用和删除远程 MCP Server；V1 用户可配置传输仅支持 Streamable HTTP，内部受信服务可由平台配置。
- **AS-MCP-002** Server 配置保存 URL、认证类型、Secret 引用、超时、并发、允许域和状态，禁止在 Agent Spec、日志或前端响应中返回明文密钥。
- **AS-MCP-003** 保存前执行 URL/Origin/私网策略校验；连接与重定向必须防 SSRF、DNS rebinding 和跨租户凭证使用。
- **AS-MCP-004** 支持无认证、Bearer Secret 引用和 OAuth 2.1；OAuth Token 必须绑定资源受众和租户连接。
- **AS-MCP-005** 发现结果保存 Server ID、Tool ID、JSON Schema、schema hash 和发现时间；工具列表变化不得自动改变已发布 Version。
- **AS-MCP-006** 发布前重新检查绑定 MCP 的健康、认证、工具存在性和 schema hash；不兼容变化阻断发布并给出可操作差异。
- **AS-MCP-007** 运行时强制超时、并发限制、响应大小、熔断和审计；失败只降级该能力，不暴露内部网络或密钥信息。
- **AS-MCP-008** 每个 MCP/Connector 连接必须声明凭证主体：`service_account`（租户服务身份）或 `user_delegated`（当前登录用户授权）；Agent Version 绑定的是连接策略/引用，不是 Token。
- **AS-MCP-009** `user_delegated` 运行只可解析当前调用用户自己的 grant，必须记录 grant owner、scope、audience、过期、刷新和撤销；不得回退到管理员或其他用户 grant。
- **AS-MCP-010** 匿名 Hosted/Embed 默认禁止 `user_delegated`，也不得使用租户 service account；只有 Tenant Admin 对指定只读工具和渠道作显式授权后，service account 才可用于 public/embed，并继续受渠道能力交集限制。
- **AS-MCP-011** V1 只允许绑定平台已经支持的 Connector 类型（当前包括 Confluence）；Connector 创建/凭证解析使用同一主体、租户、渠道和审计契约，新增 Connector 类型不在 V1。

### 6.5 Skills

- **AS-SKL-001** 工作区 Skill 目录必须按租户/用户授权隔离，列表、读取、更新、启停和删除均不可依赖跨租户进程级缓存。
- **AS-SKL-002** Agent 绑定精确 `skill_version_id`；发布后 Skill 更新不得静默影响既有 Agent Version。
- **AS-SKL-003** Runtime 加载完整 SKILL.md 指令、Manifest、权限和入口，不得只从元数据重建空壳 Skill。
- **AS-SKL-004** 上传、更新、启停必须持久化并返回失败；数据库保存失败时 API 不得报告成功注册。
- **AS-SKL-005** Skill 所声明权限仍受 AS-CAP-004 能力交集约束，不能借 Skill 绕过工具白名单或审批。
- **AS-SKL-006** V1 用户上传 Skill 只能包含指令与声明式元数据；服务端忽略并拒绝用户伪造的 `entrypoint`、`source`、`builtin://`、本地路径、网络或可执行入口，并规范化为不可执行的 `db://<skill_id>/<version_id>`。
- **AS-SKL-007** 可执行 bundled Skill 属于独立的平台管理制品，必须由部署清单提供 hash/来源/权限，租户上传 API 不能创建或覆盖该制品类别。

### 6.6 知识库与附件

- **AS-KB-001** Agent 可绑定一个或多个当前租户有权读取的知识库 Dataset，并配置现有检索模式、Top K、阈值和图片检索能力。
- **AS-KB-002** 保存、发布和每次运行均校验 Dataset 权限；被撤权或删除的 Dataset 不得继续通过历史绑定访问。
- **AS-KB-003** V1 Version 固定 Dataset 身份与检索配置，但读取 Dataset 最新已发布内容；每次 Trace 记录 Dataset revision fingerprint，用于证明当次内容来源和定位漂移，不声称能重放已被覆盖的历史内容。
- **AS-KB-004** Dataset 不可用时预览显示字段级错误；已发布渠道返回可理解的降级提示并上报告警，不得悄悄改用模型常识声称来自知识库。
- **AS-KB-005** 附件类型、大小、病毒扫描和保留策略沿用现有平台约束，并在渠道策略中独立开关。

### 6.7 记忆、会话与运行隔离

- **AS-RUN-001** 每个新会话在创建时固定 `agent_id`、`agent_version_id`、`publication_id/channel`；继续会话不得静默切换版本。
- **AS-RUN-002** Preview 会话与生产会话使用独立命名空间、配额、Trace 标签和数据保留规则。
- **AS-RUN-003** 记忆模式为 `off`、`session` 或 `user`；匿名 Hosted/Embed 默认仅 `session`，禁止写入共享用户长期记忆。
- **AS-RUN-004** Prompt 按平台安全层、不可变 Agent 指令、渠道上下文、能力元数据、RAG/记忆和对话输入分层组装；外部工具、网页、文件和知识库文本均视为数据而非高优先级指令。
- **AS-RUN-005** Run 与 Trace 必须记录 Agent/Version/Publication、模型指纹、Prompt 指纹、工具 schema 指纹、Skill 版本、KB revision、策略决策和审批结果。
- **AS-RUN-006** 幂等请求键不得跨 Agent、Version、租户或渠道复用结果。
- **AS-RUN-007** Gateway 是 Agent 身份/版本解析的唯一外部权威：外部请求只能提交 message/session 与 Preview revision 或 Publication ID；Gateway 生成携带规范化 Snapshot 的签名 `AgentRuntimeEnvelope`，签名覆盖 tenant、agent、version、publication/channel、session、request body hash、canonical Snapshot/spec hash、issued-at、expires-at 和 nonce。Assistant 必须重算 body/Snapshot hash、原子消费 nonce，并拒绝伪造、篡改、过期、重放或通过通用 Assistant body 注入的 model、Prompt、capability、snapshot 字段。

### 6.8 Studio 编辑与预览

- **AS-UI-001** 提供 `/agents` 列表、`/agents/new` 创建和 `/agents/:agentId` Studio 路由。
- **AS-UI-002** Studio 使用“配置区 + 固定预览区”布局；桌面可并排，移动端切换为标签页，不能产生水平溢出。
- **AS-UI-003** 配置分组至少包括：基本信息、Instructions、模型、能力、知识库、记忆与安全、评测与发布、渠道与分析。
- **AS-UI-004** 保存状态明确区分未保存、保存中、已保存、冲突和失败；离开未保存 Draft 前必须提醒。
- **AS-UI-005** Preview 可清空会话、选择草稿或某个不可变 Version，并显示实际生效能力、版本和 Trace 链接。
- **AS-UI-006** 每个配置项都有可访问标签、键盘焦点、错误关联和帮助说明；颜色不是唯一状态表达。

### 6.9 评测、发布与回滚

- **AS-PUB-001** 发布前生成确定性的 resolved spec 和配置差异，执行权限、资源健康、Prompt、工具 schema、Skill、KB、渠道和安全校验。
- **AS-PUB-002** Owner 可选择现有 Eval Dataset 运行回归；正式发布至少需要配置的阻断评测通过，失败可重跑但不可由客户端伪造通过状态。
- **AS-PUB-003** 发布创建不可变 Version；相同 Draft revision + 相同 resolved spec 的重复请求必须幂等。
- **AS-PUB-004** Promotion 原子更新 Publication 指针，并记录 actor、前后版本、时间、理由和验证结果。
- **AS-PUB-005** Owner 可查看版本列表和结构化 diff，并一键回滚到历史可用版本；回滚也必须审计和做资源就绪检查。
- **AS-PUB-006** Draft 后续编辑不得影响已发布渠道；资源被撤权时按安全策略 fail closed，不以不可变性为由继续越权访问。

### 6.10 托管页、嵌入与 API

- **AS-CH-001** Hosted Publication 提供稳定 URL `/a/:publicId`，支持 private、tenant、public 三种访问模式。
- **AS-CH-002** Hosted 页面展示 Agent 品牌、欢迎语、建议问题、附件能力、来源引用和必要隐私提示；公开页面不得泄露配置和内部能力描述。
- **AS-CH-003** Embed 支持 iframe 与版本化 JS Widget；使用独立 `/embed/agents/:publicId` 文档而非普通 Hosted SPA。该响应必须移除 `X-Frame-Options: SAMEORIGIN`，并由服务端按 Publication allowed origins 生成 `Content-Security-Policy: frame-ancestors ...`；父子页面使用受限 `postMessage` 协议。
- **AS-CH-004** 浏览器嵌入不得包含服务端 API Key；公开/签名短期 Token 只允许访问指定 Publication 和来源域。
- **AS-CH-005** Runtime API 提供流式聊天、会话继续、附件和反馈；API Token 哈希存储、可轮换、可撤销，并限定 Agent/Publication、scope、配额和到期时间。
- **AS-CH-006** 各渠道拥有独立启停、认证、限流、配额、附件、记忆和高风险能力策略，但解析到同一个不可变 Agent Version。
- **AS-CH-007** 删除、归档、禁用或回滚的传播行为必须有明确状态码和用户可理解错误，不返回底层堆栈或内部 URL。

### 6.11 运营、审计与数据治理

- **AS-OPS-001** Agent 分析展示会话、成功率、首 Token 延迟、总延迟、Token/成本、工具成功率、KB 命中、反馈和渠道分布。
- **AS-OPS-002** 每次配置保存、权限变更、MCP/Skill/KB 绑定、发布、回滚、Token 操作和高风险工具决策都写入租户审计日志。
- **AS-OPS-003** Owner 能按 Agent、Version、Publication 和时间筛选 Trace；敏感输入输出遵循现有脱敏和保留策略。
- **AS-OPS-004** Tenant Admin 可设置 Agent 数、发布渠道、并发、Token、MCP 调用和存储配额；超限返回稳定错误码和恢复建议。
- **AS-OPS-005** Agent 归档、用户请求删除和租户删除必须覆盖 Draft、ACL、Publication、Token、会话、长期记忆和派生索引，并保留法规要求的最小审计记录。

## 7. 非功能需求

### 安全与隐私

- 所有查询以 `tenant_id` 为强制过滤条件；对象 ID 不可作为授权依据。
- Secret 仅通过 Secret Store 引用流动，API、Spec、Trace、错误与浏览器不得返回原值。
- MCP、网页、文件、知识库和 Skill 内容均按不可信输入处理，进行 Prompt Injection 边界标记与输出约束。
- 匿名流量必须有 IP/Publication 双层限流、滥用检测、大小限制和成本上限。

### 可靠性与性能

- Publication 解析和 Agent Version 加载可缓存，但缓存键必须包含 tenant/agent/version 并支持撤权失效。
- 发布/回滚使用事务或等价原子机制；失败后保持上一已知可用指针。
- MCP 和外部能力使用独立超时、熔断和并发预算，不得耗尽 Agent 全局执行池。
- V1 性能预算由 AS-00 采集当前基线，AS-08 固化 P95/P99 阈值，AS-09 执行终局回归；在无基线前不得虚构数字。

### 可访问性与国际化

- Studio 和 Hosted/Embed 关键流程达到 WCAG 2.2 AA 可验证要求。
- 文案使用现有 i18n 体系，不在组件中新增不可翻译的用户可见硬编码。
- 支持键盘完成创建、配置、预览和发布；高风险确认不能仅依赖悬停。

## 8. V1 非目标

- 任意 DAG/节点式 Workflow 或 Chatflow 画布。
- 多 Agent 编排、Agent 间委派和自主创建子 Agent 的 UI。
- 公共模板市场、付费 Marketplace 或第三方分成。
- 运行用户上传的任意代码、任意本地 `stdio` MCP 或自定义容器。
- 自定义域名、白标计费、移动 SDK、语音实时通话。
- 把 Agent 自身发布为 MCP Server。
- 新增任意 Connector 类型或 Connector Marketplace；V1 只绑定已存在并满足凭证主体契约的平台 Connector。
- 冻结整份知识库内容到每个 Agent Version，或承诺在缺少历史 revision 读取能力时确定性重放；V1 只提供不可变配置和可追溯的 live-content revision/provenance。
- 让客户端自由覆盖已发布 Agent 的 Prompt、模型或能力白名单。

## 9. 关键验收场景

1. Owner 创建 Agent，绑定一个知识库和一个只读工具，预览成功，发布 Hosted 渠道；后续编辑 Draft 不改变现有 Hosted 对话。
2. Editor 与 Owner 同时编辑 Draft，旧 revision 保存得到 `409`，可查看差异并重新应用，任何一方的改动不被静默覆盖。
3. Tenant A 猜测 Tenant B 的 Agent、Skill、MCP、Dataset、Version 或 Trace ID，所有读取和运行请求均返回无泄漏的拒绝结果。
4. 已发布 MCP 工具 schema 发生不兼容变化，现有 Version 保持原 schema 快照且调用进入受控降级；新发布被阻断直到 Owner 接受更新并重新评测。
5. Skill 上传数据库失败时 API 返回失败，目录中不出现只存在于单进程内存的“成功”条目。
6. Dataset 被撤权后，预览和发布被阻断；已发布渠道不再检索该 Dataset，并产生可定位告警。
7. 匿名 Embed 试图调用写工具或长期记忆，能力交集拒绝调用并留下策略 Trace。
8. Owner 将 Production Publication 从 V3 推广到 V4 后发现回归，可回滚到 V3；新会话使用 V3，已存在会话继续固定原版本并显示状态。
9. 公开页面从未允许的 Origin 初始化 Widget，服务端和客户端均拒绝；浏览器网络请求中不存在服务端 API Token。
10. 现有 `/assistant` 会话、模型选择、知识库检索、联网和历史记录在功能开关关闭时行为不变。
11. 用户 A 绑定自己的 delegated MCP grant 后，用户 B、匿名 Hosted/Embed 和 API Client 均不能回退使用 A 或管理员的 grant；grant 撤销后缓存立即失效。
12. 用户上传带 `builtin://`、本地路径、网络或可执行 `entrypoint` 的 Skill，API 返回字段级拒绝；正常指令型 Skill 被规范化为 server-owned `db://` 版本。
13. 普通 Hosted 页面继续返回防嵌入头；专用 Embed 文档只允许 Publication 配置的 `frame-ancestors`，并在构建后的 Nginx/Helm 产物上验证实际响应头。

## 10. 开放项与决策门

以下内容需要在对应 Phase 以代码和真实运行证据决策，不能在本文档中猜测：

- 当前目标分支与 `origin/main` 的差异是否改变工具、UI 或知识库契约（AS-00）。
- Production 的 Secret Store/OAuth 回调域与出口网络策略（AS-03，需平台输入）。
- 正式发布必须通过哪些 Eval Dataset/阈值（AS-06，需产品/质量输入）。
- 公共渠道的商业配额和保留期限（AS-08，需运营/合规输入）。
- 是否在 V1 后续支持用户自定义输入变量和工作流画布（V2 决策，不阻塞 V1）。
