# Codex 内核更新与 Gateway 兼容优化计划

- status: completed_local（按用户最新收口范围完成本地升级、最小修正和独立复测；完整发布矩阵未完成）
- domain_id: agent-runtime-upstream-sync
- owner: root集成session；用户明确授权互斥路径的并行实现代理
- last_verified: 2026-09-22
- successor: none
- prerequisite: 实施已授权；当前源码目录仍仅作只读来源，源码与实机状态以执行账本及对应receipt为准。
- 平台基线：`305ef0c14bb22a65894bc8a2bfaa1a721ac1e2f6`
- 升级前 pin：`94cbbddafc1776d5e377bca1b05932c697e82238`
- 选定目标：`279ba894152b2c01c5294cc0723b463b209bdca4`
- 来源：`/Users/yang/projects/opensource-harness/codex-harness` 的本次干净 HEAD；不是对远端永远最新版本的声明。

本次结果见[实现与实机报告](../../reports/architecture/codex-harness-refresh-2026-09-22/implementation-verification.md)；下述完整矩阵保留为原设计目标，不能将未执行项视为通过。

这轮采用目标 SHA 的完整上游源码，在其上重新移植平台扩展；Codex 继续拥有唯一模型/工具循环、上下文压缩、多代理调度和中断。Gateway 保留身份、模型与工具授权、配额、计费及公开接口。不能只改 pin 后用旧 overlay 覆盖新源码，也不能为躲开迁移而另写 Agent loop。

“完美兼容”在本计划中是可检查的交付条件：现有已支持接口、历史数据、授权和用户旅程全部保持；上游新增能力逐项有接入或显式未开放的决定；没有未解释协议丢失、未知副作用重试、越权或质量退化。设计完成不证明运行时已经满足这些条件。

证据：[源码差异](../../reports/architecture/codex-harness-refresh-2026-09-22/README.md) · [验收矩阵](codex-harness-refresh-2026-09-22-acceptance.md) · [执行账本](../../deploy/runbooks/codex-harness-refresh-2026-09-22/loop-state.json) · [工作包](../../deploy/runbooks/codex-harness-refresh-2026-09-22/work-packages.yml)。旧升级程序与一期记录保留为历史证据，本计划不重新打开它们。

## 1. 已核对的差距与设计选择

范围内有 1,159 个提交、5,045 个变更路径（`--no-renames` 口径）。overlay 共 107 文件：82 个平台新增、2 个仅平台改变、23 个双边改变。23 个的三方文本预检中 10 个有冲突、13 个可文本合并；文本合并成功不等于 API、行为或编译兼容。

当前 builder 在 `git archive <pin>` 后执行 `cp -R overlay/kernel-rs/. codex-rs/`。这使旧版完整文件有能力覆盖新版同路径实现。高风险面包括 App Server 的 in-process/client/processor、核心生命周期、extension-api 和 Cargo.lock。详细到文件的分类与冲突数在源码差异 JSON 中。

| 选项 | 升级完整性 | 改动与维护成本 | 决策 |
| --- | --- | --- | --- |
| 只更新 lock，继续覆盖旧文件 | 无法证明；可能把新实现覆盖回旧版本 | 表面小，后续隐性分叉大 | 不采用 |
| 逐个 cherry-pick 1,159 个提交 | 依赖闭包难证明，容易遗漏协议变化 | 持续人工维护成本高 | 不采用 |
| 新建平台 Agent loop 或全面改为外部 App Server sidecar | 可能破坏唯一内核、PG持久化和已有宿主接口 | 架构重写、额外进程/状态边界 | 不采用 |
| 固定完整上游快照，三方重放最小平台扩展 | 可逐路径说明差异，保留新内核实现 | 一次性迁移成本较高，后续可重复 | 采用 |

维持现有 in-process App Server 方案和服务数；`AppServerHostRuntime` 是平台新增接口，不是假定上游已经提供的原生 API。保留它的预授权 thread/turn ID、ThreadStore 和扩展注入责任，再对目标 App Server 的新调用路径做最小适配。现有 ADR-006/007/009 的边界继续适用；如具体实现必须改变这些边界，先提出精确的 successor ADR，不通过代码静默改变合同。

## 2. 源码更新与制品策略

1. 冻结旧 upstream、平台 overlay、目标 upstream 三个输入及目标 tree SHA。执行中只用选定 SHA，不追移动分支。旧源码目录不写平台补丁。
2. 在隔离的 composed source 中，逐个处理 23 个双边文件；10 个文本冲突按接口/行为分组解决。13 个 clean merge 同样审查新增字段、默认值、调用次序与存储语义。82 个新增文件检查依赖和名称碰撞；2 个仅平台改变文件重新确认仍有必要。
3. 以目标 workspace 为准加入四个平台 crate（agent-runtime、capability-worker、capability-contract、office）及所需依赖；Cargo.lock 必须由目标 workspace 在 Docker/hosted CI 中重新解析，不能拼接文本或复用旧 lock 以绕过依赖变化。目标工具链当前仍为 Rust 1.95.0；同时遵循目标仓库的 Bazel lock 和生成代码约束，验证构建实际使用的闭包。
4. 上游已经提供等价 seam 的平台补丁迁入 seam；其余改动以明确理由保留。每个覆盖文件记录旧 blob、目标 blob、平台补丁、保留理由、测试和删除条件。生成 overlay 的过程遇到目标 blob 不匹配必须失败，禁止接受带冲突标记的结果。
5. builder 可以继续消费已审核 overlay，但必须验证其声明的目标 upstream blob，避免“新 pin + 旧整文件”。manifest 字节一致性只能证明输入身份，不能替代 upstream delta 审查。
6. 同步 source receipt、overlay manifest、Cargo lock、App Server schema、Capability schema、SBOM、LICENSE/NOTICE。App Server schema 必须来自目标 composed source 的生成器，并做旧/新 schema 差异和公开适配器映射；不能靠重新写 hash 把协议漂移认作通过。
7. Runtime 与 Worker 成对构建和校验。App Server probe 与 native CLI 各有独立制品身份，不以 Runtime digest 代替。新 source lock 清空旧镜像可启动资格；禁止继承上一版 `candidate_start_allowed` 或旧 digest。

## 3. 内核与平台扩展迁移

### 3.1 工具、权限与 Guardian

目标 `extension-api/src/tool_policy.rs` 提供不可在运行中放宽的启动工具上限；resume 必须重新提供，Code Mode 生成的 `exec/wait` 也需纳入明确名单。将平台已认证的不可变 capability snapshot 映射到它，同时保留 Gateway model-plane 出站过滤和 Worker dispatch 校验。三层使用相同命名空间与 revision；模型能力不构成授权，空名单不等于无限制。

目标新增 `ToolStartInput.originating_item_id` 和 `CommandStartInput/on_command_start`。记录 wrapper、call、root/child、turn 的关联，保留 yield/wait 后身份；指令完成 hooks/environment 解析后再验证真正的 executor 路径，不把远端 cwd 当宿主路径。敏感 argv 不写日志。命令回调不能引入第二套审批或让一个动态 capability 同时被两个生命周期 owner 记账。

上游 `guardianv2.thread_context` 默认值已变为 true；这**不等于 Guardian 整体自动启用**。执行时枚举目标所有 feature/config 默认差异，固化平台 feature profile。新的自动审批、hooks、plugins、Apps、浏览器、外部 MCP 发现与用户验证入口不得因升级自动取得平台权限。已有能力继续工作；若启用 Guardian，则其建议只能增加约束，不能覆盖租户 policy、人工拒绝或 capability proof，模型调用仍走 Gateway 并计费。保留 compaction 前后的用户授权证据，避免把旧自动审批计划重新注入。

### 3.2 子代理、上下文与模型请求

目标已将代理控制组织为 `AgentControl`/`LocalAgentControl`、`SpawnRequest` 和状态快照/订阅。迁移平台 `core/src/agent/control/spawn.rs` 的 scope 继承到新入口，保留 upstream 的发送、唤醒、恢复、容量和完成规则；不用 Gateway 再写调度器。

子代理继承 tenant/user/root/session、模型租约、工具上限和总预算；不能复制后扩大授权。父 run 结束、子代理结束和请求已接受是不同事件。agent snapshot 中出现 root subtree 之外的线程必须拒绝；resume、fork、steering、compaction 和 eviction 竞态要有直接反例。

采用目标 prompts/compaction/checkpoint 实现；平台仅注入已确认的系统约束、AgentSpec、Knowledge context 和运行预算。长历史中用户的新输入、拒绝、已完成工具结果和命名空间身份不能被压缩或投影丢失。避免把可变 token、时间戳、request ID 插入稳定缓存前缀。

### 3.3 ThreadStore 与数据兼容

目标 ThreadStore 新增 `ThreadPreparation`、`SteeredUserInput`、pending metadata、thread attachment 操作及 creator/workspace/lifecycle 字段；删除接口也加强了关联状态清理合同。当前 PG adapter 的 persist/flush/shutdown 是 no-op，只有在全部写入已经同步 durable 时才成立。须逐个核对 create、append、metadata、terminal 和异常路径的事务边界，而不是机械补空方法让编译通过。

PG 继续拥有托管 thread/turn/item 权威；上游本地 SQLite rollout 只服务独立 CLI，不能成为托管模式的隐性第二数据库。creation identity 来自平台认证主体，不能把 ChatGPT account 字段当平台 tenant。为每个新增字段决定存储位置、默认值、旧行回读和 N-1 可读性；禁止将 upstream SQLite migrations 直接用于平台 PG。

保留现有平台附件、artifact、Knowledge 引用。若新的原生 thread attachment 被启用，复用平台对象引用与 ACL，落实 fork 时原子复制 membership、新 ID、源/目标后续独立；未启用的原生 API 明确返回 unsupported，不能把已有附件能力也关掉。delete 必须有 tombstone、引用清理与并发幂等合同；不能因新接口语义直接物理删除用户数据。

## 4. Gateway、Web、SDK 和 Eval 兼容

| 边界 | 更新规则 |
| --- | --- |
| launch/identity | 在第一次模型调用前 durable 保存预授权 root/turn、immutable snapshot、ModelLease；新客户端字段不得绕过此顺序 |
| model plane | 唯一 provider credential/routing/pricing/usage owner 仍是 Gateway；Responses 原生与 Chat-only 两条 wire 都验证，Runtime 不直连外部 provider |
| retry/capacity | 保持 Runtime provider retries=0，由 Gateway 单一策略判定；发送后 outcome unknown 不重试；run/provider/SSE/job 容量分别统计，终态释放租约 |
| tools | 显式 `none` 跨多轮仍为 none；function-only 不自动出现 native search；同名跨 namespace 工具可请求、返回、历史重放闭环 |
| events | 从目标 protocol 生成清单，逐项映射公开 V1/V2；新增生命周期事件不得被 wildcard 静默吞掉。可忽略的诊断事件列明理由并保留内部观测 |
| history/recovery | whole-thread stream 可连续多个 turn；per-turn 仅关闭目标 run；子 run 终态不结束父 run；分页/游标、150+事件、断线续读、重启后 lease/审批一致 |
| Web | Assistant、Agent Studio、工具审批/拒绝/取消、附件、Knowledge 引用、Trace/Eval 对比及错误提示保持；新协议字段由 adapter 吸收，未经设计不改变页面范围 |
| SDK/CLI | Python/TS/Java/Dart SSE 与错误语义一致；独立 CLI 保持独立 home/state/provider 模式，流大小、背压与断连取消有界；不强制要求 Gateway 登录 |
| Eval | candidate/judge 身份、模型五字段、prompt/tool指纹、token/成本来自真实 span；job fencing/续租/reclaim 不变；质量失败、执行失败、unknown、skipped 不互相伪装 |
| Knowledge | 配置、目录分页、上传/索引/检索/引用及租户隔离不回归；embedding 失败保留可用旧 generation，不靠降低检索要求过验收 |

公开 OpenAPI 与 SSE 变更必须显式评审，更新 CHANGELOG、SDK 和兼容测试；内核升级不是改变公开接口的默许。允许新内部字段，但旧客户端与旧会话仍必须完成受支持旅程。

## 5. 优化顺序与收益判据

先产出可运行候选，再优化测量到的瓶颈；所有优化沿用相同任务成功、权限和副作用标准。没有测量值的项目写 NOT_MEASURED，不能承诺固定提升倍数。

| 优先级 | 优化 | 测量与接受条件（提议值，U0冻结） |
| --- | --- | --- |
| O1 必做 | 移除被上游取代的补丁，固定 blob 合同与升级 inventory | 每个保留 patch 有 owner/理由/直接测试；23个双边路径均有处理结论；不为追求 LOC 强删平台功能 |
| O2 必做 | 稳定模型前缀、连接复用、消除重复序列化与检索 | 复用已有 AsyncClient 和 timing，不另建池；拆分 admission、PG、Runtime、provider TTFT、投影延迟；相同模型/采样/上下文下测冷/热请求 |
| O3 必做 | PG 事件写入/读取、SSE背压与取消资源 | 仅在事务证据支持时批量写入；有界 channel/分页、退出 flush fence；慢消费者不能令内存随输出总量增长；取消后进程/slot/lease无泄漏 |
| O4 必做 | 子代理与工具并发总预算 | session级并发、层级、token/cost均有界；不把模型等待计为普通HTTP慢请求再次误熔断；排队、拒绝、取消可解释 |
| O5 发行必做 | 构建成本与依赖闭包 | Docker串行、BuildKit cache隔离、真实耗时/峰值内存/disk记录；changed-crate gate必须包含平台新增crate和上游依赖影响，不以静态测试代替编译 |
| O6 后续可选 | 异步持久化、更强缓存、扩展工具/Guardian产品化 | 只在 O2/O3 证明瓶颈且故障恢复合同可证明时做；新增功能另有授权入口，不是升级期间默认开启 |

性能样本固定任务、输入、模型/价格/工具版本、采样、硬件与并发，在旧/新候选间交错成对运行；冷启动与warm-cache分开。建议每种代表场景至少30对，覆盖短问答、多轮工具、长上下文、KB、多代理；报告成功率、总成本、TTFT/端到端p50/p95、平台净开销、RSS和取消回收时间，并给配对不确定性区间。单样本成功不是性能或质量结论。

提议发布门槛：所有确定性安全/协议用例零回归；平台净开销p95相对同环境基线退化不超过10%；同成功任务成本不增加超过5%；取消后本地执行资源2秒内回收（外部provider确认另记）；超过噪声或证据不足时追加样本，不以调整模型参数遮盖内核退化。U0可在测得噪声和已承诺SLO后修订阈值，但必须先冻结，不能看候选结果后改及格线。

## 6. 顺序、状态与回滚

| 包 | 交付结果 | 依赖 / 状态 |
| --- | --- | --- |
| U0 | 可信基线、CI失败归因、完整兼容矩阵及可用回滚清单 | 设计被选作实施入口后；queued |
| U1 | 目标源码 + 最小重放patch + schema/lock/生成物闭包 | U0 direct_verified；queued |
| U2 | 目标内核的PG/权限/工具/子代理宿主扩展可编译且反例通过 | U1 direct_verified；queued |
| U3 | Gateway/Web/SDK/CLI/Eval兼容集成通过直接和隔离测试 | U2 direct_verified；queued |
| U4 | 成对Docker候选可运行，核心真实旅程和数据安全通过，CANDIDATE_80 | U3 direct_verified；queued |
| U5 | 已测瓶颈优化，配对质量/性能达到冻结门槛 | U4 verified；queued |
| U6 | 完整故障/回滚/分发与旧客户端矩阵，RELEASE_100 | U5 verified及全部发行证据；queued |

U0当前已知输入：`305ef0c1` 的 Harness CI 已恢复，但 LOC no-growth 与 Rust builder合同测试仍失败；Rust changed-crate 因前置失败未执行，不是新目标的编译失败。修复时保留测试表达的安全合同，不删除失败测试或改 skip。源码身份本次17测试通过，只证明旧pin材料自洽。

工作包边界避免循环依赖：U0完成基线取证与归因，不要求先消除所有非功能性历史债务才能物化源码；旧失败不得伪记PASS，修复归属与最晚门槛必须登记。U1只证明目标源码物化、三方处置和workspace依赖清单，不声称完整编译或最终schema就绪。U2完成宿主API适配后生成schema/receipt并做不要求可运行镜像的source-only校验；U3完成其余集成；U4构建成对镜像后才运行要求可启动制品的完整source/release门禁。其间保持不可启动的开发状态，不能填入旧schema/digest让门禁过关。

C80 阻断项：不能启动/主旅程不通、跨租户或授权缺陷、历史不可恢复、未知制品身份、取消泄漏或重复副作用、候选没有可执行回退路径。外部目标平台和fresh-machine证据可以是R100_REQUIRED，但必须如实登记且最终完成；不能把历史缺失的 frozen bundle 当作今天可用的回滚物料。

升级前捕获当前可用的完整release unit：Gateway、Web、Runtime、Worker、Knowledge及migrator镜像digest、schema epoch、source/overlay/schema、配置schema与加密备份引用。只存引用不存秘密。当前 epoch floor/ceiling=2；先证明N-1仍可读新增数据。新增破坏性迁移必须改为expand/contract或采用停写备份恢复方案，不能默认旧镜像直接回切。回滚旧/新配对运行时与其他服务及数据库一起判断，工具dispatch账本保证回切不会再次执行写操作。

先在隔离栈验证 old→new→old→new，再串行进入当前本地栈；公共服务仍仅Gateway/Web。托管Gateway/Runtime保持现有单实例约束，不能用本轮多代理API变化推导出支持多实例部署。生产发布、共享库迁移或外部写操作仅在执行阶段的明确授权下进行。

## 7. 后续跟进制度

每次升级启动时刷新只读upstream并固定SHA；生成 source delta、feature/config默认差异、protocol差异、overlay三方差异、依赖/许可证差异五张清单。以一次可运行候选为批次，避免同时积累多个未验收分支。建议每1～2周评估一次新快照；本设计不自动创建定时任务。

每个patch标记下次upstream可替代的条件，持续减少核心覆盖面积；自动化只拒绝未知变化，不伪造测试覆盖。主Session负责单一writer与集成，独立只读评审覆盖租户、存储、生命周期和制品；评审不得通过文档口头结论替代编译后的用户旅程。

本次设计的完成标准是：当前基线和目标可复核、差异与决策逐项有来源、工作包/接口/优化/验证/回滚/风险完整、文档gate通过。实际升级的完成标准是验收矩阵全部必需项有目标版本证据；两者分别记账。

## 2026-09-22 实施调整

用户要求核心Rust直接采用目标上游代码，简化旧补丁迁移。执行按完整快照进行，仅保留预授权ID、PG store/扩展注入、工具dispatch和durability这几类无法由原生接口完整承接的最小宿主接缝。多代理按互斥路径并行，root统一source/Cargo/制品、Docker与实机验收。当前测试通过只覆盖各自已执行的离线范围；真实Rust编译和Docker/浏览器结果另记。

目标原生rollout增加旧内核无法解码的条目，回滚类别确定为restore-required；不得因DB epoch未变就用旧镜像读取目标写过的数据库。保留目标原生历史，保存基线和目标数据，在隔离数据库上验证恢复；任何未保全升级后写入的恢复方案不算无损回滚。完整R100证据仍按原矩阵，不因代码完成提前勾选。


## 2026-10-12 stable source-only follow-on

The historical 2026-09-22 local closeout above remains historical. A subsequent
source-only refresh selects official stable `rust-v0.162.1`, target
`092d3acd6bec3e3a14bdc7e7a2810ab628ab759d`. All 130 overlay paths, resolved lock,
v2 target-blob provenance, actual compiled protocol schema, source/Worker SBOMs,
receipt and notice move together. No old image identity survives this change.

The earlier release's complete 1,054-file schema was recovered from its immutable
upstream tree and exactly matched the old release bundle hash. Comparison against
the newly compiled 1,060-file schema identifies 14 additions, 8 removals and 68
changed files. Static adapter review and evidence limitations are recorded in
[the source-only update report](../../reports/architecture/codex-stable-source-2026-10-12.md).
This is source integration with startup disabled, not completion of the full
acceptance matrix, linked Runtime/Worker binaries, images or deployment.
