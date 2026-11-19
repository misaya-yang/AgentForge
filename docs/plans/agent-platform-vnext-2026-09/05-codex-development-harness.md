# 05 后续 Codex 开发 harness 与技术债治理

status: queued · domain_id: agent-platform-vnext · owner: primary Codex · last_verified: 2026-09-07 · prerequisite: 按工作包实施 · successor: null

## 1. 三种 harness 的边界

| harness | owner与职责 | 不能承担 |
| --- | --- | --- |
| 产品运行harness | pinned codex-harness + 平台compatibility hooks：执行用户任务，维护run/tool/approval上下文 | 不维护仓库任务清单，不替代Gateway身份/费用或Knowledge索引 |
| 评测harness | Gateway Eval编排 + Knowledge黄金集/检索owner：冻结候选、执行任务、观察outcome、评分比较 | 不建立另一个产品AgentLoop，不让judge拥有候选工具 |
| 开发harness | AGENTS入口、owner docs、runbook、affected gates、receipts、review | 不用文档状态覆盖真实代码/运行结果，不把所有任务升级成长期大程序 |

本项目已经有开发harness雏形和机械门禁。本计划补齐事实更新、生命周期语义、例外清零、受影响执行和证据链；不再新增一套与`harness.yml`竞争的命令清单。

## 2. 冷启动：最小可用上下文

每次执行按以下顺序读取，能够定位当前包后停止扩散：

1. `AGENTS.md` → `docs/harness/README.md`。
2. 本PRD入口 → 唯一active program的`loop-state.json` → 当前package receipt。
3. 当前owner doc/accepted ADR → 涉及公共contract fixture/API/schema。
4. 当前生产入口与一跳调用者、相关数据库方法及测试。
5. 需要运行Docker/浏览器才读runtime-and-secrets、实际container labels；涉及Rust只读对应平台seam及源锁。

第一条机器输出说明：`当前HEAD/脏路径/owner包/目标行为/已知失败/允许修改路径/第一个门禁`。不得把上一次conversation或旧snapshot默认为当前事实。

### 2.1 Evidence ladder

沿用L0～L3，不新建一个混淆统计的“全绿”等级：

- L0：结构、契约schema、静态边界、manifest。
- L1：真实生产实现的离线行为/失败测试。
- L2：隔离DB/服务、跨语言合同、job与副作用/权限故障。
- L3：指定制品上的真实栈、provider、浏览器/客户端、质量、性能、升级回滚。

人工评审、统计质量和供应链是各层证据的属性，不用L0 PASS抵扣缺失L3。gate report必须显示实际层级与what-not-tested。

## 3. 一个工作包的标准执行

| 阶段 | 必需动作 | 允许停止条件 |
| --- | --- | --- |
| Observe | 复核发现当前仍存在；找事实owner，不重复读取全部历史报告 | 发现已修复→验证后标satisfied；不可辨认owner→写最小设计裁决 |
| Freeze | 固定需求ID、schema/API/fixture、数据/镜像/环境、失败oracle、owned_paths | 无法安全回滚的数据变化、契约冲突未解决 |
| Act | 最小完整实现；纯搬迁、行为修复、集成路径独立变更 | 实际超范围→拆新的顺序子包 |
| Verify | 运行affected gates与业务oracle；核对collector、分母、skip与身份 | 失败需诊断；环境不可用标blocked，不改测试绕过 |
| Review | 至多两个只读reviewer审owner/合同与故障语义；主session裁决 | 未解决高影响权限/数据/副作用问题 |
| Integrate | 串行合入共享入口；必要时启动真实候选并完成指定journey | wrong runtime、资源不足、制品缺失 |
| Decide | 更新receipt与唯一state，提供可运行候选及剩余项 | 本包verified或具名blocked；不靠对话“应该可以”结案 |

复核只跑受影响检查。没有新改动、新失败或新风险时，不扩大到全仓重复test，不因还可以优化而延迟本期交付。

## 4. 契约作为开发入口

每个新增/修改公开合同至少有：owner、schema version、consumer清单、兼容窗口、错误/取消/幂等语义、资源限制和代表性正负fixtures。Rust合同权威与Python/TS生成投影分开；必须能识别手工投影漂移。

必须登记的consumer：Assistant旧facade、Responses、V2 thread API、Studio preview、published/embed、Python/Java/Dart SDK、独立CLI、未来桌面与ACP。某consumer不支持新增能力可以明确拒绝，不能静默丢失字段/事件。

新surface的验收问题：是否只增加contract client？新增capability的问题：是否只注册descriptor/handler/permissions/schema，不修改kernel控制循环？无法满足时写具体缺失seam，而不是添加临时旁路。

## 5. 受影响门禁与CI规范

复用`harness.yml`、`scripts/harness/affected_gates.py`、`ci_gate_enforcement.py`与现有Make目标。新增gate需同时声明：

```yaml
gate_id: target_gate_id
tier: L1
triggers: [owned/source/path/**, matching/tests/**]
required_on: change
resource: offline
skip: never
timeout: REQUIRED_SECONDS
evidence: REQUIRED_RECEIPT_PATH
ci_job: EXISTING_OR_NEW_REAL_JOB
```

这是字段示意，确切格式遵守当前`harness.yml`解析器。新gate的第一项验收是一个会被拒绝的错误变体；仅创建脚本/Make目标不能算完成。

### 5.1 门禁的反作弊要求

- 使用当前生产模块；fixtures不能只验证自己构造的mock行为。
- 统计`collected/passed/failed/skipped/xfail`及scenario IDs；零收集必须失败，计数达标也不能代替行为oracle。
- 错误/取消/超时/usage-only/权限负例覆盖真实入口，不能在测试中绕过认证/初始化/adapter。
- 回归修复不允许通过删失败用例、变skip/xfail、放宽质量阈值、改expected output或偷偷换语料完成。
- provider live测试必须记录实际调用对象与fingerprint；mock provider只证明协议处理，不证明模型质量。
- live环境不可用时报告`blocked/skipped`，返回语义不能被外层shell吞成PASS。release-required skip=0。
- 不用截图存在证明用户旅程通过，不用source-lock校验证明binary真的打包，不用CI脚本语法检查证明CI实际跑过。

## 6. 源码、制品与工作区身份

一次检查的事实主键包含：Git HEAD、有脏工作时的patch hash、具体生成物identity、数据库/schema fingerprint、依赖锁、工具/配置/数据版本、场景和时间。公开报告只存非秘密配置摘要，不存env全文或凭证hash清单。

代码改变后，旧receipt只能作为基线。对于纯Markdown改动，不要求重跑provider；对于协议实现改变，不允许拿旧容器UI证明当前源码。Python可按当前runtime规则hot-update，Rust必须用已验证镜像；本机不运行host Cargo/rustc/rustfmt/clippy。

Docker为共享资源，先核对Compose working_dir。构建/启动/迁移串行，复用已有镜像；4GiB低内存profile与当前实际资源分别记录。不主动prune、重置卷或重建账号。用户本地自主授权可复用专用E2E账号，执行时读凭证并避免任何日志输出。

## 7. 技术债ledger与清理规则

未来台账每项字段：

| 字段 | 必需内容 |
| --- | --- |
| debt_id / requirement_id | 稳定ID，连到具体产品风险 |
| class | confirmed_bug / boundary_exception / duplicate_authority / stale_doc / unverified_capability / oversized_owner / dependency / release_gap |
| observed_at / source_identity | 当前证据及版本，不能复制旧数字 |
| owner / paths / consumers | 唯一所有者、真实消费者和动态入口 |
| impact / trigger / confidence | 如何影响用户或后续修改，哪些尚未实测 |
| fix_or_delete_condition | 改完什么行为或证明什么才可删 |
| target_package / expiry | 对应期次/包/日期，不用无限TODO |
| evidence / disposition | accepted/fixed/deferred/rejected及真实理由 |

优先级顺序：权限/数据/副作用/身份→持久执行/终态→质量证据→运维分发→模块清晰度。LOC仅帮助找到可能的多owner模块，不是删除理由。

四个2026-09-30到期的import例外必须在P1-08消除；不能因时间到了把expiry往后改。其余core业务迁移在P2-08按owner逐组执行。删除时证明静态与动态非使用，保留accepted ADR/迁移/历史失败证据。

### 7.1 拆分大模块的可验收方式

1. 冻结调用方和事务边界；列出inputs/outputs/error/权限语义。
2. 提取单一owner的窄接口，不改变算法/SQL/行为。
3. 迁移一个调用者，运行直接和隔离gate。
4. 完成所有合法consumer后删除shim，验证没有跨层私有调用。
5. 算法/性能变化另包做配对比较。

需要避免的形式化拆分：把同一facade切成多个mixins仍互相访问私有状态；把业务代码移到contracts；新增巨大utils或base class；每个文件都依赖`Any`注入全系统容器。

## 8. 生命周期与文档防腐

未来程序机读状态建议沿用当前`prd-phase-harness/loop-state/v4`并补语义验证，而非创建平行schema。阶段state只允许一个in_progress；terminal/superseded没有可执行next_action。旧嵌套phase历史保留，不修改成虚构done。

文档authority按问题分开：代码/运行态回答现状；accepted ADR回答边界；loop-state回答谁在执行；queued PRD回答目标；receipt回答某版本通过了什么。索引是导航，不是第二份完成状态。

owner doc的`last_verified`必须对应当前相关源码或具体receipt；“事实过期”的规则要检查语义摘要和源码指针，不能只把日期改成今天。自动inventory生成物记录source hash；人工判断单独存ADR/报告，不互相覆盖。

## 9. 评审、预算与失败恢复

两个只读reviewer分别关注合同/所有权和失败/证据即可；无需让多个Agent重复写同一篇巨型综述。主session负责裁决、整合和实际验证，reviewer数量不是证据强度。

每次长程任务保留`当前包、已完成代码、真实检查、未满足证据、已知风险、下一条安全命令、资源owner`。review发现需标accepted/rejected/deferred；针对被接受项修复并运行对应gate。

连续故障必须先判断是代码、测试、环境、provider还是制品identity问题。不要在依赖缺失时修改业务逻辑，不要在质量失败时更换更容易的任务。允许对非阻塞项先交付C80，继续推进R100；无法完成的外部条件应具体到一项可行动输入。

## 10. 交付格式

每个阶段最后给Codex和用户同一组事实：

1. 可运行候选/实际支持范围、版本与路径。
2. 哪些需求达成、行为变化与理由。
3. 哪些命令实际运行、结果与证据层级。
4. 哪些没验证/失败/被阻塞及下一动作。
5. 数据/客户端/镜像的回滚边界。

不以“所有模块已完成”“达到2026 SOTA”“测试全绿”替代上述事实。`RELEASE_100`只能描述已冻结范围内的完整结果，不能代表未来所有功能。
