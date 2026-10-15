# R1 剩余旅程实施与验收

基线 `425ce123`，开发分支 `codex/r1-remaining-acceptance`。主代理唯一写入；只读调查与独立 review。沿用 DR-01 C4（`22bf4e1ecad5`），不重复开发。此前报告是历史证据；本记录区分本轮真实链路、受控故障、自动化与未验证项。开始时不提交或合并；最新用户授权尽快收尾并合入本地 main，因此允许本轮选择性提交及本地快进合入，不推送、不删除分支。

## 需求—现状—缺口—验收

| 需求 / 旅程 | 保留现状与既有有效证据 | 本轮剩余验收 |
| --- | --- | --- |
| AS-01 / J01 | 会话管理、草稿隔离与回看 | 双击仅一 run、运行会话切换、删除深链接 |
| AS-02 / J02 | 六状态、部分正文、安全诊断；DR-01 unknown/cancel | 受影响状态回归，刷新不触发执行 |
| AS-03 / J03 | DR-01 原 run/turn 重启、审批续接、结果去重 | 断网重连、后台切回、停止后下一轮 |
| AS-04 / J04 | 激活模型、默认和手选规则 | 有效参数、不兼容 409 保草稿入口 |
| AS-05 / J05 | 文本上传、绑定和真实读取 | 三附件一失败、重试/选择/移除、过期 |
| AS-06 / J06 | KB 检索/无答案/定位、私有分享拒绝 | 后续范围切换、撤权后引用/正文/API |
| AS-07 / J03 | 活动去重、历史不滚底 | 长任务停止及 1000 事件样本 |
| AS-08 / J29 | 长期记忆查看改删、开关持久化 | 关闭后真实禁写、重新登录、用户隔离 |
| AS-09 / J05/J08 | 图片失败/未知持久、安全投影 | 真实视觉和生成成功、刷新回看 |
| AS-10 / J01 | 390px 历史、换行和空闲 ESC | 窄屏上传/审批/停止/引用、焦点、中英文 |
| TL-01 / J07 | 有效目录与不可用原因 | 页面有效集合及设备/配置引导 |
| TL-02 / J07 | 原审批恢复、批准/拒绝、服务端核验 | 重复点击、过期、参数改变零 dispatch |
| TL-03 / J03 | 原任务恢复、未知不重放 | 长 Python 停止与 Worker 结果/资源事实 |
| TL-04 / J07 | 当前 safe 合同、V1/V2 拒绝旧权限 | Studio/助手回读、跨入口负测 |
| AR-01 / J08 | Quiz/DOCX 真实产物与归属 | PDF/图片、部分失败、零字节、列表去重 |
| AR-02 / J08 | 认证 API 下载及权限负测 | 内置浏览器下载真实字节，预览/失效 |
| AR-03 / J08 | 普通分享/预览/撤销、每次下载鉴权 | 最终页面产物范围/过期/撤销下载 |
| AR-04 / J08 | 登录/访客 Quiz 作答、刷新、幂等回归 | 分享受影响链路的定向复测 |

## 启动阶段观察（历史记录）

- Git：初始工作树干净；创建开发分支，保留既有实现。
- 环境：Compose 属于本仓库，但服务已全部停止。启动预检发现本地非秘密 kernel revision 陈旧，按源码锁显式指定 C4。旧 migrator 镜像只有 epoch3，拒绝已验收 epoch4；仅更新迁移镜像，不重建 Rust、不改数据库已有事实。
- 权限调查：V1 历史有 KB 撤权遮蔽；V2 事件重放缺少同等检查是待实际复现项，尚未将静态疑点写成实测泄漏。
- 启动阶段尚无本轮功能 PASS；最终结果以下文收尾记录为准。


## 最终修正与验收（2026-09-27）

本次收尾交付是已验证的 R1 安全增量。DR-01 C4 已在 main，原 run 重启恢复不再是阻塞；不重建 Rust、不重复六窗口验收。本次没有新增执行引擎、数据库迁移或依赖。

### 实现

- 复用持久 snapshot 和现有 KB 可见目录，沿原 run 的时间线累计来源；关闭本轮 KB 不移除 Core 前文。V1/V2 新轮、显式恢复、后台恢复模型调用均重新检查当前来源权益。
- 历史、SSE、正文、思考摘要、活动、审批参数、Quiz、派生文件及下载保持同等权限。待审批撤权仍可拒绝；批准拒绝且零写 dispatch。GET thread 新增可选 restricted_source_run_ids，只读轮询不发消息、不创建 run。
- 冷恢复保留 runtimeThreadId，每条助手历史保留原 runtimeRunId，遮蔽同 run 的前置消息与最终回答。正文不可读时保留运行/成功/失败/取消/未知事实和安全 ID。
- Quiz 匿名创建及公开读题/start/submit/result 共用原 run 累积来源；私有源禁止匿名分享。可信且无私有来源的旧测验保持可用；无法核验原 execution/snapshot 的旧链接匿名 404，新建分享 409。这是 PRD 的 fail-closed 行为变化。
- E2E 新增 existing-account-only 模式，避免正常验收重建账号或修改密码；新增单附件503及真实Office下载定向用例。

### 本轮真实链路 / 内置浏览器

| 场景 | 实际结果与证据 |
| --- | --- |
| 关闭长期记忆后请求写入，批准再取消、刷新 | Qwen 实际任务 memory=off / context not_loaded，零 Worker dispatch；存储条数和哈希不变；刷新取消一致；恢复原开关。memory-off-*.json |
| 390px 三附件、一格式无效、仅选择 alpha | 两文件真实上传，一文件本地格式拒绝；排除 beta、移除无效项；真实 read_attachment 一引用/一执行，返回 alpha 正文。390-attachment-run-facts.json |
| 普通成员读取私有 KB | 既有专用 subject 临时普通成员角色，私有合成资料 viewer；真实 Qwen 两次只读检索成功、正文/引用真实一致。KB 本轮关闭后下一轮仍继承前文，实机出现 generate_quiz 审批 |
| 待审批时撤权 | 参数不再包含私有测试值，can_approve=false，批准 API409；UI禁止批准、允许拒绝。原 Quiz run 最终 failed，安全ID仍可见，零写执行；会话始终2个run。source-revoke-pending.json / source-final-reads.json |
| 终态冷打开后闲置撤权 | 恢复 viewer后刷新读取原结果，再撤权；没有新事件，3秒只读轮询隐藏同run全部前置/最终助手消息与来源，保留 succeeded/failed终态。tmp/r1-remaining/source-cold-idle-revoked.png |
| 撤权历史及原事件重放 | 3条助手消息正文/引用全部隐藏；两原run重放118/131事件，私有标记均不返回，terminal可见；两次 search_knowledge_base 原结果保留，generate_quiz执行数0。最终run数量2 |
| Office历史 | 内置浏览器重开真实DOCX卡片。此前IAB download事件未观测；独立Playwright真实下载字节、ZIP头、扩展名、SHA256与认证API完全一致，刷新仍可见。不得把Playwright下载记成IAB已观测 |

临时 subject 已恢复原 model_tester roles，私有 fixture viewer grant 已撤销，未新建账号或修改凭据；保留合成资料和任务事实。source-subject-role.json / kb-source-fixture.json。

普通无知识来源Quiz追加真实API回归：复用C4原测验，认证读取/匿名创建/读题/start成功；答前无correct_answer/explanation，重复提交同一attempt且cached=true；撤销后404，原generate_quiz执行数仍为1。quiz-share-real-regression.json。

### 受控故障 / 自动化（与真实 provider 分开）

- 原缺陷：受控 in-process V2撤权事件重放仍返回私有正文，revoked-events-before.json。最终定向回归和真实事件重放均拒绝泄漏。
- 受控单文件HTTP503：alpha/gamma真实上传，beta首次注入503，仅beta重试；请求次数1/2/1，取消选择/移除不删除原文件，不发送provider任务。Playwright通过。
- Python定向 **199 passed**；Node终态/来源纯测试 **10 passed**；独立review自行复跑Python46、Node10均通过（不可加总为新用例数）。
- app/node直接TS、受影响ESLint、Ruff、i18n通过；OpenAPI **2 passed**；source contract **18 passed**；architecture608文件零违规。
- 最终 make hot-update --all包含前端构建与Python复制；实际容器12个修改Python文件与187个JS/CSS资产逐一哈希匹配；4容器Compose owner均本仓库，Runtime/Worker仍为C4。make validate/status通过。runtime-identity.json与独立日志记录详情。
- E2E首次两失败是新用例使用错误本地化按钮名、漏填草稿、误读session.title；修正为实际Retry/metadata.title等，最终 **2 passed**。没有用测试失败掩盖产品缺陷。

### 独立 review

首轮确认匿名Quiz继承来源 high、终态冷恢复权限watcher medium；主代理最小修复，并补同run前置消息权限回归。复核无剩余blocker/high，也未发现新增重复run或工具执行入口。见 reports/r1-remaining/independent-review.md。

### 未验证与收尾边界

保留历史有效AS-02/模型选择/R1既有验收和DR-01证明。本轮未重跑完整R1所有旅程；真实图片成功与视觉、长Python停止资源回收、1000事件活动样本、PDF和部分分享窄屏路径仍未逐项实机证明；此前未配/结果未知的图片能力不能写为成功。IAB原生下载事件尚未观测。引用普通成员跳转KB管理页未在本轮点击验证。

本地main可合入已验证安全增量；**不声明完整R1或整个产品验收完成**。剩余范围明确列为未验证，不是Runtime持久恢复无法实现，也不自动重试未知写操作。

实现提交60ede5c5；src/main.py来源checker接线与证据单独进行命名integration提交，随后本地main快进。临时fixture恢复及正常Quiz兼容回归均完成，无额外provider任务。

## 实际本地合入

2026-09-27，codex/r1-remaining-acceptance 已快进合入本地main：实现60ede5c5，Gateway接线/证据66202d8b，日志规范2919a587。已合入Git blob与Docker源指纹一致；未推送、未删除分支。随后只记录本地合入收据，无产品代码变化。完整R1仍按上述未验证边界记录。
