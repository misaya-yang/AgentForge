# 04 分期、工作包与迭代执行合同

status: active (phase one) · domain_id: agent-platform-vnext · owner: primary Codex · last_verified: 2026-09-07 · prerequisite: 用户已授权一期且P1-00冻结事实 · successor: null

## 1. 激活规则

初版只交付需求文档；用户已于2026-09-07另行授权创建子分支并开始一期。主session已在[执行runbook](../../../deploy/runbooks/agent-platform-vnext/README.md)采用唯一loop-state与work-packages ledger。历史CORE/ARC/CLI/RAG义务已映射并保留，不恢复旧next_action，不因旧状态文件写active就同时开启多个writer。二/三期不随一期自动启动。

每个包按 `observe → freeze → act → verify → review → integrate → decide` 顺序执行。两个只读reviewer可以在主session处理另一独立读取/设计任务时并行；同一个产品只有主session写。包之间的依赖默认要求`direct_verified`，涉及权限、数据或公开契约未解问题时必须`verified`后才继续。

本期R100是本期整体，不能让后一期功能成为本期完成的隐式条件。估时采用包的实际收敛速度，不在无基准时承诺某周交付。下列包是逻辑边界，单包过大可拆为顺序子包，但必须保留同一期完整验收。

## 2. 所有工作包继承的完整字段

下表默认值与后文每包行组合构成完整工作包定义，不能只复制后文简表。

| 字段 | 规则 |
| --- | --- |
| `id/result` | 后文固定ID和系统/用户可见结果 |
| `base_sha` | 设计观察基线为`26dfbbc2…`；执行时记录真实HEAD及patch digest，不能直接套用旧sha |
| `depends_on` | 后文依赖；隐含同一期P?-00/入口事实冻结（一期为P1-00） |
| `owned_paths` | 后文的现有owner目录+对应直接tests；Act前收缩为真实文件allowlist；新路径须标new |
| `forbidden_paths` | 其他包业务文件、upstream模型循环算法、秘密/auth fixture、未授权环境；任何无关重构/删改 |
| `integration_paths` | `src/main.py`, `src/api/router.py`, Compose, Makefile, harness.yml, `.github/workflows`, `docs/README.md`, `database/schema.sql`；仅包已稳定后单独集成 |
| `must_preserve` | ADR-006～009、C-01～09、已发布API/事件、租户/ACL、现有KB引用与旧版本数据、CLI本地身份；安全下限优先于兼容旧漏洞 |
| `direct_gates` | 06列出的现有gate + 本包新增行为oracle；必须执行生产实现，记录collector/skip |
| `integration_gates` | 06的S场景绑定到isolatedDB/HTTP/worker，不用fixture重放冒充 |
| `live_gates` | 后文指定S场景，重型可集中在本期末，但本包状态须awaiting_final_live |
| `skip_policy` | 本期承诺的功能/平台：none；未承诺实验项显式not_applicable；环境缺失为blocked |
| `rollback` | 后文类别+固定前一个已知安全artifact/schema；不得回退到已修复越权逻辑 |
| `review` | 一个独立只读review；高风险身份/数据/计量/副作用由第二reviewer审failure oracle。不得用自评冒充独立review |
| `evidence` | `deploy/runbooks/agent-platform-vnext/receipts/<id>.yml`；含sha/patch/images/config/command/result/skip/scenario/negative proof |
| `stop_conditions` | 改变数据owner/服务数量/权限模型、依赖升级超范围、public contract无迁移方案、wrong runtime、未授权不可逆操作、达到资源上限 |

失败时允许修本包；发现其他领域问题登记到对应包。stop_condition要求的是停止受影响动作并写明确裁决，不是放弃全部已授权工作。外部CI/构建资源阻塞时继续独立可完成的源码或文档工作，但不宣布该release完成。

回滚类别：`R0`源码/配置兼容回切；`R1`扩展式schema+兼容reader；`R2`generation切回；`R3`完整image/schema/client兼容release unit；`R4`restore-required。必须标明实际类别，不能所有改动一律写“git revert”。

## 3. 一期：可信运行底座

### 3.1 一期完整旅程

用隔离测试租户创建/选择测试KB → 正常导入并检索 → 用已保存Agent配置在Web执行 → 一次工具审批/拒绝和一次取消 → 断线后恢复历史 → Trace显示实际run和账态 → 发起一个真实候选Eval → worker重启后恢复 → 查看真实健康/费用 → 独立CLI在不依赖Gateway时完成一个任务 → 安全版本回切后再验证。

不要求本期完成所有高级解析/路由/桌面特性，但一期已承诺的普通路径必须可靠；安全修复不能以“二期再加平台治理”延后。

### 3.2 顺序工作包

| 包 | 需求ID | 结果 / 依赖 | owned_paths（初始范围） | 直接与集成/活栈验收 | 回滚 |
| --- | --- | --- | --- | --- | --- |
| P1-00 | HX-01,HX-02,HX-04,HX-05 | 建立唯一执行owner、现状/契约/制品/失败基线；无依赖 | `deploy/runbooks/agent-platform-vnext`, `docs/harness`, `scripts/harness/affected_gates.py`, `scripts/harness/ci_gate_enforcement.py`；只改已确认缺口 | harness/import/core/singleton/OpenAPI；冻结S01～S15等必需oracles与镜像身份；标记当前event_stream源码漂移 | R0 |
| P1-01 | ID-01,ID-02,ID-03,KB-01 | 修正Eval租户身份及Dataset主体边界；依赖P1-00 | `src/services/eval`, `src/api/v1/knowledge.py`, KS `auth`, `dataset_service.py`, `persistence`, `database/authority`, `database/migrations`, contracts | S01/S02/S03；DB privilege matrix、旧ACL迁移/public-sharing矩阵；先离线反例，再isolated HTTP/DB | R1；旧reader须安全栅栏 |
| P1-02 | ID-04,CP-01,CP-02,CP-03,CP-04,UX-02 | 模型工具授权、wire、事件终态与恢复一致；依赖P1-01安全边界 | `src/services/agent_runtime`, `src/adapters/openai.py`, CLI `provider`, Rust平台tool/approval hooks，`web/src/api/agentThreads.ts`, `features/chat`, Studio preview | S04～S08；现有SSE/Agent contract+每adapter负向fixture；至少两wire、一root+child、150+事件、网络中断 | R0/R1；不能回退权限修复 |
| P1-03 | ID-05,CP-05 | 代码执行与内部broker有硬资源/路径/取消边界；依赖P1-01/02 | Rust `ai-platform-capability-worker`平台执行文件、Gateway相关capability broker、local-node边界tests | S09；真实隔离容器内总磁盘/inode、进程组回收、input/output、错误audience/SSRF负测 | R3；安全修补前版不可用 |
| P1-04 | GW-01,EV-01 | 模型/价格身份、Eval候选/judge模型与权限固定；依赖P1-01/02 | `src/services/llm/model_service.py`, `services/billing`, `services/eval`, core当前usage owner、contracts、相应迁移 | S12/S40；双租户/双provider同名模型、失败同步、改价不改历史；judge工具负例在CP-01修复后验证 | R1；历史价格snapshot保留 |
| P1-05 | GW-02,GW-03 | 所有入口的容量/重试语义一致；依赖P1-02/04 | `src/core/gateway`, `src/proxy`, `src/adapters`, `src/connectors`相关HTTP调用边界 | S13/S14；run/provider容量各1、多轮不死锁；stream绕限反例、请求发送后断连unknown | R0；保持既有资源维度 |
| P1-06 | EV-02,EV-03 | Eval job可reclaim且结果语义诚实；依赖P1-01/02 | `src/services/eval`, core当前`eval/outbox_worker`, `agent_trace_repository`, 相应DB迁移与Web状态 | S25；领取批次后kill，旧claim写拒绝，取消/超时/无效judge不伪成功 | R1；旧running任务收养方案 |
| P1-07 | KB-02,KB-04,KB-05 | 普通知识生命周期可用，配置提前失败，目录/全量请求不截断；依赖P1-01 | KS dataset/retrieval config、目录API、worker/ingestion、`web/src/pages/knowledge`对应表单 | S19/S21/S22；201库、10001段边界；坏parsing config；embedding失败保留旧generation | R1/R2 |
| P1-08 | GW-04,UX-01,HX-03,OB-02 | 状态/宣传真实，四个到期例外清零；依赖P1-04/06/07 | health/metrics owner、ProviderStatusCard/Dashboard/Login/Eval概览、四个allowlist源文件及owner模块 | S15；零样本/采集错误/Gate未跑；import gate四项违规与entry均0，保留负向自测 | R0；不可用显示unknown |
| P1-09 | CL-01,CL-02,CL-06,CP-06 | 已支持CLI可独立运行；Runtime/Worker来源及工具/stream边界可验证；依赖P1-02/03 | `sdk/cli`, Rust平台artifact hooks，`scripts/harness/agent_runtime_supply_chain.py`及现有构建脚本 | S10/S29/S30/S32的Linux目标；native真实执行，不能只跑launcher Node tests | R3 |
| P1-10 | RL-01,RL-02 | 一期集成C80→R100与完整发布证据；依赖P1-01～09 | 已声明的integration paths、release manifest/receipts、发布CI、当前runbook | S37/S38与一期完整旅程；实际UI/provider、fresh machine、全部冻结artifact回切；补全部awaiting_final_live | R3/R4按迁移标记 |

P1-02内部必须先修provider body/identity，再修event projection，再做UI恢复；P1-08将边界移动与UI状态修复做独立子提交。任何包过大按合同拆P1-xx-A/B顺序执行，不把统一目标分散到并行分支。

四项import例外有2026-09-30硬到期。若执行开始已到期或预计当前顺序无法及时清零，P1-00立即把纯边界迁移提为P1-08-A先行子包，独立冻结消费者与直接gate；其余健康UI仍按原依赖顺序。不能用延期豁免换取中间版本绿色。

### 3.3 一期候选与发布

**C80最小可用证据**：P1安全/正确性项已过直接和必需isolated gate，现有栈运行完整Web KB→Agent→Trace→Eval路径，CLI已声明目标实际执行；没有未知制品身份或无法回滚的安全修复。运行画面、trace、job收据可查看。

**R100追加且必须完成**：承诺的负向矩阵零skip，四个边界例外清零，Runtime/Worker完整可信unit，发布promotion消费匹配证据，干净安装与完整回滚，文档/旧客户端支持窗口明确。外部CI或冻结旧镜像缺失时记录`candidate_80 + blocked_release`，不降低gate，也不在等待外部条件时追加二期功能。

## 4. 二期：质量驱动工作台与本地客户端

### 4.1 二期完整旅程

真实多页资料进入IR → 选择检索profile并比较质量 → 固定Agent版本和模型路由策略 → 重复Eval并生成配对报告 → 发布符合门槛的策略 → Web与macOS桌面分别完成同一语义任务 → CLI/ACP独立工作 → 对失败candidate执行回滚并解释原因。

| 包 | 需求ID | 结果 / 依赖 | owned_paths（初始范围） | 验收 | 回滚 |
| --- | --- | --- | --- | --- | --- |
| P2-01 | ID-06,ID-07,CP-07,CP-08 | Agent policy/spec版本与memory/retention合同完整；依赖一期R100 | Agent catalog/spec/contracts、launch resolver、必要Runtime hooks、artifact/memory治理、Studio | S11；preview/published等价、权限diff、旧spec、撤权及物理pin不可用；上游seam inventory | R1/R3 |
| P2-02 | GW-05,GW-06,GW-07 | launch前版本路由、能力证据、预算/成本对账；依赖P2-01 | Gateway llm/routing目录（增量new）、admission/accounting、管理API/UI | S16/S17；rule dry-run→发布→真实调用→回切；无授权运行中换provider | R1/R0 |
| P2-03 | KB-03,KB-07,KB-08,KB-09,KB-10,KB-12 | 真实解析/统一检索计划/语义质量/来源同步闭环；依赖P2-01 | KS parsing/ingestion/retrieval/connector/config、现有golden store、知识UI | S20/S24；真后端+页码；相同effective config；ACL同步；query质量阻止坏迁移；facade收敛前后兼容 | R2/R1 |
| P2-04 | KB-06 | 大库生命周期任务化；依赖P2-03合同稳定 | 现有DocumentBatchStore/worker/API，Dataset delete/dedupe及进度UI | S23；复用operation ID，10k+完整遍历，kill/reclaim、tombstone、全generation清理；不另造队列 | R1/R2；删除按不可逆阶段 |
| P2-05 | EV-04,EV-05,EV-06,EV-07,EV-08 | 冻结候选、独立oracle、统计比较、发布gate联通；依赖P2-02/03 | Gateway Eval owner、KS golden合同、trace/资产/发布UI | S26/S27/S28；人工review真实，隐藏集隔离，RAG黄金owner不重复，失败候选不能promote | R1 |
| P2-06 | CL-03,CL-04,CL-05,CL-06,CL-07 | headless/TUI/doctor/provider/ACP与macOS native候选；依赖P1-09、P2-01 | `sdk/cli`、新ACP适配层、native packaging workflows、协议fixtures | S31/S32；Linux+macOS各从真实安装包跑text/tool/deny/cancel/resume；至少一真实ACP客户端 | R3/本地state备份 |
| P2-07 | DS-01,DS-02,DS-03,DS-04,DS-05,DS-06,UX-03,UX-04,UX-05 | 可用macOS桌面Local/Hosted工作台；依赖P2-01/05/06 | 拟新增`apps/desktop`、最小client-contract包、现有Web纯展示/事件组件、local-node broker | S33～S35；真实目录任务+Hosted Agent+恢复；权限负测、keyboard/zoom、长流UI；不内嵌新loop | R3；原子profile/state升级 |
| P2-08 | HX-06,HX-07,HX-08,RL-04,OB-01,OB-03,OB-04 | 余下core领域owner、开发环境/观测/供应链收敛；依赖相关业务包稳定 | core consumer指定模块、owner repos、observability、开发harness、SBOM/NOTICE、runbook | 无反向import；trace跨域关联；遥测中断有界且可见；高基数/脱敏负测；冷启动恢复与artifact核验 | R0/R3 |
| P2-09 | 本期全部需求的集成关闭 | 二期C80→R100；依赖P2-01～08 | integration paths与收据 | 真业务质量/成本对照、桌面安装升级、客户端/平台兼容、退化回切；所有承诺平台零skip | R3/R4 |

P2-03在源码与真实后端闭环后再扩大parser集合；P2-07先验证最小真实任务再铺开工作台。人审黄金集未完成时，仍可交付自动候选、可用桌面及待审报告，但不能签发对应质量release receipt。

## 5. 三期：经证据支持的规模与生态

| 包 | 需求ID | 结果 / 依赖 | owned_paths（初始范围） | 验收 | 回滚 |
| --- | --- | --- | --- | --- | --- |
| P3-01 | GW-09,RL-03 | 先证明leader/job/run/stream所有权，再有条件解除单实例guard；依赖二期R100+容量baseline | Gateway lifecycle/schedulers、Runtime平台存储/订阅hook、DB lease、single-instance gate、Helm/Compose | S18/S39；双owner竞争、kill/partition/重连、旧owner fencing；新ADR接受规模边界后才能改replicas | R1/R3；保留singleton fallback |
| P3-02 | CP-09,EV-09 | 受控外部Agent与外部评测harness接入；依赖C-01/04/08稳定 | Gateway adapters、外部task identity/catalog、协议fixtures | A2A等固定版本、取消/失联/权限映射；外部协议不扩大本地能力；非承诺extension不阻塞核心release | R0 |
| P3-03 | DS-07,DS-08,CL-08 | 多平台可信发行、本地状态保护、显式导出/导入；依赖P2客户端证据 | 桌面/CLI packaging、updater、OS credential store、state migration | S36；错签名、升级中断、老版本、卸载保留用户文件；未支持OS不显示可下载 | R3 |
| P3-04 | GW-08,KB-11 | 可选择的质量/成本实验；依赖冻结baseline/统计门禁 | routing experiment模块、KS实验retrieval profile、Eval reports | 授权模型切换新lease负测；rewrite/高级retrieval配对净收益；不获益的实现不默认开启 | R0/R2 |
| P3-05 | 本期全部承诺需求的集成关闭 | 容量/生态/客户端版本完整发布 | release manifest、integration paths、runbook | 新部署profile端到端任务、备份恢复、current→frozen→current、跨版本客户端、可观测未知态 | R3/R4 |

GW-08/KB-11/DS-08中标F的探索项是否纳入三期release，必须在P3-00等价的阶段冻结中决定；未采用的实验明确not_selected并记录理由，不要求为了勾选实现所有算法。

## 6. 何时新增/调整ADR

- AgentSpec后继版本的权限、budget和跨版本默认（保留旧spec hash，生成迁移版本）。
- Dataset ACL主体资格与显式public/cross-tenant共享、安全回滚版本下限。
- 物理知识snapshot/pin、GC/retention与撤权优先关系。
- 运行中跨provider attempt的新授权/lease合同；不与launch前路由混成一个包。
- Desktop Local/Hosted IPC、OS权限和可信更新边界。
- 多实例运行的lease/fencing/notification与持久scheduler owner；如需新增服务还要容量证据。

不为普通文件拆分或小函数调整创建ADR。每个ADR比较保留现状、最小演进和更大方案，说明选择理由与失败/回滚条件，不写空泛“更现代”。

## 7. 评审与集成节奏

1. Observe只读当前包证据与一跳调用者，不线性重读整个仓库。
2. 先建立能失败的行为oracle；对于边界已有直接测试，补缺失反例，避免镜像实现的测试。
3. 第一个可运行candidate稳定后安排独立review；高风险包从权限/数据和failure injection两个角度审。
4. review发现只接受有触发/影响/证据的问题；记录accepted/rejected/deferred及理由。
5. 受影响门禁通过后停止重复测试；本期末串行一次集成窗口补所有指定live场景。
6. 用户未授权commit/push/生产变更时，不执行这些动作；可以完成本地候选与可审阅diff。不能把发布权限缺失说成代码已发布。

## 8. 完成与交接的机器输出

未来每包receipt至少包括以下字段（**示意schema，不是已运行收据**）：

```yaml
package_id: P1-02
requirement_ids: [ID-04, CP-01, CP-02, CP-03, CP-04, UX-02]
state: awaiting_final_live
tested_source:
  base_sha: REQUIRED_AT_EXECUTION
  head_sha: REQUIRED_AT_EXECUTION
  patch_sha256: REQUIRED_IF_DIRTY
runtime_identity: REQUIRED_FOR_LIVE
checks: [] # 实际运行后填写 command/status/exit_code/collected/passed/failed/skipped
pending_scenarios: [S04, S05, S06, S07, S08]
rollback_class: R1
review: REQUIRED_AT_EXECUTION
next_action: Execute the named remaining live scenarios on the identified candidate.
```

不允许把这个模板原样作为完成证据。`verified`要求所有必需checks有真实输出，pending为空，review已裁决；program complete要求本期集成和回滚通过。文档设计完成与软件升级完成永远分开。
