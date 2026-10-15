# R3 Agent 与 Eval 执行记录（历史 CANDIDATE_80 → 本地 R3 完成）

> **2026-09-29 最新结论：J14～J18 本地真实功能验收已完成。** 当前收尾分支为 `codex/r3-agent-eval-closeout-20260929`，用户授权验收后提交并普通推送；下文“未完成”等状态是各次历史记录。最终矩阵、276 项回归与验证边界见 [完整收尾报告](r3-agent-eval-closeout-2026-09-29.md)，状态权威为 runbook 的 `loop-state.json`。

基线 `main@8b3a877a`，工作分支 `codex/r3-agent-eval-20260928`。用户在收口阶段授权提交并本地合入 `main`，未授权推送。范围、并行文件归属和状态以 `deploy/runbooks/r3-agent-eval-2026-09-28/` 为准。此记录区分代码/自动化已通过与 J14～J18 尚未完成的真实验收；**不声明完整 R3 已完成**。

## R2 遗留项分类（本轮只核对一次）

| 项目 | 分类 | R3 处理 |
| --- | --- | --- |
| J09～J13/J30 的最终内置浏览器矩阵 | R100_REQUIRED：开始时 Mac 锁定；最终 IAB 可访问但旧登录态过期 | 本轮最终打开 `/agents` 仍跳到 `/login`；不把独立 Playwright 浏览器充作 Codex IAB 已验 |
| 双租户双角色撤权/分享 | R100_REQUIRED：仅一个已确认既有 E2E 身份 | 保留权限自动化，真实矩阵待既有账号前提；不创建新账号 |
| 已配置连接器的更新/删除/失效凭据恢复 | R100_REQUIRED：当前 Runtime 未配置对应连接器 | 只记录现有 `not_configured` 真实状态，待连接器前提到位 |
| 批量 POST 已成功但响应和 operation ID 完全丢失 | FOLLOW_UP：服务端结果未知，现有 UI 要求人工对账 | 不自动重发；不把人工核对说成确定未执行 |

上述是 R2 未验/后续事项，不是 R3 完成状态。R3 使用已授权的合成知识和现有 Eval 样本库切片；若发现会实际阻断 J14～J18 的 R2 合同缺陷，只修该前提并单列证据。

## R3 初始需求—现状—缺口—验收

| 需求 | 现有入口 | 待判定及验收 |
| --- | --- | --- |
| AG-01～04 | Agent 目录、草稿、预览、资源解析已有实现 | J14 固定草稿 r1/r2、实际模型/工具/KB/记忆权限与失败/冲突回读 |
| AG-05～08 | Runtime 预览、发布页、版本与回滚 API 已有 | J14/J15 审批/取消/刷新、受众、发布固定版本、旧 run 与新 run 归属 |
| AG-09/EV-07 | Trace 与反馈页面/API 已有 | J16 差评/失败一跳到授权 Trace，待审核用例不重复导入 |
| EV-01/02/08 | R2 已把 KB 失败写入权威 pending 修订 | J16 固定数据集/候选快照，真实运行与 Trace 重评分分开，KB 链路可回溯 |
| EV-03～06 | Eval 任务、评分、比较与取消已有实现 | J17/J18 故障/未评分分母、可核对效果、A/B 逐例与不可归因提示、选定失败项安全重试 |

当前表仅指调查入口，不将页面/API 存在记为验收通过。自动化、真实服务、故障注入和内置浏览器结果将分层追加。

## 本轮实现与复核

| 范围 | 修正的用户事实 | 当前证据层 |
| --- | --- | --- |
| AG-03/05 | 预览固定草稿修订；保存 r2 不清除 r1 运行。刷新先核验 Agent/用户/preview 会话 pin，再按同 session/run/thread 恢复状态、审批、终态消息和授权产物；未确认终态时禁止新建、清除或切换预览。产物只显示已保存且可下载项，并通过鉴权 Blob 下载 | 自动化 + Docker 单次真实预览；r1 运行中保存 r2、审批重启仍待真实故障场景 |
| AG-04/06/07/08 | 发布评测冻结受众与公开截止时间，过期后运行和回滚均拒绝。tenant/API Token/Embed/public 使用对应实际读者身份预检；匿名身份经签名传到 Knowledge Service，仅公开 Dataset 可读，避免公开 Agent 暴露租户可见资料 | 自动化 + 本地 Docker 私有 Dataset 匿名拒绝；公开 Dataset/双角色真实矩阵未配置 |
| EV-01/02/08 | 用例 PATCH 验证合并后的 trajectory/assertions；重新评分冻结版本和哈希，完整样本仅在私有 outbox，公开 run/experiment 输出连旧快照也剥离样本正文 | 自动化，未跑 KB 撤权后的真实 Eval 执行 |
| EV-03/04/05 | 执行故障、未评分、质量分和发布 gate 分开；裁判/候选故障不算质量 0；部分 live run 失败；A/B 对比标明相同失败、未评分与证据不足 | 自动化，真实 provider 故障/裁判故障未注入 |
| EV-06 | 只对用户明确选定且可证明无工具副作用的失败样本新建单次 attempt；未知执行、工具轨迹、KB 来源不明或撤权均拒绝。幂等键跨刷新保存，同一请求重放返回同一 run，不自动执行 | 自动化，真实重复 POST/进程崩溃未演练 |

独立只读 reviewer 首轮指出 Eval PATCH 校验遗漏、重新评分快照越权、直接评测缺失数据集返回 500、发布按 owner 预检、匿名 KB 越权、预览刷新后的重复执行与产物下载不鉴权、重试未知副作用及无幂等。主代理协调最小修复后，reviewer 复核最终 A/B/C 差异，**没有确认的合入硬阻塞**。该结论是静态 review，不代替下述实测。

## 已运行的自动化与本地 Docker

| 层 | 实际命令/动作 | 结果 |
| --- | --- | --- |
| 隔离 PostgreSQL authority | `AGENT_PUBLICATION_EXPIRY_DB_LIVE=1 uv run --all-packages --extra test pytest -q --no-cov tests/database/test_agent_publication_expiry_migration_live.py` | 1 通过；冻结 baseline→epoch 7 的结构/ACL/扩展/参考指纹匹配 |
| 发布原子性 | `uv run --all-packages --extra test pytest -q --no-cov tests/database/test_agent_publication_atomicity.py` | 17 通过；隔离测试库经 authority 安装 007 |
| Agent Studio 聚合 | `NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost make verify-agent-studio` | **40/40 门禁通过、source_stable=true**；含 Studio 27/27、既有路由 51/51 浏览器模拟用例，证据在 `reports/agent-studio/agent-studio-regression-v1-result.json` |
| Eval/Agent | `make verify-eval-dev`；`make agent-eval-core-gate`；`make eval-e1-gate` | 全部退出 0；Agent core 两组为 18/18 与 99/99，E1 204/204 |
| Knowledge/边界/API | `make kb-unit-gate`；`make architecture-boundary-gate`；`make verify-openapi-contract`；`make harness-check` | KB 2022 通过、1 个需独立 DSN 的用例跳过；架构 0 违规；OpenAPI 兼容、Harness 通过 |
| Web/聚焦 | Web TypeScript、lint、build、i18n；A/B/C 聚焦 Python/Node 回归 | 全部通过；各流记录 A 63、B 155、C 7 Python + 11 Node，通过数存在门禁重叠，不能相加充作独立覆盖 |
| Docker 部署 | Compose owner label 核对；`make doctor`、`make migrate`、`make hot-update ARGS=--all`、`make migrate-status`、`make status` | owner 指向本仓库；epoch 7 已应用；Gateway/Knowledge/Web 源码或入口文件哈希与容器相同；所有服务健康；doctor 仅提示 Docker 3 GiB 内存低于建议 4 GiB |
| 真实 Docker 浏览器 | 现有 E2E 账号 + `playwright.live.config.ts`，`E2E_EXISTING_ACCOUNT_ONLY=1`，仅运行 `agent-studio-live.spec.ts` 和 `agent-publish-live.spec.ts` | **2 通过、1 因未启用回滚 feature flag 而跳过**。真实创建/保存/刷新/预览并收到非 stub 回复，内部发布/回滚/资源阻断通过；临时 Agent 按测试清理。原始非敏感结果见 [`r3-live-e2e-2026-09-28.md`](r3-live-e2e-2026-09-28.md)，截图在 `reports/agent-studio/as-05/` 和 `as-06-screenshots/` |
| 真实 Knowledge 签名链路 | 在最终 Gateway 容器调用 Resolver，使用既有私有 Dataset 和匿名身份 | `anonymous_private_dataset=denied`；无租户可见/公开 Dataset fixture，因此该两类未实测 |
| Codex 内置浏览器 | 最终访问 `http://127.0.0.1:8081/agents` | 跳转 `/login`，旧登录态已过期，**认证后的 J14～J18 IAB 验收未执行** |

聚合门禁前两轮曾失败：第一轮并发写文件造成源不稳定和前端 import 瞬态；第二轮暴露模拟 API 未跟进预览历史读取、隔离发布测试库未应用 007、以及本机代理对 localhost 临时 nginx 返回 502。分别修复夹具/迁移流程并设置 `NO_PROXY` 后，第三轮 40/40 通过。前两轮不计通过。

## J14～J18 收口判定

| 场景 | 当前判定 | 剩余 R100_REQUIRED |
| --- | --- | --- |
| J14 草稿 r1/r2 与预览 | CANDIDATE_80：真实单次草稿/保存/刷新/预览通过，r1 运行中保存 r2 的自动化通过 | 真实长 run 中修改草稿、审批/取消/产物回看，Codex IAB 登录恢复 |
| J15 内部发布与回滚 | CANDIDATE_80：真实内部发布、阻断、回滚通过 | 双角色权限、公开/Token/Embed 实际受众和失效期限的真实矩阵 |
| J16 Trace→样本→真实候选 | 自动化通过关键合同 | 合成知识五类用例的真实 A/B 执行与来源撤权回查 |
| J17 provider/裁判失败/取消 | 自动化和故障分类通过 | 真实 provider/裁判受控故障、进程恢复与选定样本重试对账 |
| J18 固定 A/B 比较 | 自动化证明不可归因/同失败/未评分分类 | 真实固定候选同一数据集逐例比较和发布决策回读 |

本地 `main` 合入的是可复核安全增量；R3 **尚未达到 J14～J18 全部真实验收**，完整 R3 不标完成。未推送远端；保留开始时已有的 `reports/r1-boundaries/*.json` 未跟踪文件。

## 2026-09-28 续验修正

上述“旧登录态过期”只是页面现象，不是充分的阻塞判断。同一已有 E2E 身份在本地 Docker Playwright 已成功登录。主代理随后在 Codex IAB 尝试正常登录：该浏览器使用独立虚拟剪贴板，系统剪贴板粘贴无数据；打开本机凭据文件的 `file:` URL 被浏览器安全策略明确拒绝，并明确禁止通过间接路径绕过。已停止绕过尝试并异步请求用户在 IAB 手动登录；其余 API/真实服务验收继续进行。此项仅是 **IAB 交互输入依赖**，不能阻断可独立执行的 J14～J18 后端/Playwright 工作。未输出或保存账号凭据。

继续调查确认另一项 **R3 范围内的代码缺口**：现有 Eval `live_candidate` 经 V2 Thread/Turn 真实执行，但启动的是内建 Assistant，既没有 Studio Agent ID，也没有固定草稿/版本身份。因此此前自动化的 A/B 分类不能声称已经验证“固定 Agent A/B”；也不能把 `rescore_trace` 当新执行。后续使用现有预发布不可变 Version 作为 typed Eval target，由 Gateway 服务端解析版本与当前资源授权，保持原 Runtime/Worker 执行和 durable handle；五类样本真实 A/B 通过后再核发布决策身份。该工作仍在进行，未改变本报告上方已通过的安全增量证据。

## 2026-09-28 固定 Version 安全增量收口

上段所述 typed target 已实际实现：Eval 的 `live_candidate` 接收服务器核验的 `agent_id`/不可变 `agent_version_id`，冻结模型、KB、spec/runtime 指纹；worker 使用原创建者授权，V2 Thread/Turn 核对同一 pin 后执行。Web 可选定和回读版本。内部 linked publish 需显式选择已完成、完整评分、与 release eval **同一数据集和 manifest** 的 run；复用已评测 Version，审计只称五样本有限证据。公开/token 的该路径继续拒绝，待受众专属质量规则，不扩大受众。

真实 Docker 暴露并修复了两处数据库合同错误：预发布 Version Preview 原受旧 shape CHECK 拒绝（epoch 8）；reviewer 随后发现 NULL/UNKNOWN 漏洞（epoch 9）；有 Hosted 历史会话时旧 composite FK 又阻止 Publication 指针回滚（epoch 10）。三个均为前向 authority 迁移，旧 epoch 未改。普通 Preview SSE 断开改为同一持久 Runtime 游标的后台终态重读，避免把断开误报取消；该补写在 Gateway 冷重启后仍无持久重试。

本次实测详情与每个 run 的区分见 [固定 Version 本地实机补验](r3-typed-agent-live-2026-09-28.md)。J14 的 r1 运行中保存 r2、刷新后开独立预览 **Docker Playwright 1/1 通过**；J15 在旧 v2 Hosted 会话存在时回滚 v1，首次 500 后应用 epoch 10，同用例复测 **1/1 通过**。五样本真实 A/B 均经 Runtime/provider 执行；最终 A 有 2 个 `stream_interrupted` 未评分，B 执行 5/5 但预设行为断言 0/5，质量 gate 拒绝，比较标记证据不足。带 KB 来源失败样本的显式重试返回 409，无新 run。不能把这些负向结果写成完整 J16～J18 通过。

独立只读 reviewer 的数据集错配、Web selected run 缺口、Preview detach Trace、shape NULL 漏洞均已最小修正并复核；epoch 9/10 最终静态复核无合入硬阻塞。Codex 内置浏览器仍停留登录页，账号输入受该浏览器安全策略限制；普通 Docker Playwright 使用原专用账号成功。真实裁判故障、五类行为全部通过的固定 A/B、Trace 冷重启补偿、完整双角色/公开受众矩阵仍未验收，**R3 不标完成**。

## 2026-09-28 最后一轮安全增量收口

用户本轮明确授权提交、推送并合入 `main`，替代上文当时的“未授权推送”状态。本段记录基于 `main@f733eb5f` 的新增差异；历史验收结果保持原样。

- 修复固定 Version 的 V2 和旧版预览/已发布入口首次建 Runtime Thread 时的指令绑定；仅 resume 传入指令时，已加载 Thread 可能忽略覆盖。新增旧版 preview/published 首轮请求回归。
- 内部 linked publish 只接受已选定、同数据集及 manifest、且 Eval gate 明确通过的 run；失败 gate 在发布页显示不可用于关联。A/B 相同的关键失败继续阻断比较，裁判失败后候选执行成功仍显示为成功执行且未评分。已接受的安全重试幂等回放返回原回执。
- 新增只读持久 Trace 补账：Gateway 冷重启后从原 run、原 turn 终态事件回填一条 Trace 与一条 outbox，不启动模型或工具。reviewer 找到 Rust 取消持久事件 `compat/v1/cancelled` 遗漏，已补终态查找、校验、事务约束和回归。

| 证据层 | 本轮实际结果 |
| --- | --- |
| 自动化 | 受影响 8 个 Python 文件 127/127；取消补账 10/10、旧版固定 Version 首轮请求 3/3；Ruff、Web app/node TypeScript、lint、build、i18n、`make harness-check` 通过。`make eval-e1-gate`、`make agent-eval-core-gate`、`make verify-eval-dev`、`make architecture-boundary-gate`、`make verify-openapi-contract` 退出 0。 |
| 本地 Docker | Owner labels 指向本仓库；`make hot-update ARGS=--gateway` 和 `--frontend` 后 `make status` 全健康；Gateway 补账源与容器、前端 `dist/index.html` 与容器 SHA-256 一致。旧失败 run `ad46d3d7-7216-4c5e-a830-8b15d28ce9fb` 冷重启后为 Trace 1、outbox 1、模型调用 1、原事件 1248，没有重放。 |
| 浏览器 | 发布页 Playwright 11/11；开启 live 开关后现有 E2E 账号在 Docker 上发布/回滚 1/1。Codex 内置浏览器本轮仍在 `/login`，认证后的交互未验；不能把 Playwright 算作 IAB。 |
| 独立 review | reviewer 提出的取消事件和旧版首轮指令两个确定缺口已修并复核；最终静态复核未发现剩余确定合入阻塞。扫描终态 run 缺少匹配时间排序的索引，是未压测的规模风险，未伪称已验证性能。 |

仍未把 J16～J18 的五类行为全部跑出可发布质量证据；真实裁判/provider 故障、双角色/公开受众矩阵与内置浏览器登录后流程未在本轮完整验收。故该提交是可合入安全增量，**完整 R3 保持进行中**。

## 2026-09-29 直接收尾

接手时 `main` 与本地 `origin/main` 均为 `561f5f5a`，只有 13 份既有 R1 未跟踪证据，全部保留。本轮固定为已有升级的运行缺陷与核心实机验收，不启动 R4/R5；新增修复尚未提交或推送。

### 五个实际缺陷与修正

1. **Knowledge 冷启动失败**：数据库已到 epoch 10，共享兼容上限仍为 7。仅把上限补到 10；真实 manifest 启动测试仍拒绝旧版与未来版本，没有修改数据库。
2. **部分 Trace 恢复缺明细**：主记录先写入、span/event 未写完时，恢复只补 outbox，后续永久跳过。现在先锁定并验证 scope，再按唯一键补齐缺失子记录，最后写 outbox；不覆盖已有证据、不重放运行。
3. **KB-only Agent 无法检索**：KB bindings 与普通 capabilities 分开存储，空 allowlist 把检索工具过滤掉。现在对已授权 KB 的 auto/tool 模式补入 `search_knowledge_base`；off、空 KB、无关工具仍关闭，Thread/Turn 共用投影。
4. **真实评测缺指纹**：旧 Eval 依赖实际未收到的 context_budget 事件取得指令和工具 hash。现在持久 Turn snapshot 记录最终 base/developer instructions 与 dynamicTools 的 canonical SHA-256，Eval 仅读取两个 hash；旧缺值继续缺失，门禁不放松。
5. **已有终态 Trace 阻断 Eval 恢复**：普通 reconciler 先写终态 Trace 后，Eval 未读取模型账本，因缺 model_ref_verified 拒绝。现在恢复分支只读原 run 的账本、与 snapshot 匹配的 Session/Agent Version pin，补足评分观察并校验模型/版本；不启动候选、不重放调用。真实 PostgreSQL 只读连接上的恢复复验通过，模型/工具调用为 0。

候选为 `561f5f5a` 加本轮六个生产文件；排序后文件哈希清单的 SHA-256：`c25c7a8a41356db81c5eab562b6487bbaaab3153ee7ce7db6eccf59998116078`。明细在 `tmp/r3-closure-candidate.json`，Gateway、Knowledge API/worker 的 10 组相关文件比较一致。前端未修改，容器入口与本地 dist 哈希一致。

### 实际验收

| 层 | 结果 |
| --- | --- |
| 定向测试 | 7 个受影响 Python 文件 **117 passed**；`tmp/r3-closure-focused.log` |
| 门禁 | `make verify-assistant-runtime-dev` **5/5 组通过**；`make verify-eval-dev`、改动文件 Ruff、`make harness-check`、`make architecture-boundary-gate` 均退出 0。Web 门禁提示本机 Node 24 与建议 Node 22 不同，检查通过 |
| Trace 真实 PostgreSQL | `uv run --all-packages --extra test python tmp/r3_closure_trace_sql.py` 在连接私有临时表模拟主记录已写、明细未写；恢复两次后 Trace/span/event/outbox 各 **1**。实际运行表零写入，模型/工具零调用 |
| Eval 恢复真实账本 | `uv run --all-packages --extra test python tmp/r3_closure_eval_resume.py` 使用只读连接和本轮真实 A 结果，模拟终态 Trace 缺少 Eval metadata；恢复原 Version/model/usage 并补评分观察成功，candidate.run 未调用。是受控恢复分支实测，不夸大为所有进程崩溃场景 |
| 独立 review | `/root/closure_review` 复现 Trace 缺口，修正后独立运行 **14 项 Trace 测试通过**；最后的 Eval 恢复修正又独立运行 **14 项模型证据测试通过**。KB 与 fingerprint 最终 diff 静态复核未见新增硬问题。reviewer 未操作共享运行资源 |
| Docker | 恢复原有容器并用 `make hot-update` 同步；未删除容器/卷、未执行迁移。`make validate` 和最终 `make status` 通过；日志 `tmp/r3-closure-status.log` |
| 真实浏览器 | 现存账号、`playwright.live.config.ts`：`agent-studio-live.spec.ts` **2 passed，1 disabled-feature 用例跳过**；`agent-publish-live.spec.ts` **1 passed**。覆盖草稿、模型回复、r1 运行中保存 r2、刷新、新预览、内部发布及保留旧会话的回滚 |
| Codex 内置浏览器 | 用户正常登录后：草稿 r2 新建隔离会话，实际执行知识检索，得到 **KITE-R2-52、520 CNY/day** 及文档来源；刷新后同 session 回答仍在。error/warn 日志为空、无错误遮罩。截图 `tmp/r3-closure-iab-20260929.png`，session=`f3c10d15-95cf-44ca-bd95-6e5c1ade6746` |

### 固定 A/B：正向结果与失败均保留

两例分别验证已知事实与无答案，不冒充完整五类行为矩阵。数据集 manifest=`c590544804a28d56835f086991538346978bbfd284f80996e0b866ebaafabc02`，Agent=`936ca430-9df4-4412-8e0b-fb0cf188c6e0`。

- 修正前 A=`2722445f-5446-40c3-bca0-5e618c96002b`、B=`26615d37-ef50-4ee5-8101-435bab7bd22a` 均 1/2；事实题无工具而回答未知，比较缺指纹，原失败保留。
- 修正后 A=`b7aa597f-169d-4d61-a8c7-16759587800b`，固定 Version=`7ca349cd-f47d-4832-b39e-6b9cf4ff8fa9`：**执行和评分 2/2**，fingerprint_complete=true，来源 version=4/hash=`932430033b82aaf3e303722bf0deac86f9f09f664187649cfcbe5faa724611d7`。gate=warning，不称足够发布质量证据。
- 修正后 B=`61231708-25cf-49a2-925f-ad7c4c318ed7`，固定 Version=`b8f4d0eb-f117-48be-9a0c-734d46db73c4`：2 次模型调用均 unknown/stream_interrupted，case=failed/unscored，run=failed、gate=unavailable，比较拒绝。根因尚未确定为平台或上游，不盲目重放、不修改断言。

### 剩余边界

五类修复及上述核心路径已验证，**完整 R3 仍未通过**。仍保留固定 A/B 正向质量比较、完整五类行为/裁判故障/双角色受众矩阵；R2 真实双租户及已配置连接器验收；R1 图片 CDN 原前提。IAB 登录阻塞已解除，不能继续作为挂起理由。没有把受控恢复分支测试称为完整 Eval 崩溃矩阵验收；后续应针对具体未验项收口，不自动扩为全平台开发。

## 2026-09-29 完整 J14～J18 收尾（进行中）

用户明确选择“继续补齐 J14～J18 全部验收，达到完整 R3 后再提交推送”。本轮工作分支 `codex/r3-agent-eval-closeout-20260929`，基线 `561f5f5a`；完整验收之前不提交或推送。保留既有 13 份 R1 未跟踪证据。

- 重跑原七文件定向回归：117 passed；原六生产文件与上一候选哈希一致。Ruff、Harness、架构边界、Runtime 5/5 分组与 Eval dev 门禁退出 0；Doctor 只有 Docker 3 GiB 内存建议。
- 新独立固定 B 运行 `6fb21182-dec6-4df6-9d2b-f915269119de`，相同 Version/manifest，执行两例成功；旧 `61231708-25cf-49a2-925f-ad7c4c318ed7` 的未知失败保留。原中断不能由现有证据归因于平台或上游。
- 逐例回读发现 B 无答案回复只是引用预期短语，旧 contains 规则将其判通过；不能据此宣称语义通过。新增可选完整响应断言 `output_equals`，新修订重验，旧评分不改。精确断言及相关评分回归 118 passed。
- 已分类协议故障原来只留下泛化 stream_interrupted，新增两例回归先失败后通过，保留精确错误码且继续把已分发结果标 unknown、不重试、不补造 usage。模型与内部 API 定向 67 passed；修正了既有 facade 签名测试缺少 source_access_checker 的预期。
- 既有普通成员正常登录核实为 `agent-studio-isolation-dcac20a12618`，与专用管理员同属 default；未新建/重置账号。建立独立 KB `kb_r3_final_1790704217`、文档 `c260feeb-063c-48cd-8401-95ed8a4e8c62`、Agent `441d6434-127f-4651-884e-4afd050e4307`，只含合成事实，并授予该成员本夹具权限。
- 当前聊天 IAB 首次调用超时，重取后可用；用户正常登录后已确认 dashboard 与 Agent 目录。新会话的状态单独验收，不能沿用先前截图。

完整矩阵仍在执行，`loop-state.json` 是状态权威；本段不声明完整 R3 通过。
