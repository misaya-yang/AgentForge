# R3 固定 Agent Version 本地实机补验（2026-09-28）

环境：本仓库所属 Compose，`make status` 全部健康；已有专用 E2E 账号只在执行进程读取。Gateway 与前端分别用 `make hot-update ARGS=--gateway`、`--frontend` 更新；数据库 authority `make migrate` 应用 epoch 8，`make migrate-status` 确认 1～8 均已登记。以下运行均为本地真实 Runtime/provider 路径；自动化和 Codex 内置浏览器另列，不能互相代替。

## 真实链路与发现

| 阶段 | 事实 | 处理 |
| --- | --- | --- |
| 固定合成 KB 五样本、Agent Version A 初跑 | run `933462c1-25f7-41fc-a123-9754aa6af4d2` 五例均在 V2 Thread 创建前失败，HTTP 500，Trace 0；后端约束 `sessions_agent_runtime_shape_check` 拒绝预发布 Version Preview | epoch 8 前向迁移加入互斥的 Preview Version pin 形态，保留草稿/发布/builtin 形态；旧 run 终态失败留存，未重发 |
| 迁移后新 run | run `849bd192-0473-45a6-bb16-d135d009eeb7` 五例在 V2 Turn 创建前 HTTP 409，Trace 0，错误码 `AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_THREAD_RECREATE_REQUIRED` | 创建 Thread 时固定相同 Version 的能力 allowlist；旧 Thread 不被复用，新 run 独立创建 |
| 能力修复后低预算 A/B | A `d826a403-79db-409d-af0e-cee1de8e5398`、B `fadab1c9-7241-4dad-a517-b269a7efd490` 均进入真实模型；各有 4/5 执行失败、1/5 执行成功，失败模型调用状态 `unknown/stream_interrupted`。公开结果显示未评分/不可用 gate，不冒充满分；A/B 比较标记不可归因 | 使用更高输出预算建立新不可变 Version，保留这两个失败 run；不把 provider 未知执行自动重试 |
| 高预算 A/B | A `6a207aad-db66-40c9-91ab-ca136c3b253f`：3/5 执行成功、2/5 `stream_interrupted`，run 失败；B `173bbd09-a3e9-4b8a-9ffe-7ff4a05e9a90`：5/5 实际执行成功，预设 `R3_OK` 文本行为断言 0/5，质量 gate 失败。两版的模型/KB/版本指纹均在 run 回读；比较将失败样本与缺少指纹标为不可归因 | 当前结果是拒绝发布的有效证据，不能宣称五类行为或 Agent A/B 质量通过；测试用例断言和 provider 稳定性需另行核验 |
| 选定失败样本重试 | 对高预算 A 的一个失败 case 显式提交 `acknowledge_replay=true` 和唯一幂等键，API 返回 HTTP 409，无新 run；数据集带 KB 来源，当前安全合同不允许在未重新验证来源授权时重放 | 拒绝保持原 run/trace，不把未知 provider 调用自动重试；这是负向安全验收，不算选定重试成功 |
| 已发布会话存在时回滚 | 首次 J15 Docker 浏览器运行在已有 v2 Hosted 会话后回滚 v1，API HTTP 500、指针仍 v2；DB 外键 `sessions_runtime_publication_identity_fk` 错把历史会话版本绑定到可变的当前指针 | epoch 10 改四张运行事实表引用稳定发布身份，同时保留各自 Version 外键；`make migrate-status` 确认 epoch 8～10。再次运行同一 J15 用例 **1/1 通过**（22.5 秒）：旧 v2 会话保留，新会话读回 v1，回滚事件/指针一致 |

本轮 Agent 配置含已有私有合成 KB `kb_34d201bc655345e3b204e94f7c8d38ec`、模型 `qwen3.8-flash`、只读知识能力；五个固定问题都指向这份合成资料。没有为评测运行创建用户账号、公开发布或调用写工具。更高预算后仍有 `stream_interrupted`；该状态保持未知，不自动重放。

## 浏览器与自动化

- Docker Playwright 真实 J14：`keeps an r1 Preview run pinned while saving r2 and restoring after refresh` 初次因旧 r1 长任务占用时“预览当前草稿”被禁用 90 秒失败；修复为可在旧预览运行时创建独立 r2 会话，保留旧 run、审批与发送保护。更新前端后同一测试 **1/1 通过**（约 1 分钟）。
- 该刷新测试的旧 run `dce4cb72-3a46-4590-b256-b9fb045b00fe` 后续在清理临时 Agent 时终止；DB 回读为 run=`failed`、Trace=`failed`，说明普通断线后的后台终态重读实际写入同一 Trace。Gateway 进程重启期间的持久补偿仍未覆盖。
- 首轮 `make verify-agent-studio` 到 AS-05 前端静态门暴露新增文案缺少 i18n keys；已中断本轮门禁，补齐后必须完整重跑，不能计通过。
- 独立 reviewer 在 epoch 8 检出 SQL `CHECK` 对全空 Preview 目标的 `UNKNOWN` 放行；epoch 9 前向修复显式非空和互斥条件。隔离 authority 测试与本地 `make migrate-status` 均已核对；epoch 8 文件保持不变。
- 独立 reviewer 发现 linked publish 曾可把数据集 B 的 run 关联到数据集 A 的 release eval、Web 发布页未传 selected run、脱离 SSE 后可能缺终态 Trace。前两项已修，后者增加同一持久事件游标的后台只读终态重放与定向测试；Gateway 本身重启前的 Trace 缺口仍需单独标明。
- Codex 内置浏览器当前位于登录页；浏览器安全策略拒绝本地凭据文件导航，未绕过。已有账号的普通 Docker Playwright 与 API 登录成功，不等于内置浏览器登录验收通过。

真实 provider 的执行成功与质量断言通过是两回事。上述样本不足以证明通用质量、权限撤销/工具拒绝或完整 J16～J18。

## 最终检查与 review

| 检查 | 实际结果 |
| --- | --- |
| 受影响 Python API、Runtime、Eval、DB/迁移聚焦回归 | `uv run --all-packages --extra test pytest -q --no-cov` 指定 11 个相关文件：**200/200 通过** |
| Agent Studio 聚合 | `make verify-agent-studio`：**40/40 门禁通过**，`source_stable=True`；结果在 `reports/agent-studio/agent-studio-regression-v1-result.json`。前一轮 i18n 缺 key 后中断，不计通过 |
| Eval | `make verify-eval-dev` 退出 0（分组 66、139、204、8、18 均通过）；`make agent-eval-core-gate` **108/108**；`make agent-runtime-eval-contract-gate` **22/22**；`make eval-e1-gate` **204/204** |
| 规范与本地栈 | `make harness-check`、`make architecture-boundary-gate`、`make verify-openapi-contract` 全部退出 0；`make migrate-status` 到 epoch 10；最终 `make status` 所有服务健康。Gateway Trace 源码与容器、前端 `dist/index.html` 与容器哈希一致 |
| 真实 Docker 浏览器 | J14 r1 运行中保存 r2/刷新/独立新预览 **1/1**；J15 旧 v2 Hosted 会话、回滚 v1、旧/新会话版本核对 **1/1**。这些是 Playwright 操作本地 Docker，不是 Codex IAB |

独立只读 reviewer 首轮指出发布数据集错配、Web 发布页未传 selected run、断线 Trace 缺口；复核后这三项普通路径已修。第二轮指出 epoch 8 的 NULL/UNKNOWN CHECK 漏洞，epoch 9 修复；实机 J15 发现历史会话阻断回滚，epoch 10 修复。reviewer 复核 epoch 9/10 未见新的合入硬问题，静态结论不代替实机。

剩余边界：Gateway 冷重启后缺少持久 Trace 补写；本轮真实 A/B 质量 gate 未通过，无法把它们作为发布依据；裁判故障、权限撤销/工具拒绝五类合成用例、双角色与公开受众矩阵、Codex IAB 认证后交互未实际通过。因此当前交付是可合入安全增量，**完整 R3 不标完成**。
