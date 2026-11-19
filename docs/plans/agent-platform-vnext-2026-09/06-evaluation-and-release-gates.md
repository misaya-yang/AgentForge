# 06 评测、故障验收与发布门禁

status: queued · domain_id: agent-platform-vnext · owner: primary Codex · last_verified: 2026-09-07 · prerequisite: 工作包冻结候选与环境 · successor: null

下列为**未来验收要求**，不是本轮执行结果。本轮真正运行的命令及限制只记录在[调查报告](../../../reports/architecture/agent-platform-vnext-2026-09-07.md)。凡写“目标”的命令和场景必须先在对应包实现，不能原样复制到PASS清单。

## 1. 已有门禁优先复用

| 现有命令 | 可以证明的范围 | 不可以替代 |
| --- | --- | --- |
| `make harness-check` | 命令/文档/结构/登记 | 全部程序状态语义和产品功能 |
| `make architecture-boundary-gate` / `make core-boundary-gate` | 静态边界/owner清单/限制 | 动态权限与完整模块化 |
| `make single-instance-guard` | 当前singleton边界不被绕过 | 多实例恢复/容量 |
| `make verify-openapi-contract` | 当前in-process OpenAPI合同 | live部署与旧客户端场景 |
| `make gateway-unit-gate` | Gateway直接实现 | DB/真实provider/上线状态 |
| `make kb-unit-gate` / `make kb-migration-gate` | KB生产单元 / 指定Postgres迁移矩阵 | 未运行的语义质量和数据规模 |
| `make kb-golden-gate` / `make kb-release-evidence-gate` | 开发fixture / 受审发布证据，二者不同 | 人工复核尚未发生的标签 |
| `make rag-live-quality-gate` | 指定真实KS/黄金集上的检索质量 | 生成答案质量、工具安全和其他语料 |
| `make eval-e1-gate` / `make agent-eval-core-gate` | 既有Eval/Agent离线合同 | 完整候选任务运行与副作用outcome |
| `SDK_SSE_CONTRACT_REQUIRE_ALL=1 make sdk-sse-contract` | 声明SDK事件合同 | native CLI打包与真实provider完整能力 |
| `make independent-cli-gate` | 当前CLI配置/launcher/adapter的实际收集范围 | 打包后native安装/执行，需另有receipt |
| `make agent-runtime-source-contract` / `make agent-runtime-contract` | 当前实现覆盖的源锁/制品检查 | 缺失的Worker成对检查；不能据名称推测覆盖 |
| `make agent-execution-integration-gate` / `make knowledge-integration-gate` | 明确配置下跨服务组合验证 | 没有执行的provider/rollback |
| `make platform-db-convergence-gate` | 实际DB authority/fingerprint/迁移矩阵 | 缺DB环境时的成功 |
| `make compatibility-manifest-gate` / `make platform-release-gate` | 按其level检查匹配release unit | draft manifest的结构通过不是release |
| `make fresh-install-gate` / `make agent-runtime-rollback-rehearsal` | 隔离新安装 / 完整冻结回滚 | 当前机器已缓存镜像的一次status |
| `make web-quality-gate` | TS/Node/lint等当前配置真实收集 | 浏览器可用性与跨端工作流 |
| `make validate` / `make status` | 当前栈配置/依赖/健康 | 模型质量、历史恢复、当前HEAD源码完全匹配 |

Rust fmt/check/changed-crate tests只在hosted CI运行；本机使用Docker-contained build或已验证镜像，不运行host Cargo。Web E2E文件只能在`web/e2e/`。Python使用`uv run --all-packages --extra test pytest -q --no-cov <真实测试路径>`，Web使用pnpm和commands.md的直接TS检查。

### 1.1 建议新增的聚合gate（尚不存在，不在本轮运行）

| 目标Make命令 | owner包 | 合同与通过条件 |
| --- | --- | --- |
| `make vnext-identity-contract-gate` | P1-01；P1-04补judge | S01/S02/S03/S40；实际HTTP/DB或清楚区分L1子证据，负向case全部拒绝 |
| `make vnext-provider-compatibility-gate` | P1-02 | S04～S08/S30；共享wire矩阵，所有承诺consumer零意外skip |
| `make vnext-recovery-gate` | P1-03/06/07集成于P1-10 | S09/S22/S25及相关取消/重启场景，确认实际资源/ledger |
| `make vnext-routing-gate` | P1-04/05；P2-02扩展 | S12～S17；身份/价格/资源/attempt决策一致 |
| `make vnext-client-gate` | P2-06/07 | S29～S35；packaged native与desktop实际执行，而非只启动Node |

这些名称是执行合同的提案。若已有gate可完整覆盖，应扩展现有gate并更新此映射；不得同时维护两个同义oracle。外层聚合必须传播失败/blocked/skip，证据标出哪些子项真跑。

## 2. 验收场景目录：Given / When / Then

每个场景都要在执行包中形成明确test case ID、生产入口、fixture、命令和独立oracle。表中的规模只用于指定边界，不是要求本轮创建数据。

| ID | Given / When | 必须断言的Then | 首次期/层 |
| --- | --- | --- | --- |
| S01 | A权威凭证执行B租户Eval job | 在Runtime创建与trace入库前拒绝；A/B无错归属资源；合法B service identity正常 | 一/L1+L2 |
| S02 | A/B都有analyst；A私有库授A角色；B请求所有读路径；另测显式public/user共享 | B拒绝；合法共享按矩阵；旧grant回填不扩大；回滚旧reader仍不放行 | 一/L2+L3 |
| S03 | KS DB角色与删除确认凭据 | KS写Gateway用户/RBAC拒绝；无password hash读取；换对象/过期/重放proof拒绝；合法删除确认通过 | 一/L2 |
| S04 | Read/Write/Unknown工具与已过期/换args/用过的approval | 只读无需误审批；未授权写0次；批准一次只产生一次逻辑副作用 | 一/L1+L2+L3 |
| S05 | 两wire，工具历史+none/function-only+native-search profile | 每round授权不扩大；none无tools；function-only无native search；parallel约束保持 | 一/L1+L3 |
| S06 | 两namespace同名工具、分片args、别名返回、恢复历史 | 正确handler+同一身份；碰撞/未知identity不执行；Gateway与CLI语义相同 | 一/L1+L2 |
| S07 | 同thread两turn，150+事件后terminal，审批后断流与一次网络失败 | 两run ledger terminal/lease释放；whole-thread继续；per-turn结束；UI恢复不锁死、不重复结果 | 一/L2+L3 |
| S08 | 每个adapter遇HTTP429/500、usage-only、畸形frame、提前EOF、tool-only完成 | error不final-success；usage安全解析；不完整流失败/可恢复；有效终态仅一次 | 一/L1+L3 |
| S09 | 隔离测试代码创建小文件风暴/多文件/fallocate、spawn子进程后取消 | 总字节/inode受限；所有进程回收后释放slot；工作目录无残留；输入不是输出 | 一/L2+L3 |
| S10 | 替换Worker tag、缺digest、错overlay、仅Runtime制品 | contract和启动均拒绝；合法成对unit能启动；开发豁免明确不准release | 一/L0+L3 |
| S11 | 相同effective AgentSpec通过Assistant/preview/published/Responses/V2；旧v1存在 | 相同语义fingerprint；显示真实draft/version；无隐藏权限；旧版本默认不扩权 | 二/L1+L3 |
| S12 | 两tenant、两provider同名model不同价格；改价与同步失败 | 不覆盖其他身份价格；历史snapshot不变；错误不记成功；估算/真实分离 | 一/L2 |
| S13 | 并发上限1；invoke/stream/proxy混合；单Agent多轮内部model call | 公平/上限有效；run/call/SSE维度不重算/死锁；队列有deadline；取消释放 | 一/L2+L3 |
| S14 | 上游写POST已接受、返回前断连；另测发送前连接失败 | 前者unknown不盲重放且可对账；后者仅在允许幂等+总budget内重试 | 一/L2 |
| S15 | 零请求、metrics读失败、provider仅配置未probe、已过期probe | 显示no_data/collection_error/unverified/stale；百分比有n/window；不显示伪100%健康 | 一/L1+L3 |
| S16 | 同policy/input，能力或预算不满足候选；publish/rollback策略 | launch前选择可解释，未授权候选排除；已有run不被偷偷换provider；新run采用目标版本 | 二/L1+L3 |
| S17 | provider usage缺失/重复、reasoning/cache token、unknown attempt、价格更新 | 账本reserve/settle/reconcile闭环，费用不双算，不把unknown当0费用成功 | 二/L2+L3 |
| S18 | 两Gateway/Runtime实例抢run，kill owner/网络分区/旧owner复活 | 同时至多一个有效writer；旧epoch拒绝；事件重放可恢复；不靠单纯sticky session保证正确 | 三/L2+L3 |
| S19 | 错parser名/参数shape/空stages；旧flat config/新nested config | 保存时稳定4xx并定位字段；合法配置返回相同effective plan，不延迟到ingestion才失败 | 一/L1+L2 |
| S20 | 中英多页文本+扫描+跨页表格PDF，真实parser，重启后续跑 | IR页码/bbox/内容可核对，引用定位正确；已完成页缓存复用，不只page1 | 二/L2+L3 |
| S21 | 201个dataset，10001段且跨边界重复 | 目录cursor完整；全量任务全遍历；若限制则明确incomplete并拒绝误报total | 一/L2 |
| S22 | 普通upload/reprocess/reembed/rechunk时embed失败、取消、kill | 新空库无需人审集就能正常导入；已可用旧generation保留；过期worker不能publish | 一/L2+L3 |
| S23 | Dataset删除/dedupe已有durable operation，worker半程退出 | 原operation可查可reclaim；delete先deny再清理；全对象/collection代收据一致；取消边界真实 | 二/L2+L3 |
| S24 | 新embedding候选原段自检通过但真实query相关性退化；pin经过cutover/GC/撤权 | 语义质量阻止策略晋升；物理版本不存在报snapshot_unavailable；不静默用latest，不因pin绕撤权 | 二/L2+L3 |
| S25 | Eval批量claim后立即kill，另一worker重启；旧worker回写 | 正在执行和未开始项都可reclaim；旧claim写被fence；已完成效果不重复；取消能terminal | 一/L2+L3 |
| S26 | 复现过去一次Eval；模型排序/price/KB默认已改变 | 使用固定actual provider/config/index；KS黄金集版本通过合同引用；缺历史版本明确失败 | 二/L2+L3 |
| S27 | Agent说文件已生成但实际不存在/内容错误；judge被提示说PASS | outcome oracle判失败；judge不能改golden/执行工具；返回独立错误项而非只总分 | 二/L1+L3 |
| S28 | 同任务baseline/candidate各3次，含timeout/infra_error | 报n与有效分母、单次/至少一次/全成功分别统计；paired task-level区间；基础设施错误不偷偷剔除 | 二/L1+L3 |
| S29 | Gateway停止，CLI使用自己profile；再进入显式gateway命令 | local独立执行，hosted路径按预期失败/恢复；配置和历史无隐式迁移 | 一/L3 |
| S30 | CLI无分隔符trickle、巨大args、慢消费者、consumer断开 | 内存有限，背压/abort传上游，唯一failed/cancelled，不产生completed假终态 | 一/L1+L3 |
| S31 | headless/交互、stdin/json、SIGINT、审批deny、resume、更换工作目录 | stdout协议完整、stderr日志、退出码正确；拒绝无副作用；恢复保留原scope | 二/L3 |
| S32 | 干净OS/arch安装实际发布包，无开发override | native binary+source receipt+license实际包含；text/tool/deny/cancel/resume通过；未支持target明确失败 | 一Linux/二macOS/L3 |
| S33 | Desktop Local/Hosted切换、断Gateway、本地文件发远端 | 执行位置和数据去向明确；历史/秘密隔离；上传/本机权限有真实边界 | 二/L3 |
| S34 | 桌面sidecar崩溃、升级协议不兼容、关闭window/退出app | 不出现孤儿进程/串错profile；重启恢复或可诊断拒绝；没有第二loop | 二/L2+L3 |
| S35 | 桌面真实目录任务→审批→结果文件→取消/恢复→重开 | UI状态与实际outcome一致；trace/artifact可查；键盘/焦点/长流可操作 | 二/L3 |
| S36 | 可信升级包被换签名、下载中断、安装失败、旧版state | 错签名拒绝；恢复/回滚保留用户数据；OS认证/公证与updater签名分别证明 | 三/L3 |
| S37 | tag对应未通过release commit、少一个Worker或错receipt digest | 发布promotion拒绝；构建产物不可自签测试通过；合法匹配完整unit方可promote | 一/L0+releaseCI |
| S38 | 无缓存的新环境安装；current→frozen→current；含迁移101等restore边界 | 真pull/启动/鉴权/KB/Agent通过；旧artifact实际存在；数据恢复与image回切分开记录 | 一/L3 |
| S39 | 冻结负载下持续运行、重启、备份恢复、磁盘/队列压力 | 达到预声明SLO，RTO/RPO有测量，错误预算/资源/unknown事件可观察 | 三/L3 |
| S40 | judge面对恶意候选输出、历史tool transcript、native-search profile | judge授权tools为空，capability dispatch=0；预算/身份固定；评分失败为invalid，非自动0分通过 | 一/L1+L2 |

## 3. 性能与可靠性目标：先冻结基线再优化

下面是设计默认假设，不是行业指标，也不是当前成绩。P1-00冻结测量方法，P2/P3按真实需求接受或通过ADR修订；不能测完失败后临时换窗口/分母/阈值。

| 画像 | 作用 | 默认负载/环境 |
| --- | --- | --- |
| H0 local acceptance | 日常功能与故障验证 | 当前低内存Compose，记录实际CPU/RAM；合成小库/少量并发，不作生产容量外推 |
| H1 controlled service | 平台自身开销与负载对比 | 建议专用8vCPU/16GiB；固定镜像、100k段RAG corpus、10并发；warmup5min+steady30min，至少3独立窗口 |
| H2 recovery / scale | 三期多实例和RTO | 2个应用实例、明确DB/Redis/Qdrant资源；故障脚本、网络与数据规模冻结 |

| 指标 | 默认目标 | 定义与排除项 |
| --- | --- | --- |
| Gateway本地授权/路由开销 | H1 p95≤150ms，且相对baseline无>10%无解释回退 | 完整墙钟本地阶段；queue/provider时间另列，不拿socket接受时间当模型TTFT |
| 已持久事件到客户端交付 | H1 p95≤250ms，断线可按cursor补回 | 仅平台段；客户端慢消费者单独报告 |
| Runtime终态到UI可继续 | 正常H0/H1 p95≤2s | 网络故障进入可诊断reconnecting；恢复后收敛，不能无限锁 |
| Cancel后本地资源回收 | 默认5s内完成受控测试进程回收 | 外部不可取消操作进入unknown/reconcile，不能假装立即停止已发生动作 |
| Job crash恢复 | 不超过已声明lease TTL + 2个poll interval + 5s | 不凭任意固定60s掩盖现有120s lease；执行时固定TTL/heartbeat与误回收风险 |
| RAG交互预算 | 默认5s总deadline，超限给typed error/degraded | embedding/recall/rerank均计入；provider依赖单列，不用无期限fallback |
| 已确认metadata丢失 | 在声明持久化边界内RPO=0 | 与DB commit/replication设置绑定；不是一般外部副作用exactly-once保证 |
| 三期恢复 | 目标RTO≤2min | 故障模式与DB拓扑限定；单节点磁盘永久损坏需要备份恢复画像，不冒充2min |
| 生产可用性 | 三期目标99.9%月度符合条件请求 | 发布前故障/负载测试只能提供证据；月度SLO需要实际观察窗口 |

Provider TTFT分别记录p50/p95/p99、cold/warm、模型/profile、输入/输出token、网络/地区、time-to-first-useful-output。不能把provider不可控延迟全部归为Python问题，也不能只优化首个占位事件来“达标”。

## 4. 质量评测设计

### 4.1 数据与规模

- 一期回归：固定权限/协议/恢复/资源场景，使用专用测试租户和合成数据；安全反例必须全部通过，不能被平均质量分抵扣。
- 二期RAG发布：沿用既有T0的200–400个真实、受审用例合同，按中文/英文、文本/表格/扫描、精确数字、无答案、多来源、跨页、ACL/删除、更新漂移分层；不新造更低门槛。
- 二期通用Agent：默认至少100个任务，每候选每任务3试次，覆盖研究摘要、KB问答、结构输出、受限文件生成、工具审批/拒绝、多轮恢复、失败诊断。样本与试次可据成本在冻结前调整并说明能力声明影响。
- 训练/开发回归/隐藏release集隔离；同一来源的相近题不能随机拆分后当独立holdout。所有source/label都有来源和review状态。
- 本轮UI中一个KB查询和一个preview输入不进入这些发布样本，也不作为业务质量基线。

### 4.2 指标与验收

RAG分别报告 Recall@k、nDCG@k、来源/页定位正确率、答案groundedness、citation precision/coverage、无答案拒答率、ACL误召回、延迟/成本。以task/query为配对单位，固定corpus/index/ACL/config；只有要研究的变量改变。

默认二期策略晋升合同：主要相关性指标的paired 95%区间下界不低于baseline-2个百分点；安全与关键数字/引用回归场景零新增失败；目标优化维度有预声明收益（例如相近质量下成本下降≥15%，或质量改善而成本不超过冻结预算）。这些是本项目暂定非劣/收益界，不是SOTA声称；样本不足以分辨时结果为inconclusive，追加独立样本或保留旧策略，不能当PASS。

Agent报告：单次成功率、k次至少成功一次的任务比例、k次全部成功的任务比例分别统计；不把pass@k当一次请求可靠性。终态成功与outcome成功分开，工具安全、权限、正确artifact是硬门槛。

重复试次在同task内相关，bootstrap以task为cluster，不把3次试验当3倍独立题数。记录paired差值/95%CI、seed支持情况、失败切片、重试成本、judge disagreement。温度/seed/提示配置仅固定可控部分，不承诺LLM位级确定。

LLM judge需固定模型/provider/prompt/version/有限预算/空工具权限。用真实人工审样做校准；judge自报confidence不是统计置信度。无法取得人工review时保留unreviewed candidate，不能由Codex代签。

### 4.3 防止评测污染

被评Agent不能访问golden答案、评分器源代码/配置、其他试次产物、无关用户资料或持久profile秘密。工具stub与真实工具证据分层。调参集结果不可进入release门禁分母；基础设施错误、授权失败、超时、取消须有预声明处理，不因降低分数而事后丢弃。

## 5. 发布与回滚的不可替代证据

每期release manifest需要：Git/patch、upstream/overlay、Runtime+Worker/Gateway/Knowledge/Web镜像digest与平台、DB authority/checksum/fingerprint、OpenAPI/SSE/capability版本、SDK/native包、配置和fixture摘要、实际gate receipt、未验证项、回滚bundle。

推广顺序：构建→校验来源→L1/L2→真实候选→本期quality/client/live→冻结完整release unit→签名/摘要证据→发布promotion。发布workflow消费与该commit匹配的证据；测试失败、缺平台artifact或required skip时不得只凭tag发布。

回滚演练必须执行current→safe-frozen→current并验证旧任务、知识权限、artifact和账态。冻结版不能带回已修复的ACL/工具授权漏洞；需要安全后补丁或版本拒绝栅栏。DB restore-required单独演练dump/restore；文件/向量/对象代与DB必须匹配。

正式发布前的缺环境是明确blocker，不是“可选跳过”。然而它不阻止本轮需求文档完成，也不授权编造receipt或无限增加范围。

## 6. 本PRD自身的文档验收

本轮只要求：所有分册/源码引用存在；稳定需求ID唯一并至少有一个工作包；S01～S40全部定义且引用可解析；当前命令确实存在，计划命令标planned；文档表述区分SRC/PROBE/GATE/LIVE/PLAN；原有dirty内容保留；`make harness-check`与新增Markdown链接/覆盖检查真实运行。

不为Markdown任务添加产品测试，不把上述未来40个场景记录为本轮PASS。最终报告只写已执行的事实。
