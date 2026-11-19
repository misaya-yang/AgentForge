# R3 Agent Studio 与 Eval 闭环

范围：PRD `04-agent-studio-evaluation.md` 的 AG-01～AG-09、EV-01～EV-08；验收 J14～J18。复用 R1 助手审批/恢复、R2 权威 Eval 样本库与来源权限。`loop-state.json` 是进度状态权威。

2026-09-29 收尾在 `codex/r3-agent-eval-closeout-20260929` 串行执行，基线
`561f5f5a`。用户要求 J14～J18 全部验收后提交并普通推送；本轮不合入 main。
最新结果见 [完整收尾记录](../../../reports/product/r3-agent-eval-closeout-2026-09-29.md)。
下述并行开发和 main 合入授权是 2026-09-28 的历史记录。

## 并行代码流与归属

用户已明确授权本轮 A/B/C 并行开发；三流只写各自路径，主代理负责公共接口、集成、Docker/数据库/浏览器与跨流 review，不另造 Agent 执行引擎。

| 流 | 结果 | 独占主要路径 |
| --- | --- | --- |
| A | 草稿修订、发布/回滚、授权与受众合同 | `src/api/v1/agents.py`、`agent_public.py`、`agent_repository.py` |
| B | Eval 样本/运行快照、三层状态、A/B比较与安全重试 | `src/api/v1/eval.py`、`_eval_dataset_routes.py`、`ai_gateway_core/eval/`、Eval 主页面 |
| C | Studio 预览与已发布入口、Trace/反馈跳转 | `web/src/pages/agents/`、`agent-public/`、Runtime preview route、`trace_feedback.py` |

共享 API/schema、迁移、路由注册、文档与验收由主代理串行修改。每流先复现差距，再做最小实现和定向测试，完成后交叉只读 review。

## 证据层

先用合成知识内部 Agent、固定 A/B 候选与五类用例串通 J14～J18，再核 P0/P1。自动化、受控故障注入、真实 Docker/API、内置浏览器分别记录；缺少第二租户身份和内置浏览器登录态仅挂起依赖场景。R2 的待验项保留在其账本，不因 R3 工作被标通过。2026-09-28 用户追加授权安全收口后提交、合入 `main` 并普通推送；不删除分支。
