# R1-A / AS-02 实施与本地验收记录（2026-09-23）

## 范围与根因

本包只处理 AI 助手失败提示与历史恢复。开始时 `main` 已有未提交的 `docs/README.md`、本 PRD 目录和 `reports/product/`；原有工作未清理。本包在 `codex/as02-assistant-failure-history` 开发，未提交、推送或合并。

原 `ChatMessage` 的无正文分支只认取消，否则显示“无回复”；有正文时没有失败提示。历史恢复虽将 `process_summary.status=failed` 映射成失败状态，但展示层没有使用。Runtime 历史 SQL 对已形成 `agent_message` 的部分失败遗漏终态事件，也过滤了成功空正文的终态记录。

## 修复

- 消息展示明确区分运行中、成功、失败、取消、结果未知、成功无正文；失败及未知提供影响、人工下一步和可复制的 UUID run ID。取消提示不保证此前外部动作无副作用；未知提示先核查目标系统，均不自动重试。
- 实时 `side_effect_unknown` 与 `run_error.terminal_envelope.exit_reason=side_effect_unknown` 保持未知；历史 `outcome_uncertain` 恢复同一分类。原始异常文本不用于用户提示；部分正文保留。
- Runtime 历史投影在两条 assistant 路径包含 `run_finished`、`run_error`、`cancelled` 和 `side_effect_unknown`，允许终态空正文进入历史；写出 `metadata.runtime_events` 前把终态数据收窄到状态与受控的未知原因，不转发原始错误。
- 历史查看只读，不建立 run；现有审批和其它模块流程没有改动。

## 验证结果

| 类型 | 具体场景与结果 |
| --- | --- |
| 真实链路，Codex 内置浏览器 | 本地 Qwen 页面看到“运行中”后，收到 `AS02_REAL_SUCCESS_OK` 和“已完成”。另一轮主动点击“停止生成”，刷新及重开历史后仍显示“已取消”，历史中已有的部分正文保留。旧真实会话的失败、取消、成功历史在刷新和重开后分类可见。浏览器页面非空、标题与 URL 正确，无相关控制台错误或框架错误覆盖层。 |
| 受控持久消息注入 | 通过专用 E2E 账号向本地会话写入六类消息：首 token 前失败、部分正文后失败、取消、成功空正文、正常成功、结果未知。内置浏览器初看、刷新、重开均显示对应状态；部分正文保留。注入了假内部错误字样用于 UI 隐藏检查，页面未显示。此数据走 legacy 会话写入接口，不等同真实 Runtime 执行。 |
| 无自动执行 | 受控会话打开、刷新、重开前后，`assistant_runs` 总数为 1915；真实两轮执行后为 1917。新增 Playwright 回归还监听 V2 建线程/启动 turn 的 POST，历史查看、刷新、重开期间计数为 0。 |
| 真实 Runtime 历史 SQL | 后端热更新后，已有真实会话的 `/api/v1/assistant/sessions/{id}/history` 查询成功，返回 8 条 assistant 消息和 8 个终态事件；终态数据只包含白名单字段。此检查实际执行了 PostgreSQL 投影查询。 |
| 自动化 | `tests/services/agent_runtime/test_thread_store.py` 11 passed；前端 outcome/status Node 测试 6 passed；`web/e2e/assistant-history.spec.ts` 3 passed（含六状态刷新、重开、剪贴板复制与不发起新 turn）；TypeScript app 检查、相关 ESLint、Python Ruff、i18n 检查、前端 build 均通过。最终 E2E 通过 `tmp/as02/playwright.safe.config.ts` 禁用仓库全局 setup 运行。 |
| 本地 Docker | 容器 Compose `working_dir` 为本仓库；`make doctor`、`make validate-config`、`make validate`、`make status` 通过；`make hot-update ARGS="--all"` 和后续 `--gateway` 成功，核对容器中本次源码和前端资源。 |

## 运行边界与未验证项

- `make deploy` 的迁移步骤因既有 baseline manifest 的 `source_git_sha` 来源校验失败而中止。`make migrate-status` 显示基线已采纳、旧迁移待处理数为 0；随后使用仓库提供的 `make deploy-app ARGS="--no-migrate"` 启动并热更新，运行时校验通过。未修改基线或迁移。
- 真实 Runtime 没有稳定产出“成功但无正文”的样本；本次以受控持久消息的浏览器检查和 Runtime 投影单元回归验证该状态。Python 投影测试使用预造数据库行并断言 SQL 文本，真实 PostgreSQL 查询已在其它终态记录上执行，但没有直接命中成功空正文分支。
- 首 token 前失败、部分正文失败和外部结果未知使用受控持久消息检查最终页面；没有人为触发真实外部工具的不确定副作用。真实主动取消及其历史恢复已实测。
- Docker Desktop 配置为 3 GiB，`make doctor` 给出 4 GiB 建议值；本次服务健康检查通过，未运行 Rust 编译或镜像重建。
- 初次通过仓库现有 `playwright.live.config.ts` 运行的历史测试会执行 `global.setup.ts`，该 setup 无条件对 5 个本地 model tester 账号调用创建或重置流程。发现后改用临时安全配置禁用该 setup，最终 3 条历史测试重新通过；未对账号做额外清理或变更。此为验收 harness 的范围外副作用，不能把初次运行描述为“只用了专用 E2E 账号”。

最终合并核验同时覆盖追加模型需求：安全配置下 6 条 Playwright 通过，相关 Python 测试合计 33 passed，Node 测试 14 passed，TypeScript app/node、相关 ESLint/Ruff 与 `make harness-check` 通过。

## 独立 review

只读 reviewer 发现成功空正文 Runtime 历史被过滤，以及终态原始错误可能随 `runtime_events` 返回客户端；两项均已修正并回归。reviewer 复核修正后未发现阻塞问题；还提出 `status` 若为非字符串可能导致历史解析失败，已加类型守卫。最终差异和本记录的复核结论以最终 reviewer 回执为准。
