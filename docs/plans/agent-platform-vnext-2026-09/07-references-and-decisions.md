# 07 2026 一手资料对照与架构决策

status: queued · domain_id: agent-platform-vnext · owner: primary Codex · last_verified: 2026-09-07 · prerequisite: 相关技术选型时刷新版本 · successor: null

## 1. 对照方法与结论

这些资料用于判断工程方向，不构成统一行业标准、性能排行榜或本项目达标证明。日期以页面明确发布日期为准；未列发布日期的是2026-09-07访问的滚动官方文档。实现时必须固定协议/软件版本，不跟随`latest`自动升级。

当前项目已经具备单内核、控制/执行隔离、持久知识生命周期、工具审批和评测合同等基础；主要差距在跨入口正确性、真实后端装配、证据、发布和可操作性。下表“采用”均是结合本仓源码作出的设计判断，不是来源对本项目的背书。

## 2. 一手来源与具体采用范围

| Ref | 来源与时间 | 支持的工程方向 | 本PRD采用/边界 |
| --- | --- | --- | --- |
| R01 | [OpenAI：Harness engineering](https://openai.com/index/harness-engineering/)；2026-02-11 | 仓库知识可导航、机械约束、运行与观测对Agent可见 | HX-01～08：短入口/owner docs/真实门禁/可运行候选；不复制文章项目的吞吐量或效率成绩 |
| R02 | [OpenAI：The next evolution of the Agents SDK](https://openai.com/index/the-next-evolution-of-the-agents-sdk/)；2026-04-15 | 模型原生harness与sandbox执行是Agent基础设施的一部分 | CP-05/08：保留codex-harness，改善平台sandbox/协议接缝；不更换现有内核来追SDK功能清单 |
| R03 | [Anthropic：Scaling Managed Agents](https://www.anthropic.com/engineering/managed-agents)；2026-04-08 | session log、harness与执行环境可以分离 | C-04/05：持久事件与执行sandbox职责分离；本项目不照抄其托管服务拓扑 |
| R04 | [Anthropic：Harness design for long-running applications](https://www.anthropic.com/engineering/harness-design-long-running-apps)；2026-03-24 | 分解长任务、结构化交接、独立评估器与明确标准 | HX-08/EV-05：可恢复包与failure oracle；不增加无限并行writer或无止境review |
| R05 | [Anthropic：Demystifying evals for AI agents](https://www.anthropic.com/engineering/demystifying-evals-for-ai-agents)；2026-01-09 | task/trial/grader/outcome区分，组合代码/模型/人工评估，重复试验 | EV-03～08：独立outcome、重复试次和明确分母；不以Agent声称成功或单次样例代替质量 |
| R06 | [Anthropic：Effective context engineering](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents)；2025-09-29 | 上下文是有限资源，需要选择、压缩和结构化管理 | 只影响平台知识/工具/策略输入与开发上下文组织；不重做上游compaction算法 |
| R07 | [LangGraph：Persistence](https://docs.langchain.com/oss/python/langgraph/persistence)；滚动文档 | thread checkpoint与跨thread store有不同用途 | C-04/Agent memory：短期运行恢复与长期记忆分清；不把LangGraph嵌成第二内核 |
| R08 | [MCP Authorization，2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization) | HTTP授权、受众绑定、resource metadata、scope等协议安全 | ID-05/CP-09：按固定协议版本接入；STDIO与HTTP凭证模式分开，内部token不外传 |
| R09 | [MCP Tasks，2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25/basic/utilities/tasks) | 该版本Tasks明确为experimental，定义长期任务交互 | 可选adapter，不能取代本平台durable job权威；未协商则不用；不宣称是已稳定通用任务协议 |
| R10 | [Agent Client Protocol v1 Overview](https://agentclientprotocol.com/protocol/v1/overview)；滚动文档 | Agent与编辑器客户端的会话/消息/权限交互边界 | CL-07：ACP是CLI/IDE surface适配；不与MCP混同 |
| R11 | [A2A Protocol specification](https://a2a-protocol.org/latest/specification/)；滚动官方规范 | 外部Agent发现、消息、任务与互操作合同 | CP-09：三期按明确版本试点；`latest`只是研究入口，发布前必须锁revision和认证矩阵 |
| R12 | [Qdrant：Hybrid and Multi-Stage Queries](https://qdrant.tech/documentation/search/hybrid-queries/)；滚动文档 | dense/sparse组合、prefetch、多阶段融合 | 现有hybrid/RRF已实现；统一score semantics/profile版本后比较，不直接更改旧RRF默认常数 |
| R13 | [Qdrant：Multitenancy](https://qdrant.tech/documentation/manage-data/multitenancy/)；滚动文档 | payload/shard/collection的隔离与开销权衡 | 先修ACL与data owner，再按规模测collection方案；不为“新架构”直接合并所有租户集合 |
| R14 | [LiteLLM：Router / Load Balancing](https://docs.litellm.ai/docs/routing)；滚动文档 | 明确的部署集合、routing策略、timeout/cooldown/限额 | GW-02/05：引入可解释策略与统一资源语义；保留本项目lease规则，不照搬隐藏retry或另部署一个代理 |
| R15 | [OpenTelemetry GenAI attributes](https://opentelemetry.io/docs/specs/semconv/registry/attributes/gen-ai/)与[semconv入口](https://opentelemetry.io/docs/specs/semconv/)；滚动文档 | GenAI operation/tool/retrieval等语义属性；部分仍Development且入口已迁移 | OB-01～04：版本锁定的OTel导出映射；领域事件协议自有稳定版本，不把实验semconv变成永久公共API |
| R16 | [Tauri 2：Capabilities](https://v2.tauri.app/security/capabilities/)；滚动文档 | window/WebView/command能力边界，多个capability权限组合需谨慎 | DS-03：显式IPC allowlist与负向测试；不能假定使用Tauri即自动sandbox全部自定义命令 |
| R17 | [Tauri 2：Updater](https://v2.tauri.app/plugin/updater/)；滚动文档 | updater签名校验与实际平台artifact分发 | DS-07：签名升级、失败恢复；OS签名/公证另验，不与updater签名互相替代 |

引用只支持对应工程特征；性能/SLO/样本规模与三期安排是本项目设计假设，不能归因于这些来源。产品、协议、工具文档会变动；后续固定版本前再次核对，不使用未经验证的转载结论。

## 3. 多角度差距判断

| 角度 | 现有基础 | 关键差距 | 选择 |
| --- | --- | --- | --- |
| 架构 | 三域、单kernel、polyglot、DB角色 | 一部分core业务仍跨域，活文档与边界状态漂移 | 维持部署，按owner收敛，消除明确例外 |
| 模块化 | control/model已拆，RAG组件与纯contracts存在 | 私有反向调用、重复配置/评分语义、多份适配器错误规则 | 明确ports与单一effective-config/identity authority |
| 功能 | KB/Agent/Trace/Eval/CLI入口完整 | IR后端未接通、恢复/异常路径不完整、桌面未形成产品 | 从可验收旅程增量补齐，不再堆菜单 |
| 可靠性 | durable store/lease/index fence | Eval claim无reclaim、SSE终态缺口、资源预算不彻底 | 恢复状态机、fencing、幂等与unknown outcome |
| 质量 | 有golden/评测器/真实RAG gate | 结构自检与语义质量混用，候选身份/人审证据不足 | 真候选、独立oracle、分层统计与发布绑定 |
| 性能/成本 | capacity、缓存、预算、计量已有 | 入口资源策略/模型价格身份分裂，无当前受控容量结论 | 先测平台/Provider分段，再版本策略优化 |
| 安全/治理 | 租户/ACL/approval/local-node边界 | 同名role、eval身份、工具授权、过宽DB grant | 先修具体边界，禁止“配置即授权” |
| 开发效率 | Make/CI/harness较丰富 | 旧文档、例外、缺release证据、过大owner | 可执行工作包和真实receipt，防止AI coding重复/虚假完成 |
| 客户端 | Web成熟、独立native CLI路径成立 | OS/arch打包证据不全、stream无界、桌面需设计 | 一套事件语义、Local/Hosted身份隔离、可信分发 |

## 4. 明确选择与未选择的方案

### DEC-01 不继续全量Rust化

保留Python Gateway/Knowledge与Rust Agent Execution。性能是否需要语言替换必须由相同负载的瓶颈证据决定。当前已确认的大部分问题是授权、状态、数据/配置owner与证据问题，换语言本身不会解决。

### DEC-02 不新增常驻服务来表达每个逻辑模块

Eval、routing、governance先在现有部署内形成清晰owner。只在资源干扰、容量、故障隔离有测量且运维成本可接受时由新ADR拆服务。保留现有单实例guard，不能靠调replicaCount制造HA。

### DEC-03 保留上游kernel，建立平台compatibility层

采用固定source/overlay/seam清单和consumer/wire矩阵，改最小hook。替代方案“每种surface写一个Agent loop”被排除，因为其事件/权限/取消/上下文语义会分裂。上游新增能力先归类supported/unsupported/unverified，不能依赖上游存在就宣称本平台兼容。

### DEC-04 两种部署身份，协议语义复用

Local CLI/Desktop保留本地provider和state；Hosted继续由Gateway管理租户、密钥和计费。共享fixture/规范化类型/纯reducer可降低漂移；共享数据库、secret和默认history会破坏ADR-009，排除。

### DEC-05 显式策略先于学习路由

先提供ModelRef、RoutePolicyVersion、explain与稳定预算/价格，再建立实验分流。自学习路由或semantic cache需要真实任务数据与质量约束。不能只以每token价格低为目标，也不能在已有signed lease下直接换provider。

### DEC-06 普通知识入库与策略晋升分开

普通上传/重处理要完整性、ACL与generation一致；parser/embedding/retrieval默认策略的晋升才要求冻结query语义质量。原段自检保留为基础gate。所有高级RAG方法默认是实验profile，不等同效果改善。

### DEC-07 Tauri是桌面候选，不是未经验证的唯一真理

基于现有React/Rust技术与本地App Server，先用Tauri 2验证IPC隔离、sidecar、凭证、更新和真实任务。若WebView兼容/无障碍/打包/插件需求无法满足，采用同一旅程比较Electron的代价，不同时实现两套桌面UI。必须走真实hosted macOS/Windows builder，不突破本机禁止host Cargo的约束。

### DEC-08 物理版本与安全优先

记录hash不自动获得可重放历史内容。只有保留物理内容/索引且有访问权时才允许pin；否则明确unavailable。ACL安全修复不可因通用N-1回滚重新放开；允许回切的是上一个安全且兼容的release unit。

## 5. 实现前必须冻结的变量

不需要本轮向用户提问；下一期执行者在P1-00/P2阶段输入中填入实际值。

| 变量 | 默认/裁决 |
| --- | --- |
| 当前源码、patch与部署artifact | 按机器实际记录；本轮label/sample只是证据，不作为未来常量 |
| 模型/provider/profile可用集合 | 从当前受控配置和probe读取，不把特定商业模型永久写入公共合同 |
| 原生平台目标 | 一期Linux实际包；二期macOS arm64；其余平台按receipt进入支持矩阵 |
| 接入协议版本 | MCP/ACP/A2A具体版本与feature subset固定，experimental显式关闭或实验 |
| 黄金集与review owner | 现有KS golden store保留；无review时候选unreviewed，不伪造 |
| 质量/延迟/成本目标 | 06为设计假设，实际baseline冻结后先裁决再实验 |
| 公开共享与ACL升级 | 明确tenant/public/user-share语义，旧role grant迁移和安全回滚矩阵 |
| 数据/索引保留与pin | 明确物理版本、最大保留期、GC与预算，不默认永远保存 |
| builder/registry/签名/发布权限 | 缺失则release blocked；不阻断独立源码开发，不自动购买资源或公开发布 |

## 6. 后续SOTA研究的进入条件

只有当一个明确产品指标未达标、现有瓶颈已测量、可复现baseline存在时，才启动新的技术比较。实验必须一次控制一类变量，并记录候选质量、成本、延迟、可维护性、数据/授权与回滚代价。对某项功能的行业领先声明需要同任务同预算的外部可复现实验；本PRD不预先签发这种声明。
