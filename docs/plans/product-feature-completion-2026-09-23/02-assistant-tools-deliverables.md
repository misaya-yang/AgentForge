# 02｜AI 助手、工具审批与交付物

现状依据见[证据E02、E04～E06、E16～E17](01-current-state.md)。本分册的要求是后续目标，不是当前全部已实现的声明。

## M01：AI 助手

**用户与价值。** 普通成员从一条自然语言需求开始，在同一会话中补充资料、修改要求、调用工具并取得结果。用户不必先理解模型协议或Runtime概念。

**已有与主要问题。** 多轮、历史、模型选择、附件、知识选择、活动面板已有；文字、审批和冷恢复已有局部实证。最明确的问题是失败消息刷新后可能只显示“无回复”。其余重点是让复杂状态仍能继续操作，而非增加更多对话按钮。

**主流程。** 新建/打开会话 → 选择可用模型及资料 → 输入/上传 → 流式回答或明确澄清 → 工具审批 → 查看结果/引用 → 继续、停止或恢复 → 日后回看。

| ID | 优先级/类型 | 需求与交互规则 | 可判定验收 |
| --- | --- | --- | --- |
| AS-01 | P1 验证 | 新建、重命名、搜索、切换、删除会话形成完整管理；切换保留未发送草稿，运行中的会话有标识；删除说明影响范围 | 两会话交替输入不串草稿/历史；切回运行会话可见状态；误双击不创建两个run；删除后列表/深链接一致 |
| AS-02 | P0 修正 | 明确区分运行中、成功、失败、取消、结果未知和成功但无正文；错误提示包含影响、下一动作及可复制的安全诊断ID | 首token前失败、部分输出后失败、主动取消、成功无正文四种情况，刷新前后分类一致；失败不回退为普通“无回复”，已有正文保留 |
| AS-03 | P0 验证 | 断网重连、页面刷新、后台切回与Runtime重启后恢复同一run；用户可选择继续查看或停止 | 任务不重复创建；迟到事件不把终态改回运行中；停止后可开启下一轮，冷恢复不改变provider或授权 |
| AS-04 | P1 补齐 | 模型/思考参数展示有效值与不可选原因；更换模型导致当前线程不兼容时说明原因，提供保留草稿的新会话入口 | 参数保存后显示有效值；旧线程不兼容返回明确指引；新建会话保留待发送内容，不自动重发已执行操作 |
| AS-05 | P1 验证 | 附件选择、上传进度、类型/大小限制、失败重试、移除和发送前就绪检查；区分“本地选择”“已上传”“已纳入本轮” | 一批3文件中1失败，仅成功文件可明确选择后发送；重试不重复附加；撤回未发送附件不删除用户原文件；过期引用能定位原因 |
| AS-06 | P1 补齐 | 本轮资料范围可见：已选知识库、附件、联网搜索；目录变更或授权撤销时显示状态，不静默扩大搜索范围 | 换KB只影响后续请求；撤权后的引用不能继续打开；无答案时说明资料不足，不能捏造引用；普通问答不因未配可选搜索阻塞 |
| AS-07 | P1 验证 | 活动面板只展示必要步骤、工具摘要、耗时和结果；长任务有可理解进度，用户阅读旧内容时不强制滚动 | 同一工具只出现一个执行身份；失败/取消/unknown可区分；显示可公开的推理摘要而非隐藏推理；1000事件样本可滚动和停止 |
| AS-08 | P1 补齐 | 记忆模式、范围和生效时间可解释；有查看/修正/关闭入口时必须对应真实存储与授权 | 关闭长期记忆后后续run不新增长期记录；删除/修改结果回读一致；会话记忆与用户长期记忆不混淆，不承诺无法证明的即时全链路遗忘 |
| AS-09 | P1 补齐 | 支持已配置模型的图片输入与现有图片生成入口；未配置或不支持时解释前提，产物走统一管理 | 图片模型/文本模型切换后控件能力一致；生成失败不留永久占位；已成功图片刷新后可查看；不以所有模型都支持多模态作为前提 |
| AS-10 | P1 验证 | 键盘发送、换行、粘贴、复制、滚动、窄屏输入及焦点恢复可用；中英文状态语义一致 | 390px和常用桌面宽度完成输入、上传、审批、停止、查看引用；Tab/Enter/ESC不误发送；错误提示可被辅助技术感知 |

### AS-08 Native 控制一致性增量（2026-10-12）

Web 查看/修正/关闭入口和租户+用户隔离合同保持不变。当前源修复使 Worker 普通
记忆工具与管理 API 同样拒绝 `__assistant_` 保留控制键，并让 Native 新 run 的
已解码 JSONB 开关只接受真正的布尔 `true`，避免设置显示关闭却被字符串二次解析
开启。生效时间、普通键/profile、在途 run 和历史记录的边界见
[长期记忆控制合同](../../design/general-agent-product-contract.md#46-用户长期记忆与控制边界)。
该增量的离线生产函数证据与真实服务/浏览器验收分别记录；不改写既有 J29 历史证据，
不表示当前 Native 完整旅程或 R1 已完成。

### AS-02 实施记录（2026-09-23）

R1-A 仅实施 AS-02。已修正消息终态展示与 Runtime 历史投影：失败、取消、结果未知和成功空正文有独立提示；失败时保留部分正文，提示下一步并显示可复制的安全 run ID。终态事件的原始错误字段不返回给历史客户端，刷新和重开历史不触发执行。真实 Qwen 成功与主动取消、受控六状态历史以及自动化回归的证据和未实测边界见[本包验收记录](../../../reports/product/r1-a-as02-acceptance-2026-09-23.md)。本记录不表示 M01 其它需求或完整 R1 已完成。

### 追加需求：助手模型列表与主模型（2026-09-23）

按用户本次追加要求，助手模型入口仅展示模型已启用且供应商已配置的条目；新会话优先使用 `qwen3.8-flash`，旧会话继续显示其保存的模型，用户明确手选的可用模型保持偏好。部署默认模型与 DashScope 初始目录也同步更新，详见[模型选择验收记录](../../../reports/product/assistant-model-selection-2026-09-23.md)。这是 AS-04 的局部实现记录，不代表 AS-04 或完整 R1 验收完成。

### 完整 R1 扩展实施记录（2026-09-23，进行中）

用户已授权本轮实施完整 AS-01～AS-10、TL-01～TL-04、AR-01～AR-04；上述 R1-A 首包限制只描述已完成的历史开发包，不限制本轮。当前按[执行状态](../../../deploy/runbooks/r1-assistant-closure-2026-09-23/loop-state.json)与[分层验收记录](../../../reports/product/r1-progress-2026-09-23.md)继续实施。会话草稿/恢复、长期记忆管理、工具有效性及审批预览、产物下载、分享和 Quiz 幂等已有范围内修正；真实 Quiz 审批/作答/访客刷新、真实 KB 检索/无答案、私有来源分享拒绝已得到局部浏览器证据。

目前 R1 **未验收完成**。附件上传与 `read_attachment` 的会话 ID、存储前缀及原始参数哈希合同已修正；最终 Docker Runtime/Worker 镜像及内置浏览器真实 Qwen 读取文本、刷新恢复通过，证据和适用边界见[阶段收口记录](../../../reports/product/r1-progress-2026-09-23.md)。Runtime 重启仍会取消待审批 run，进程内审批等待器无法安全恢复同一 run，这是 J03 的实质缺口。当前历史明确呈现重启中断及人工下一步，不把取消伪装为继续或自动重试。图片真实 provider、Office 产物及部分跨权限场景尚未完整验收；只有 J01～J08/J29 所有范围内必需验收通过后，才更新本记录为 R1 完成。

**2026-09-24 追加记录。** 会话删除增加影响说明与确认，首轮失败后草稿不再残留在新对话。真实 Qwen/Worker 通过审批生成短 DOCX，刷新仍可回看；认证 API 下载并检查了 DOCX 正文。真实图片 provider 返回结果未知，未取得成功图片；旧前端将图片未知误报为完成，新图片会话的历史恢复还会清空运行消息。现已把图片运行/失败/未知及安全诊断任务 ID 持久化，修正新会话绑定和切换时异步结果串写，并阻止 provider 成功后存储故障被当作确定失败。受控故障与真实链路的界限、测试和剩余验收见同一[阶段收口记录](../../../reports/product/r1-progress-2026-09-23.md)。**J03 同 run 重启恢复与若干 J05～J08/J29 实机路径仍未通过，R1 状态保持进行中。**

**2026-09-25 安全增量记录。** 经用户选择，先本地合并已验证的增量，R1 状态仍为进行中。独立 review 找出的直接调用批准 API 可绕过动作核验、分享撤销后旧预签名下载仍有效两处问题已修复并定向回归；本地 Docker 已同步最终 Gateway 代码且 validate/status 通过。J03 同 run 重启恢复仍是明确的 P0 缺口，其他未验证旅程不写成通过。检查明细和边界见同一[阶段收口记录](../../../reports/product/r1-progress-2026-09-23.md)。

**2026-10-13 当前源码澄清。** 上述 9 月 23～25 日的 J03 缺口描述是当时快照，不是当前实现状态。9 月 26 日 DR-01 已实现原 run/turn 的 fenced ownership、持久调用与审批、Worker 原执行对账和 Core 冷恢复；原审批 ID、参数、期限与决定不依赖进程 waiter。[DR-01 历史验收报告](../../../reports/product/runtime-durable-recovery-2026-09-26.md)记录了确切 C3/C4 镜像的真实 Qwen/Worker 崩溃窗口和最终 C4 浏览器证据，不代表后来升级后的恢复构建已重新验收。本次进一步将受管理动态审批的决定与结果回执合并为同一事务，提交后才通知 waiter；相同内部重试只回读原决定，不重放副作用或延长授权。原生 ServerRequest 的失联恢复不伪装为动态审批，原有在线处理保持不变。实现及边界见[持久恢复合同](../../design/agent-runtime-durable-recovery.md#atomic-managed-dynamic-approval-decisions-2026-10-13)。本次六文件实现已通过固定 Rust 1.95.0 对实际 Runtime 生产与测试目标的 `cargo check --locked --offline --verbose -p ai-platform-agent-runtime --all-targets`：首次 609.887 秒，四个改动 Rust 文件格式化及独立无语义变化复核后，缓存复查 10.052 秒通过；固定 rustfmt 检查也通过。源码、工具链、锁与依赖字节均保持可核对。随后独立评审并单独准入的固定 deferred-Core 测试构建在 1,022.475 秒内完成，实际 Runtime 库测试可执行文件执行了九个新增纯函数测试：9 通过、0 失败/忽略，86 个无关测试被过滤；回执 SHA-256 为 `6bcff5400954a5190edbf823b9eb17066c7e531d4fc081e8de523c3ecfce4f44`。单作业、debug/incremental 关闭和原内存/双文件系统守卫保持不变，无守卫停止或残留进程；源码、工具链、锁和 registry/Git 依赖源码字节未变。测试 ELF、dep-info 与九个实际测试名均已绑定。这些测试验证回执身份、参数绑定、重试比较和响应形状。随后单独准入的 Runtime 主二进制编译在 477.53 秒内通过，回执 `5732eac41dc1f9f2ad3f3dd6a33b57e6f286d8dfa69b7396bc6ac14b1bf856ad`；原后置证明解析器因 dep-info 包含目录依赖而退出 1，该失败独立保留，另经评审的只读验证器绑定了实际 ELF、2,274 个文件和 23 个源码目录，未重编译或启动产品。此 Runtime 主程序结果本身不是完整安装包。随后单独准入的 Worker 四目标构建在 293.389 秒内通过，主程序、health、Python supervisor 和 plugin-packs 四个实际 ELF 均绑定到对应源码/dep-info；回执 `9b700c104272a2c6722d0ce957ca8551e8e21d4735559171ce3b9a723abe9951`，四产物绑定 `b050f6add1e28a262a84988bc687802a768b6c5811bc141cd984c434b48bd043`。源码和 registry/Git 依赖字节、原 Runtime/测试 ELF 与 dep-info 未变，无守卫停止或残留进程；四个产品均未启动。此前 10 个纯路由和 8 个惰性文件绑定测试通过。当前完成的是主机二进制编译范围，不代表 OCI 镜像/安装包组装或部署验收。随后用完整测试名精确执行八个现有 Worker `MemoryStore` 测试：8 通过、0 失败/忽略，73 个无关测试被过滤；守卫构建/测试耗时 11.554 秒，回执 `7cde18192f891e1a38ab4ada5b5b49f7ddfc88da58926ed0bc87d5fc162eb253`。这些测试验证内存参考实现的作用域/幂等、Runtime 绑定、事件游标、租约 fence 与未知副作用恢复；原六个产品/测试 ELF 及 dep-info、源码和依赖字节未变。这一窄缓存测试经单独评审使用 8 GiB 启动预算，原停止/内存守卫和先前 12 GiB 产品构建合同不变，仍不证明生产 SQL 原子性或真实重启。数据库事务/并发、当前 Runtime 重启及真实产品验收仍未完成，不能据此把整个 R1 或当前构建的 J03 标记通过。

**本模块不做。** 不重写Codex规划/压缩/多代理循环，不添加按关键词绕开Runtime的快路径，不强制默认多代理，不为“完善”要求所有模型支持相同能力。

## M02：工具、审批、连接器与本地节点

**用户与价值。** 使用者知道Agent可用哪些能力、操作将影响什么；管理员能接好服务，限制访问，并判断为何执行失败。

**已有与主要问题。** 有人工审批、工具授权、MCP/Skills/Connector及Local Node基础。平台总目录与会话有效工具不同；设备工具还存在设备选择等必填前提。权限三档目前是研究结论，不能把一个UI开关当成执行策略。

**主流程。** 查看当前可用工具 → 发现配置/授权缺口 → 由有权用户补前提 → 预览动作对象和影响 → 人工或已批准策略审核 → 执行 → 取得结果/处理失败 → 撤销集成或权限。

| ID | 优先级/类型 | 需求与交互规则 | 可判定验收 |
| --- | --- | --- | --- |
| TL-01 | P0 修正 | 分开呈现平台目录、租户授权、当前会话有效集合；不可调用原因包括未配置、未授权、会话未启用、设备离线、缺必填输入 | 目录中有todo工具但当前会话隐藏时不显示“可直接使用”；local_node_catalog缺device_id先引导选择设备；不通过放宽工具上限消除差异 |
| TL-02 | P1 补齐 | 审批卡展示工具、实际目标、关键参数、读/写/外发影响、有效期和批准范围；允许拒绝并说明后果 | 页面刷新恢复同一审批；通过/拒绝重复点击只结算一次；参数变化旧批准失效；过期卡不能执行；拒绝后不换工具绕过 |
| TL-03 | P0 补齐 | 区分“停止Agent继续工作”“已发动作的执行结果”和“执行资源回收”；unknown显示核实入口，不自动重试 | 长Python停止后run终态可确认；若Worker未证明停止则标待核实，不能显示“全部已撤销”；确认完成/失败后状态收敛且留历史 |
| TL-04 | P0 修正 | UI、V1、V2、Studio对现有权限档位的支持声明一致；当前仅safe可用就只允许该语义，旧字段不能无提示地被忽略 | 相同档位跨入口得到一致有效配置或明确不支持；保存后回读一致；用户不能通过旧API获得更宽能力 |
| TL-05 | P1 补齐 | Connector配置与用户连接分开；显示连接健康、权限范围、到期/失效、最近成功同步；MCP工具发现和Skills启停有版本及影响说明 | 错凭据/连接失效可修复；断开后新调用被阻断；删除仍被Agent引用的配置先显示影响；凭据只写不回显，诊断包不带密钥 |
| TL-06 | P1 验证 | 本地节点明确执行设备、在线状态、授权目录/应用/域名及数据去向；配对/撤权/离线都有可操作状态 | 节点离线时保留待执行内容；选设备A不能默认切到B；撤销目录后新请求拒绝；上传远端前呈现所需授权范围 |
| TL-07 | P2 新增 | 统一三档权限作为独立增量：请求批准、帮我批准、授权范围内自动执行；复用上游审核机制，保留平台最终执行校验 | 三档的有效配置持久化；人工/自动/策略决定均可追踪；自动审核不可用时回退人工或拒绝；拒绝不得被后续审核覆盖；租户、工具和设备上限保持 |

### 权限三档产品定义建议

| 档位 | 用户预期 | 必须保留的边界 |
| --- | --- | --- |
| 请求批准 | 按平台已声明的规则，对需确认动作逐次询问 | 明确目标、参数、范围和有效期；已有只读能力不应无故增加审批 |
| 帮我批准 | 在相同资源范围内，由自动审核判断低风险动作，疑义交回用户 | 审核有可见结论摘要和来源；不确定/故障不能默认为批准 |
| 授权范围内自动执行 | 对明确授权范围内的动作减少逐次打断 | 不扩张租户/工具/本地节点授权；管理员可限制，用户可撤销；不等同于共享宿主机任意访问 |

这三个档位是**建议方案**，命名、默认值、哪些动作始终需要人工确认见[决策D02](07-delivery-and-acceptance.md)。在策略、Runtime、动态工具和Worker未全链路接通前，TL-07不可作为普通前端开关上线。它不阻塞已有人工审批功能完善。

## M03：产物、分享与交互测验

**用户与价值。** 用户拿到真实可用文件或可交互结果，并决定分享给谁。回答文本说“已生成”不构成完成。

**主流程。** 生成/上传结果 → 预览内容 → 下载/使用 → 回到会话 → 选择可分享内容 → 访问者查看 → 管理者撤销或过期。

| ID | 优先级/类型 | 需求与交互规则 | 可判定验收 |
| --- | --- | --- | --- |
| AR-01 | P1 验证 | 统一展示已有文件、图片、Office、Quiz等产物的名称、类型、产生时间、所属任务和就绪状态；输入附件不冒充新产物 | 生成两文件仅一个成功时逐项呈现；下载内容与预览一致；零字节/生成失败不显示已完成；文件列表刷新不重复 |
| AR-02 | P1 补齐 | 不支持浏览器预览时提供可靠下载；过大、过期、权限变化和服务不可用分别说明 | PDF/图片/至少一种Office产物执行查看、下载、刷新回看；预览失败不使已存在文件不可下载；撤权下载被拒绝 |
| AR-03 | P1 验证 | 分享前预览访问者实际可见的消息、附件和产物，明确匿名/需登录、范围、到期；撤销入口可找到 | 分享不得意外包含隐藏消息、密钥或私有Trace；撤销后新访问及受控下载失效；无法收回已下载副本的事实明确说明 |
| AR-04 | P1 补齐 | 已有测验支持创建、作答、提交、反馈及会话恢复；访客与登录用户的结果归属清楚 | 刷新不丢已接受的提交；重复提交不重复计分；答案不在答题前泄漏；失效分享页给出明确状态 |
| AR-05 | P2 新增 | 有明确需求后增加统一导出/批量管理，先定义导出内容、权限和体积限制 | 导出清单和实际文件一一对应；部分失败可定位；导出不包含其他租户或未勾选内容 |
| AR-06 | P1 补齐 | 知识派生回答、文件与受控分享保留来源关联，并按下列统一规则处理版本、撤权和删除；无法核对来源许可时限制分享 | 来源更新不改写旧回答；撤权后的系统内派生内容/分享/下载访问受限；不得把引用失效误称为已收回用户下载副本 |

**跨模块依赖。** AS-02/03提供状态与恢复，TL-02/03提供动作事实，知识库KNO-05提供引用权限。分享扩大受众是独立操作，不能由“生成产物”隐含授权。

### AR-02 文件访问错误增量（2026-10-12）

原生 Gateway 的文件下载继续逐次核验账号、租户和知识来源；Web 产物抽屉、图片预览、消息内文件、文档预览及 Studio Preview 现在复用安全的错误分类与中英文下一步提示。403 提示核对权限；404 保留“不适用于当前账号”的隐藏存在性语义，不声称文件已删除或过期；只有明确的 410 才提示引用过期/不可用。409、413、429、5xx 和网络失败分别提示未完成、体积限制、稍后重试、服务暂不可用或检查连接，不显示服务端正文。

预览窗口被阻止时不发送文件 GET，明确提供下载替代入口；预览失败不会禁用独立下载。图片预览状态绑定当前文件地址，切换文件时先隐藏旧 Blob，再读取新文件；新文件被拒绝时不残留前一文件的预览。所有重试均为用户重新发起的文件读取，不自动重试权限失败、不重新运行生成任务，也不改写 API、来源授权或匿名分享默认值。新增离线回归执行真实下载/预览函数与错误分类，覆盖无下载副作用、失败窗口清理及预览失败后的手动下载。真实浏览器、存储失效与跨账号/撤权的集成验收仍待补证；此记录不表示 AR-02 或完整 R1 已验收完成。

### AR-02 正文与历史文件链接增量（2026-10-12）

已绑定到当前消息的产物在正文中出现规范下载链接时，正文链接复用现有文件下载与安全错误提示，不再直接打开需要认证的 API 地址。普通外链、锚点和未匹配产物身份的地址保留原行为；不将任意正文 URL 送入认证下载。若历史消息既无已绑定的产物详情，也无保留的明确绑定 ID，未匹配的旧链接仍按原方式导航，本增量不声称修复此类旧链接的失效反馈。相同产物的读取尚未结束时忽略重复点击，失败后用户可再次点击重试，不自动重跑生成任务。普通 Markdown、GFM 和公式正文共用此入口。

离线回归执行完整生产组件、历史产物绑定及下载函数，以受控 HTTP/DOM 验证成功字节、404、显式 410、503、失败后手动重试和重复点击。此处的 410 是受控响应，不表示规范产物链接存在自然到期机制；404 仍保留当前账号不可用语义。真实浏览器下载、原生 API/存储与自然失效未执行，AR-02 的完整验收状态不变。

### 知识来源变化后的统一访问规则

以下为默认产品方案，开发前按D03确认；其适用范围是系统能够关联来源的回答、文件和分享，不承诺识别用户在系统外另行复制的内容。

| 来源变化 | 会话中的派生回答/生成文件 | 引用/原件 | 系统托管的分享页 |
| --- | --- | --- | --- |
| 切换有效知识版本，权限未变 | 保留当时结果与来源版本，不自动改写或重新生成 | 能保留旧版本则打开旧版本；已不可用则说明，不能偷偷指向新版 | 保留当时分享快照与版本，权限仍按当前规则检查 |
| 查看者被撤销来源访问权 | 隐藏受限派生内容；保留可公开的任务状态；下载重新鉴权 | 不再允许读取 | 内部查看者同样受限，不能借分享链接绕过撤权 |
| 来源删除或资源策略禁止继续访问 | 系统保留的受限内容不可再读；按保留政策处理实际存储，不把逻辑失效说成已物理清除 | 标记不可用 | 关联受限内容停止提供访问；无法可靠拆分时整个分享不可用 |
| 用户已经下载或复制到系统外 | 不声称能远程收回副本 | 后续系统内访问仍检查权限 | 撤销只阻止后续受控访问，不承诺收回外部副本 |

默认不开放私有知识派生内容的匿名分享；阅读权限不等于公开发布权限。已有无私有知识来源的普通会话分享仍按AR-03管理。若后续支持把私有知识派生成果显式公开，必须另定资源级发布授权和撤销规则，不作为AR-03的隐含能力。

R1先完成普通产物/会话分享；对无法核验来源权益的内容禁用分享并说明原因。R2通过AR-06与KNO-05的联合用例后，开放相应的内部知识成果分享，不以未经授权的临时放行为过渡方案。

**AR-06 R2 集成候选（2026-09-28）：** 会话与独立 Quiz 的内部分享在创建时冻结可验证来源范围，每次读取、文件下载和测验提交均重查当前账号、租户和来源版本权限；内部 Quiz 尝试 token 与账号绑定，页面本地进度按账号隔离。真实本地 API 已验证可核权内部会话分享预览、创建、登录读取、匿名拒绝、撤销后拒绝。跨账号/跨租户、撤权与历史版本的浏览器矩阵仍待 J30；本增量不表示 AR-06 或 R2 已验收完成。证据见 [R2 集成记录](../../../reports/product/r2-closure-progress-2026-09-28.md)。


### 2026-09-26 Rust 持久执行与重启恢复实施记录

DR-01 已实现并完成本地范围验收：有持久上下文的原 root 动态工具任务在 Runtime 重启后保持同 run/turn、模型、原 lease/snapshot/审批期限；PostgreSQL owner/fence 防止多个执行者推进，审批决定续接，Worker 原 execution/call/attempt 和结果复用。未知写操作不会自动重放，取消或撤权后不恢复执行。真实 Qwen/Worker 内置浏览器待审批 SIGTERM 重启后批准原动作，测验只生成一次；刷新/重开历史保持结果，六个成功崩溃窗口及拒绝/取消/撤权/未知的受控矩阵通过。

本轮还修复了 SSE EOF 误取消原run、审批重投影冲突、冷取消缺历史receipt、恢复started重复正文和unknown历史误标完成。已有 AS-02 纳入定向回归；AS-03/TL-03 的 **Runtime 同run重启恢复缺口关闭**，不把这项等同于两需求所有旅程或完整R1通过。其他未验证路径与不支持的恢复边界保留，详见[分层证据与独立review](../../../reports/product/runtime-durable-recovery-2026-09-26.md)。本轮未提交或推送。

### 2026-09-27 R1 来源权限收尾增量（R1-RP01）

保留已合入的 AS-02、激活模型/default 规则和 DR-01 C4 原run持久恢复；本轮修正私有来源在后续KB-off任务、后台恢复、审批、事件、历史、派生文件下载与Quiz间的权限不一致。撤权后隐藏正文/引用/活动/参数，保留公开终态与诊断ID，允许拒绝动作，阻止批准与恢复执行；只读轮询不创建run。独立review发现匿名Quiz累积来源遗漏和冷恢复threadID遗漏，已修复并复核，无剩余blocker/high。

真实Qwen/内置浏览器证明关闭记忆不写入、390px仅读取已选附件、私有KB问答及待审批/终态闲置撤权；真实API事件/历史遮蔽及普通无KB Quiz匿名分享/幂等提交/撤销回归通过。Python199、Node10、受控单附件503+真实Office下载E2E2、OpenAPI2、source18与受影响规范检查通过；最终Docker源/静态资源逐一匹配且健康。旧匿名Quiz无法核验来源时404，新建分享409，可信无私有来源的旧Quiz仍可分享。

按用户最新要求收尾并合入本地main，不推送、不删除分支；此安全增量及DR-01完成不等于完整R1所有旅程已通过。真实图片成功/视觉、长任务资源回收、1000事件、PDF和余下分享/窄屏场景仍未逐项实机验证；IAB原生下载事件未观测，独立Playwright下载已验证真实字节。详见[本轮分层证据和限制](../../../reports/product/r1-remaining-acceptance-2026-09-27.md)。

### 2026-09-27 产物与终态收尾增量（R1-RP02）

保留DR01/RP01成果。修复Wan2.6 choices图片响应解析、图片持久事实的只读历史恢复、PDF认证打开入口、停止/未知活动分类与迟到取消回执；独立review发现的待审批覆盖取消/失败也已修正。终态原thread继续发送的旧租约检查错误改为复用不可变权限上限的只读路径，写入fence和未知动作不重放合同保留。

已运行Python89、Node14、源码合同18、OpenAPI2及受影响规范检查。真实PDF内容与390px认证下载/完整Chromium预览、受控图片pending→unknown恢复分别记录；真实长Python停止资源证据复用本轮报告。图片provider实际成功后CDN下载仍受本机Fake-IP DNS阻塞，未降低SSRF保护或改共享网络。最新用户授权快速收尾及本地main合入，不代表完整R1/J01～J08/J29全部验收完成；实际Docker/Rust/原thread续接及未验证项由[验收记录](../../../reports/product/r1-artifacts-long-task-acceptance-2026-09-27.md)和执行状态补全。

R1-RP02实际本地合入main：实现9373a650；新a55739b4a6c5 Runtime/Worker、Rust160（PG2忽略）、原thread取消/成功旧owner过期后新轮、刷新/重开不新增执行均通过，review PASS。未推送；完整R1未验证项不变。

### 2026-09-27 独立剩余流程收尾（R1-RP03）

完成已发现的发送双击误停止、上传附件冷恢复缺失、分享PDF白名单未绑定文件缺卡、390px长地址溢出及分享弹窗键盘问题；Quiz答前分享/安全预览、期限与刷新后本人链接撤销管理已补齐，新管理GET及OpenAPI同步。Python83/Node8、真实分享E2E2、受控活动E2E2与受控到期中英文E2E1通过；IAB视觉/冷恢复/PDF/Quiz/ESC/到期页和最终Docker源码/前端哈希及健康通过。独立review PASS，无blocker/high。

按最新用户授权本地合入main，不推送/删分支；保留DR01/RP01/RP02。完整R1仍不声明完成：真实图片CDN Fake-IP阻塞与其余未验证必需旅程如实列在[RP03收尾记录](../../../reports/product/r1-remaining-flows-2026-09-27.md)及执行状态，不把受控场景混称真实provider/自然到期。

### 2026-09-27 失败与权限边界补验（R1-RP04）

在已本地合入main的RP03基础上，补齐受控409草稿/附件保留、422失效附件手动重传指引、原run实际模型/温度/思考值刷新恢复、普通viewer引用只读打开与撤权遮蔽、两账号记忆隔离、真实600秒审批自然到期、零字节/部分失败/503产物展示以及断线同turn重连。撤权后V2 `run_started` 不再回填快照参数。真实审批到期后的工具失败且执行数0，模型随后给出如实文本，run终态为成功，不将其误记为工具成功。

实际Python93、受控新增E2E5、真实原PDF下载E2E1、Docker源码和前端构建身份、内置浏览器及独立代码review结果与剩余边界见[RP04分层报告](../../../reports/product/r1-final-boundaries-2026-09-27.md)。最新用户授权将此已验证安全增量本地合入 `main`，不推送或删除分支；完整R1不声明完成：图片生成正例仍受外部CDN Fake-IP解析阻塞，后台切回/会话切换的独立实机组合未补证。

RP04 的早期 Playwright 命令漏设 `E2E_EXISTING_ACCOUNT_ONLY=1`，当时的全局初始化重置了5个既有 `model_tester` 测试账号密码；无新账号，角色未改变。已修正 `global.setup.ts` 为默认只使用现存账号，漏设开关不再进入账号管理写路径；全新隔离环境须显式 `E2E_PROVISION_ACCOUNTS=1`，并先核查无现存凭据/测试账号。fake-fetch 受控4项、真实无开关E2E1项及账号时间戳复核通过。真实已结束任务的浏览器后台/会话切换、刷新回看和执行账本不变另有E2E1及内置浏览器证据。原5个账号的凭据变更仍未回滚。详情见RP04报告。

### TL-02 审批失败回读增量（2026-10-12）

Runtime 审批提交失败不再被 Web 回调吞掉并误记为已决定。失败后审批卡只读回查：
已批准、拒绝、取消、过期或已消费保持不可操作；回查失败保持不可操作并说明结果未确认。
仅回查仍为 pending 时允许用户重新核对后手动提交同一审批 ID，不自动重发决定或创建新 turn。
Runtime 的 pending/未过期条件更新先于执行唤醒，只允许同一 ID 成功结算一次；即使原请求
与手动请求竞态，也不能重复唤醒。错误 HTTP 状态不证明动作未执行（事件持久化可能晚于决定）。
受控生产回调与审批卡进程内测试覆盖上述状态、重复点击及失败后手动恢复；不替代真实
浏览器、数据库并发、Gateway/Runtime 联调或工具副作用验收。

### AR-02 历史绑定 ID 的按需读取（2026-10-12）

历史消息仍保留 `metadata.artifact_ids`、但文件列表未提供可用详情时，用户点击与该绑定 ID 完全一致的规范下载链接，才调用现有产物详情 GET。详情必须返回相同 ID、相同规范下载地址及有效文件名；未完成文件不尝试下载。随后复用独立认证下载，服务端再次检查当前用户、租户和来源访问；详情成功不代表随后下载必然成功。读取失败沿用安全本地化反馈；重复点击在当前读取结束前合并，后续手动点击重新读取详情，不缓存授权、不自动重试。

部分文件列表水合时保留消息原有绑定 ID，只为实际返回的文件构造详情，不因同消息另一文件成功水合而丢弃缺失文件的绑定。此增量只覆盖明确消息绑定 ID。完全没有绑定元数据、非规范/外部地址仍保持原导航；不从正文猜测 ID，不发现任意链接对应的文件，不声称修复所有历史链接。离线生产模块受控回归与真实浏览器、原生 API/存储验收分开记账；AR-02 总体验收状态不变。

### TL-05 连接目录读取失败恢复（2026-10-12）

助手连接器面板将目录 GET 失败与成功的空目录分开：失败时明确提示当前状态无法确认，保留此前读取的连接和计数，但不显示旧连接状态或操作按钮。用户可明确重试只读查询；重叠重试只发一次读取，关闭或切换面板后的旧响应不能更新新视图。读取失败不自动连接、激活、断开或重试，不清除服务端配置。

离线回归执行完整生产组件，受控调度与 HTTP 输入覆盖首次失败、成功空目录、已有连接读取失败后的恢复、并发重试合并及旧响应隔离。此证据不等同真实浏览器或原生 API 验收，不宣称 MCP 健康状态全链路验证完成。

### TL-05 每个连接器的 MCP 状态读取失败（2026-10-12）

目录成功返回的“已连接”事实与每个连接器的 AI 工具状态分开观察。MCP 状态 GET 失败或返回非布尔的激活状态时，面板保留目录连接事实和计数，但明确显示工具状态无法确认，不沿用旧激活标记，也不将未知当作未激活或已断开。未知状态不提供基于“未激活”推断的激活按钮；用户可单独重试该连接器的只读状态查询，其他连接器的已知结果不受影响。查询期间显示正在检查，重复点击只发一次 GET；关闭面板或重新读取目录后，旧状态请求的成功、失败和结束回包均不能覆盖新观察或释放新请求的占用。

本增量不自动连接、激活、断开或重放修改操作，不改变服务端配置和权限。离线生产组件回归覆盖初次状态失败、旧激活标记失效、手动读取恢复、逐连接器隔离、重复读取合并与旧回包隔离；真实浏览器、Native API 和第三方连接健康仍未验收。目录连接事实与工具状态查询并非服务端原子快照，不能解释为持续健康保证。


### TL-05 插件版本能力影响预览（2026-10-13）

插件设置的生命周期修改现将现有目录中的可见能力描述连接到提交前的影响预览。以工具名称对齐，显示加入、移出、描述变更和未变更；比较能力 ID、版本、Schema 哈希、分类、风险、读写影响、必需权限和声明权限，并并列展示原值与拟议值。当前目录可用性单独展示，不当成保存后的准入预测。

仅当前受管且描述仍匹配的已启用选择可作为已知原始集合；已停用选择的准入集合为空。部署默认、旧版本不可用或目录描述已变化时，历史原值保持未知，只列出拟议能力，不伪造新增/移除结论。目录按当前权限过滤，预览不枚举隐藏能力或受影响 Agent 引用，不代表完整影响分析或安全证明。

用户必须核对当前具体选择及未知范围后确认。确认绑定租户、插件、生命周期状态/修订、目录修订、目标版本/启停与所见描述；修改候选、取消、目录/身份变化均使旧确认失效。提交仍使用原有租户和修订 CAS；没有新增授权、自动批准或重复提交路径。已有执行的取消及新调用的最终准入仍由后端负责。

离线生产模块与受控组件验证和真实验收分开记录：此增量不代替真实 Gateway/数据库并发、浏览器、Runtime/Worker、跨权限完整性或 TL-05 全旅程验收。


### TL-01：返回工具集合的只读检查（2026-10-13）

- 工具弹窗分别显示平台目录、下一次运行预估和最近运行固定集合；直接使用各自返回的描述项，因此 MCP/deferred 或仅旧运行存在的工具不会被目录交集隐藏。
- 每项展示返回的租户权限、effect、approval、设备与必填输入；缺省权限/元数据明确为“未报告”，不推导授权或免审批。预估不可用与无固定运行分别说明；集合成员不代表执行成功。
- 名称/描述本地筛选与返回数量、显式刷新/失败重试；读取受会话、认证代次、请求及关闭/重开生命周期保护。没有新增权限修改、后端协议或模型调用。
- 验证范围：受控生产组件回归覆盖不相交集合、筛选、加载/失败/重试、刷新和会话/认证/关闭后的过期回调；7/7 定向测试、app/node TypeScript、scoped lint（零警告）与 i18n 检查均通过（测试源清单 `3b9eebc8d33d60a5d7caa5c1c278b9cf271eafc0a77e9983d7bc9dc3e52f7e70`）。无真实模型、服务或浏览器验收；客户端集合显示不构成服务端完整目录或实时可调用性保证。


### AR-01/AR-02：已保存文件列表恢复（2026-10-14）

产物抽屉提供显式刷新/失败重试，仅调用当前会话已有文件读取 API，不重新生成、不修改聊天、草稿或执行状态。加载与运行期间禁用新刷新；一次读取尚未结束时重复点击不重复发请求。读取失败保留已有列表，确认当前账号/会话不可访问（403/404）则清空列表并给出安全提示。

每个已有产物写入入口同步增加本地版本。手动读回只有仍属于同一认证、会话、抽屉开关代次、历史恢复及流代次，且没有更新产物时，才完整替换列表；所以空的成功快照能移除已不在返回集合的旧文件，但旧快照不能覆盖新的流产物。若读取途中有新产物或新运行，保留新列表并提示重新显式刷新。初始历史/产物对账未完成时不并发启动手动读回；新聊天、发送接管或认证变化同步退役旧恢复所有权，不等待旧网络返回；不改变已有来源权限检查和认证下载。

验证：11/11 定向测试、app/node TypeScript、scoped lint（零警告）与 i18n 检查通过（测试源清单 `654b6ae872549f1484840db5f3f9504c4b3ac34862e32860fd76e6f8d7f00834`）。生产恢复 hook/API/抽屉与从 useChatSession 提取的实际版本/抽屉 setter、认证订阅的受控回归，覆盖失败重试、空快照、拒绝访问、重复点击、认证/会话/关闭重开、放弃恢复及流产物竞态。不等同完整浏览器、存储删除、模型生成或所有历史产物来源权限验收。


### TL-05：OAuth 返回个人连接（2026-10-14）

Python 与 Native OAuth 成功回调返回已登录用户的 `/connections`，复用助手现有个人连接面板；不再把普通成员送到需要设置权限的供应商配置页。个人页面重新读取当前账号的目录与 MCP 状态；URL 的 provider/status 参数不能证明连接或工具已激活，不自动激活工具。账号、登录会话或权限变化重建面板，关闭返回工作台。`/settings/connectors` 的原权限及配置管理保持不变。

受控路由/组件与回调测试和真实 OAuth、浏览器及第三方健康验收分别记账；未进行真实登录、凭据交换或工具执行。此处仅修正成功回调的返回位置，授权拒绝/回调缺少 code、交换失败沿用既有错误行为。本增量不表示 TL-05 完整验收通过。


本增量离线验证：Web 33/33（生产路由/权限守卫与个人页/连接面板，受控认证、Hook 和 HTTP），Python OAuth 状态/回调 8/8、两端 scoped lint、app/node TypeScript 与 i18n 检查通过。Native 实际 foundation library check 与公开回调函数 integration tests 2/2 通过（零失败/忽略）；源、工具、依赖与保留制品身份均验证一致。首次 Web 四项新增用例因测试加载器 default-export 包装错误失败，修正仅限测试并保留原失败记录。未进行真实浏览器、OAuth/provider、完整产品或 TL-05 全旅程验收。

### TL-05：授权拒绝返回（2026-10-14）

Python 与 Native 回调允许无 code 的非空 provider error。沿用已签发状态的期限、tenant/user/provider/nonce 校验与消费后，只返回固定 `/connections?status=authorization_failed`；拒绝路径不读取配置或解密密钥，不交换 token、不更改连接。当前 GET+DELETE 状态消费仍有并发窗口，本增量只验证顺序重放拒绝，不声称新增原子单次消费。

个人面板仅将该 URL 值作为“返回链接提示授权未完成”的不可信提示，仍通过当前登录账号 GET 判断连接；提供现有手动连接入口，不自动重试或激活。错误文本、error_description/error_uri、code、state 与账号信息均不反射到返回 URL/页面。缺少 code/error 保持 422，混合 code+error 或空 error 为 400；无效/过期/不匹配状态、存储异常、交换失败仍保留 HTTP 错误，不重定向或自动交换重试。成功回调及固定来源校验保持不变。

验证范围为离线生产 Python ASGI、Native 实际挂载 Router/服务与 Web 组件的受控回归；实际执行结果另记回执。没有真实 OAuth、浏览器、数据库/Redis 并发或第三方健康验收，不表示 TL-05 全旅程完成。

本增量离线执行结果：Python 实际状态/ASGI/公开查询参数合同 27/27、Web 生产个人页/连接面板/路由受控回归 35/35、两端 scoped lint、app/node TypeScript 与 i18n 检查通过。首次轻量验证 Python 27 项通过，但测试新增 import 分组触发 3 个 lint 错误；保留原失败记录，仅补 4 个空行后，使用同一快照重新执行上述七阶段并全部通过。

Native 实际 foundation 库检查 14.814 秒通过，真实挂载 callback Router 的受控服务测试及既有返回函数测试共 8/8 通过（35.121 秒、零失败/忽略）；生产 service/API 与测试文件由实际编译 dep-info 绑定。测试仅增加已锁定 tower 0.5 util 的 dev-dependency 边以在进程内调用 Router，不新建监听器或引入生产依赖变化。单作业、locked/offline、debug/incremental 关闭、外部输出、资源守卫以及源/依赖/保留产物身份复核均保持；没有守卫终止。一个共享源码快照和既有依赖清单按引用复用，未重复克隆。上述结果不代表真实 OAuth/provider、浏览器、Redis/数据库并发或完整 TL-05 验收。


### OAuth state atomic consumption (2026-10-14)

Python and native connector callbacks consume issued state with one Redis GETDEL
before checking the decoded record. This requires Redis >=6.2; the repository's
Compose and Helm defaults use Redis 7. Redis documents GETDEL as available since
6.2.0: https://redis.io/docs/latest/commands/getdel/. Custom adapters must support atomic
consumption; command/connection errors fail closed with 503 and never fall back
to GET plus DELETE. Shared RedisStorage retains its existing JSON decode layer.
Malformed records are intentionally consumed before decoding; expired/mismatched
records remain rejected, and denial/success HTTP behavior is unchanged.
Concurrent controlled-store callback tests and production-adapter command tests
are offline evidence only, not real Redis or provider/browser acceptance.


Offline verification outcome (2026-10-14): Python 32 tests and scoped Ruff
passed. The first run's tests passed but lint rejected a constant setattr;
the exact test-only assignment repair and original failure are retained, and
both light phases were rerun successfully. Installed redis-py 8.0.0 dispatched
GETDEL through the production API at a controlled command boundary.
The current native gateway library check passed; the actual adapter packed-command
unit test passed (174 unrelated unit tests filtered), and all nine connector
callback tests passed, including the existing eight and deterministic contention.
Native phases had no failures, ignored tests or retries; peak observed process-group
RSS was 1,923,010,560 bytes. Source, dependency and tool identities remained stable.
These are controlled offline callback/adapter checks, not live Redis, provider,
browser, deployment or complete TL-05/enterprise acceptance.

### AS-07 长活动列表导航增量（2026-10-14）

活动抽屉每次最多挂载50个已加载步骤，保持现有步骤顺序和各步骤状态；较早/较新/最新按钮只切换显示窗口，不丢弃历史或重新执行任务。首次打开显示最新已加载步骤；任何页面（含首次打开的页面）在追加活动后都保持起始身份，用户明确点击“最新活动”才跳至最新窗口。切换消息重置窗口；范围文案仅描述已加载步骤，不宣称服务端历史完整。待审批卡片置于历史列表前，继续使用原审批授权、过期和终态规则。

离线生产组件测试覆盖1000步窗口、逐页遍历、追加、消息切换及审批可达性。组件输出的步骤描述符数量和受控交互不等于浏览器帧率、滚动/焦点或390px实机验收；真实长任务恢复/停止、浏览器和完整J03仍需独立验收。


### TL-05: Expired authorization recovery (2026-10-14)

The personal connection panel distinguishes the server's expired and revoked
statuses from a never-connected provider. Expired connections offer explicit
reconnect and disconnect (clearing saved tokens); revoked connections offer
reconnect and do not imply that the provider has revoked its remote grant.
All panel mutations have synchronous ownership and disabled controls until
completion. Closing/reopening retires callbacks. The shared panel uses the
existing authenticated scope/key pattern, so authentication replacement retires
callbacks from both personal `/connections` and the Assistant entrypoint.
Disconnect and activation reconcile through authoritative catalog/status reads,
including after transport errors; failed readback exposes GET-only retry rather
than repeating a mutation. OAuth is initiated only by an explicit user action.

Controlled component tests are offline evidence, not real provider authorization,
revocation, callback exchange, browser or database acceptance. Callback exchange
errors and invalid/expired state still use the existing backend HTTP error paths;
this increment does not claim all callback failures return to the personal page.

### TL-02 审批到期恢复增量（2026-10-14）

Runtime 的持久审批行可能在原 waiter/恢复清理完成前仍标记 pending，但摘要已返回
非正的 expires_in_seconds。Web 现在同时读取这一权威剩余期限，即使动作预览不可用，
也显示已过期并禁用两种决定；点击前刷新与不确定 POST 后只读回查使用相同判定。
已批准/拒绝/取消/consumed/unknown 的结果不会仅因原期限过去而被改写为过期。
未过期但无法核验动作时仍只允许明确拒绝；本增量不产生自动重试、重新授权或工具执行。
受控生产组件回归与当前类型检查分别记录，不代表真实 provider/浏览器、SQL 并发或
完整 J07/CL05 验收；Runtime/Worker 的原执行身份、原子审批和未知副作用屏障保持不变。


### AS-03：恢复运行的停止请求归属（2026-10-14）

恢复运行的 Stop 操作绑定到读取时的会话、运行和恢复/流代次。发送前与失败回包后均核对同一归属；切换会话、同会话换运行、新聊天、认证更换、发送接管或卸载使旧回调失效，切回相同会话或账号也不复活旧归属。当前归属下重复点击只发送一次取消请求；未受理或请求结果无法确认时保留原运行观察，仅允许用户明确手动重试，不自动发送提示词、恢复执行或创建新 turn。

取消接口返回 accepted/cancelled 只表示请求已受理，界面继续等待原有只读历史/运行状态对账确认终态，不能解释为外部动作已经停止。受控离线生产 Hook 与父 Hook 生命周期回归不等同真实浏览器、Gateway/Runtime/数据库并发、工具副作用停止或完整 J01–J03 验收；实际执行结果单独记账。


### AR-02: Metadata recovery independent of storage URL signing (2026-10-14)

The retained Python artifact list/detail/create responses already expose only the
authenticated Gateway download route. They no longer generate and discard a
storage presigned URL first: a signing/client initialization failure cannot hide
otherwise readable metadata or turn an already completed creation into an error.
Session/owner/tenant/source checks and readiness are unchanged. Download still
rechecks current source access and reads actual bytes; metadata success never
promises successful download or refreshes an expired external storage link.
Offline production-router regressions are separate from real storage, browser,
Native Gateway and complete J08/J30 acceptance.


### Source-access regression fixture recovery (2026-10-14)

The retained Python source-access tests now supply an owned, tenant-bound builtin
Session through their in-memory session-manager fixture. The production session
storage and pin guards remain unchanged: the previously missing dependency had
stopped eight existing tests before their intended source-revocation assertions.
All original ACL, document-version, history filtering, approval redaction/rejection
and no-runtime-dispatch assertions remain intact. Explicit missing-storage cases
cover thread reads, turn creation, recovery, approval reads and approval acceptance,
requiring SESSION_STORAGE_UNAVAILABLE and no Runtime calls. This is offline test
fixture recovery, not a production authorization change or live ACL acceptance.


### J05 attachment cancellation recovery (2026-10-14)

Removing an in-flight attachment aborts its upload transport; removing a queued
attachment prevents its later dispatch. Send readiness follows the remaining visible
pending/uploading entries, so removed work does not leave the composer blocked.
Clear and unmount retire uploads; late progress/results cannot restore removed chips.
Queued and active entries follow stable IDs when a draft is bound to a session.
Partial failures retain individual retry and successful-file selection. Existing
ATTACHMENT_UNAVAILABLE handling still preserves the draft and requests manual
removal/reupload, without automatically starting another run. Cancellation does not
claim deletion of any server object already received or deletion of the user's file.
Controlled hook/transport checks are separate from live browser, storage expiry and
provider acceptance; J05 remains subject to those acceptance gates.

### TL-02 explicit approval read recovery (2026-10-15)

The approval card offers a read-only current-request check after an initial
preview read failure or an uncertain decision whose readback also failed. It
uses the same owner-bound Runtime/SDK approval GET and never resends a decision.
A verified pending result permits the existing separately chosen decision flow;
resolved, inactive and expired results retain their existing non-actionable state.
An unavailable result remains unconfirmed. Checking does not claim a tool ran.

Reads and decision preflight are single-flight in the mounted card. Existing
authentication, request identity, source-access, exact binding and expiry checks
remain in force; stale card callbacks cannot initiate a check. Controlled
component tests are offline evidence, not browser, service, database concurrency
or complete J07 acceptance.

### AS-03 image task receipt checks (2026-10-15)

An uncertain image outcome with a known imt_* diagnostic ID now offers an explicit
read-only task check. The existing owner-authenticated GET must return the exact
task and current conversation, and the image turn when already known. Missing,
denied, mismatched or malformed receipts remain unavailable. No receipt is fetched
automatically; reads never dispatch generation, resume work or claim cancellation.

The separate current-view receipt shows the saved status, valid reported progress
and effective model when present. Actual producer states include pending, claimed,
running, completed, failed, unknown and dead_letter; future nonempty status strings
remain visible rather than being converted into success. No generated URL, prompt
or raw backend error is rendered, and the original chat outcome/history is not
rewritten. Authentication and selected session/task lifetimes retire old handlers
and responses; double clicks share one read. Controlled component/API tests are
offline evidence, not provider polling, browser, database or full AS-03 acceptance.

### Python V1 non-streaming terminal integrity (2026-10-15)

The Python `/api/v1/assistant/chat` aggregation requires this run's governed
`run_finished`/`succeeded` terminal before returning a successful ChatResponse.
Recovery terminals may omit the envelope; supplied terminal envelopes must agree
with the run identity and status. Other runs' events cannot complete this request
or contribute text. Failed, cancelled and blocked outcomes return safe HTTP409
errors; incomplete, contradictory or unknown outcomes return HTTP502. Errors
retain the server-owned run/session identities for explicit status inspection and
do not expose partial answer text or raw Runtime failures. There is no automatic
resubmission or cancellation claim. Empty text with a genuine success terminal
remains successful.

The existing conversation-source guard remains authoritative. Revocation is
sticky and returns HTTP403 without previously collected text. Both stream layers
are closed on failure/cancellation. These focused ASGI checks use controlled
Runtime IO, not a live service or provider. Native Gateway aggregation remains a
separate known gap pending native implementation and admitted validation.
Citation contexts and usage aggregation are also deferred: this change does not
invent source projections, token counts or accounting finality.


### Python non-streaming governed citation response (2026-10-15)

The Python V1 chat collector retains matching run/session/thread context_retrieved
from the guarded Runtime channel, with a nonempty tool_call_id. This is channel
provenance, not a cryptographic proof. A successful terminal is still mandatory.
Before returning citations, the existing Gateway Knowledge HTTP proxy rechecks
current dataset/document access and exact immutable source version/hash access.
Failed, unknown, revoked, or unavailable source checks never return partial output.

Response contexts contain current dataset ID/name and bounded chunks. Chunk
metadata allows only document/segment identity and supplied source version/hash;
arbitrary tool metadata, arguments, tokens, links, and source references are not
copied. The hash identifies the full source, not the displayed excerpt (maximum
2000 characters). At most 20 datasets and 20 chunks per dataset are retained;
identical projected chunks are deduplicated while distinct excerpts remain.
Authorization references are tracked independently, including hidden/missing-call
contexts, and all are rechecked after cleanup. More than 256 combined unique
dataset/document/version references fails closed rather than dropping authority
checks; document/dataset identifiers over 512 characters also fail closed.
Absent query/timing/score data is omitted; only finite supplied numeric scores
are retained. Usage remains empty pending real attributed usage evidence.
Legacy unversioned citations stay unversioned and require current document access.

Verification is controlled mounted-ASGI plus actual source guard and SDK response
decoding with mocked Runtime/Knowledge IO. Live Worker, database and native Gateway
parity remain separate acceptance requirements.


### Offline Python citation HTTP composition (2026-10-15)

A composed regression fixture connects the actual Python SDK knowledge.ask and
HTTPTransport, mounted Gateway chat/source-proxy routes, KBProxyClient and
InternalServiceClient, Knowledge capability/catalog/document/source-authorization
and immutable-source handlers, and the real v2 gateway-signature middleware.
All HTTP hops use in-process ASGI transports, with synthetic secrets and an
in-memory replay store. Wire assertions bind owner, tenant, session/run, request
body and source hash. Unsigned and identity-tampered private calls are rejected.

The controlled Runtime adapter requests signed retrieval and maps its returned
rows using the inspected Runtime projection contract before emitting a terminal;
this fixture does not execute Rust Runtime or Worker. Storage/retrieval, actor
resolution, and Gateway model/session admission are controlled doubles. Assertions
cover stale-index identity replacement, exact full-source opening, modified
version/hash rejection, revocation before answer/source read, failed terminal,
invalid capability proof, unversioned legacy citations and single submission.
Current focused producer, aggregation and terminal/source-guard suites accompany
this composition. No live DB/provider/native acceptance or attributed usage is
inferred from this bounded offline check.

The composed source-opening step now uses the public SDK knowledge.get_source
method with the returned citation identity, exercising its bounded JSON read and
full-content SHA-256 validation through the same in-process service composition.
The controlled Runtime/storage and offline acceptance boundaries above still apply.
