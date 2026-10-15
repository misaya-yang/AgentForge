# R3 Agent / Eval 收尾验收（2026-09-29）

J14～J18 的本地真实功能验收通过，R3 收尾完成。最终修复已回归并与本地容器对齐。
基线为 `561f5f5a`，工作分支 `codex/r3-agent-eval-closeout-20260929`。
用户最初授权验收后提交和推送分支，随后明确授权合入 main。
功能提交 `19361e6c` 已 fast-forward 合入本地 main；功能与测试范围保持冻结。
[原始进度记录](r3-agent-eval-progress-2026-09-28.md)保留历次失败；
[结构化回执](r3-agent-eval-closeout-2026-09-29.json)保存实际对象、权限响应及账本。

## 用户能感知的升级

- **草稿和预览可靠了**：保存新草稿不会改掉正在运行的旧任务；刷新能找回审批和结果，配置的知识库能够实际检索。
- **版本评测能用于上线决策**：真实执行选定的 Agent Version，逐例指出退化；模型调用失败、评分失败与答案质量失败分别呈现，关键失败阻止关联发布。
- **发布、权限和回滚闭环可用**：内部、公开、Embed 与 API Token 入口按实际受众运行；撤权或到期生效；回滚后新任务用旧版本，正在运行的任务保留原版本。
- **运行结果能追查和复用**：运营页能显示实际运行并打开对应 Trace，失败可转成待审核用例；重评分和安全重试不混淆原执行，也不自动重放已执行工具。

多数页面在本轮前已经存在，视觉变化有限。主要升级是补齐这些入口之间的版本、权限、恢复和证据链路；本轮不宣称模型本身的普遍能力提升。

## 真实验收结论

| 场景 | 实际结果与证据 |
| --- | --- |
| J14 草稿与预览 | Codex IAB 在 r1 审批等待时保存 r2，刷新后仍批准原 r1。Quiz 只执行一次，真实返回两题，Quiz ID `01a0ee6b-0dd5-73a2-b048-78f5288c9533`；r2 新预览历史隔离。拒绝和取消均没有 Quiz 执行；取消后新算术任务输出 `R3_FINAL_R2.5`。产物证据是 Quiz，不宣称验证了 Office 文件下载。 |
| J15 发布、受众、撤权与回滚 | 原有 admin / 普通 user 两种身份实测。成员真实读取合成 KB；成员发布 404、匿名私有入口 401、读取他人 Trace 403。撤掉 KB 后旧任务和新会话 409；撤掉 Agent membership 后两入口 404。恢复原授权。C→A 回滚时等待审批的 C run 保持 C，批准后 Quiz 执行一次；新 A session 的实际任务和持久 snapshot 均核对为 A。最后归档本轮 Agent 并停用 publication，两入口 409 `PUBLICATION_DISABLED`。拒绝前后 run/model/tool 数量一致。 |
| J15 公开 / Embed / API Token | 用独立无 KB、无工具 Agent 验证受众，私有 KB Agent 未公开。Hosted 匿名真实回复 `R3_RECOVERY_OK`，私有 session 复用 404、伪造 KB 输入 422；到期后新会话和旧任务均 422 `PUBLICATION_EXPIRED`。Embed 合法 Origin 201 并真实回复，错误 Origin / 缺少 grant 401，CSP 限定 Origin。API Token 真实回复；缺 token 401、缺 feedback scope 403、轮换旧 token / 撤销新 token / 到期 token 均 401。一次性测试 token 已撤销或过期，原文未保存。 |
| J16 Trace→审核→真实候选 | 从原失败 Trace 导入一例，初始 pending / behavior unconfirmed，重复导入仍同一 ID、数量 1。人工编辑并审核后再次导入保留审核和预期。不可变 Version A 真执行该例 3 次，均通过；是 1 个 case 的 3 次重复，不算 3 个独立样本。规则 rescore 仅评分原 Trace，并返回有效任务回执。 |
| J16 运营与反馈归属 | Runtime Trace 曾只写 metadata、导致运营页错误显示 0。现从原 scoped run/snapshot/session 补齐身份列；实机读回 42 次历史 run、1 条差评，原 Agent / A Version / Publication 归属一致。不重放模型和工具、不以 metadata 提示作为归属权限。 |
| J17 故障、取消、恢复、重试 | 受控 provider 故障为执行失败、1 未评分、pass rate / usage 未知，未知副作用重试 409。显式新健康 Version 可恢复，旧失败保留。裁判故障不把 source candidate 变成质量 0。安全重试仅创建指定失败 case 的新 attempt，重复相同 POST 回读同一 ID；质量失败仍保留。真实 Gateway 冷重启保留原 turn 和 model call，provider revision 不变，批准后原 Quiz 执行一次。随后取消第二 case，执行 0；第三 case 未 dispatch。最终 1 passed、2 unscored，整体 cancelled，没有假 100%。 |
| J18 比较与发布决策 | 相同完整 manifest 的 A/B 五例真执行，A 行为 5/5、B 3/5（两条有意的关键退化），执行均 5/5。逐例比较 0 improved / 3 unchanged / 2 regressed；B gate fail，关联发布 409 `AGENT_EVAL_RUN_HARD_FAILURE`。A 与后续 C 的已选 run 可发布，重复发布版本/Publication 相同。缺实际指纹比较为 unverifiable/incompatible；只重试一例与原两例比较因 manifest/case set 不兼容；实际 temperature 变化比较显示 sampling 和 confounded，不宣称更优。 |

正向五例覆盖参考事实、520 CNY/day、Operations 联系人、精确无答案、不可用外发工具拒绝。
权限撤销、审批拒绝与恢复故障另用真实控制矩阵验证，不把权限失败改写为模型质量通过。

## 关键对象与分母

- 内部 Agent：`441d6434-127f-4651-884e-4afd050e4307`；合成 KB `kb_r3_final_1790704217`。
- 固定数据集：`92e21fbb-ed66-4c37-b7d6-64b239481016`，manifest
  `e13d1bba803437eb6ecab6d67590f18c4957dd2be055fb1f1205090e8e9bd9af`。
- A3 Version `0368aa7f-38ac-4074-a56a-0c539d09aff8` / run `b9e0be23-e001-437c-a8d3-4f61d42c3929`：执行 5/5、行为 5/5。
- B2 Version `1baa3624-8c8b-480c-ba4e-9bcc25310669` / run `ecbcc7e5-da46-48c6-afea-bafdf76c4fe1`：执行 5/5、行为 3/5、关键失败 2。
- C4 Version `0204d904-a836-4a97-8db2-8c1c3dc9c238` / run `11b57f38-58cf-4ef0-a3da-f5c31899b606`：执行 5/5、行为 5/5。
- 回滚前 C run `21814c7d-d503-41d3-85ed-3f8923cfbf05`；回滚后 A run `53465dd1-1c9f-481a-b52d-d48328948109`，snapshot pin 实查正确。
- Trace 导入 dataset `56dbd2bf-39fe-4cc7-8935-53e5918cdfd4` / case `809babf8-d836-53d0-b2cf-27809cd1ff9a`；审核后 live run `338f3b11-8f72-48a0-99a9-920942598a84`。
- 修正后的冷恢复 Eval `9ded949a-801d-450d-99ad-a356123fa0a9`：原第一 turn `cac10129-df47-41f7-9b41-39235d3ae325`，第二取消 turn `6f3a0be3-1d18-4e71-9ac9-0ada304d1dc3` 的 Trace 最终补齐为 1；第三 case 无 turn。
- 健康 rescore `c1d6fd7e-44b6-4a99-8486-05e0d6b3c9d2` 的 source Trace 仍为 `a1331a6c-e85f-4341-bfab-69e6b49d03d7`；裁判/重评分开始后该 source Runtime 新模型调用 0。最终本轮运行中 Eval 数量 0。

## 修复与检查

生产修复包括 epoch 10 兼容上限、授权 KB 工具投影、原 Turn 指纹、部分 Trace/outbox 补齐与原 run 冷恢复，
以及实测中发现的精确输出断言、完整 manifest、渠道绑定控制、待审核导入幂等、UUID 回执、
原生工具效果账本、未知 usage 展示、启动配置幂等、公开有效期/空工具 launch、Embed header 和 Trace 归属。
没有新增依赖、修改 Rust wire、执行数据库迁移或删除历史数据。

| 检查 | 结果 |
| --- | --- |
| 改动测试 | 末轮 20 个改动测试文件 **276 passed**，含真实 PostgreSQL 连接私有临时表的 8 项身份/范围/幂等测试；无跳过。相关 reconciler/ingest 聚焦 24 passed。 |
| Runtime / core | `make verify-assistant-runtime-dev agent-eval-core-gate` 退出 0：Runtime 5/5 组，core 两组 22 / 110。 |
| Eval | 最后增量后 `make verify-eval-dev` 复验退出 0（内部含 `make eval-e1-gate`）。 |
| Agent Studio | 补账前聚合 **40/40、source_stable=true**；最后的 Trace 身份补账增量通过上述 276 项、Eval 门禁和运营页实测。冻结范围后没有再次跑完整聚合，不把旧 source hash 称作最后增量的 hash。 |
| Web | app/node TypeScript、lint、build/bundle budget、i18n 与 Node 单元测试通过；独立浏览器四项回读 **4 passed**（B、provider 故障、rescore、冷取消），运营页→原差评 Trace **1 passed**。两条命令验证 5 个不同页面用例，不算独立质量样本。 |
| 边界 / OpenAPI / Harness | 架构、OpenAPI 与最终 Harness 退出 0。 |
| 本地 Docker | Owner labels 为本仓库；source-only hot-update、doctor/validate/status 通过。末轮 Gateway **21 个生产文件**与容器 SHA-256 一致，前端入口与本地 dist 一致；最终 `make status` 全部健康。 |

具体执行日志保存在 `tmp/r3-final-*.log`；重复门禁包含重叠测试，数量不相加。
最终生产文件清单 hash：`b563e954626c78ed891d654379f3537216f28a53f32c7059dd4ea26a0fdf70d8`。
本机 Node 24.14.0，pnpm 10.33.0，已记录与要求 Node 22 的 engine 提示；未单独验证 Node 22。
Doctor 的 Docker 3 GiB 低于建议 4 GiB 是容量提示，本轮服务健康，不声称压测通过。

## 保留的失败与验证边界

- 早期 stream interruption、旧 manifest mismatch、公开 launch/catalog/header 拒绝及第一次冷恢复 provider revision failure 全部保留；只将修复后的明确复验计为通过。
- evaluator response validation 的 400 实际已接受任务。诊断时误重试了两次，共 3 个裁判 rescore job；均已对账、保留原记录，source candidate 未重放。这三份 job 不算三个独立样本。
- 浏览器末轮冷取消断言最初误期望 UI 的 `Skipped 2`，实际 API 的质量分类是 `Unscored 2`；按真实合同修正断言，并核对 1 passed / 2 unscored、整体 cancelled、无 100%。不修改历史评分。
- 运营页只读测试最初定位到了 Ant Design 虚拟列表中隐藏的 option，随后修正为可见内容节点；原失败保留，最后该用例 1 passed。这是测试定位修正，没有扩大产品功能。
- Codex IAB 用户正常登录后的 r1/r2、审批/拒绝/取消/继续、Trace 入口和 A/B 比较已实测。最后再次访问运营页时 IAB 登录已到期；其末轮运营/结果页面由使用原账号的独立 Playwright 验证，二者分别记录。
- 五例属于固定合成样本证据，gate 保留 `fixed_sample_only` / `insufficient_evidence`。没有证明普遍模型质量、统计显著提升、业务 SLA、Office 下载、Runtime/worker 任意崩溃恢复。
- R2 双租户与未配置连接器矩阵仍留在 R2 账本；本轮双角色在同一既有租户，不能冒充双租户验证。
- 仅归档本轮内部 fixture 验证停用，其他历史失败、KB 和 Trace 保留。开始时已有的 13 份 `reports/r1-boundaries/*.json` 保留且不暂存。
