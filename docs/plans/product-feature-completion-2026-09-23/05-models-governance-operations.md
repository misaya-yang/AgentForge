# 05｜模型、治理和运营页面

依据见[现状E10～E15、E19](01-current-state.md)。以下按用户职责拆模块，不要求拆服务或新增一级菜单。

## M07：服务、厂商与模型管理

**用户/目标。** 管理员接入一个可用模型，验证必要能力，并让相应用户在正确入口使用。已有服务/厂商/模型三页签与配置、同步、能力编辑接口；需要把“保存了配置”和“真正可用”区分开。

**旅程。** 选择厂商模板/自定义接入 → 保存凭据引用 → 验证连通性 → 同步/选择模型 → 验证需要的能力 → 设置可见范围和默认值 → 在调试/助手试用 → 停用或修改。

| ID | 优先级/类型 | 需求与规则 | 可判定验收 |
| --- | --- | --- | --- |
| MO-01 | P0 验证 | 厂商、模型、服务身份与租户归属明确；同名模型可区分来源，配置回读一致 | 两厂商同名模型不覆盖；错协议/地址/缺凭据定位到接入问题；当前任务用固定配置，新设置只影响声明范围 |
| MO-02 | P1 补齐 | 文本、工具、图片、结构化输出、搜索等能力分别标注声明值、实测状态、测试时间及适用协议 | 文本通过不自动让全部能力变绿；未测试显示未知；能力失效后选择器提示影响；模型测试不暴露密钥 |
| MO-03 | P1 补齐 | 模板、模型同步和自定义编辑预览新增/变化/停用影响；用户手动覆盖不被无提示覆盖 | 同步两次不重复模型；厂商删除模型可定位受影响Agent/默认值；测试失败仍能编辑修正，不需重建整个厂商 |
| MO-04 | P1 验证 | 启停/默认模型/价格与路由设置有清楚生效时间，删除被使用对象显示影响；保留修改审计 | 停用后新任务拒绝或按明确策略选择，运行中不换模型；历史费用不被新价格回算；错误修改可恢复原配置 |
| MO-05 | P1 补齐 | 首次配置引导：配置前提 → 最小连通测试 → 可用模型 → 首次成功调用；普通成员看到“联系管理员”而非管理员密钥表单 | 无模型部署能完成引导；无可选OpenAI凭据但默认Qwen可用不算失败；环境配置与UI配置覆盖关系可解释 |

MO-01/MO-04 的同名模型选择链路见[厂商与模型成对选择](../../design/provider-model-selection.md)：Web 保存并发送厂商身份，V2 首次建线程、启动轮次与图片能力检查使用同一身份；旧版仅模型 ID 的记录只在唯一可解析时恢复。此项不代表实测模型能力或完整端到端验收。

MO-01/MO-02 的厂商连接测试只读取服务端已保存配置，不测试编辑器中尚未保存的草稿。
保存成功不表示连接验证成功。当前页面收到保存成功后立即清除该厂商旧测试反馈，
即使后续列表刷新失败或尚未完成；回读到厂商新 `updated_at` 时也使旧反馈失效，
因此仅修改凭据也无需在客户端保存凭据副本或生成秘密指纹。失败的保存不会被当作
已确认的新配置，也不自动重放保存或测试。

测试结果和正在测试的状态绑定到当前观察到的厂商身份、配置时间戳及本页保存代次；
切换、卸载或出现新代次后，旧测试的成功、失败和结束回包不能覆盖新视图状态。
一次正在进行的测试只派发一次，用户可在结束后明确再次测试。此限制不停止已发出的
上游请求，也不是服务端版本锁：列表回读或新的本页保存成功之前，无法知道其他客户端
是否修改了服务端配置；时间戳只是既有 API 的观察信号，不保证并发原子性。
连接测试不验证所有模型声明能力，也不代表当前始终可用。

离线回归执行实际 Services 保存回调、ProviderForm、API 序列化和 ProviderCard 模块，
使用受控 React 调度、表单值、查询/修改适配器和 HTTP 替身，覆盖保存后回读失败、
凭据编辑的新时间戳、旧测试迟到以及显式重测。这不等同浏览器、真实供应商、数据库
或真实 React/TanStack 调度验收。

MO-01/MO-04 的模型列表使用租户、厂商和模型的完整身份区分行与启停状态。
启停在本页同步锁定，保存前重新读取；已观察到状态或更新时间变化时不写入，要求核对。
操作明确禁用修改重试，成功和失败都只回读，不自动重放。只有响应已接受且对应模型
状态回读匹配才显示确认；响应不确定、回读失败、对象消失或状态不同时，保留观察值并
要求显式刷新核对后再操作。启停等待中保持禁用，避免同一渲染内重复点击或迟到回包
提前解除另一行的等待状态。同名模型的 React 行身份也包含厂商与租户。

管理列表使用现有认证范围机制隔离缓存；账号、凭据、权限变化或页面离开后，旧请求
不能发布读取、提示或发出后续写入/回读。该范围不改变其他厂商/模型 CRUD 行为。
API 没有状态版本锁，预读与写入之间仍有竞争；回读只是当时的观察值，不承诺未来
状态或运行中任务行为。离线回归使用受控调度/传输，不等同真实浏览器、数据库、
多管理员并发或停用后的新任务准入验收，不能据此关闭 J19。

## M08：模型调试 Playground

**用户/目标。** 模型管理员和model_tester验证单模型请求与参数效果，保存可复现问题。已有服务选择、会话和流式调试；其任务是诊断模型接入，不承担完整Agent工具规划。

| ID | 优先级/类型 | 需求与规则 | 可判定验收 |
| --- | --- | --- | --- |
| PG-01 | P1 验证 | 请求编辑、模型/服务选择和参数只展示实际支持项，明显显示当前有效值及所用协议 | 同一参数在调试与真实调用含义一致；不支持值发送前解释；切模型保留用户文本并提示参数调整 |
| PG-02 | P1 补齐 | 流式响应、停止、错误、usage和耗时按来源展示；记录可重现的请求快照 | 首帧前/后失败可区分；停止不伪装成功；缺usage显示未知；复制请求不包含Authorization或原始密钥 |
| PG-03 | P1 补齐 | 调试会话可保存、回看、清空；从模型管理进入时带入目标，从故障返回时保留请求 | 重开保存会话可恢复参数；清空只影响选定会话；model_tester仅能访问已授权范围，不因调试绕过限制 |
| PG-04 | P2 新增 | 同输入的多模型对照作为独立增强，明确请求数与预计成本；允许先用已有Eval完成对照 | 显式开始后才发各模型请求；部分失败分别显示；不把不同采样的结果当严格质量对比 |


PG-02/PG-03 的本地停止与会话切换使用同一请求归属边界：普通模型、Dify、LangGraph
和 SDK 流在点击停止时立即保留已显示内容并标记本地 `cancelled`，不等待传输返回，
也不将部分输出改称成功。选择、新建、删除当前会话或切换服务时，旧流失去更新消息、
loading 和标题的权限；账号/凭据变化后的迟到回包同样不得更新当前视图。停止若发生在
调试会话初始化等待期间，初始化返回后不再启动模型请求。`cancelled` 只说明客户端
停止读取，不证明上游任务、工具副作用或费用结算已经停止；SDK 的服务端取消仍走已有
运行身份控制。停止不自动重放请求，用户可显式重新发送；直接重新生成也会先终结被替代流的本地状态。
新建会话先进入独立空白视图，再等待服务端身份；创建完成仅绑定身份，不清除等待中
发送的文本。创建失败或响应不确定时，不自动再次创建、不把请求发往前一个会话；
无待发送消息时显示安全错误提示并停留新视图，有待发送消息时由该消息展示失败。
只有用户再次点击新建或显式发送才启动新的尝试，前一会话仍可从历史中选择。

PG-03 的导航代次独立于发送代次：选择、新建、切换服务、账号/凭据变化和卸载后，
旧创建、列表、历史及线程准备的回包不得发布到新视图。同一新建操作重复点击共享
一次创建；同一视图发送可继续等待原线程准备，旧发送也不能清除新视图的线程 promise。
侧栏刷新不阻塞已创建会话的首次发送，初始恢复未完成或失败不阻止服务切换。
这些是客户端归属与失败处理，不证明服务端创建精确一次；迟到成功可能留下可从
服务端列表找回的空会话，客户端不会自动删除它或重复模型调用。

生产 hook 的离线回归覆盖首帧前/后停止、停止后新会话重新发送、迟到失败、账号/服务
切换、新建→发送→选择、重复创建失败、列表/线程准备竞态及现有 SDK 流语义。它们使用传输与 React 调度替身，不等同于浏览器、
真实供应商或服务端取消验收；请求快照导出和参数恢复仍待单独验收，不能由这次修正
推定 PG-01～PG-03 全部完成。

PG-02 的非 SDK 流完成必须有协议终结证据：Gateway 终结块、归一化终结事件或
LangGraph 根 `end`（含无 data 行）或 `[DONE]`。`messages/complete` 只结束
一条消息，不能跳过后续消息或错误；带命名空间的 `error` 仍表示失败，显式 `custom`
事件的内层同名字段保持业务数据。SSE 控制帧保留仅由 Playground 显式开启，其他
共享传输消费者及 SDK 数据流保持原行为。Dify 确认终结的空输出也可成功。
Dify 已收到的非终结文本增量在保存上游会话绑定前进入当前轮次，绑定保存失败时保留
这部分文本并明确失败，不重放模型请求。保存仍按顺序等待；终结块在绑定保存成功后
才处理。等待保存时停止或切换会话仍遵守当前请求归属，迟到回包不更新新视图。
离线 hook 回归使用受控传输和保存 Promise，不证明真实浏览器、服务端持久化或上游取消。
只有连接 EOF、部分文本、工具调用或历史答案不表示成功；缺终结
标记的流保留已收到的文本和工具信息并标记失败，明确上游可能仍在运行。传输在首帧
前失败也不证明请求未执行，因此不再自动改用同步 invoke 重放。只有显式再次发送
才能开始新模型请求。LangGraph 确认终结的空输出或仅工具输出也可成功，不依赖
线程历史读取。旧历史答案没有可靠的当前 run 归属，因此不再自动替代本轮缺失正文；
将来的历史补全必须有运行身份绑定，并且不能决定已确认终结的成败。该约束沿用现有取消、
会话初始化与 SDK 归属，不提供上游终止、精确一次执行或真实浏览器验收保证。

非 SDK 用量展示现在区分服务端返回值、未知字段和独立的本地文本估算：缺少输入、输出
或总量时逐项显示“未知”，不将缺失当作 0、不把部分字段相加伪装为完整总量。明确返回
的 0 保留；未标注来源的服务端数值显示来源未知，不能称为实测或已结算费用。只有
Playground 自己调用文本估算器得到的数值才显示“本地文本估算”，独立保存，不回填到
reported 用量字段、不用于配额或计费。该估算仅覆盖可见输入/输出文本，不包含历史
上下文、系统提示词、工具和隐藏推理，也不计算费用。

可选的 `usage/stats.token_provenance` 逐项标注 `input_tokens`、`output_tokens` 和
`total_tokens` 的 `upstream` / `estimated` / `derived` / `unknown`。Web 只接受与有效
非负安全整数计数（含旧版十进制字符串）同时出现的已知标记，并显示为上游返回、服务端估算、计算值或来源未知；
上游返回仍不等同于独立验证的实测值。无标记的新旧服务端仍可使用，原数值不被重新
计算。字段覆盖时清除旧来源，只有时序信息的后续帧不清除已收到的计数。LangGraph
单消息 usage 只是当前收到的观察值，不宣称聚合了整个多步运行。

手动保存的 LangGraph 会话在 `stats.token_usage_source` 中保存显示来源，另存
`stats.text_token_estimate`，回看时恢复对应标签。旧记录没有来源时保留有效数值并
显示来源未知；无用量也显示未知。停止和失败保留已收到的用量，不改变取消、终结、
会话/账号归属或 SDK 专用用量合同。这是 UI 的来源区分，不修改服务端账本、配额、
定价或历史记录。生产函数与实际统计组件的离线回归覆盖缺失、0、部分/非法数值、来源
混合、会话回看及停止/失败，不能替代浏览器或真实供应商验收。

上游未标注估算的具体兼容边界仍需后端独立验收：原生
`platform-gateway-foundation/src/stream_statistics.rs` 的终结/会话统计与
`usage_parser.rs` 的同步历史补全，以及 Python `src/core/gateway/dispatcher.py`
的同步历史、终结块和最终历史统计，都可能补充规范 token 字段。已有 alias 与补充的
规范字段同时存在时，Web 保持规范字段优先，不擅自改变已返回数值；不能用账单中
`token_source` 或字段存在推定计量来源。请求快照、参数恢复、后端来源标注和实测
验收仍分别跟踪，PG-02 不因本次展示修正被整体标为完成。

PG-02/PG-03 新增“复制请求快照”：在实际发起的每次流请求处捕获客户端观察值，
包含本次服务、Gateway/LangGraph 协议、会话/线程或 Dify conversation 身份、Memory
开关，以及实际发送的 LangGraph stream_mode/durability。当前页面的快照包含本次
提交文本，复制按钮明确提示分享前检查敏感内容；它不包含传输 Authorization、
服务端凭据、供应商地址或任意 connector metadata。用户文本自身仍可能含有秘密，
因此不能把包含文本的导出称为完全脱敏。快照不随后续开关编辑重新计算，也不提供自动重放。

仅当该请求实际具有 effective session 时，客户端通过现有会话 PATCH 的独立
`metadata.playground_request` 字段保存最近一次请求的设置，不在 metadata 重复
存储提示词。重开时从选中会话的 GET 读取（列表摘要不保证包含该字段），核对版本、
服务和会话身份；显式返回的服务身份不匹配也拒绝恢复。旧版 GET 没有 service_id 时
仍可核对快照自身绑定。合法的 Memory=false 明确保留；无有效会话的无记忆请求仅在
当前页面保留，不把设置写到此前选中的其他会话。新建、切换服务/会话和账号变化后，
旧读取不得恢复到新视图；读取等待期间用户已编辑的 Memory 值也不被覆盖。

保存按当前页面的队列顺序提交，旧保存完成不能改变较新快照的显示状态。诊断 GET/PATCH
沿用共享 API 的地址配置和认证处理，并设置 10 秒超时；普通失败只影响设置保存或
恢复，不把模型成功改成失败、不再次发起模型调用。同一有效身份的 401 仍按既有规则
清除失效登录并跳转，不因诊断用途隐藏权限撤销。收到 PATCH 成功仅表示该次接口确认，
不代表跨浏览器多写者版本锁或永久持久化保证；超时也不能证明服务端已停止写入，
旧请求仍可能在服务端晚到完成，重开看到的仅是当时保存值。停止后仍可复制已捕获的本次请求；切换
视图或账号后的旧复制动作失效。恢复快照明确标为“仅请求设置”，不从当前输入或历史
最后一句猜测原请求文本。

这是最近一次客户端请求的诊断与受支持设置恢复，不是完整逐轮请求档案或确定性复现：
服务端模型、采样默认值、历史上下文和上游配置可能已变化，现有页面也没有新增模型/
temperature 等采样编辑器。离线生产 hook 回归覆盖实际发送参数、设置 roundtrip、原生
GET/PATCH/精简列表形状、失败保存、重复发送顺序、停止及迟到读写隔离；不代表真实
数据库、浏览器剪贴板、供应商或跨页面多写者验收。PG-01～PG-03 的完整端到端验收仍未完成。

## M09：任务管理

**用户/目标。** 用户找回离开页面后的工作，知道是否需要处理、结果在哪里。当前页面挂载收件箱与ID查询；定时任务只是未挂载占位，不能称已支持。

**范围建议。** 首轮明确显示当前收件箱实际覆盖的任务源，并支持返回源页面。后续聚合Agent、知识处理、Eval时，以已有任务源为事实，通过关联展示统一入口，不另建第二套执行状态机。

| ID | 优先级/类型 | 需求与规则 | 可判定验收 |
| --- | --- | --- | --- |
| TK-01 | P1 修正 | 页面标题/说明准确描述覆盖范围；列出类型、名称、发起时间、状态、结果入口；定时未支持时不显示可创建入口 | 同一任务从源页面可定位到收件箱或清楚解释暂不覆盖；未知进度不显示确定百分比；按状态筛选与详情一致 |
| TK-02 | P1 补齐 | ID查询、详情、结果、来源会话/文档/评测与失败说明互相跳转；列表支持足够分页 | 用户能找回早于第一页的任务；无权限与不存在按安全约定返回；从详情返回保留筛选条件 |
| TK-03 | P0 验证 | 取消、重试和删除的含义分开；UI先确认请求已接受，再呈现最终状态；保护所有权 | 正在取消不等于已停止；未知副作用不提供直接重放；租户管理员不能借admin通用标签访问其他租户任务，平台全局管理例外必须明确且审计 |
| TK-04 | P2 新增 | 定时任务另立增量：时区、下次执行、暂停/恢复、失败策略及执行记录 | 仅在显式批准范围后实施；夏令时/重复触发/错过执行有定义；不能用占位组件计入功能完成 |

## M10：登录、用户与访问管理

**用户/目标。** 管理员创建、调整或停用成员；成员知道如何登录、改密和申请权限。基线沿用管理员开户。忘记密码当前为联系管理员提示，因此需完善这条处理路径，不能宣传成自助找回。

| ID | 优先级/类型 | 需求与规则 | 可判定验收 |
| --- | --- | --- | --- |
| US-01 | P1 验证 | 登录、过期、退出、首次改密、锁定与停用显示不同处理方式；未提交内容在重新登录后可恢复至安全范围 | 错密码不泄漏账号是否存在；过期后不丢草稿但不重放已发送动作；退出后受保护页与旧凭据按合同失效 |
| US-02 | P0 验证 | 明确普通成员、租户管理员和平台管理员的范围；角色与实际API能力一致，页面隐藏不是权限实施 | 建立动作×角色×租户矩阵；列表、详情、结果、下载、修改均测；跨租户管理仅限明确平台角色；不能用本租户admin隐式获得全局范围 |
| US-03 | P1 补齐 | 用户新增、编辑、角色调整、停用/恢复、密码重置和配额关联可完成；敏感操作展示目标和影响 | 停用后新请求拒绝，已有连接按既定策略处理；最后一个管理员的危险操作有保护；重置不在日志/列表暴露新凭据 |
| US-04 | P1 补齐 | 忘记密码有真实支持信息与管理员处理步骤；若另做自助找回，则需单独确定邮件通道与有效期 | 当前管理员模式下能从请求到完成重置并重新登录；无可用支持地址不展示假地址；自助找回未实现时不显示发送成功 |
| US-05 | P1 验证 | 权限拒绝说明所需能力与联系路径；必要时允许返回可用页面，不展示无权访问的业务内容 | 403/404、菜单可见性和服务端动作一致；角色更新后旧页面操作仍被重新检查；中英文提示一致 |

US-02/US-03 的 Web 角色创建切片通过 `/users/roles` 连接现有角色与权限目录、
创建接口及精确名称回读，再链接回既有用户编辑流程。只有同时具备 `platform_admin`
角色与 `role:create` 权限的当前视图提供创建；服务端仍独立决定权限。角色定义为平台
共享，确认前展示名称、说明与明确选择的目录权限，默认不选任何权限，不自动分配。
提交前重读名称与权限目录只是观察，不是并发锁。提交一次后锁定创建，失败或回读
不确定只提供 GET 恢复；匹配定义不证明创建者身份，也不证明权限关联表、缓存传播
或实际访问已完成。页面离开及账号、凭据、权限变化使旧回包失去更新视图的权限。
创建请求另在传输前再次核对视图归属，覆盖异步拦截器排队期间的身份变化。
精确名称应由操作者保存；本切片不提供跨刷新自动恢复、角色修改或删除。
现有 `GET /roles/permissions` 为目录路由，因此新创建表单明确拒绝同名角色，
不修改后端路由或已有角色。现有用户编辑的授权及保存行为不变。
受控回归应执行真实页面与 API 适配器，并覆盖单次提交、回读、失败与身份边界；
这不代表真实服务、数据库、浏览器、跨租户授权或角色最终生效验收。

US-02 Python hybrid Roles 边界与 native Roles 保持一致：七个角色路由均要求已认证、
非空用户与租户，并通过现有数据库读取确认当前账号属于该租户且账号、租户均 active。
所有操作保留凭据权限上界，并再次通过现有 RBAC 检查当前权限：JWT 使用当前账号角色
与严格无缓存的数据库有效权限；绑定 API Key 保留数据库读取的 Key 自身角色、显式权限
及当前拥有者有效权限，不用拥有者角色提升受限 Key。严格读取不读写权限缓存，任何
权限查询失败或池缺失均拒绝执行；原有非严格调用的缓存与兼容回退语义不变。
共享角色创建、修改、删除额外要求请求具有精确 `platform_admin` 角色，数据库账号
`roles` 及当前未过期 `user_roles` 关联也必须确认该角色；仅 `role:*` 或租户 admin
不授权全局写入。目录保持共享；列表、详情、更新回读的成员数及成员列表只查询本租户
未过期关联，平台管理员也不例外。删除前仍使用全局关联数（包含过期关联）保护其他
租户的引用；旧全局数据库方法语义不变。请求/响应形状不变，不新增 schema 或角色策略。
受控 ASGI/数据库协作者回归验证实际路由、认证依赖与 SQL 参数边界；不替代真实
PostgreSQL/RLS、撤权并发、权限缓存传播或跨服务最终验收。

SSO、组织邀请和自助注册可另立后续需求；它们不作为本轮管理员开户模式完善的前置条件。

## M11：配额、API Key 与系统设置

**用户/目标。** 管理员知道限制什么、影响谁、何时生效以及如何撤回。当前具备配额/成本治理和设置页；本模块不引入支付订阅。

| ID | 优先级/类型 | 需求与规则 | 可判定验收 |
| --- | --- | --- | --- |
| CF-01 | P1 补齐 | 配额展示周期、时区、使用量、估算/已结算区别、超额策略和临时额度；禁止把成本估算叫已扣款 | 周期边界、未知usage、部分失败和重复回执不误计；超额阻断/降速/降级/告警按明确配置生效；用户能解释本次为何被限制 |
| CF-02 | P0 验证 | API Key创建、一次性展示、授权范围、有效期、使用情况和撤销完整；重名可区分 | 撤销后新请求失败；列表/日志/导出不显示完整Key；创建回包丢失有安全恢复路径，不能默认再次创建无限Key |
| CF-03 | P1 补齐 | 认证、限流、容量、负载均衡配置区分当前有效值与待保存值，说明作用对象、重启要求和覆盖来源 | 保存失败保留表单；多用户编辑冲突可处理；配置无效提前说明；不允许表单放开单实例约束或无效并发能力 |
| CF-04 | P1 验证 | 配置修改可审计与回看；恢复旧值提供差异预览，敏感配置保持只写 | 错配回退后有效值回读一致；审计有操作者、对象与时间，敏感值脱敏；不自动撤销其他管理员后续修改 |

CF-01 的用户配额概览保留原有 `daily_tokens_limit`、`monthly_tokens_limit`、
`monthly_cost_limit_cents` 配置值，并新增可选 `limit_observation`：UTC 观察时间、
日/月 Token 与月成本的有效值、每项来源，以及原始临时增量、到期时间和是否生效。
同一响应中的所有用户共用一个时刻；仅到期时间严格晚于观察时刻时应用临时增量。
来源仅表示配置基础值、配置值加当前临时增量或无限制，不推断租户/角色继承关系。
`null` 始终无限制；已有存储零值保持原语义：无有效增量时无限制，有有效增量时
可以形成有限值。设置接口原有零值归一化及 SQL 更新语义保持不变。

概览 `status` 和本响应 `summary` 改为相对于有效额度计算使用快照，枚举和原先的
达到额度 `>=` 判断不变；因此旧客户端仍读到配置值，但状态可能因临时增量而变化。
用户配置 GET 的字段完全不变；独立 `/quota/summary` 接口不属于此次概览修正。
界面分开标注有效快照与配置基础值，提示观察时刻、临时增量和来源；旧服务端未提供
观察字段时明确显示仅有配置、有效额度未知。此快照不是下一请求的准入保证：
原有 Token 准入判断仍使用已用量加预计请求量 `>`，达到额度时零 Token 请求可能
获准，正数请求可能被阻断；到期、用量及其他限制也可随后变化。

Python 与原生实现复用各自准入使用的纯额度解析函数；概览不调用准入检查，
不预留 RPM、不生成安全事件，也不改变配额配置、使用量、重置或超额策略。
离线检查覆盖活动/到期/等时刻/无到期时间、无限制、零值、大整数、同响应时钟、
原配置序列化和完整 API→面板呈现。真实数据库、供应商、浏览器及未来请求准入
仍未验证，不代表 CF-01 整体完成。

CF-03 的负载均衡编辑现在将最近运行时回读与未保存选择分开：首次读取失败时不提供可保存的默认值；后台刷新和失败后的恢复读取不覆盖草稿。保存前再次只读检查，发现策略已变化时保留选择并要求明确采用当前值，或按当前值重新核对草稿。保存中的重复点击及迟到编辑不会发出第二次写入；账号、凭据或授权变化及页面离开后，旧回包不能影响新编辑。共享 Axios 请求认证拦截器同步捕获发起身份，避免排队后采用替换账号的凭据；
认证或存储读取异常直接抛出并阻止传输，缺少凭据时保留原有匿名请求语义。
共享 Axios 401 拦截器同时核对请求发出时的凭据与用户/角色/权限/层级快照；旧身份请求的 401 仍返回错误，但不清除新身份登录。当前身份的 401 保持原有清理和登录跳转行为，快照不写入请求配置或错误序列化。

两个服务实现均可能先更新运行时策略、再等待审计。因此保存响应失败不代表修改未发生。页面只自动回读，不自动重放 PUT；失败、回读不可用或回读与选择不同，均保留草稿并提示核对。只有保存响应被接受且回读匹配时显示确认。读取失败后显示的旧值明确标为上次回读，并禁用保存，直到重新读取成功。应用中只保存 Gateway 当前运行时策略，不修改启动配置、服务级覆盖、单实例限制、认证、配额或计费。

这一检查不是服务端版本锁：现有 API 没有 revision / ETag / CAS，预读与 PUT 之间仍可能发生其他管理员写入；回读也只是当时的观察值，不承诺之后不变。没有新增配置持久化、历史回滚、原子多管理员冲突处理或重启保证。离线回归运行完整 Settings 生产页面和真实 Query/Mutation Observer，React 调度与 HTTP 使用受控替身；不等同浏览器、真实服务、审计落库或路由行为验收，也不代表 CF-01～CF-04 整体完成。

### CF-03 / J25 authentication settings recovery (2026-10-14)

The existing authentication tab separates its latest runtime metadata read, startup
settings read and unsubmitted form. Neither response proves which authentication
validator is enforcing a setting. PUT changes the existing runtime metadata store;
this UI does not claim to reconfigure validators, persist startup configuration or
survive restart. Python/native APIs, authentication policy and credentials remain
unchanged. Runtime algorithms are preserved explicitly on PUT; raw API keys are
omitted. Blank JWT replacement retains the existing secret under both APIs.

Background reads and failed preflights preserve input. Invalid/partial reads never
supply savable defaults. One fresh GET precedes the explicit PUT; an observed
baseline change requires adoption or deliberate draft review. There is no server
revision/CAS, so this does not prevent an intervening write or detect replacement
of one already-configured secret by another. The API can mutate before cache or
audit failure: failed or malformed acknowledgements remain unknown even if the
next read matches. Only a success acknowledgement plus matching non-secret fields
is labelled confirmed, with no claim of exact secret equality or enforcement.

The write-only JWT input remains in component memory, never query/mutation caches
or storage. It survives an unsubmitted preflight failure, but clears at submission;
unknown replacement recovery requires fresh explicit input. Non-secret drafts and
uncertainty survive ordinary Settings tab switches through a hidden force-mounted
auth tab. Route departure and authority/credential changes discard that local state
and fence late callbacks, including store-observed A→B→A changes. Recovery is not
durable across a forced page reload. Refresh only reads; no automatic PUT replay,
rollback or restoration of another administrator's values is added.

Production-page regressions use actual query observers, authentication scope and
Axios with controlled React scheduling/transport and tab-presence semantics. They
are bounded offline evidence, not browser, live API, audit persistence or complete
CF-03/CF-04/J25 acceptance. Rate-limit, capacity and legacy API-key forms remain
outside this authentication-editor slice.

## M12：仪表盘与诊断

**用户/目标。** 管理员发现异常、理解影响、找到对应操作；普通成员只看到被授权的自身或团队数据。已有总览、运营、可靠性、治理和追踪视图。

| ID | 优先级/类型 | 需求与规则 | 可判定验收 |
| --- | --- | --- | --- |
| OB-01 | P0 验证 | 每个核心指标有定义、时间窗口、来源、更新时间和缺失状态；请求成功、任务成功、质量评分和可用率不同 | 零样本、指标读取失败、未探测模型分别显示；估算费用不冒充已结算；无法获得准确分母时不显示精确成功率 |
| OB-02 | P1 补齐 | 筛选服务/模型/用户/时间后各卡片口径一致；异常可下钻到授权Trace、任务、模型或配额 | 选一个失败任务可走到具体原因与原任务；返回保留筛选；不同时间范围不复用旧窗口值；无权字段不因聚合页泄漏 |
| OB-03 | P1 补齐 | 区分连接未配置、配置就绪、最近探测成功、当前降级、没有样本；刷新与自动刷新状态可见 | 配置Qwen不要求其他可选厂商也健康；探测失败保留上次成功时间但不继续显示当前成功；后台暂停/恢复刷新不重复计算业务操作 |
| OB-04 | P1 验证 | 故障详情提供安全code、影响范围和下一动作；支持复制脱敏诊断摘要，不要求用户看内部堆栈 | 摘要能关联任务与版本；无token/DSN/原始密钥；连接器、模型和执行失败分别归类，能分派到对应维护者 |

## 模块协作原则

MO是配置事实入口，PG是模型诊断入口，Assistant/Studio是任务入口，TK是已接受工作入口，Eval是质量判断入口，OB是运营聚合入口。信息可以互链，同一参数、状态和费用不应各自重新计算。跨模块需求引用对应ID，不以新增微服务作为默认方案。


PG-02/PG-03 的 LangGraph `gateway_model_failover/selected` 事件现在保留本轮最近一次
有效的上游报告选择，在答复统计中显示厂商与模型，并随手动保存的消息统计及流终结遥测
保留。它只是上游报告，不证明实际执行、费用结算或配置候选真正被调用；请求快照继续
只表示客户端请求设置，不被报告选择改写。未知旧记录保持未知。仅已确认根终结的
空答复或工具答复也可保存该观察；选择事件本身不能使运行成功。

身份字段按有界标识符白名单处理，排除 URL、查询参数、控制字符及常见凭据形态；
原因只保留已知枚举，不保存任意错误正文或原始配置。后续无效选择清除旧观察，避免
将旧模型误称最近选择。该限制不是任意秘密识别保证。新发送、账号与会话归属沿用
已有边界，停止及失败不触发重试。这里的诊断指流终结遥测，不包括“复制请求快照”
导出内容；真实供应商的首帧前切换与首帧后不重放仍需服务端单独验收。


OB-01 的成本分析面板按今日、本周、本月、服务分解、厂商分解和趋势分别显示观测状态。
某个读取失败或缺少有效成本值时，不再补成零；其他成功的卡片和图表保持可见。
无样本与读取错误分开显示，真实数值零仍按原有 USD 估算成本格式呈现。
加载或刷新期间隐藏该观测值；失败刷新即使保留缓存也隐藏旧值。来源标记为延迟或
陈旧的数据明确标记陈旧，不作为当前成功；局部重试仅重读对应 GET，面板刷新仍按
原行为重读六个来源。未更改计价公式、统计窗口、API 或结算语义。
该行为由完整生产模块、受控传输和 React 服务端渲染验证，真实浏览器与服务链仍需验收。

### TK-02 retained partial generic-task output (2026-10-12)

The generic task detail view exposes a failed/cancelled receipt's non-null public
`result` as escaped JSON behind an explicit reveal control and partial-output warning.
JSON formatting starts only on reveal and is memoized for the same result identity.
The full result is retained; scroll height limits the viewport, not revealed payload
size or memory. A changed result is reformatted while open; navigation resets reveal. It retains the
terminal outcome and error, never interprets output as success, and performs no
rerun or mutation. Empty strings, false and zero are results, not absence. Public
response output preserves JSON null and typed binary data's existing base64 form.
No MIME rendering, automatic download, stored codec tags or new endpoint is added.
Missing/null top-level failed/cancelled results produce no partial-output panel:
the public receipt does not distinguish absent result from explicit JSON null.
Pending/processing, wrong-ID and unavailable receipts do not expose partial data.

Completed tasks keep their existing `/result` flow and safe retry/error behavior.
That endpoint is typed UnifiedResponse. Completed receipts additionally offer an
independently labelled, collapsed task-detail output snapshot for every non-null
public `result`, including legacy false, zero, empty string/list/object and raw
response-shaped JSON. Availability depends only on the successful, exact-ID detail
read, never a `/result` status code or guessed payload type. The label identifies
its latest successful detail-read provenance and possible age relative to the
separate result read; it does not claim that read succeeded. Typed result loading,
error, retry and success remain visible and unchanged. Top-level missing/null
remain indistinguishable and produce no snapshot; nested null remains visible.
Snapshot JSON formatting is reveal-only and memoized, navigation resets reveal,
and unavailable/wrong-ID details suppress the panel. Actor-scoped query isolation
is unchanged. No additional request, mutation, MIME rendering or download is added.
Payload transfer and explicit reveal costs remain unbounded; scroll height only
bounds the viewport. Controlled production-component tests cover the display
boundary; live browser, API/service chain, clipboard and provider acceptance
remain unverified.


### CF-02 Knowledge API key lifecycle (2026-10-13)

Settings links to a separate Knowledge API key page. It consumes the existing
`/api/v1/api-keys` create/list/revoke contract in both Python and native profiles;
the legacy administrator `/config/api-keys` surface remains unchanged. Issuance
defaults to `knowledge:read` and seven days, with an explicit optional write scope
and one/seven/thirty-day expiry. This does not grant general model, console or
administrative access. The server remains responsible for caller tier, tenant,
owner and access admission. The existing exact `admin` role may list/revoke other
owners' keys within its tenant; the UI labels owners and never infers ownership
from a name. No backend policy, credentials or database schema is changed.

The complete UI path is review name/scope/expiry, explicitly issue, manually save
the one-time value, hide it, inspect current metadata, review an exact key ID,
explicitly revoke, and read metadata again. Empty and failed reads are distinct.
The complete server list is rendered in twenty-row client-side pages; this bounds
rendered rows, not transfer or memory. Enabled is a stored flag, not current
validity: expiration and permissions remain independently relevant.

The secret is held only in component memory, outside query/mutation caches,
local/session storage, logs and automatic clipboard handling. Account, credential
or authority changes remount the page and fence late responses. Metadata is
explicitly projected without credential fields. Navigation/dismissal loses the
secret intentionally; metadata cannot recover it.

Neither backend offers idempotent key issuance. An unconfirmed write is not
repeated automatically and blocks another creation until the user has read a
fresh list and explicitly chooses a separate grant. Names and timestamps do not
attribute an uncertain request. The in-page uncertainty state is not durable
across reloads; the confirmation always repeats the recovery warning. Revocation
is separate and targets a reviewed exact ID. A disabled metadata observation is
reported as an observation, not proof that a particular lost response succeeded;
accepted work is not undone.

Only controlled synthetic source/component/API tests are permitted for this
increment. Actual issuance/revocation, live authentication rejection, browser,
database roles/cache propagation and full CF-02 acceptance remain unverified.

### CF-02 existing database-key management coverage (2026-10-13)

Source review corrected the earlier distinction: issuance through
`/api/v1/api-keys` is limited to Knowledge scopes, but its metadata and revocation
operations also cover legacy database-backed administrator-issued keys. Both
Python `APIKeyService` and native `self_service_api_keys_store` use the stable
public identity `COALESCE(key_id, 'ak_legacy_' || id)` without a key-class filter.
The Web page keeps `/settings/knowledge-api-keys` for compatibility, now labels
itself database API key management, and links directly from the legacy Settings
key list. It reads a fresh canonical list and targets only a server-returned ID;
it never derives a public ID from the global configuration list's numeric ID,
name, key prefix, or tenant. Unknown scopes, roles and tier remain visible metadata;
they are not recast as Knowledge-only grants or effective-access guarantees.

The existing authenticated route applies current-tenant filtering and owner
filtering unless the resolved role list contains literal `admin`. This does not
inherit the legacy config route's `rbac.require(roles, "admin")` permission test,
nor does it grant cross-tenant management. An empty canonical list can be correct
even when the separate config list shows entries. No new revocation endpoint,
authorization policy, storage schema, config revision/CAS guarantee or global
administrator bypass is introduced. The existing update matches exact public ID
plus applicable tenant/owner predicates and changes only enabled/updated_at.

After explicit review and confirmation, the page invokes the existing revoke
route once, reads metadata again, and reports a database-disabled observation.
A lost/mismatched acknowledgement is never automatically replayed. Config changes
are neither submitted nor overwritten. Native bound database admission resolves
enabled/expiry on each request; Python database admission checks the enabled flag,
but its configured static-key fallback can still accept a separately configured
copy of the same credential. The runtime config API's shadow arrays are not active
validator authority. Consequently, static/config-array keys are explicitly outside
this page's revocation scope; database disable is not all-path credential
revocation, cache convergence proof, or cancellation of already admitted work.
Those operator/static-admission and live rejection acceptance gates remain open.

Synthetic route/service tests exercise legacy public-ID mapping, same-tenant
administrator list/revoke/readback, cross-tenant refusal, non-null public-ID
preservation, literal-admin ownership semantics, cache invalidation and
unauthenticated denial. Synthetic component/API tests cover legacy/unknown/null
metadata and exact-ID revocation alongside existing lost-response protections.
These do not prove actual database execution, fresh credential authentication,
static configuration changes, distributed cache propagation or live admission.


### TK-02/TK-04 older scheduled receipts and exact links (2026-10-13)

The scheduled-task page now traverses schedules and their execution history in
20-row server pages. Optional paired `before_at` / `before_id` cursors use
immutable creation time plus UUID, descending; the extra lookahead row is never
used as the cursor. Existing no-argument API consumers still receive `tasks` or
`executions` arrays with the previous 200/100 defaults, plus `next_cursor`.
The page URL preserves each list cursor; browser Back restores prior traversal,
and Latest explicitly resets that list. New inserts do not shift a cursor page,
but pages are independent observations, not a repeatable-read snapshot.

`GET /api/v1/scheduled-tasks/{task_id}` and the corresponding
`/executions/{execution_id}` read exact existing receipts, so a copied task or
execution link no longer depends on inclusion in the newest page. Every list and
exact read resolves current identity and scopes to the tenant/user; an execution
also must belong to the requested owned schedule. Missing and outside-scope
receipts share the existing 404. Responses are `no-store`. No administration
bypass, second execution identity, migration, dispatch or acknowledgement is
introduced. Python services and scheduler mutation/launch semantics are retained.

The selected schedule appears independently of the current schedule page; a
linked execution outside its current history page is explicitly labelled.
Loading, successful empty pages, invalid links and failed reads are distinct;
refresh repeats only GETs. Every navigation visit fences late responses and
aborts its reads on departure; account/credential/authority changes remount the
schedule editor. Fifteen-second polling skips reads already in flight, retains
same-navigation result components while rereading, and hides failed receipt
reads. Polling never replays an action or starts another execution. Legacy servers
without a cursor are labelled as bounded; unavailable exact-read routes are not
silently replaced with guessed list identities.

Controlled component/API and native producer tests cover cursor traversal,
identity matching, malformed inputs, old-link recovery, delayed reads, errors,
and output continuity. The consuming native Gateway library is compiled;
source predicates and producer schemas are reviewed separately from real SQL
execution. Real PostgreSQL tuple ordering/authorization/concurrency, running
Gateway/Runtime, authenticated browser history and whole TK-02/TK-04 acceptance
remain unverified. The full native OpenAPI gate remains incomplete; the additive
receipt operations and list metadata do not close its unrelated historical gaps.

### MO-03：原生目录审核同步（有界源码实现）

Web 和 Desktop 的厂商同步入口先调用 `/api/v1/providers/{provider_id}/models/sync/preview`。
此调用复用现有可信模板与发现逻辑，但将候选写入内存收集器，不修改模型。
页面显示每个模型的逐字段变化、现有管理员设置、未纳入建议而保留的模型、发现警告和
未知能力模型。未知能力模型仍需手动配置，不由发现结果自动推定能力。
用户选择模型并明确确认替换现有元数据后，才调用 `/models/sync/apply`。
新模型保持停用；现有价格、访问范围、启停和 `capability_overrides` 保留。
目录可能替换 display/context/output/vision/tools/sort/catalog 元数据中的手动值，必须在
预览中明示并单独确认。预览不会删除、停用或重命名任何现有模型。

审核身份绑定当前 actor、tenant、provider 和完整已观察模型/引用快照；凭据不进入审核
结果。预览在发现前后检查快照，应用在同一个 SERIALIZABLE PostgreSQL 事务中锁定
厂商和模型、重新比较快照，然后复用 ModelService 的模型/能力校验执行全部选择。
并发变化或校验失败拒绝本次事务；不会通过重新发现悄悄改变用户审核的建议。
现有单次同步接口为兼容保留；新的 Web/Desktop 流程绝不回退调用它。
保留的 Python/hybrid 服务没有这些原生审核接口时，应显示不可用，用户仍能手动编辑模型。

受影响引用仅列出当前用户为明确成员的 Agent 当前草稿和活跃发布，提供 Agent 身份与
入口；缺少 provider 的旧引用明确标为潜在匹配，不声称属于该厂商。读取审核时重新检查
成员范围，不返回此前缓存的私有引用。未授权/其他 Agent、进程默认值、服务默认值及
外部客户端未评估；这一界面不是完整引用盘点，也未完成 MO-03 的所有默认值验收。
列表是厂商引用清单，审核时需按所选模型进一步检查。

审核最多 128 项、15 分钟有效，仅保存在当前 Gateway 进程。请求发送前消费审核身份；
重复点击、网络失败、取消观察或未知 COMMIT 结果不恢复再次写入权限。
`GET /models/sync/reviews/{review_id}` 返回已知审核状态及当前模型；成功的 `applied`
确认数据库事务已提交，不表示后续审计或失效发布一定成功。应用后仍调用现有失效与
配置审计路径；发布器保留其现有普通发布失败非致命语义。提交后的缓存/审计失败可能
使请求报错，用户必须回读。过期或重启后审核不存在，需刷新模型并比较字段，再决定是否
生成新预览；不提供持久化精确一次保证，不自动重放或删除已提交结果。

离线生产函数、路由处理器和 Web/Desktop 状态/组件夹具不接触真实供应商、数据库、
监听端口、浏览器或用户电脑。数据库并发、真实权限/引用数据、发布消费方、真实 GUI 与
跨进程恢复仍需独立验收。原生 OpenAPI 导出仅增加本流程的源代码所有权声明；完整原生
合同门禁仍失败，不因这些新增有界接口转为绿色。


### OB-04 / TK-02：任务诊断交接

任务 ID 详情在成功读取且身份匹配后提供诊断面板，区分等待、处理中、完成、失败、取消和未知状态；结果接口读取失败不把已完成回执改为任务失败。刷新只重新读取回执，不发起执行、取消或重试。失败和取消明确提示已发生的副作用、执行资源回收不能由该回执证明；不根据任意错误字符串推断原因。

用户先预览再复制受限元数据快照，包含规范 UUID 任务身份、归一化状态、结果读取观察和客户端观察时间；非 UUID 身份省略。复制不包含服务名称、提示词、结果、原始错误或凭据字段。快照不会发送给第三方；剪贴板失败明确提示手动复制，不声称复制成功。已有详情输出与部分结果入口保持不变。此功能不等于完整故障分类、服务端版本追踪或远程清理证明。

验证范围为生产纯函数、实际组件事件处理和任务详情集成的受控离线测试；浏览器、系统剪贴板、真实服务、数据库和供应商未在此验收。

### MO-05 / PG-01：Playground 服务入口恢复

Playground 区分服务目录初次读取、读取失败、当前范围没有已启用服务，以及此前选择的服务不再可用。失败可显式重读，重读只调用现有目录 GET，不发起模型请求。成功返回的目录只表示客户端当前观察，不代表厂商配置完整、连通性通过或下一次请求一定获准。后台重读仍在进行时，可继续使用上次成功读取的有效选择；重读失败后暂停新请求，直到再次读取成功。

无可用服务或所选服务缺失时，有 `console:services:view` 权限的用户可进入既有服务管理；其余用户显示联系管理员的说明，不提供密钥表单。当前视图只允许对成功目录中的已启用服务发送、重新生成或创建新会话。初次没有选择时仍沿用既有默认选择；已有选择失效时不会自动改成另一服务、清空会话或重放请求。用户明确选择其他服务才沿用既有会话切换流程。

目录读取状态不会卸载输入框或清除草稿，也不会终止已有流；停止按钮和 Escape 停止保持可用。服务是否真实可用、当前授权及费用仍由服务端独立判断。本切片不修改认证、权限、请求快照、参数恢复、传输或取消协议，也不宣称修复了服务端授权漏洞。生产页面、实际输入组件、目录 API 适配器和会话默认选择逻辑的受控离线测试用于验证入口恢复；真实浏览器、剪贴板、供应商和完整 MO-05/PG-01 验收仍待进行。

### TK-03 generic task cancellation from Web detail (2026-10-13)

Exact-ID, successfully read generic task details offer cancellation for `pending`
and `processing` only. A separate confirmation identifies the task and explains
that history and retained results remain. It calls the existing authenticated
`DELETE /api/v1/tasks/{task_id}` in both native and retained Python profiles;
there is no new delete-history, create or execution-retry endpoint. The server
remains the authority for tenant/owner authorization and terminal-state races.
Native cancellation conditionally persists cancellation and signals best-effort
local dispatch interruption; the explicit durable profile additionally fences its
existing delivery record. Neither profile proves remote execution, tool effects
or final billing stopped, and the default memory profile gains no crash recovery.

A matching `cancelled` response is acknowledgement, shown separately from a
subsequent exact-ID GET receipt. Failed/malformed mutation responses remain
unconfirmed even if readback shows cancellation. Readback may instead show a
winning completed/failed outcome or a still-active task. Read failures and unknown
statuses do not fabricate terminal facts. Background detail loading/errors retain
the selected visit’s cancellation controller and its read-only recovery control;
they cannot reset an attempted mutation. Only a valid detail unlocks an idle action.
Inline confirmation retains its trigger, announces the task/warning, restores
trigger focus on dismissal and moves focus to the status region on submission. The same detail visit submits at most
one cancellation; subsequent recovery controls only GET the original receipt.
Repeated clicks are synchronously fenced. Navigating away, changing task or
changing credentials/authority abandons old callbacks and cache invalidations;
returning to the same task does not revive old responses. Reopening a detail is
a new visit and can offer a new explicit cancellation if the task is still active.

Bounded offline validation completed on 2026-10-13: 114 focused Node tests and
1,355 full Web Node tests passed with zero failures or skips in their final runs.
The focused suite executes production parent/component, API-adapter and controller
code for confirmation/dismissal, pending/processing/terminal cases, mutation
uncertainty, terminal races, read-only recovery, duplicate clicks, detail-read
failure/recovery ownership, stale navigation/account responses and explicit focus
calls. React scheduling, DOM, auth and transport remain controlled collaborators.
App and Node TypeScript checks (`tsc --noEmit -p tsconfig.app.json` and
`tsc --noEmit -p tsconfig.node.json`), changed-source ESLint and the existing i18n
checker also passed. The first full suite attempt had four module-load failures
because the Web-only snapshot omitted four existing adapter fixtures; that failed
receipt is retained. Copying those exact source-bound fixtures, without changing
tests or Web code, enabled the passing full rerun. Complete source/dependency
membership and hashes were unchanged before and after execution on Node 26.11.1.

These checks do not establish browser accessibility, live authorization, database
races, worker interruption, provider cessation, production build/deployment or
complete TK-03 acceptance. The final owning-plan outcome amendment changes no
tested Web code. The native-enterprise program retains the validation receipts
and separately records postintegration source equality.

### TK-03/TK-04 existing schedule creation acknowledgement recovery

The mounted schedule form now consumes one submission synchronously and retains
its exact submitted fields while POST is pending or its outcome is uncertain.
A successful acknowledgement must carry a valid UUID and the submitted immutable
fields; only that response can supply the creation identity. The existing exact-ID
GET then reads its current state independently. Failed/malformed acknowledgements
do not trigger another POST or infer ownership from matching titles, times or list
rows. A failed/wrong-ID read preserves the acknowledged ID and retries only GET.
The read-only review action refreshes existing schedule history; it never creates,
pauses, resumes, cancels or replays a schedule. Only a valid exact readback offers
an explicitly additional blank form, leaving the acknowledged schedule unchanged.

Navigation between history selections preserves this form's submission fence.
Leaving the tab/page or changing credentials/authority abandons its callbacks;
a later visit starts blank and cannot receive the old response. This is local
in-memory recovery only: reload or leaving loses it, and the page says so before
submission. No persisted deduplication, server idempotency, new scheduling policy,
API, schema or execution-retry behavior is introduced. The native endpoint still
generates a new UUID for each POST, so a recreated request can duplicate a schedule.
The original schedule lifecycle actions and execution owner are unchanged.

On 2026-10-13, the targeted creation and existing history suites passed all 26
Node tests with zero failures or skips. These execute the production mounted
form/API/controller with controlled React/auth/HTTP collaborators, covering double
submit, retained payload, exact readback, response uncertainty, malformed
acknowledgements, navigation and authority changes. App and Node TypeScript
checks, scoped ESLint on the four changed TS/TSX paths and i18n validation also
passed on the same candidate bytes with source/dependency identities unchanged.
No full Web suite, browser, real HTTP/DB, scheduler or model execution was run for
this slice. These are independently verified subfunctions, not live end-to-end
acceptance. The final documentation amendment changes no tested Web source.

### TK-02/TK-04 scheduled-history visibility and read freshness

The existing schedule list, selected schedule, execution page and exact execution
reads pause while the browser document is hidden. A hidden mount starts no reads.
Hiding aborts their current GET lifetimes and clears their polling timers; retained
same-navigation receipts are explicitly last observed values, without a misleading
loading indicator. Foregrounding starts one fresh read per enabled resource and
restores the existing 15-second interval. Duplicate visibility events and queued
old timers do not create duplicate reads; independent outstanding resources remain
single-flight. Navigation and existing authority-driven component disposal retain
their previous cancellation/identity fences. Failed reads still hide unavailable
protected content, and a small freshness status distinguishes paused, refreshing,
partially unavailable and last-successful-read states.

This reduces hidden-document metadata polling (up to four GETs per tick for a
fully selected view) without changing endpoints, pagination, scheduler actions or
execution. Output viewing and notification polling are separate owners, unchanged
here. The browser still downloads ordinary response payloads when reads run; this
is not a backend query/response-size improvement. Controlled component/API tests
cover visibility/timer request counts and lifecycle/freshness behavior; real browser
scheduling, resource measurement and live authorization remain separate acceptance.

Validation: all 19 focused scheduled-history component/API regressions passed
(0 failures, 0 skips), together with app and Node TypeScript checks, scoped
changed-source ESLint and the i18n key check. Receipts bind the exact external
Web snapshot, pinned Node 26.11.1 and unchanged existing dependency inventory.
These are model-free checks; no full suite, build, browser timing/performance
measurement, live scheduler execution or real-model acceptance was performed.


### MO-05：Assistant 模型入口恢复（2026-10-13）

模型目录与助手配置可显式只读刷新；加载、读取失败、返回空集合、未选择和已选模型不在目录分别展示。成员得到联系管理员提示，具有服务页面访问权限的用户可进入已有厂商设置。返回目录只说明当前客户端观察，不证明厂商健康或执行授权。

刷新只读取模型与配置，不重新初始化会话、知识源、连接器或草稿，不提交模型请求。选择使用厂商与模型成对身份；保留显式及会话恢复的选择，不因迟到配置或刷新改成默认模型。目录不可用仅阻止新发送，正在运行的 Stop 控件保持可用。认证/凭据代次、请求及卸载保护旧回包与旧刷新回调。

验证：9/9 受控离线回归通过，执行生产 catalog hook/API、状态组件、实际输入组件及从页面 AST 提取的默认选择 effect 与文本/图片 Send 回调；不等于完整页面调度。app/node TypeScript、scoped lint（零警告）与 i18n 均通过，测试源清单 `9937768d29ea8efeb6f4d7c64b2bbc53268fc4ee6981949d9ab14269889e265e`；没有真实模型、浏览器或服务端配置验收，不宣称完整 MO-05 完成。


### OB-02：失败分组 → 采样 Trace（2026-10-14）

失败分析中的服务、模型、厂商分组可显式进入采样追踪，携带错误类型、失败状态、原时间窗及服务/用户筛选；使用现有只读 API，不发模型请求或变更配置。URL 记录范围和来源，返回失败分析或浏览器后退保留原筛选及分组维度；更改全局范围会清除旧分组条件。

追踪区说明最多返回 15 条采样记录，只查询 `status=error` 子集，不覆盖聚合中其他非成功状态或未采样请求，不把分组计数当作追踪覆盖率。聚合会把 NULL/空错误类型归一化成 `unknown`，而现有 Trace API 按原值匹配；此类不确定分组的入口禁用并解释，不通过丢弃错误筛选扩大查询。分组模式显示来源条件，清除分组后才允许任意请求 ID 查询。目录读取失败与空采样不同；导航、认证代次或替换响应使旧详情及旧行回调失效，查 ID 的迟到响应不得覆盖新范围。服务端继续负责租户/用户授权；本改动不增加权限。

验证：8/8 定向测试通过；生产失败/追踪组件、读取 API 和认证 hook 在受控 React/查询/路由替身下测试，另检查纯导航编码、浏览器后退模拟、三种维度的精确请求参数、失败状态及过期回调；app/node TypeScript、scoped lint（零警告）与 i18n 检查均通过，测试源清单 `7e67833c6786e65ce165b9ec3fd612bec005f33c7147a0900d78ec3caf9e7fe2`。无真实浏览器、服务端采样或模型验收。不宣称完成全部 OB-01/OB-02 统计口径工作。


### OB-03：看板刷新生命周期（2026-10-14）

看板自身的自动刷新定时器在页面隐藏时暂停；恢复可见时仅在间隔已过期后合并发起一次刷新，不逐次补发遗漏的 tick。未过期时等待原剩余间隔。手动刷新保持可用并重置下一次自动刷新时限；切换间隔重新开始该间隔，0 表示手动模式，不因返回页面自动刷新。卸载后旧定时器、可见性事件及手动回调无效。

顶部明确区分自动、手动和隐藏暂停，时间标为“刷新请求时间”，不是读取成功或数据更新时间。各数据面板仍使用自身的来源新鲜度与失败状态；暂停只减少此看板调度器引发的读取，不取消进行中请求，也不宣称停止其他服务轮询。已有请求所有权检查不变。

验证：8/8 受控时钟、可见性及真实 Context/hook/header 组件测试通过，使用真实 QueryObserver 的受控请求探针验证隐藏期零新增调度、过期恢复一次；app/node TypeScript、scoped lint（零警告）和 i18n 均通过。测试源清单 `1afea64df69eeca14e89b91124200ea18a0dd8ed3e7176022332a632e4364a39`；只提供客户端调度与请求触发次数证据，无浏览器、网络性能或真实服务验收。


### TK-02 returned task output browser (model-independent candidate)

Completed generic-task typed results now provide 20-item pages and an explicit
selected-item view. Text and JSON are rendered as literal text, never executable
HTML or Markdown. File/media URLs remain inert descriptors: no remote fetch,
encoding guess, generated-artifact identity or source-session route is invented.
The task/request identity must match the typed response. Empty output, unsupported
items and mismatched observations are distinct; receipt status is not inferred from
output shape. Failed/cancelled partial detail snapshots remain separately labeled.

Display serialization is bounded to 16,000 characters, 512 visited nodes, depth 8
and 50 entries per container. Large previews explicitly explain their limits; the
copy button copies exactly the displayed selected-item text. It is not a full
export. Raw returned item fields are opt-in and use the same bounds. Display bounds
do not limit server response size or prove browser performance at arbitrary scale.

Task detail/result reads use an optional opaque authority-visit query suffix,
preserving existing cache-prefix invalidation and other callers. Retired reads
check ownership before dispatch and after completion. Task/auth/result replacement
resets output selection and invalidates old copy callbacks; page/item/view changes
also retire callbacks synchronously. Clipboard completion cannot update a retired
view. A clipboard write already started cannot be revoked by subsequent navigation.
No task is retried, cancelled or mutated by output inspection or copy.

Validation: 25/25 focused tests, app/node TypeScript, zero-warning scoped lint and i18n passed on source manifest `2a0490d46bc0df4941e7e8f4ba6396029bf1d040b914fe70f540a1313a1020c2`. All five serialized gates shared one immutable Web snapshot and one hash-bound dependency inventory, freshly rechecked before/after each command. Production output-component rendering and auth-subscription execution used controlled VM hosts; page integration was source-asserted, not mounted browser acceptance. Bounded production component and existing query/API regressions
cover mixed/empty/unknown output, 45-item navigation, safe literal rendering,
truncation, copy failure and task/result/authority disposal. These are controlled
offline checks, not live model/service/browser or full TK-02 acceptance.


### TK-01：任务收件箱缺失进度展示

收件箱不再把缺失、null、非数字、非有限数或越界进度转换成确定百分比；
这些观察显示“未提供”，不影响实际状态、任务详情链接和分页。数值 0～100
沿用既有显示兼容行为：0～1 按比例显示，其余按百分数显示；不根据 completed、
failed 或 cancelled 推导进度，也不把进度显示作为完成或副作用停止证明。

这仅修复客户端未知／畸形值展示，不建立新的进度协议。当前 Python 与原生
通用任务都以 0 初始化，公开 schema 未声明单位，生命周期转换也不会更新该值。
因此默认 0 与实际测量 0 无法区分，1 的比例／百分数歧义仍需生产者合同解决。
不能据此声称完整 TK-01 进度验收。验证应覆盖实际收件箱组件的生产渲染路径、
两种语言、所有状态和有效数值兼容；受控离线验证不能替代真实浏览器或后端验收。


### TK-02: exact request trace navigation

Task detail links only its explicit, well-formed request ID to the existing
dashboard tracing route. The exact lookup performs a read-only lookup without automatic retries, displays only a matching response, and never falls back to an unrelated
sampled request. Missing/malformed IDs do not create a link or a lookup. Failed,
forbidden, missing and mismatched trace responses display unavailable without
changing task status, retrying execution or inventing a source session.

The return link reconstructs the same task detail plus the inbox status/page
context from bounded route parameters; it cannot redirect to an external URL.
Existing dashboard/task route permissions and trace API authorization remain
unchanged. Authority changes and disposed navigation visits retire late reads.
This is a model-independent client change; browser/live service/role acceptance
and the broader TK-02 journey remain separate required gates.


### Python Redis task queue recovery (2026-10-14)

With the built-in memory fallback, a Redis pop/decode error no longer strands the
worker forever waiting on an empty fallback. It checks any already queued memory
work and otherwise waits one second before polling Redis again; healthy empty
polls retain their existing cadence and Redis-first scheduling. Diagnostics omit
backend exception details. Worker cancellation still propagates and joins its lane.
No ambiguously popped member is recreated or replayed; a lost pop acknowledgement
can still strand that task. This is same-process observation recovery, not a durable
claim/ack queue. Custom fallback implementations retain their previous blocking
dequeue behavior and do not gain this recovery guarantee.

Controlled production queue/manager/worker checks cover recovery, malformed removed
members, pending cancellation, shutdown and fallback paths. Redis command and
provider seams are controlled; no live Redis, database, provider, process restart
or complete J03/J21 acceptance is established by these offline checks. Persisted
cancellation still does not prove external side effects or charges have stopped.


### US-01/US-03: password-change dialog ownership

The password-change dialog binds its transient credential fields and completion
to the current authenticated view. Closing or replacing that view discards the
form; late success/failure cannot clear a replacement login, navigate it, or
repopulate its errors. Submit is guarded synchronously against repeated clicks.
Dismissal is unavailable while the request is pending, and forced password-change
dismissal remains unavailable. A successful current attempt still clears the old
session and asks the member to sign in again. No password is persisted by this
change, and backend password/session, role and tenant contracts are unchanged.

Controlled component tests are the intended verification of these client-local
behaviors, not proof of live account mutation, browser acceptance, password reset
delivery or the complete J22/J23 journeys. Backend credential invalidation and
unknown transport outcomes remain governed by their existing server contracts.

The shared Axios 401 interceptor additionally captures an in-memory auth
generation at synchronous request dispatch. The existing store subscription
boundary (token, authentication state, user ID, roles, permissions and tier)
retires earlier requests even when A→B→A restores equal identity values. A stale
401 still rejects with its original error but cannot clear either auth storage or
redirect the replacement session. A current-generation 401 retains logout and
login redirect behavior; ordinary profile/loading updates do not retire it.

This generation is neither persisted nor added to serialized Axios config. The
existing credential and authority checks remain, including storage-fallback
compatibility. Raw storage-only changes without an auth-store notification do
not gain history/ABA detection. Controlled Axios adapter regressions exercise
client-local ownership only; live J22/J23 acceptance and server-side credential
invalidation remain separate gates.

Missing responses, server errors and request timeouts are shown as unknown
outcomes. Credential fields are cleared, resubmission is blocked for that form,
and an explicit sign-in action provides reconciliation without replaying the
password write. Ordinary 4xx rejection remains editable. This is UI recovery,
not proof that the server did or did not apply the change.


### OB-01/OB-02: performance observation recovery (model-independent candidate)

The performance panel includes the current service/user filters in both its
read-only request and cache identity. The existing auth-visit scope and keyed
filter lifetime retire old reads and refresh callbacks on navigation, refresh,
authority change or unmount. Explicit retry is a coalesced GET, with no automatic
query retries and no model execution or configuration changes.

Loading and failed reads hide cached statistics. Empty, unusable and zero-request
series do not become zero-millisecond observations. Each absent, null, negative or
non-finite duration is unknown; returned numeric zero with a positive request count
is retained. Zero-sample chart buckets are gaps. Statistics are explicitly the
last returned bucket, with its request count, requested window and browser read
time. The data-status badge retains the service's freshness observation.

Gateway usage records are aggregate source data, not a live availability probe.
Both existing native and Python producers can coalesce absent phase measurements
to zero; this client cannot reconstruct that lost distinction and says so. Read
time is not source ingestion time; backend freshness is not proven filter-local.
Python also catches recorder database errors and returns an empty series, so the
client cannot distinguish every backend failure from an empty observation. It
says data is unavailable rather than claiming no traffic or a healthy service.
No API/schema/accounting change or complete OB-01/OB-02 acceptance is claimed.

Controlled production component/API/auth and query-observer regressions, direct
app/node TypeScript, scoped lint and i18n are the planned offline gates. They do
not replace real browser, database, server authorization or provider acceptance.


### TK-02 / TK-03: generic inbox authority ownership

The generic-task inbox now uses the same ephemeral authenticated read-visit scope
as task detail/results. Its query key retains the existing task/user/filter/page
prefix and adds an opaque visit identity; credentials never enter the key. A new
login, token replacement or authority change cannot reuse the prior visit's cached
rows. Retired list callbacks reject before dispatch and after successful transport
completion, including an A→B→A authority change before React renders again.

Refresh, read retry, filter/page changes and task-detail links refuse callbacks
from a retired inbox view. Current pagination, lookahead, active/idle polling and
exact task links stay unchanged. No execution, cancellation, retry or history
mutation is added. Existing server-side tenant/owner authorization remains the
security boundary; this client change neither extends it nor proves live ACLs.

Planned production hook/component and actual React Query cache tests use controlled
auth, React scheduling and HTTP collaborators. These checks do not establish browser,
real authentication, service-chain or full TK-02/TK-03 acceptance.

### TK-02/TK-04 explicit scheduled action reconciliation

Pause, resume and cancel now have a current-view action receipt. A fresh exact
schedule GET precedes every explicit POST; a changed state prevents submission.
Cancellation requires a separate consequence confirmation. The server action
response is validated by schedule ID, requested state and cancellation flag.
After either an accepted or ambiguous response, one exact GET observes the
current state. Recheck is read-only; a failed read retains the outcome as unknown
and never resends a POST. Another action requires explicit review and a fresh
read. Same-tick duplicate actions, retired authentication/selection and late
completions cannot dispatch or overwrite a replacement view. Background history
read failure does not discard the current action receipt.

The native endpoint has no optimistic version precondition or action idempotency
key. The preflight read cannot eliminate concurrent changes, and matching state
does not prove that this request caused it. Cancellation marks active executions
for cancellation; it does not prove they stopped or undo existing effects. This
UI does not add scheduler execution, notification delivery or backend guarantees.
Focused controlled UI/API tests are offline evidence, not a real browser, live
scheduler/database or complete task-lifecycle acceptance.
