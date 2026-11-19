# R1-RP04 剩余边界收口（2026-09-27）

基线为本地 `main@317f1a14`，在 `codex/r1-final-boundaries` 实施 RP04；最新用户授权将已验证增量本地合入 `main`，不推送。DR01/RP01/RP02/RP03 已有证据沿用；本轮没有重跑 Rust 崩溃矩阵、重建引擎、重复执行旧工具或调用图片生成。下表把真实服务、受控注入和自动化分别记录。

| 需求与旅程 | 本轮结果 | 证据层 |
| --- | --- | --- |
| AS03/J03 客户端断线与切换 | 一次受控事件 GET 503 后，原 thread/turn cursor 重连；thread POST=1、turn POST=1、interrupt=0，正文只出现一次。真实本地 Chromium 将已保存的审批到期原 run 与 PDF 会话相互切换、打开另一浏览器标签再切回、刷新，历史正文和 run ID 不变，执行 POST=0；内置浏览器也完成两会话往返。账本中两 session 的 run/执行数保持1/0与1/1。原 run 重启恢复沿用 DR01。 | `chat-experience.spec.ts` 受控 E2E 1 passed；`r1-background-history.spec.ts` 真实服务 E2E 1 passed，`background-switch-ledger-compare.json`、`iab-session-switch-return.png`；DR01 是此前真实 Runtime 验收 |
| AS04/J04 409 与生效参数 | 受控 409 后草稿和已选择上传均保留，点击保留草稿新会话无多余上传/turn。原真实 run 不可变快照为 qwen3.8-flash、temperature 0.7、请求 auto / 生效 minimal；Gateway 历史和内置浏览器刷新后的活动面板一致。修复了恢复函数覆盖历史参数。 | 受控 E2E 1 passed；真实快照 `approval-after.json`、历史白名单 API 与 `effective-settings-after-refresh.png` |
| AS05/J05 422 | 受控 ATTACHMENT_UNAVAILABLE 显示重新上传并手动发送的下一步，原草稿/选中文件保留；turn POST=1 次尝试、被拒绝的 accepted run=0，不自动重试、不显示注入的内部错误。 | 受控 E2E 1 passed；不是自然存储失效案例 |
| AS06/J06 普通成员引用 | 现有普通 user 仅获合成数据集 viewer，文档 API 200，旧知识控制台路由 403。改为助手内只读预览；真实 Qwen 回答及引用可打开。撤销该测试 grant 后文档 API 403，打开的预览和受影响回答被清除；未扩控制台权限。 | 真实 provider/IAB，`ordinary-citation-authorized.png`、`ordinary-citation-revoked.png`、`ordinary-kb-grant.json` |
| AS08/J29 记忆隔离 | 现有 A/B 两账号同租户不同 user：A 写后重登录可见，B 不可见；B 同 key 写删不影响 A；A 关闭记忆不删已存事实。测试 key 清除、两账号开关恢复。 | 真实 API/存储，`memory-isolation-real.json` |
| TL02/J07 审批自然到期 | 真实 Qwen 请求 `generate_quiz` 后不审批，600 秒自然到期先于 900 秒模型 lease。审批 cancelled，批准请求 409，执行记录 0；刷新/重开无待审批卡。**整个 run 最终 succeeded，是模型对工具到期给出如实文本；工具显示 failed，未生成 Quiz。** | 真实 provider/Runtime/IAB，`approval-before.json`、`approval-after.json`、`approval-expired-decision.json`、截图 |
| AR01/02/J08 产物异常 | 单条/创建的零字节 API 均 ready=false，下载409；图片/文件卡不发下载也不显示永久 spinner，实时事件不贴成功卡。历史 GET 503 显示暂不可核对，正常 PDF 恢复后仍可下载。受控第二输出失败时正文、失败状态、原真实 PDF 都保留且刷新一致。 | Python 单测；受控 E2E 2 passed；原 PDF 真实字节与下载 E2E 1 passed。第二输出失败、零字节和503均为受控注入 |

## 实际修正

- `src/api/v2/agent.py`：撤权投影的 `run_started` 不再回填私有快照参数。
- `src/services/agent_runtime/thread_store.py`：按 tenant/user/session/thread/run 联合边界批量投影原 run 的模型、温度、思考选项；撤权消息跳过。前端历史和 `restoreLatestRun` 保留同 run 的已保存参数，活动面板同时显示生效模型、温度和 think 值。
- `ContextDisplay`：引用打开现有 ACL 校验的 `getDocument`，关闭/切换忽略迟到响应，重新核权失败清除预览，不跳转需要全局知识控制台权限的路由。
- 422 失效附件给出明确操作指引；产物零字节、读取失败和部分失败按已有状态分别展示，不自动重建任务。
- E2E `global.setup.ts` 默认仅使用现存账号，缺凭据、登录失败或要求改密码时停止；旧安全开关继续生效。只有显式 `E2E_PROVISION_ACCOUNTS=1` 且无已有凭据、无 model-tester 账号的全新隔离环境才走初始化路径。入口示例和运行文档已同步。

## 测试与运行身份

- Python 定向 **93 passed**（thread store、V2、来源撤权、Assistant artifact/session），命令与结果见 `reports/r1-boundaries/python-final.log`；独立 reviewer 另跑其子集 **70 passed**，不叠加计数。
- Playwright 三份专用 spec 的定向组合 **6 passed**，最终命令显式设置 `E2E_EXISTING_ACCOUNT_ONLY=1`，逐例结果见 `reports/r1-boundaries/e2e-final-existing-only.log`：本轮新增受控5项（409、422、断线重连、零字节/503/下载失败、第二输出失败）；复用真实原 PDF 下载与390px 1项。初次受控测试固定装置的 `New chat` 查找错误已修；第二输出断言曾错误比对 Markdown 原文与渲染文字，修断言后通过；产物下载测试曾选中禁用按钮，改为就绪按钮后通过。这些初次失败不算产品通过。
- 后续真实服务后台/会话切换 E2E **1 passed**，见 `reports/r1-boundaries/background-switch-real.log`；内置浏览器同 run 往返及只读数据库前后对账见 `background-switch-ledger-compare.json`。另用**不带账号模式开关**的默认设置运行1项真实本地 E2E 通过，5个既有测试账号的创建、密码变更时间与角色均未再变化，见 `e2e-default-existing.log` 与 `e2e-default-no-account-writes.json`。
- 新增受控 fake-fetch Node 测试 **4 passed**：默认、漏设旧开关、缺凭据/拒绝/强制改密均无账号管理写请求；显式初始化对已有账号在写前停止，fresh 路径只打到假 API，未修改真实账号。
- 真实内置浏览器：普通成员引用预览/撤权、自然到期审批刷新/重开、原 run 参数与快照对照均通过；`approval-after.json` 在刷新后记载同 session **run count=1、execution count=0**。
- 直接 app/node TS、全 web lint、i18n、前端 build+bundle budget、Ruff、architecture（609文件/0违规）、harness、diff-check 通过；`make validate` 与 `make status` 退出0，validate 有1条非失败 warning。Node 24.14.0 与项目期望22有 engine warning。
- Compose 的 Gateway、Frontend、Runtime、Knowledge 工作目录标签均为本仓库。按规范 Gateway 与 Frontend 分别 hot-update；3个改动 Python 文件在 Gateway 容器 SHA一致，Web 构建除运行时注入的 `runtime-config.js` 外 **255/255** 文件 SHA一致。旧带 hash 资源仍留在容器，不影响当前 `index.html` 引用。
- 账号默认模式和后台切换的追加差异仅在本地 E2E 脚本、配置、测试及文档；不进入 Gateway/Frontend 镜像或静态 bundle，因此没有对 Docker 进行第二次无关热更新。追加后复查 Compose 标签、`make status` 和 `make validate` 通过，容器继续运行上述应用构建。

## 独立 review 与剩余边界

`closure_review` 对 RP04 最终代码 **PASS，无确认 blocker/high**；独立核对撤权白名单、owner 范围、同 run 参数、文档 ACL、零字节和不重放。Reviewer 未操作浏览器/Docker；实机结论仅由上述主代理验收承担。

本轮**不声明完整 R1 完成**：图片生成正例仍被 DashScope OSS CDN Fake-IP 解析阻塞，安全下载拒绝（沿用 RP03 的单域名方案，未改共享网络/SSRF）。后台/会话切换复用既有已结束的真实 run；**运行中同时断网、后台切回与会话切换的组合**未作为新的真实 provider 长任务测试。其余上述边界按真实/注入层级闭合。合成 viewer grant 已撤销，记忆测试事实已清除。

验收脚本副作用：本轮较早的 Playwright 命令漏设 `E2E_EXISTING_ACCOUNT_ONLY=1`，当时的 `global.setup.ts` 对5个既有 `model_tester` 测试账号执行密码重置及既有角色回写。数据库只读核查确认这5个账号均早于本轮创建，角色仍为 `model_tester`，密码变更时间在本日；证据在 `reports/r1-boundaries/e2e-account-side-effect.json`。没有新建账号、没有输出密码；专用 E2E 登录仍有效。原密码无法从当前记录恢复，不能把这一点记作“无凭据变更”。**本轮已在代码层将默认值改为现存账号只读模式**，并完成上述无开关实测与受控无写请求测试；此前5个账号的凭据变更事实仍未回滚。
