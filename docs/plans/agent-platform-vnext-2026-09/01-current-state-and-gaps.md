# 01 当前架构事实与升级差距

status: queued · domain_id: agent-platform-vnext · owner: primary Codex · last_verified: 2026-09-07 · prerequisite: 实现前刷新HEAD · successor: null

## 1. 调查口径

源码基线：`26dfbbc2c2e03c4daa1eef6f64dc7cf79e395c3f`。本轮检查核心入口、owner、典型成功/失败路径、契约、持久化、构建和验证链；不是对约39万行源码逐行做完穷尽安全证明。

证据标签：

- `SRC`：本轮直接读到的源码/配置结构。
- `PROBE`：从当前生产类/方法体提取、用纯内存替身复现；不是正式项目测试套件，也不是HTTP/真实provider验收。
- `GATE`：本轮实际运行门禁并得到退出码/统计。
- `LIVE`：本轮本地现有容器/UI上的有限行为；不推及未执行场景。
- `RISK`：由已观察结构推导的风险，尚未故障注入或基准测量。
- `PLAN`：新需求，当前实现不宣称存在。

本轮开始前已有 `docs/README.md` 修改和未提交核心修复计划，均保留。容器原处于停止状态；经确认属于当前checkout后启动已有容器，未重建镜像、未修改产品源码。Gateway/Web/Knowledge镜像标签revision为`750b25db…`，Runtime/Worker为`94cbbdd…+46158add036c`。Gateway源码抽样中native Responses与launch resolver一致，但event stream文件与HEAD不同，故本轮live不构成HEAD发布验收。

## 2. 已实现的基础：不得重复建设

| 领域 | 当前存在的能力 | 本次证据边界 |
| --- | --- | --- |
| 部署架构 | Gateway、Rust Runtime/Worker、Knowledge API/Worker，现有PostgreSQL/Redis/Qdrant | ADR-008、当前Compose；启动后健康检查通过 |
| 内核 | pinned codex-harness + overlay；Python AgentLoop退出；Hosted与Local部署身份分离 | runtime dependency gate源码、ADR-006/009；未重审全部上游内核 |
| Launch | `ResolvedAgentLaunchV1`、`assistant_entry/launch_resolution.py`；Assistant、Responses、Studio/V2已有调用点 | SRC；不是“还没有统一launch”的状态 |
| 模型控制 | lease、snapshot/hash、reservation、provider profile、计量与已有capacity/admission | SRC；不代表每个旧入口都接通这些约束 |
| Agent Studio | draft/version/preview/publication/trace；本轮现有draft preview得到真实模型回复 | LIVE一个合成输入；未测试发布/回滚/多轮工具 |
| RAG | dense+sparse、hybrid、RRF、rerank/MMR、parent-child/summary/结构路由、预算与generation fence | SRC；本轮既有测试集一次hybrid查询返回2个结果 |
| 解析/索引 | parsing IR/version/cache、durable ingestion、process-rule snapshot、embedding/BM25 migration和rollback | SRC；部分真实后端装配仍未闭环 |
| Eval | trace、golden、evaluator、candidate fingerprint、Agent/RAG gate、线上采样/反馈 | SRC；部分执行身份和job恢复仍有缺口 |
| CLI | 本地native启动器、隔离home、Responses直连、Chat兼容代理、显式legacy gateway命令 | SRC/ADR-009；未在本轮重新构建或运行native包 |
| 开发harness | import/core/singleton、受影响门禁、CI enforcement、证据和release gate | 本轮4个静态门禁通过；不是所有release gate通过 |

### 2.1 本轮静态门禁实际结果

| 命令 | 结果 | 含义与边界 |
| --- | --- | --- |
| `make harness-check` | PASS；37 gate schema，16 programs；1既有warning | 结构/登记验证，不证明所有程序语义或功能已完成 |
| `make architecture-boundary-gate` | PASS；587文件，0未豁免违规，4有效allowlist，负向自测通过 | 仍有4项边界债务，不能写“零例外” |
| `make core-boundary-gate` | PASS；8 contracts modules、133 core modules；Knowledge→core 13 | no-growth边界通过，不等于core都是纯基础设施 |
| `make single-instance-guard` | PASS，负向自测通过 | Gateway/Runtime仍明确单实例；不能因此宣称HA |

这些统计是首次调查门禁结果；文档新增后的最终harness统计见调查报告。完整命令与live记录见[本轮报告](../../../reports/architecture/agent-platform-vnext-2026-09-07.md)。

## 3. 源码发现与需求映射

### 3.1 系统兼容与Agent产品边界

| ID | 证据与触发 | 影响/判断 | 需求 |
| --- | --- | --- | --- |
| D01 | [architecture.md](../../harness/architecture.md)仍有“无静态gate/共享DB登录”等旧事实；[core README](../../../packages/ai-gateway-core/README.md)称只有Protocols，而实际存在具体repository/evaluator | SRC：活文档与实现漂移会使Codex重复建设、误删或绕错边界 | HX-05 |
| D02 | [import allowlist](../../../scripts/harness/import_boundary_allowlist.json)4项均到2026-09-30；core包含7256行Agent repository、3417行Trace repository等具体业务 | GATE/SRC：边界已有防护，但共享包仍是领域依赖集中点；LOC仅作定位 | HX-03/06、KB-12 |
| D03 | [AgentSpec](../../../packages/ai-gateway-core/src/ai_gateway_core/agents/spec.py):13–42白名单缺少统一permissions/mode/budget；与产品law的表达能力不同 | SRC：策略分布在其他snapshot/入口，spec本身不足以表达完整通用Agent。不能写成完全没有权限 | CP-07 |
| D04 | [native Responses](../../../src/services/agent_runtime/model/native_responses.py):197–254与[Chat](../../../src/services/agent_runtime/model/chat_completions.py):273：profile可注入native search；历史工具transcript影响none/parallel语义 | SRC高置信：能力与授权混合，多轮可能重新暴露未授权工具；未实测外部副作用 | CP-01、ID-04 |
| D05 | Gateway与CLI的Chat工具转换没有完整共用namespace identity合同；native Responses已有alias处理 | SRC：不能笼统说“没有namespace支持”；缺的是跨wire/历史/客户端完整一致性 | CP-02、CL-02 |
| D06 | [thread events](../../../src/services/agent_runtime/control/event_stream.py):256仅turn过滤流完成ledger；[UI snapshot](../../../web/src/api/agentThreads.ts):93–122从0读且100ms idle中止 | SRC高置信：whole-thread ledger/lease收尾与长页恢复有缺口；实际HEAD故障注入未做 | CP-03 |
| D07 | [Python执行器](../../../rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-worker/src/python_code_execution.rs)使用持久workspace、退出后收集总输出及RLIMIT_FSIZE；异步调用封装需将取消传递到阻塞执行 | SRC/RISK：单文件限制不等于总磁盘/inode限制，取消/输入输出归属需要实测 | CP-05 |
| D08 | [Makefile](../../../Makefile):82–94偏重Runtime；[deploy.sh](../../../scripts/new/deploy.sh):150–186有不同构建入口；[build compose](../../../docker-compose.build.yml)与local tags的公开安装语义需统一 | SRC：Runtime/Worker成对制品验证覆盖不一致。当前已有配对本地镜像，不等于公共多架构安装完成 | CP-06、RL-01/02 |

### 3.2 知识库与RAG

| ID | 证据与触发 | 影响/判断 | 需求 |
| --- | --- | --- | --- |
| D09 | [dataset ACL](../../../apps/knowledge-service/src/knowledge_service/services/knowledge/dataset_service.py):1544–1551纯role名grant不校验same_tenant；Gateway proxy把ACL交给KS，retrieve也复用该检查 | SRC+PROBE高置信：tenant B的analyst在局部方法探针中获得A私有库viewer。跨租户HTTP未跑；必须关闭同名角色碰撞并明确显式共享 | ID-02、KB-01 |
| D10 | [grants baseline](../../../database/baselines/2026_08_post_kb_v1/grants.sql):1691–1699给KS roles users/RBAC CRUD；dataset delete读取用户password hash | SRC：已有独立角色但仍过宽；本轮未核对活DB实际grants。密码确认是Gateway身份责任 | ID-03 |
| D11 | [ingestion](../../../apps/knowledge-service/src/knowledge_service/services/knowledge/ingestion_service.py):393–418生产PageJob把整文source_text装成page1；[registry](../../../apps/knowledge-service/src/knowledge_service/services/knowledge/parsing/registry.py):66–72裸构造多个默认无client后端 | SRC高置信：IR/cascade不是缺失，真实页producer与后端运行装配未完成；不能声称启用cascade就有完整OCR/布局 | KB-03 |
| D12 | [dataset config](../../../apps/knowledge-service/src/knowledge_service/services/knowledge/dataset_service.py):764–769,893–896未统一验证parsing；目录:676–679限200；[dedupe](../../../apps/knowledge-service/src/knowledge_service/api/routes/knowledge.py):4173–4196限10000段 | SRC：配置错误推迟到job阶段，部分全量管理静默截断；其他已分页接口不受此断言覆盖 | KB-02/04 |
| D13 | [delete_dataset](../../../apps/knowledge-service/src/knowledge_service/services/knowledge/dataset_service.py):1277–1311请求内租约逐文档删对象/全代索引 | SRC/RISK：已有删除栅栏与重试，不是无数据安全；大规模HTTP生命周期与进度/reclaim仍不完整 | KB-06 |
| D14 | [embedding gate](../../../apps/knowledge-service/src/knowledge_service/services/knowledge/embedding_gate.py):54–75,251–335随机原段查询自身；[golden manifest](../../../tests/fixtures/eval/rag/golden/manifest.json):13–18为18个待人审机器样例 | SRC：自检能证明索引链路，不能证明真实query相关性；repository fixture状态不等于活DB绝无黄金集 | KB-09、EV-06 |
| D15 | [Knowledge facade](../../../apps/knowledge-service/src/knowledge_service/services/knowledge/knowledge_service.py):145–191后置注入`_ks`；retrieval回调facade私有方法，worker反调ingestion私有方法，structured conversion平行实现 | SRC/RISK：内部owner不清、难以单独构造/测试，拆文件不能自动解决 | KB-07/12 |

### 3.3 Gateway、路由与Eval

| ID | 证据与触发 | 影响/判断 | 需求 |
| --- | --- | --- | --- |
| D16 | [EvalCandidateClient](../../../src/services/eval/eval_candidate_client.py):93–159固定token/API key，tenant只放提示header；[outbox wrapper](../../../src/services/eval/eval_outbox_worker.py):117–128按job tenant写trace | SRC高置信：执行身份与trace归属可能错配；本次没有发起跨租户live任务 | ID-01、EV-01 |
| D17 | [claim_outbox_jobs](../../../packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_trace_repository.py):1736–1767只queued→running；[worker](../../../packages/ai-gateway-core/src/ai_gateway_core/eval/outbox_worker.py):49–73批量claim串行处理，stop取消 | SRC/RISK高置信：进程退出可能留下无法reclaim的running项；待真实kill注入 | EV-02 |
| D18 | [OpenAI adapter](../../../src/adapters/openai.py):57–94请求usage却直接choices[0]；未校验HTTP和正常终态 | SRC+PROBE：usage-only报IndexError；429与提前EOF可输出final=True。是实际类方法体内存回放，不是全HTTP产品测试 | CP-04 |
| D19 | [model pricing sync](../../../src/services/llm/model_service.py):644–705以tenant模型同步到[global pricing](../../../src/services/billing/model_pricing.py):183–238 `ON CONFLICT(model)`；error字典也可被记成功 | SRC高置信：同名model跨tenant/provider价格覆盖；UsageRecorder按model查价而Runtime用snapshot价 | GW-01/07 |
| D20 | [model_service](../../../src/services/llm/model_service.py):90–128允许同名model但不带provider时取排序第一；Eval candidate/judge请求仍依赖model_id | SRC/RISK：配置变更可能改变被测/被调provider，身份与可复现性不足 | GW-01/05、EV-01/04 |
| D21 | [dispatcher](../../../src/core/gateway/dispatcher.py):203–209,265–269 invoke用缓存semaphore；:414–425 stream无同样入口；proxy已有CapacityAdmissionController | SRC：不同入口容量策略分叉；缓存限制更新、排队deadline需统一，不能再新建一套限流器 | GW-02 |
| D22 | [dispatcher](../../../src/core/gateway/dispatcher.py):271–286对任意Exception重试，默认max_retries=3；最终成功后计一次usage | SRC/RISK：发送后未知结果可能重复副作用/费用；不可迁移成model plane隐藏retry | GW-03/07 |
| D23 | [ProviderStatusCard](../../../web/src/components/ProviderStatusCard.tsx):873–877硬编码100/35；[health](../../../src/api/v1/health.py):217–254只读配置；metrics无请求/读Redis失败给100%success | SRC+LIVE：UI中的“健康度”与窗口可靠性混淆；有明确代码解释，不是仅外观意见 | GW-04、UX-01 |
| D24 | [docker-publish workflow](../../../.github/workflows/docker-publish.yml):15–18,27–47,75–88 tag构建push没有消费完整release receipts，矩阵无Runtime/Worker | SRC：仓库内发布门禁未闭环；远端保护规则/历史实际发布未查，不能据此断言曾发生违规发布 | RL-01/02 |

### 3.4 CLI、桌面端与产品可信度

| ID | 证据与触发 | 影响/判断 | 需求 |
| --- | --- | --- | --- |
| D25 | [CLI SSE reader](../../../sdk/cli/src/provider/sse_reader.ts):17–24 buffer无frame总上限；[projector](../../../sdk/cli/src/provider/chat_stream_projector.ts):127,337累积文本并忽略write背压 | SRC/RISK高置信：异常/慢stream可无限内存，客户端断连/超限终态合同不全 | CL-02 |
| D26 | [CLI README](../../../sdk/cli/README.md)明确Darwin/Windows制品仍是release工作；legacy Node客户端与native launcher共存 | SRC：需要明确支持矩阵与弃用窗口，不可把有npm包当跨平台可用 | CL-01/03/05/06 |
| D27 | tracked应用清单未发现desktop/Tauri/Electron入口；[local-node](../../../apps/local-node/README.md)提供能力节点基础 | SRC调查范围内未发现桌面产品；属于PLAN，不是桌面bug；复用设备权限/ledger而非重造 | DS-01～08 |
| D28 | [Login.tsx](../../../web/src/pages/Login.tsx):240,243,255,275,320硬编码99.99%、P99 38ms、4820req/s、节点数与SOC-2认证 | SRC；宽屏布局含这些语句，本轮窄屏截图未显示侧栏宣传。无依据声明削弱产品可信度 | UX-01 |
| D29 | [useChatSession](../../../web/src/pages/assistant/hooks/useChatSession.ts)3761行；已有features/chat reducer/terminal latch、Studio单独预览投影 | SRC/RISK：已有拆分基础；应收敛事件/错误/恢复语义后提取共享客户端，不复制第三套桌面状态机 | CP-03、DS-05/06 |
| D30 | 本轮Eval概览显示已评分Trace=0、最新Gate=not_run，同时质量通过率/轨迹通过率100%；新preview trace显示成功但Token/评分为0 | LIVE：呈现的质量语义和缺失数据不明确；[Eval UI](../../../web/src/pages/eval/index.tsx):1151–1221分别消费这些字段。未追完整计量入库根因，不能断言实际费用为0 | OB-02、EV-03、GW-07 |

## 4. 技术债优先级与处理规则

第一优先是行为错误和跨租户/资源/副作用边界：D04、06–10、16–19、21–22、25。第二优先是质量真实性和生命周期：D11–15、20、23–24。模块化、文档和桌面新能力按对应包推进，不插入上游内核重写。

具体删除/迁移前必须回答：谁调用、谁拥有数据、谁负责事务、哪种动态入口还需要、哪些旧客户端在支持窗口、哪个负向测试会阻止错误。只要这六项没清楚，不能以“统一”为名批量删代码。

### 4.1 规模快照只用于导航

按tracked的`.py/.rs/.ts/.tsx`统计（包含这些目录中的部分测试；不含完整外部upstream）：apps 93430行、packages 53445行、rust overlay 58526行、sdk 8873行、src 87768行、web/src 88845行。总计约39.1万行。文件计数/行数不是复杂度、正确性或生产覆盖率。

主要定位点：Knowledge persistence/database 8996行、AgentRepository 7256行、Knowledge route 4735行、vector_store 4179行、retrieval_service 4082行、useChatSession 3761行。重构应消除多重owner和反向依赖，不要求把上游大文件机械切小。

## 5. 本轮未作出的结论

- 未证明完整跨租户HTTP exploit，也未做外部恶意执行；局部负向探针只说明该边界不能自证安全。
- 未跑真实多副本、压力、故障恢复、完整索引切换/回滚、fresh-machine、多架构native build或正式发布。
- 未用真实业务黄金集比较算法质量/成本，不能给RAG或路由一个SOTA百分比。
- 未把容器health pass、单个检索命中、单个Agent回复等同于本期升级已完成。
- 未把上游codex-harness功能丰富程度当作平台集成正确性的替代证据。

后续优先刷新具体发现，直接实现对应工作包；不再次把所有历史报告和全部upstream功能线性重读一遍。
