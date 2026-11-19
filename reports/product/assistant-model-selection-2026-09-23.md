# 助手模型列表与主模型追加需求验收（2026-09-23）

## 解释与改动

本次将“激活过的模型”落实为两项现有事实：模型目录 `is_enabled=true`（助手模型 API 过滤，缺失启用标记也不放行）且模型所属供应商在当前租户 `available_providers` 中已配置。前端模型菜单只展示两项都满足的模型；目录加载前缓存选择仅用于显示，发送需等待当前目录验证。此判断不把“配置了密钥”夸称为每个模型已经实时调用成功。

新会话优先选择 `qwen3.8-flash`；旧会话继续恢复其保存的可用模型，用户手动选取的可用模型保持偏好。过去的 v1 浏览器缓存既记录手选也自动写入旧默认值，无法区分；新偏好使用 v2 键，使旧自动保存的 `qwen3.7-plus` 不遮盖新默认。部署默认值同步更新到 `.env.example`、Compose fallback、Python Settings、DashScope 初始模型目录和本地忽略的 `.env`，未打印或提交 `.env` 内容。

## 实测

| 证据类型 | 结果 |
| --- | --- |
| 本地 API | 既有租户目录 22 条、`is_enabled=true` 22 条；已配置供应商为 Anthropic、DashScope、DeepSeek。`qwen3.8-flash` 属于已启用的 DashScope 模型，更新后 `/api/v1/assistant/config` 默认值为 `qwen3.8-flash`。 |
| Codex 内置浏览器 | 新会话按钮显示 Qwen 3.8 Flash；菜单 13 条，只包含上述已配置供应商，未显示未配置的 Google/OpenAI 组。旧 Qwen 3.7 Plus 会话刷新仍显示旧模型，新会话回到 Qwen 3.8 Flash。用新默认模型真实发送短消息，看到运行中、最终正文 `AS02_QWEN38_OK` 和成功终态；刷新后仍一致，控制台无相关错误。截图在 `tmp/as02/`。 |
| 自动化 | `modelSelection.test.ts`、`lastModel.test.ts` 连同 AS-02 outcome 测试共 11 passed；`assistant-model-selection.spec.ts` 在禁用全局 setup 的安全临时配置下 3 passed，含真实目录对照、受控目录过滤与手选偏好、旧会话模型恢复。TypeScript app 和 node 检查、相关 ESLint、Python provider/setup 测试 22 passed、Ruff、i18n、前端 build 均通过。 |
| 运行时 | 当前 Compose 属于本仓库；使用 `make deploy-app ARGS="--no-migrate"` 使新本地默认值进入 gateway 容器，再 `make hot-update ARGS="--all"` 和前端补丁的 `--frontend`。`make validate`、`make status`、`make validate-example-config`、`make harness-check` 通过。 |

与 AS-02 合并做的最终定向核验为：安全配置下 6 条 Playwright 通过，相关 Python 测试 33 passed，Node 测试 14 passed，TypeScript app/node、相关 ESLint/Ruff 和 harness 检查通过。
之后针对“缺失启用标记仍被视为激活”的边界收紧 Gateway 投影，并单独运行助手控制面路由测试 24 passed；缓存模型在目录核验前不再可发送，TypeScript app 和相关 ESLint 通过。最终再次 `make hot-update ARGS="--all"`，内置浏览器确认新模型页面和 13 项菜单无相关错误；安全配置下 6 条 Playwright 复跑通过，`make validate`、`make status` 和真实 Runtime 历史只读查询通过。

## 边界

- 本次只收窄助手选择器与默认模型；未批量关闭模型目录记录，也未改变其它控制台的模型列表。已配置供应商的凭据实时有效性仍由实际调用验证；本次真实调用仅验了 Qwen 3.8 Flash。
- DashScope 目录参数按当前本地已启用 `qwen3.8-flash` 记录写入模板；新部署模型可用性仍受供应商配置与网络影响。
- 首次 AS-02 Playwright 历史回归使用仓库默认全局 setup，触发其本地 model tester 账号创建/重置流程。后续模型与 AS-02 定向测试改用 `tmp/as02/playwright.safe.config.ts`，不调用该 setup；没有删除或再次修改那些账号。
