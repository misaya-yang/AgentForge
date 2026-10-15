# 03 模块需求与功能验收合同

status: queued · domain_id: agent-platform-vnext · owner: primary Codex · last_verified: 2026-09-07 · prerequisite: 主 PRD 激活 · successor: null

本册是需求 ID 权威。表中“一/二/三”指首次必须交付的期次；跨期项按工作包明确拆分。`B` = C80_BLOCKER，`R` = R100_REQUIRED，`F` = FOLLOW_UP。所有条款均是目标，不是现状声明。来源 `Dxx` 见现状分册，`C-xx` 见契约分册。验收场景 `Sxx` 见评测分册。

## 1. 身份、租户、授权与数据治理

**产品结果：** 管理员能解释谁、以哪个身份、在什么边界内执行；读写、检索、评测和本机工具不能通过不同入口绕过同一个权限。

| ID | 期/级 | 必须行为 | 可判定验收 |
| --- | --- | --- | --- |
| ID-01 | 一/B | 所有 Hosted 调用先解析权威 tenant/actor；Eval job 使用目标租户 service identity 或验证凭证与 job 的绑定 | A 凭证+B job 在创建 Runtime thread 和写 trace 前失败；两个租户都无错误归属记录；S01 |
| ID-02 | 一/B | Dataset role grant 绑定 tenant+role；旧grant按dataset tenant回填，歧义隔离；public和显式user共享另定矩阵 | 两租户同名analyst不能碰撞；列表/detail/retrieve/QA/export/artifact均拒绝越权；安全回滚不能重新放行裸role；S02 |
| ID-03 | 一/B | 去掉 Knowledge 对 Gateway 用户/RBAC 的写能力；需要再次确认的管理操作由 Gateway 签发对象/动作/期限绑定证明 | KS DB role 无权修改 users/user_roles/user_permissions；不读取密码 hash；伪造/过期/换 dataset 的确认凭据失败；S03 |
| ID-04 | 一/B | approval 固定 tool identity、args hash、tenant、run、有效期和一次性消费；Read/Write/Unknown 由可信注册表声明 | 已知只读工具不误入写审批；写工具未经批准不执行；unknown external tool 不自动当只读；重放 proof 失败；S04 |
| ID-05 | 一/R | 内部 API 按 audience/scope 验证，外部 URL/文件路径/插件输入不变成内部授权；默认最小出站权限 | 错 audience、重定向到内网、symlink 逃逸、远程内容触发 shell、被撤销设备均拒绝；失败原因可诊断 |
| ID-06 | 二/R | 明确 retention/deletion/export：thread、artifact、memory、dataset、trace、eval 各有 owner/TTL/可见性 | 删除私有知识后，旧 cache/trace 下载/向量 generation 不再成为旁路；保留的审计元数据不能恢复原文 |
| ID-07 | 二/R | 管理策略具有版本、审计 diff、生效范围、撤销行为；组织策略只能收窄本地设备授权 | 降权影响新执行与需要重新校验的旧执行；紧急 deny 立即生效，不依赖模型遵从 |

源码调查不是完整安全审计。实现 ID 项必须补真实两租户 HTTP/DB 负向证明，不能只把局部 `if tenant` 加到一个路由。

## 2. Agent 平台与 codex-harness 兼容层

**产品结果：** Assistant、Studio Preview、Published/Embed、Responses 和 V2 API 在相同有效配置下具有一致语义；CLI/桌面作为不同部署身份，复用相同内核谱系及协议测试。

| ID | 期/级 | 必须行为 | 可判定验收 |
| --- | --- | --- | --- |
| CP-01 | 一/B | 修复 `tool_choice=none`、native search 自动注入和 parallel-tool 授权漂移；不得从 profile 或历史推导新权限 | 现有 transcript + none 在两个 wire 的所有 round 都不发送可调用 tools；只允许 function 的请求不出现 web_search；S05 |
| CP-02 | 一/B | namespaced tool 编码/解码在 Gateway、CLI、Runtime registry 与历史记录一致；无碰撞 alias | 两 namespace 同名工具、多次恢复、provider 分片返回均命中正确 handler；未知 alias 失败；S06 |
| CP-03 | 一/B | whole-thread/per-turn 的终态、ledger、lease 和 UI 恢复满足 C-04 | 同一 thread 连续两 turn 都清账且订阅继续；terminal 在第150事件以后仍可恢复；一次网络错误不永久锁住输入；S07 |
| CP-04 | 一/B | 统一所有现有 provider adapter 的 HTTP error、usage-only、EOF、取消和终态语义 | 429 不输出成功；choices=[] 可计 usage；无终态 EOF 标 interrupted/failed；每个工具只出现一个逻辑终态；S08 |
| CP-05 | 一/B | Python/代码执行具备运行中总字节+inode、CPU/内存/进程限制，取消传到底层进程组；input/output 分离 | 小文件风暴、多文件写、fallocate 不超总预算；取消后进程和工作目录归零；输入附件不成为生成物；S09 |
| CP-06 | 一/B | Runtime/Worker 与源锁/overlay/协议作为一个可信兼容单元；启动时分别验证实际 image identity | Worker 缺失、tag 被替换、Runtime 与 Worker revision 不同均拒绝；只能用明确开发豁免作非发布候选；S10 |
| CP-07 | 二/R | AgentSpec 的权限、budget、delegation、memory、output contract 成为版本化数据；复用 resolved launch | default Assistant 能导出为等价 spec；fork 后无隐藏特权；preview 与 published 的实际指纹可比；旧 v1 reader 兼容；S11 |
| CP-08 | 二/R | 对上游兼容建立 seam inventory：哪些 overlay 是平台 hook、哪些是移植、哪些能上游化；升级只改必要 seam | 锁定同一 upstream 时，新增 surface 不更改内核算法；升级前后的 protocol fixtures、取消/审批/恢复和工具身份通过 |
| CP-09 | 三/F | 外部 Agent 作为独立服务集成，声明 identity/capability/auth/task lifecycle；保留不可表示语义 | 第三方超时/取消/失联有明确任务状态；不能嵌套本平台 kernel 或取得平台 provider secrets；A2A 适配通过专项合同 |

### 2.1 Agent 构建与发布流程

默认 Agent → fork draft → 声明目标与输出 → 配置知识/权限/模型策略 → 校验有效 launch → 隔离 preview → 运行冻结 Eval → 查看差异与风险 → 发布不可变版本 → 观察 cohort → 回滚。

每一步必须知道正在使用 draft revision 还是 immutable version，是否保存，当前 effective profile 是否改变；发布结果必须记录 `spec_hash + policy_hash + eval_run_id + release_manifest`。禁止预览使用最新未保存设置而发布固定旧配置的隐式差异。

### 2.2 不在本计划内重做的 Rust 功能

规划、compaction、工具发现、subagent 内核、线程存储算法等已经由 codex-harness 提供的能力，只有具体跨系统合同失败时才修改平台 hook。兼容性审查以真实 composed source + overlay + fixtures 为对象，不能只测 upstream 原仓，也不需要逐函数重审所有上游能力。

## 3. Gateway、模型路由、容量和计量

**产品结果：** 管理员配置一次模型身份/策略，所有入口执行相同的限制；能解释路由与费用。失败、慢流和上游不确定结果不被当成成功。

| ID | 期/级 | 必须行为 | 可判定验收 |
| --- | --- | --- | --- |
| GW-01 | 一/B | model identity 使用 tenant/provider/model/revision；价格至少绑定 provider/model 与 price version，租户覆盖明确 | 两租户同名 model 定价不互相覆盖；错误返回不被记录成功；历史账单不被改价重算；S12 |
| GW-02 | 一/B | invoke、stream、proxy、model plane复用admission；run/provider-call/tool/SSE槽分维度、固定领取顺序，同维度转接不重复计数 | 并发上限1时stream不能绕过；单Agent在run/provider容量均1时多轮不死锁；deadline/取消释放正确；S13 |
| GW-03 | 一/B | retry 依据 retryability、dispatch 阶段、幂等性、总 deadline；不同层不倍增重试 | 写 POST 上游已接收后断流，不能无条件重放；保留 unknown + reconcile；鉴权错误不反复重试；S14 |
| GW-04 | 一/R | 无样本/未探测/配置就绪/真实探测/运行错误独立；指标显示分母、窗口、来源和新鲜度 | 零请求、Redis读失败、无模型均不展示伪成功率；“最近探测”来自实际验证时刻；S15 |
| GW-05 | 二/R | 版本化RoutePolicy在launch前选择；支持显式候选、硬过滤、priority/weighted/cost-latency与explain/dry-run | 决策可解释；非法能力过滤；policy发布/回滚影响新run；不放松已有lease的provider/model绑定；S16 |
| GW-06 | 二/R | 能力 profile 记录来源与实测证据；配置模型不等于验证所有功能 | UI 展示 text/tools/images/structured outputs/native tools 的支持/未知；unsupported 在调用前失败；过期 probe 不算当前通过 |
| GW-07 | 二/R | reserve/settle/reconcile 统一预算与费用；token/费用估算和真实 usage 分离，价格版本冻结 | usage 缺失、缓存、reasoning、重复回调、未知失败都有账本闭环；平台预算与本地预算不混算；S17 |
| GW-08 | 三/F | 在线优化与运行中跨模型fallback须独立合同；后者创建新授权snapshot/lease/attempt，保留旧账态 | 授权A→B成功、未授权B拒绝；A snapshot不变、A unknown不盲重试；实验有质量下限/成本收益/回切 |
| GW-09 | 三/R | 多实例前明确 scheduler/limiter/session/job/stream owner；用 DB fencing 与可恢复通知解除 singleton guard | 两实例抢同 run 只一个有效 owner；旧 owner 不可结算/写副作用；关任一实例客户端可恢复；S18 |

### 3.1 路由配置 UX

保留服务路由、模型路由、Agent 选择的语义区别。面板按“对象→规则→有效配置→模拟→证据→发布”组织；某模型不可选必须解释是未配置、无权限、不支持工具、context不足、预算不足还是验证过期。不要只给多个没有解释的模型下拉框。

传统服务适配器可以继续存在，但每个公开入口都必须出现在支持矩阵中；兼容入口不得享有弱化的授权、计费和终态规则。

## 4. 知识库、解析、索引与 RAG

**产品结果：** 一份资料从导入到可引用结果有完整的来源/版本/权限链，后台操作可恢复，管理员能用真实检索数据选择配置。

| ID | 期/级 | 必须行为 | 可判定验收 |
| --- | --- | --- | --- |
| KB-01 | 一/B | 所有 Dataset 访问使用 ID-02 的 tenant-qualified ACL；覆盖缓存、检索、QA、下载、来源与导出 | 撤权后旧缓存和 pinned run 都不能继续读；未授权错误不泄漏文档存在性；S02 |
| KB-02 | 一/R | parsing/chunking/embedding/retrieval 配置在保存时统一编译校验；返回 effective config、来源与警告 | 无效 backend/shape/empty stages 保存即4xx；不可用后端不显示为可执行；兼容旧字段但歧义拒绝；S19 |
| KB-03 | 二/R | 接通真实 page/block producer 与至少一种真实 parser backend；保留已有 IR/cache/version | 多页中英扫描/文本/表格 PDF 页号、reading order、bbox 和引用对应；重启后可复用已完成页；S20 |
| KB-04 | 一/R | 数据集目录分页完整；全量操作有完整性标记，不能固定200库或10000段静默截断 | 201个库全部翻页可达；第10001段参与全量任务，或清楚拒绝超范围；S21 |
| KB-05 | 一/B | 保留 ingestion durable claim、process-rule snapshot、generation fence；错误保留可用旧版本 | reprocess/reembed/rechunk 语义分别验收；失败/取消不把老可检索代清空，重启能接管；S22 |
| KB-06 | 二/R | 复用现有DocumentBatchStore/worker和batch-reindex/batch-delete operation，扩展Dataset删除和dedupe，不另建队列 | 保持现有operation ID/state；202后可查进度；删除栅栏先于清理；断连/kill后恢复；S23 |
| KB-07 | 二/R | 形成单一 RetrievalPlan 与 effective-config compiler；legacy/flat/nested/preset/API/Agent投影兼容 | 相同语义配置得到相同plan hash；预设与手动修改不静默漂移；端到端 deadline覆盖embedding/recall/rerank |
| KB-08 | 二/R | 检索 trace 展示各阶段候选、score semantics、过滤/降级、时间、索引/ACL/config版本；引用可定位页/块 | 用户可从结果回到授权来源；归一化score不叫置信度；rerank不可用明确degraded而非伪执行 |
| KB-09 | 二/R | 索引自检与真实query相关性质量gate分开；索引切换同时满足结构正确和质量无回归 | “原段找自身”通过但真实query失败的候选禁止发布；黄金集绑定真实文档/段和revision；S24 |
| KB-10 | 二/R | Connector 同步有来源游标、增量/删除语义、ACL映射、重试/限额、连接健康与最近成功同步 | 来源删文/撤权后索引和cache收敛；重复同步不重复文档；credentials只在server broker解析 |
| KB-11 | 三/F | query rewrite/HyDE/agentic retrieval/late-interaction/多模态增强作为可选实验profile | 同语料同ACL同预算配对比较；报告收益/回退/成本，不因算法名默认全开 |
| KB-12 | 二/R | 按实际所有权消除 `_ks`反向注入、私有方法调用、重复structured conversion；明确ports | retrieval/ingestion可由显式依赖单独构造；冻结样本的chunk ID/来源/结果兼容；不引入另一个万能facade |

### 4.1 后台生命周期规范

用户看到的是“上传→解析→切分→嵌入→索引校验→可用”，而不是多个不对应真实阶段的 loading。每个阶段都显示操作版本、完成/总量、重试和下一动作。API 断开不取消已接受的 job；取消被接受但已发生不可逆删除时，不得承诺撤销。

导入支持的文档格式、页数/大小/语言/图片限制来自运行能力，不能只来自扩展名白名单。OCR/parser 缺失时说明哪项能力不可用，保留原件与已完成处理结果。外部下载、HTML/Office解析和压缩包需要现有安全限制持续覆盖。

### 4.2 RAG 质量范围

必须分别测解析、chunk覆盖、检索Recall/nDCG、引用定位、答案faithfulness/abstention、多轮知识选择以及ACL隔离；不能用单一RAGAS总分替代所有维度。表格、数字、中文术语、跨页内容、无答案、矛盾来源和已删除文档是必需切片。数据规模与基线见06，不在此伪造“真实业务SOTA”。

## 5. 通用评测平台

**产品结果：** 用户能从一次失败复现出用例，冻结候选并重复试验，再用证据批准 Agent、检索和模型路由的版本升级。

| ID | 期/级 | 必须行为 | 可判定验收 |
| --- | --- | --- | --- |
| EV-01 | 一/B | Eval identity满足ID-01；candidate/judge固定ModelRef；judge launch显式空工具权限及有限预算 | 身份错配先失败；恶意候选输出、工具历史和native-search profile均不能使judge产生任何capability dispatch；S01/S40 |
| EV-02 | 一/B | outbox具备lease/owner/reclaim/fencing/幂等终态；运行中取消和热更新不丢任务 | claim批次后kill；未执行项可重新claim；已执行副作用不重复；旧worker写结果拒绝；S25 |
| EV-03 | 一/R | 单次试验区分succeeded/failed/cancelled/infra_error/invalid/skip，不把无输出和judge失败归为正常低分 | 截断流、judge超时、dataset错版均保留原因和分母；不被总分掩盖 |
| EV-04 | 二/R | 通用Suite/Trial由Gateway拥有；RAG黄金集与release pointer继续由KS golden store拥有；跨域引用版本合同 | 结果可恢复配置；Gateway不直写KS黄金表；无两份独立可改的RAG标签；S26 |
| EV-05 | 二/R | outcome-first：文件内容、DB状态、工具效果、引用与任务完成由独立oracle判定；LLM judge补充语义质量 | Agent声称完成但无文件/内容错误必须失败；模型不能通过输出“PASS”骗过gate；S27 |
| EV-06 | 二/R | 分离开发集/回归集/隐藏评估集；黄金标签需真实review provenance；训练/调参不可污染holdout | 人审缺失明确blocked release；样本生成者不能自签reviewed；来源/许可/脱敏状态齐全 |
| EV-07 | 二/R | 重复试验、配对比较、置信区间、失败切片和成本/延迟共同展示 | 每个指标带n、attempt、有效分母和uncertainty；多次尝试不冒充单次可靠性；S28 |
| EV-08 | 二/R | Eval结果直接绑定Agent/RoutePolicy/Index publication；online sampling→去敏→评审→回归集 | 失败候选不能publish；人工例外带期限/范围/签名依据，不能改黄金答案使其过关 |
| EV-09 | 三/F | 支持外部harness任务适配和隔离执行资源；统一资源/版本/失败协议 | 不运行第二套产品Agent loop；外部bench仅声明其覆盖范围；任务间文件/网络/身份隔离 |

## 6. 独立 CLI：必须是可用产品

| ID | 期/级 | 必须行为 | 可判定验收 |
| --- | --- | --- | --- |
| CL-01 | 一/B | 保留本地composed native CLI、隔离home、CLI自有provider profile；Gateway兼容命令显式命名 | Gateway关闭时local模式仍可启动、调用已配置provider；hosted秘密/会话不自动导入；S29 |
| CL-02 | 一/B | Chat adapter有界frame/文本/reasoning/工具参数/事件队列，响应遵守drain与客户端取消 | trickle无分隔符、超大args、慢消费者、断连均有限内存并单一失败终态；S30 |
| CL-03 | 二/R | 交互和headless共有命令合同：stdin/文件输入、结构输出、stderr诊断、signal、resume、审批拒绝 | `--json` stdout只含schema事件；失败非0；SIGINT不重复写；恢复保持工作目录/权限；S31 |
| CL-04 | 二/R | provider/profile配置有验证、显式优先级、secret env引用/OS store；不可表示能力提前报错 | 非TLS远端拒绝；密钥不在argv/config/子进程env非授权字段；多个profile互不污染 |
| CL-05 | 二/R | doctor检查runtime identity、协议能力、可选工具、文件权限和provider就绪；输出可分享脱敏包 | 缺binary/receipt/不支持架构可定位；不要求安装完整Gateway；绝不上传诊断包默认外传 |
| CL-06 | 一/R→二/R | 声明真实OS/arch矩阵；首期完成已有Linux目标，二期补macOS；Windows不得无制品宣称支持 | 从干净安装包执行text/tool/deny/cancel/resume；LICENSE/NOTICE/SBOM和来源receipt随包；S32 |
| CL-07 | 二/R | ACP作为客户端适配层，映射会话、流、tool permission、取消与错误；不内嵌IDE专用loop | 支持至少一个真实ACP客户端；不支持能力显式协商；IDE重连不重复执行 |
| CL-08 | 三/R | local state升级/导出/备份/诊断有格式版本与恢复合同；本地知识附件有明确容量与清理 | 升级失败旧版本可恢复；用户文件不因任务清理被删；无“清缓存修复一切”默认流程 |

## 7. 桌面端：从真实工作流设计

本轮源码未发现可分发桌面应用入口。`apps/local-node` 已有配对、目录授权、审批、设备驱动边界，它是能力节点基础，不等于桌面产品已完成。默认二期交付macOS arm64候选，三期扩展已验证平台；不一次规划所有平台却都不可运行。

| ID | 期/级 | 必须行为 | 可判定验收 |
| --- | --- | --- | --- |
| DS-01 | 二/B | 工作区先选Local或Hosted；显示执行位置、模型profile/tenant和数据去向，保留各自历史 | 断Gateway只影响Hosted；切模式不会带错密钥/历史/KB；本地文件上传远端明确提示；S33 |
| DS-02 | 二/B | Local sidecar使用同源App Server的稳定authenticated IPC；启动/端口/锁/崩溃/重启受控 | 关闭窗口/应用退出/sidecar崩溃三种策略明确；孤儿进程0；旧sidecar版本不兼容拒绝；S34 |
| DS-03 | 二/B | Tauri IPC按window/command/capability限定；remote HTML/消息/插件输出无shell/secret权限 | 恶意文档链接或注入脚本无法invoke filesystem/process；allowlist可负测；CSP有效 |
| DS-04 | 二/R | 文件/目录/设备权限采用OS broker与scope；credentials存OS安全存储，不在renderer/localStorage | 只授权目录A不能读B；撤销后已打开任务也重新校验；renderer无法读取provider secret |
| DS-05 | 二/R | 真实任务工作台包含任务列表、审批收件箱、恢复状态、文件/引用预览、可取消进度 | 同一任务在列表/详情/通知状态一致；审批展示实际对象与动作；trace错误可定位；S35 |
| DS-06 | 二/R | 复用客户端事件reducer与展示组件；键盘、焦点、长列表、缩放和系统主题可用 | 10k事件不每次全量渲染；新消息不抢用户滚动；Tab/ESC/屏幕阅读器状态合理 |
| DS-07 | 三/R | 签名/公证/更新/版本兼容/回滚/崩溃报告闭环；按每个平台真实打包验收 | 错签名更新拒绝；下载中断可恢复；保留用户状态；诊断上传需明确选择；S36 |
| DS-08 | 三/F | 受控远端连接/跨设备导出导入；未来同步先定义冲突与撤权规则 | 无后台静默同步；跨环境的local工具授权不继承；import只创建新owner下受限副本 |

### 7.1 第一条桌面端完整旅程

安装可信包 → doctor通过 → 选择Local → 配置已有provider → 授权一个测试目录 → 读取合成文件 → 生成一个结果文件 → 明确审批 → 取消一次任务并恢复另一次 → 查看artifact与实际写入 → 退出并重开恢复历史。

Hosted旅程单独验收：登录现有平台 → 选择发布Agent/KB → 执行 → 查看相同trace/事件语义 → 断线恢复。两条旅程均不得以嵌入网页能打开作为完成。

## 8. 产品可操作性与信息架构

| ID | 期/级 | 必须行为 | 可判定验收 |
| --- | --- | --- | --- |
| UX-01 | 一/R | 移除无证据的SOC-2认证、固定QPS/P99/可用率；demo数据有明确标签且与真实观测隔离 | 无请求页面不出现被当作实测的宣传数；认证只有可引用有效证明才显示 |
| UX-02 | 一/R | 错误统一包含可安全公开code、阶段、retryability、run/trace关联、下一动作 | 用户不需要刷新/清localStorage/重建账号才能处理普通网络错误；内部堆栈/秘密不出UI |
| UX-03 | 二/R | 以任务→资源→评测→发布→运维导航组织，保留现有可用路由和深链接 | 从Agent失败一跳进入对应trace，再生成脱敏eval用例；返回不丢上下文 |
| UX-04 | 二/R | 配置控件对应有效合同；高级检索/模型参数渐进展开，明确默认/继承/覆盖/版本 | UI保存后读取effective config一致；CLI与Web同值不同默认时显示部署差异 |
| UX-05 | 二/R | 核心旅程覆盖loading/empty/denied/degraded/retrying/cancelled/failed/stale，不只有成功态 | 键盘与窄屏可完成核心操作；动态状态可辅助技术读取；截图只证明可见布局，不等于完整无障碍认证 |

## 9. 可观测性与运维证据

| ID | 期/级 | 必须行为 | 可判定验收 |
| --- | --- | --- | --- |
| OB-01 | 二/R | run/model attempt/tool/retrieval/job/usage贯穿correlation；复用现有trace与OTel，通过版本化映射导出 | 从一条任务定位路由、KB、工具和费用；跨HTTP/worker关联不断；不把实验semconv当公共event协议 |
| OB-02 | 一/R | dashboard/Eval百分比区分请求成功、任务outcome成功、评分通过、gate通过；未知token/未评分不能写成真实0 | Gate=not_run且无评分时不显示已验证质量100%；usage缺失显示unknown；S15扩展覆盖 |
| OB-03 | 二/R | 遥测队列有界、采样/保留/脱敏按tenant策略；失败可见且不阻塞产品主路径 | 观测后端中断时主请求遵守SLO，队列不无限增长；丢失数与backlog可见，不生成假完整trace |
| OB-04 | 二/R | 指标label限制基数，避免user/run/raw-query作为metric label；trace原文权限、审计和retention独立 | 大量唯一run不爆指标内存；未授权用户不能读他人prompt/artifact；导出不带秘密或隐藏CoT |

## 10. 规范化开发、清债与发布

| ID | 期/级 | 必须行为 | 可判定验收 |
| --- | --- | --- | --- |
| HX-01 | 一/R | 一个激活program、一个writer，需求ID/owned_paths/契约/失败oracle/门禁在包开始前冻结 | 冷启动只读入口即可恢复；旧next_action不能触发第二个执行程序 |
| HX-02 | 一/R | 复用affected-gates/CI enforcement；新增路径/契约必有实际收集门禁，门禁失败闭合 | 负向修改确实触发对应gate；零收集、意外skip、命令缺失不被绿灯吞掉 |
| HX-03 | 一/R | 一期消除四项2026-09-30到期import违规与对应allowlist entry，不直接延长；其余core迁移归二期 | 这四项live violation和allowlist entry均为0，负向自测仍拒绝违规；原API/事务行为保持 |
| HX-04 | 一/R | 证据记录测试源码+patch/image/schema/config/fixture身份；历史receipt不代替当前HEAD | source与running image漂移明确标示；不能拿旧UI或某个unit pass宣称整栈通过 |
| HX-05 | 一/R | 清理文档事实漂移；未来维护短入口+owner doc+自动inventory，不重复巨大harness规则 | 本轮已存在的gate/role/launch不再在活文档写“尚缺”；旧计划有successor/处置表 |
| HX-06 | 二/R | 技术债ledger按影响/触发/owner/删除条件追踪，重构围绕权威边界 | oversized/duplicate/unused候选需动态消费者证明；不为降低LOC重排上游overlay |
| HX-07 | 二/R | 开发环境可重复bootstrap且有限资源；共享Docker/数据/缓存/账号有owner和锁 | 错worktree拒绝runtime变更；文档只读任务不重建；失败可输出一个具体可恢复blocker |
| HX-08 | 二/R | 长程coding有阶段收据、未验证列表、下一个可执行命令、人工校准反馈 | context丢失后不依赖聊天恢复；review修复后仅重跑受影响验证，不无限循环 |
| RL-01 | 一/R | 发布从同一commit收集完整Runtime/Worker/Gateway/Knowledge/Web/迁移/SDK兼容manifest | tag构建不得跳过required证据；缺失一个已承诺artifact或receipt不得promote；S37 |
| RL-02 | 一/R | 干净机器安装、真实provider、数据库升级与完整冻结回滚是单独证据 | `make status`不能替代fresh install；本地tag不能冒充公开可拉取多架构镜像；S38 |
| RL-03 | 三/R | 容量、可用性、故障恢复、数据备份恢复按明确环境和负载验收 | 达到06的默认目标或经数据修订的新目标；不能仅将replicas改2；S18/S39 |
| RL-04 | 二/R | 供应链锁、依赖owner、SBOM/许可证与漏洞处置跟随实际制品 | source lock、overlay、binary、NOTICE版本一致；publish的内容就是被测包 |

## 11. 全局验收约束

任何“支持”必须有实际范围：具体入口、wire/provider版本、OS/arch、数据规模、凭证模式、故障类型与证据层级。一个成功样例只证明该样例路径；不能推广为整个模块SOTA。

所有需求不得退化为自证明测试。比如脚本存在≠已构建native；数据库字段存在≠重启能reclaim；IR类存在≠真实PDF按页解析；profile有tools≠本次允许tools；截图能看≠用户能完成工作流。工作包完成要同时满足行为、边界与证据。
