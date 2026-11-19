# AI Gateway 前端 UI 全面升级优化总纲

> 日期：2026-07-17
> 分支：`codex/front_up`
> 模式：现有产品 Upgrade，不改变后端接口与公开数据契约
> 视觉方向：Calm Operations Console（安静、可信、信息密度可控的 AI 运营控制台）

## 1. 目标、范围与非目标

### 1.1 目标

本轮升级同时解决四类问题：

1. 让所有已注册路由和关键非路由界面形成统一的视觉系统；
2. 修复桌面布局直接压缩到手机所造成的裁切、双滚动、不可操作和状态遮挡；
3. 让动效只承担层级、连续性、反馈和少量公共页氛围，不制造模板化“AI 感”；
4. 修复走查中暴露的真实前端缺陷、无效 CTA、路由断裂、键盘语义和状态误导。

### 1.2 范围

- 全局应用壳层、侧栏、页头、主题、用户菜单、路由 loading、403、404、错误恢复；
- Dashboard、Services、Knowledge、Playground、Assistant、Eval、Tasks、Settings、Users、Exams；
- Login、Share、Quiz 公共页面；
- Confluence 已存在但未注册的页面族；
- 页面内弹窗、抽屉、空态、加载态、错误态、选择态、保存态和移动端交互；
- light / dark、中文 / 英文、键盘、触屏、reduced motion。

### 1.3 非目标

- 不改后端 API、数据库 schema、权限命名和核心业务流程；
- 不引入新的 UI 或动画依赖；
- 不把 Dashboard、Eval 等密集工作台改造成营销 Landing Page；
- 不增加虚构指标、伪社会证明、无业务意义的装饰模块；
- 不以大规模文件重构代替可验证的界面升级。

## 2. 证据与当前基线

### 2.1 已检查证据

- 路由、导航、权限守卫、页面入口和页面内子视图；
- 全局 CSS token、Ant Design token、Dashboard 私有 token、Assistant 私有 token；
- `Button`、`Card`、`Select`、`Dialog`、`Tabs` 等共享原语；
- 14 个路由在 1440×900 与 390×844 下的 28 张 Playwright 基线截图；
- 页面运行错误、console error、页面级横向溢出检查；
- type-check、lint、i18n 基线。

### 2.2 基线结果

- `pnpm type-check`：通过；
- `pnpm i18n:check`：通过；
- `pnpm lint`：0 error，38 个既有 warning；
- Open-source 动态路由 E2E：2/2 通过；
- 28 张基线截图：mock 修正后无 runtime/console error；
- Node 当前为 24，项目声明 Node 22，所有 pnpm 命令会显示 engine warning。

### 2.3 基线硬问题

- Dashboard 在手机端实际仍是 1180px 桌面画布，只显示左侧切片；
- Assistant 手机端历史栏占据约 280px，主聊天区被压缩到不可用；
- Playground 手机端会以持久化“历史栏已打开”状态进入遮罩态，顶部控制也未重组；
- Tasks 手机端保留 4/8 双栏和 viewport 固定高度；
- Knowledge 创建页 Stepper、选择卡片、操作区在手机端严重挤压，Next 按钮文字因颜色冲突不可见；
- Knowledge 列表手机端筛选 rail 和空态操作裁切；
- Services 手机端四个 Tab 裁切，空态标题强制换行；
- Users 手机端桌面表头和空态内容被截断；
- Exams 手机端标题、搜索、筛选和四张统计卡直接横向压缩；
- Settings 手机端五个 Tab 裁切，部分 grid 未重排；
- 多处存在 hover-only 操作、点击 `div`、无效 CTA、原生 `alert/confirm` 和不可见 async error。

## 3. 路由、权限、导航与布局模式

| 路由 | 权限 / 可见性 | 核心任务 | 布局模式 | 优先级 |
|---|---|---|---|---|
| `/login` | 公开 | 登录、强制改密、恢复深链 | public | P0 |
| `/dashboard` | `console:dashboard:view` | 系统态势、运营、可靠性、治理、Trace | workspace | P0 |
| `/services` | `console:services:view` | 服务、Provider、Model 配置 | standard | P0 |
| `/knowledge` | `knowledge:dataset:view` | 查找、筛选、创建知识库 | standard | P0 |
| `/knowledge/create` | `knowledge:dataset:create` | 三步创建知识库 | wizard | P0 |
| `/knowledge/:datasetId` | `knowledge:dataset:view` | 文档、检索、QA、来源、配置、权限 | workspace | P0 |
| `/playground` | `conversation:playground:access` | 服务试验与会话 | immersive | P0 |
| `/assistant` | playground 权限；model tester 限制 | 正式对话、Activity、Artifacts | immersive | P0 |
| `/eval` | `console:eval:view` | 评测、Trace、资产、门禁 | workspace | P1 |
| `/tasks` | `console:dashboard:view` | 同步任务和任务查询 | standard | P1 |
| `/settings` | `console:settings:view` | 网关全局策略 | standard | P1 |
| `/users` | `user:list` | 用户列表与管理 | standard | P1 |
| `/users/:userId/edit` | `user:edit` | 身份、角色和权限编辑 | form-workspace | P1 |
| `/exams` | `console:dashboard:view` | 考试创建、发布和关闭 | standard | P1 |
| `/exams/:examId` | `console:dashboard:view` | 参与者、题目、报告 | standard | P1 |
| `/quiz/:shareCode` | 公开 | 答题与结果 | public | P1 |
| `/share/:shareId` | 公开 | 阅读分享对话和产物 | public | P2 |
| `/confluence/*` | Knowledge 权限 | 连接、绑定、同步页面 | hidden-standard | P1 |
| `/403` | 公开 | 权限失败恢复 | system-state | P0 |
| `*` | 公开 | 未找到页面和恢复 | system-state | P0 |

### 3.1 信息架构决策

- Exams 作为可直达业务页面进入主导航，Services 中的嵌入入口保留为快捷入口；
- Confluence 页面族注册为隐藏路由，修复现有死链接，但不增加第五套主导航入口；
- Knowledge 继续作为数据资产主入口，Tasks 作为执行与同步任务主入口；
- 后续若要彻底合并 Confluence/Knowledge/Tasks，需要独立产品 IA 迁移，不在本轮做破坏性重构。

## 4. 视觉系统

### 4.1 一句话视觉论点

用克制的 graphite / steel / amber 体系表达“可信的 AI 运营控制面”，结构主要由排版、间距、对齐和 tonal surface 建立，而不是给每组内容叠加边框、阴影、渐变与浮起。

### 4.2 Surface 层级

1. `canvas`：页面背景；
2. `region`：主要工作区或长表单区域；
3. `panel`：需要边界的独立数据或配置区；
4. `interactive`：可选择、可展开、可点击的 surface；
5. `selected`：持久 tonal fill + marker，视觉强于 hover；
6. `floating`：Dialog、Popover、Dropdown、mobile sheet；
7. `blocking`：错误、危险确认、权限和不可恢复状态。

### 4.3 边界预算

- 普通内容优先靠间距和对齐分组；
- 长列表使用轻 divider；
- 表单、输入、危险操作和不熟悉控件保留清晰边界；
- 同一组件不得同时使用强 border、强 fill、强 shadow、glow 和 scale；
- hover 只做轻 tonal shift 或 1px 位移，selected 使用持久填充或 marker；
- 触屏不依赖 hover，键盘 focus 必须至少与 hover 同等清晰。

### 4.4 Typography

- 页面主标题只保留一层；
- AppLayout 负责 route title / subtitle，页面内部只输出工作区标题或 section title；
- 数字、ID、时间、Token、成本使用 tabular / mono 特性；
- 不再新增 emoji、彩色 category 文本或系统字体私有覆盖；
- 当前声明 Geist / IBM Plex Mono，但未加载外部字体，因此视觉必须在 system fallback 下仍成立。

### 4.5 交互状态

重要控件必须定义：

- rest；
- hover（仅 fine pointer）；
- `:focus-visible`；
- pressed；
- selected / expanded；
- disabled；
- loading；
- touch；
- reduced motion。

## 5. Motion Contract

### 5.1 Motion 只允许四种用途

- 层级：页面/工作区首次进入，240–360ms；
- 连续性：Sidebar、Sheet、Drawer、Tab、Artifact 展开，180–320ms；
- 反馈：保存、复制、同步、上传、运行、失败恢复，120–220ms；
- 氛围：Login、Share、Quiz 的单次轻 reveal，不使用持续光效。

### 5.2 禁止项

- 控制台卡片逐张长延迟入场；
- 无限摇摆、呼吸、旋转、发光；
- 大面积 blur/filter/shadow 动画；
- hover scale 作为唯一可交互信号；
- raw scroll handler 驱动动画；
- reduced motion 下仍依赖位移动画才能看到内容。

### 5.3 Reduced Motion

- 所有 Framer Motion 动画显式使用 `useReducedMotion` 或提供静态 final state；
- 取消长位移、spring、无限 repeat；
- 重要状态反馈保留即时 tonal / opacity 变化；
- 页面内容在动画失败时仍默认可见。

## 6. 响应式 Contract

| 关注点 | 390px 手机 | 768px 平板 | 1440px 桌面 |
|---|---|---|---|
| App sidebar | modal overlay，Escape/遮罩关闭 | overlay 或 collapsed rail | 固定 rail |
| 页面 gutter | 8–16px | 16–20px | 20–24px |
| Page actions | 主操作 + overflow / 换行 | 可换行 toolbar | 右对齐 |
| Tabs | 横向 rail 或 Select | 横向 rail | 完整显示 |
| Metric strip | 2 列或水平摘要 | 2–4 列 | 一行 |
| 表格 | mobile data card / drill-in | 可滚动表格 | 完整表格 |
| 双栏工作区 | 上下堆叠或 sheet | 视空间决定 | 双栏 |
| Immersive composer | safe-area + `dvh` | 固定底部 | 固定底部 |
| Wizard stepper | `当前步骤 / 总步骤` | 紧凑 stepper | 完整 stepper |

硬门槛：

- 390×844 和 390×667 不出现主要内容裁切；
- 不使用负 margin 猜测 AppLayout padding；
- 不使用错误的 `100vh - 40px`，统一按 56px Header 和 `dvh`；
- 不保留桌面固定列宽后让手机横向拖动完成核心任务。

## 7. 全局壳层与共享原语

### 7.1 AppLayout

- 导航权限与路由守卫一致；
- Exams 有明确入口和 active state；
- 标准页、工作区、沉浸页的 content padding/height 有明确契约；
- 用户菜单触发器是 button；
- mobile sidebar 支持遮罩、Escape、关闭后恢复；
- route title/subtitle 不与页面标题重复。

### 7.2 Button

- 增加明确 solid primary；
- quiet secondary 与 outline 不与 primary 等权；
- 仅过渡 color/background/border/transform，不使用无差别 `transition-all`；
- active feedback 不依赖大 scale；
- icon button 必须有 accessible name。

### 7.3 Card / Surface

- `open`：无边框内容组；
- `panel`：普通区域；
- `interactive`：可点击、有 selected/focus；
- `blocking`：错误/危险；
- 避免 card-on-card 和每个空态都使用大号虚线框。

### 7.4 Tabs / Dialog / Select

- Tabs 支持移动 horizontal rail；
- Dialog 层级高于 sidebar，手机尺寸使用 `dvh`、safe-area、focus trap 和 Escape；
- Select 只使用全局 semantic token，不使用私有 slate/gradient/shadow；
- dropdown/popover 不再以 `z-index:9999` 对抗层级系统。

## 8. 逐页优化方案与完成门槛

### 8.1 Dashboard

- [x] 移除 1180px 强制最小宽；
- [x] 真实容器宽度触发单列/双列/三列；
- [x] 手机筛选器完整可用且不横向裁切；
- [x] panel 在手机由内容决定高度，避免多层滚动；
- [x] 加载、无数据、失败不伪装为 0 或“健康”；
- [x] 删除/修复硬编码 SLO、成功率与 hover 后背景漂移；
- [x] 390px 下可见 Tabs、过滤、信号和 Trace。

### 8.2 Services

- [x] Tab rail 手机可滚动并有边缘 fade / scrollbar 提示；
- [x] 搜索和主操作在手机重排；
- [x] Add Provider / Model 使用稳定矩形 primary；
- [x] 空态收敛为紧凑 EmptyState；
- [x] Provider 操作支持 focus-within 和 touch；
- [x] Model 删除补 accessible name；
- [x] Exams tab 不显示无效的外层搜索。

### 8.3 Knowledge 列表

- [x] 统计 Card 收敛为 summary strip；
- [x] 搜索和类型筛选成为单一 toolbar；
- [x] 手机 filter 使用横向 rail 或 Select；
- [x] 空态按钮换行且无裁切；
- [x] Import 若无实现则不能保留无行为 CTA；
- [x] Dataset item 支持键盘和 focus；
- [x] query error 与 empty state 分开。

### 8.4 Knowledge 创建

- [x] 手机 Stepper 显示当前步骤而非固定连接线；
- [x] KB type/use case/visibility/chunk mode 是真正 radio 语义；
- [x] 手机一列或可读两列；
- [x] Next/Confirm 主按钮文字可见；
- [x] 文件 input 可重复选择同一文件；
- [x] 部分上传失败可见且可重试，不直接跳转掩盖失败；
- [x] Footer 在短高度和手机端可达。

### 8.5 Knowledge 详情 / Sync

- [x] Tab 状态由 URL 驱动；
- [x] `sync` 与认可的 tab key 一致；
- [x] Sources 内不重复渲染 Sync 工作区；
- [x] Documents / Retrieval / Settings 在手机堆叠；
- [x] Overview Cards、Binding、Pages 在手机重排；
- [x] query error 可重试；
- [x] 无行为按钮删除或接通。

### 8.6 Playground

- [x] 根工作区使用 `dvh`，无负 margin 猜测；
- [x] 手机历史栏为 overlay sheet，默认不挤压主区；
- [x] 手机顶部只保留核心控制，其余换行或收纳；
- [x] 去除蓝青渐变、glow、彩虹会话；
- [x] composer 有明显 focus-within；
- [x] 删除、分享失败有可见反馈；
- [x] 触屏可以发现 row action。

### 8.7 Assistant

- [x] 手机历史栏为 overlay sheet；
- [x] Enter 严格遵守 `canSend`；
- [x] 空输入、上传中、流式中不会重复发送；
- [x] WelcomeScreen 无无限模板化动画；
- [x] Activity / Artifact mobile sheet 支持 Escape、focus 和 safe-area；
- [x] Composer / attachment icon 有 accessible name；
- [x] Reduced Motion 立即显示 final state。

### 8.8 Eval

- [x] 只保留一层页面主标题；
- [x] Overview 指标重排为桌面自适应、手机 2 列 summary grid；
- [x] 日期筛选映射服务端 API；
- [x] Trace 超过 100 条可服务端分页；
- [x] Tab / family / run 写入 URL；
- [x] 本轮新增样式集中到 shared token；既有 Eval 内嵌样式保留，避免无收益的大迁移；
- [x] 手机使用 Explorer / Thread / Run 单列视图切换。

### 8.9 Tasks

- [x] render-phase setState 移入 effect；
- [x] 手机连接列表与详情上下堆叠；
- [x] 固定 viewport 高度只用于桌面；
- [x] Add Connection 接通真实路由；
- [x] Scheduler icon 有 accessible name；
- [x] Scheduled 占位不与成熟能力等权。

### 8.10 Settings

- [x] query fallback 不生成反复变化的 `{}`；
- [x] form 初始化 effect 只在真实数据到达后执行；
- [x] 五个 Tab 手机可滚动并有边缘提示；
- [x] 所有固定 2/3 列 grid 手机单列；
- [x] Switch 与 Label 绑定；
- [x] 负载均衡选项使用 radio 语义；
- [x] 保存成功、失败、loading 可见。

### 8.11 Users / UserEdit

- [x] Users 手机使用摘要卡，不裁切空态；
- [x] 排序范围和 `aria-sort` 明确；
- [x] async error 可见；
- [x] UserEdit role/permission 使用真实 checkbox/button；
- [x] 移除渐变头像、emoji、多彩卡片和延迟入场；
- [x] 保存条 sticky 到内容容器，不覆盖 sidebar；
- [x] reduced motion 下无位移链。

### 8.12 Exams

- [x] 主导航可发现；
- [x] 页面标题、操作、筛选手机重排；
- [x] Stats 手机 2×2；
- [x] 列表 item 支持键盘；
- [x] `passing_score` 使用 `??`，保留合法 0；
- [x] 错误可读；原生 alert/confirm 保持原业务确认契约，不在本轮扩大重构；
- [x] detail tabs 有 tab 语义与横向滚动提示。

### 8.13 Login / Share / Quiz / 系统状态

- [x] Login 双栏桌面、开放式移动布局；
- [x] 使用共享品牌，不再手写第二套 Logo；
- [x] username / 完整 email 不出现双后缀；
- [x] autocomplete、role=alert、loading 语义完整；
- [x] 登录成功恢复原深链；
- [x] Share loading/error 可读，长 metadata 手机重排；
- [x] Quiz 缓存结果的元数据失败不白屏；
- [x] Quiz 提交错误可见；
- [x] 403/404 使用统一 steel/amber 系统状态，并保留可测试的状态码语义。

## 9. 非路由界面覆盖清单

- 全局：RouteFallback、鉴权 hydration、用户菜单、语言、主题、Help、Profile、PasswordChange、Toast；
- Services：Service form、Service config、Provider/Model form、删除确认；
- Knowledge：删除密码、上传、切块预览、文档/段落编辑、版本历史、批处理、RAGAS、QA、Binding；
- Assistant：Customize、Share、Connectors、Activity、Artifacts、Document Preview、Citation、Search、Thinking、Tool timeline、内嵌 Quiz；
- Tasks：Binding config、Page manage、Scheduler、Task query 全状态；
- Eval：Trace inspector、Score、Export、Dataset/Evaluator/Experiment、Golden import、Comparison、Gate；
- Users：创建、批量删除、重置密码、权限折叠、未保存状态；
- Exams：创建、发布、关闭、答题、结果、报告；
- 公共错误：无效/过期 Share、无效 Quiz、403、404、Root Error。

## 10. 验收矩阵

### 10.1 视口

- 1440×900；
- 1024×768；
- 768×1024；
- 390×844；
- 390×667；
- light / dark；
- en-US / zh-CN；
- `prefers-reduced-motion: reduce`。

### 10.2 每页硬门槛

- 主任务可完成；
- loading / empty / no results / error / partial success 可区分；
- 无 console error、framework overlay、水平裁切和双滚动；
- 键盘 Tab、Enter、Space、Escape 可完成核心交互；
- focus-visible 明确；
- touch 不依赖 hover；
- selected 强于 hover；
- 主操作比次操作更明确；
- reduced motion 不隐藏内容；
- 同一 viewport 有升级前后对比证据。

## 11. 实施顺序与状态

1. [x] 路由、页面和共享组件审计；
2. [x] 桌面/移动视觉基线；
3. [x] 总纲与验收矩阵；
4. [x] 路由、权限、系统状态和全局壳层；
5. [x] 共享视觉原语；
6. [x] Assistant / Playground；
7. [x] Dashboard / Services；
8. [x] Knowledge 全链路；
9. [x] Eval / Tasks / Settings / Users / Exams；
10. [x] Login / Share / Quiz / Confluence 隐藏页；
11. [x] 严苛视觉子代理复查；
12. [x] 全量 lint / type-check / i18n / build / E2E；
13. [x] 最终页面覆盖复核；
14. [x] commit 与 push。

## 12. 视觉复查记录

### 12.1 截图与覆盖

- 升级前：14 个代表性路由 × 1440×900 / 390×844，共 28 张；
- 升级后主矩阵：24 个代表性路由 × 1440×900 / 390×844，共 48 张；
- 补充矩阵：390×667 共 9 张，768×1024 共 20 张，dark 共 14 张，zh-CN + reduced-motion 共 14 张；
- 最终 Confluence 定向复核：四个隐藏页面 × 1440×900 / 390×844，共 8 张；
- 本轮累计检查 113 张升级后截图；最终自动报告均为 0 horizontal overflow、0 runtime / React console error；
- 覆盖 Dashboard、Services、Knowledge list/create/detail/sources、Playground、Assistant、Eval、Tasks、Settings、Exams list/detail、Users list/edit、Confluence connection/create/bind/synced、Login、Share、Quiz、403、404。

### 12.2 严苛 mismatch ledger

| 级别 | 发现 | 修复 |
|---|---|---|
| P0 | 无 | 无确认 hard failure |
| P1 | Knowledge Detail 主 Tab 因 rail 缺少 flex 而纵向堆叠 | 统一 `ui-tabs-rail`，桌面恢复单行，手机 horizontal rail |
| P1 | Knowledge 文档 toolbar 与 DocumentRow 在 390px 内部裁切 | toolbar 重排；DocumentRow 改为 mobile summary row；操作压缩为 40px icon controls |
| P1 | Confluence 隐藏页仍使用蓝青渐变且 mobile header 挤压 | 四个隐藏页统一 steel surface / solid primary / responsive gutter |
| P1 | Eval 重复主标题；UserEdit 英文页混入中文 | 删除重复标题；补齐中英文 service-access i18n |
| P1 | dark 登录按钮禁用/加载态对比度不足 | primary disabled token 改为 semantic muted surface，文本对比恢复 |
| P1 | 横向 Tabs / table 后项被静默裁切 | 增加右缘 fade、thin scrollbar、scroll snap 与 40px mobile target |
| P1 | Tasks 无连接时 4/8 grid 留下大面积空洞 | 空态 Connection Pool 改为 12 列并居中；有数据时恢复 4/8 |
| P2 | Confluence / Tasks 空态与页头重复 CTA | 无数据时只保留上下文内主 CTA |
| P2 | 无行为 Meta Info；来源总数英文单数不自然 | 删除死按钮；改为中性计数文案 |

### 12.3 Motion quality rubric

| 维度 | 得分 |
|---|---:|
| Evidence and fidelity | 2/2 |
| Composition and hierarchy | 2/2 |
| Visual system | 2/2 |
| Surface hierarchy and affordance | 1/2 |
| Motion | 2/2 |
| Responsive behavior | 2/2 |
| Accessibility and resilience | 1/2 |
| Performance and implementation fit | 2/2 |
| **总分** | **14/16** |

严苛视觉子代理最终结论：无 P0、无 P1、无确认 hard failure；剩余观察均为不阻断交付的 P2 精修，并已继续处理知识库 Segmented / Service CTA 触控高度、UserEdit 权限空态、Settings 保存按钮层级与重复创建 CTA。

### 12.4 最终验证

- `pnpm type-check`：通过；
- `pnpm i18n:check`：通过；
- `pnpm build`：通过；仅保留既有大 chunk 提示；
- `pnpm lint`：0 error，17 个既有 warning（基线为 38）；
- open-source E2E：6/6 通过；
- Assistant / Playground 发送门禁与 mobile sheet 定向回归：4/4 通过；
- `git diff --check`：通过；
- Docker / 真实账号全栈 E2E：未运行；本机无 Docker CLI，且本轮不触碰仓库指定 runtime owner。

### 12.5 外部风险

- 当前 Node v24.14.0，仓库声明 `^22.12.0`；所有检查均通过但命令会显示 engine warning；
- 后端 Confluence 检查字符串仍为 `confluence:manage`，数据库与前端为 `knowledge:confluence:manage`；`admin:*` 不受影响，非管理员显式授权需要后端权限域另行统一；
- 真实 provider、真实数据密度和账号权限组合未在本机全栈环境验证，当前视觉证据使用可重复 API mock。
