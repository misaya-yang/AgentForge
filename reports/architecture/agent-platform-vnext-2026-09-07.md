# Agent Platform vNext 架构调查与有限活栈报告

日期：2026-09-07。范围：代码架构、模块边界、兼容性、RAG/Eval/路由、CLI/桌面需求设计。结论用途：为[下一期PRD](../../docs/plans/agent-platform-vnext-2026-09/README.md)提供事实；不是当前平台release receipt。

## 1. 结论

当前三域与单codex-harness内核方向合理，已有较多可用功能及门禁。核心差距集中于授权与身份一致性、异常流/终态、持久任务恢复、配置/数据owner、实际后端装配、指标真实性及完整分发证据。详细30项源码/实测发现见[01分册](../../docs/plans/agent-platform-vnext-2026-09/01-current-state-and-gaps.md)。

本轮只有Markdown需求与报告修改，没有修复产品代码、提交或推送。两个只读子代理分别审查Knowledge与Gateway/Eval；主session核对关键证据、操作本地栈和浏览器并整合PRD。不是全仓穷尽漏洞审计或质量/性能SOTA实验。

## 2. 版本和环境身份

| 项 | 本轮观察 |
| --- | --- |
| 源码HEAD | `26dfbbc2c2e03c4daa1eef6f64dc7cf79e395c3f` |
| 初始脏文件 | `docs/README.md`；未提交的`docs/plans/agent-runtime-post-upgrade-core-fixes-2026-08.md` |
| Compose owner | 本仓`/Users/yang/projects/AI--Platfform`；未发现其他checkout的同名前缀容器 |
| 初始运行状态 | 全部既有容器停止；Docker daemon运行 |
| 本地容量 | Docker约3GiB；doctor建议完整栈约4GiB；本轮复用既有镜像、未重建 |
| Gateway/Web/KS image revision | `750b25db5d376c5c659ee54cf50175e3c5da2d34` |
| Runtime/Worker image revision | `94cbbddafc1776d5e377bca1b05932c697e82238+46158add036c` |
| 当前overlay manifest | `46158add036ca5cdd80b856c86e0f3cb1224deb4287073e4016f30b0da69a153`，source `94cbbdd…` |
| Gateway关键源码抽样 | native_responses.py和launch_resolution.py与HEAD相同；control/event_stream.py不同 |

不能只按旧image label断言所有运行源码过时（既有hot-update可能改变文件），也不能因两个抽样相同就声称全部匹配。这里明确记录已观察到的混合身份，不更新容器源码来修正本轮发现。

启动命令使用`docker compose --env-file .env start`分别启动基础设施与应用。Compose按已有依赖图运行了既有init/migrate容器，再启动各服务；没有镜像pull/build、数据清理、reset、prune或新迁移文件。后续`make validate`与`make status`通过。

## 3. 实际执行的检查

| 检查 | 实际结果 | 可以证明什么 |
| --- | --- | --- |
| `make doctor` | PASS，1内存advisory；owner、端口、必要工具和模型配置检查正常 | 本机可以进行受限既有栈验收；不是provider真实调用成功 |
| `make harness-check`（修改前） | PASS；18必需文档、55目标、37 gate schema、16程序；1既有warning | 结构与登记；warning为kb-rag-ui-t5无已知schema |
| `make architecture-boundary-gate` | PASS；587文件，0违规，4allowlisted，负向自测通过 | 静态合同有效且四项债务仍存在 |
| `make core-boundary-gate` | PASS；8 contracts/133 core modules，Knowledge→core13 | no-growth/owner登记检查；不是全core纯化 |
| `make single-instance-guard` | PASS，含负向自测 | Gateway/Runtime保持明确singleton |
| `make validate` | PASS，1既有本地bootstrap配置warning | 当前依赖、配置、模型对齐；13个可用model配置 |
| `make status` | PASS；Gateway、Frontend、Runtime、Worker、Knowledge API/worker、PG/Redis/Qdrant与拓扑健康 | 当前栈健康；不代表40个未来验收场景通过 |

门禁原始本地输出位于`tmp/gate-evidence/`，属于本次工作区证据；此报告保存有用统计，不将这些scratch升级为正式发布manifest。文档最终验证结果在本报告末节更新。

## 4. 内置浏览器实际流程

使用用户明确选择的内置浏览器与已授权专用E2E账号，凭证只在登录时读取，没有输出或复制到文档。以下页面显示和测试请求全部属于现有本地环境。

| 步骤 | 操作与结果 | 判断/限制 |
| --- | --- | --- |
| UI-01 登录 | 登录表单可用，专用账号进入Dashboard | PASS登录旅程；未创建账号或修改认证 |
| UI-02 Dashboard | 无请求时显示可用率100%、错误率0；provider以100/35显示配置状态 | 功能可访问；源码证实readiness启发式被当百分比展示，D23 |
| UI-03 知识库 | 列表加载10库/13文档/129段；打开既有验收库并查“知识库验收测试” | 一次真实hybrid检索返回2结果，dense命中2/keyword命中1；rerank/MMR禁用；不是语义质量评测 |
| UI-04 Agent Studio | 打开既有测试Agent的draft r1，启动隔离preview，提交一条无敏感信息的支持分类问题 | 实际返回相关中文答复；未修改Agent配置、发布或调用写工具 |
| UI-05 Eval概览 | 显示Trace2、已评分0、Golden2、latest gate=not_run，同时通过率与轨迹通过率100% | 页面可访问，质量/状态语义不清；不能把100%当发布证明 |
| UI-06 Trace追踪 | 从preview trace链接定位本轮run，列表显示成功、qwen3.7-plus/dashscope、延迟6.30s、TTFT3.83s | PASS该次答复与trace关联；n=1，不能作性能基准；token/评分显示0不等于真实成本/质量为0 |

没有运行跨租户攻击、批量删除、真实索引cutover、故障kill、工具写入或完整评测cohort。本轮一条preview与一条检索请求产生正常本地session/trace/查询记录；既有测试数据保持原样，没有删除清理。

### 浏览器证据（本地scratch，非发布证据）

以下图片保存自本轮浏览器并已查看；没有编辑内容。它们使用浏览器默认窄面板，部分full-page截图含空白，不能据此作宽屏/响应式或无障碍合规结论。路径是当前工作区的辅助附件，文本结论不依赖它们在其他机器存在。

![Dashboard显示配置健康度](../../tmp/agent-platform-prd-20260907/02-dashboard.png)

![Agent Studio实际preview回复](../../tmp/agent-platform-prd-20260907/04-agent-preview.png)

![Eval的not_run与100%并列显示](../../tmp/agent-platform-prd-20260907/05-eval-overview.png)

其余本地截图：`tmp/agent-platform-prd-20260907/01-login.png`、`03-rag-retrieval.png`、`06-agent-trace.png`。03只覆盖当前可见检索控件和统计入口，召回文本/分数由本轮AX状态另行确认；不把它当全结果页截图。

## 5. 子代理验证与交叉审查

- Knowledge审查：源码追踪Gateway proxy→KS actor→ACL→retrieval；用当前`_effective_dataset_permission`方法体、纯内存fake DB复现同名role跨tenant获viewer。**不是HTTP穿透验证**。
- Gateway审查：用当前OpenAIAdapter类体和fake HTTP验证usage-only→IndexError、429→final=True、提前EOF→final=True。**不是正式项目测试套件或真实provider故障测试**。
- 主session重新阅读关键ACL、grants、adapter、job claim源码确认证据位置；没有把未执行故障注入写成PASS。
- 初稿review的十项反馈均已落实：黄金集owner、真实pin/GC、安全ACL迁移回滚、普通入库与策略质量区分、复用batch jobs、运行中模型授权、容量维度、防有损CLI、judge空工具权限、一期例外清零。

## 6. 未验证范围

未执行：全套Python/Web/Rust tests；完整OpenAPI/SDK矩阵；hosted CI与原生macOS/Windows构建；headless CLI实际包；真实多实例/故障恢复/压力；跨租户HTTP/DB实际grant负测；黄金集人工复核与统计质量；fresh install；完整冻结回滚；任何生产部署。

本轮网页资料只有一手工程参考作用，没有证明本项目SOTA。结论应读为“结构方向合理，已定位下一期必须补齐的可靠性/功能边界”，而不是“通过全部架构验收”。

## 7. 文档验收与工作区保留

实际文档检查已通过：11份新增Markdown，68个本地链接无断链；83个需求ID无重复且全部分配工作包；40个场景均有定义，引用无未定义项。工作包共25个，CL-06按一期Linux/二期macOS明确跨期交付。

最终文档`make harness-check`通过：18必需文档、110入口相对链接、55Make目标、37gate schema、16既有程序；仍只有原有`kb-rag-ui-t5`未知schema warning。预备runbook未创建loop-state，因此没有新增执行程序或schema warning。

`git diff --check`通过。初始未提交核心修复计划SHA-256完全不变；原`docs/README.md`全部已有行按原顺序保留，只增加本PRD/预备runbook/报告导航。修改范围为文档与本地scratch检查/截图，没有产品源码、测试源码、依赖锁、Compose或数据库迁移文件变更。

两位只读reviewer对修订后的关键合同复核通过，未发现其限定检查范围内的执行冲突；这是文档一致性审查，不是额外产品测试。后续执行从P1-00刷新事实开始。
