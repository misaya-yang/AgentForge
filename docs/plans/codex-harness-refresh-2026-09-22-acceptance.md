# Codex 更新：兼容矩阵与证据合同

- status: completed_local_scope（完整矩阵仍含未验证项）
- domain_id: agent-runtime-upstream-sync
- owner: 升级主实现 session
- last_verified: 2026-09-22
- successor: none
- prerequisite: 本次本地收口结果见[实现报告](../../reports/architecture/codex-harness-refresh-2026-09-22/implementation-verification.md)；本矩阵保留完整设计要求，不是逐项PASS记录。

所有候选证据必须绑定平台commit、upstream SHA、overlay digest、schema/DB epoch、实际运行镜像digest、平台/架构、脱敏模型身份、case版本、时间、命令与退出码。命令成功但未执行目标scenario、全部skip、沿用旧commit结果，都不满足目标版本验收。

## 1. 必测矩阵

| ID / owner | 场景与触发 | 必须观察到的结果 | 证据层 |
| --- | --- | --- | --- |
| C01 / U1 | 目标archive + 107 overlay分类；篡改旧blob、schema、Worker tag | 已审目标快照、所有变更有处置；混用任一旧材料拒绝启动 | source + Docker镜像负测 |
| C02 / U2 | 经Gateway创建root/turn，同时模拟先到模型调用 | scope、snapshot、lease已durable；tenant/user不匹配在provider和trace写前拒绝 | Rust/PG + HTTP |
| C03 / U2,U3 | start/resume/fork/subagent + `none`/空名单/function-only/native-search显式授权 | ToolPolicy、model-wire、Worker三层上限一致；恢复、子任务、hooks不得放宽 | 两种wire受控fixture + 真实工具任务 |
| C04 / U2,U3 | 两namespace同名工具；code-mode yield/wait；MCP结果历史重放 | tool/call/originating item身份可往返，无误路由或双记账 | Rust + provider wire + CLI |
| C05 / U2 | approve/deny/过期/参数改动/跨tenant proof/重发dispatch | 拒绝不执行；同一写操作最多一次；发送后不确定标unknown，不伪造success | PG + Worker + 浏览器 |
| C06 / U2,U3 | 父子run交错结束、queued message期间evict/resume、根容量=1 | 子终态不结束父；已接受消息可恢复；总预算与run/lease释放正确 | Codex依赖测试 + PG + HTTP |
| C07 / U2 | 旧会话读取、compaction后resume/fork、steered输入后进程崩溃 | 用户新输入、授权证据、工具结果、模型身份不丢；append/flush/shutdown故障显式 | PG crash/restart + 长任务 |
| C08 / U2 | 附件读取、fork、删除、跨tenant引用、并发重复删除 | 已有附件/引用可用；原生新API按声明处理；membership独立、ACL/幂等成立 | 隔离DB/对象库 + UI |
| C09 / U3 | whole-thread两turn、per-turn目标、150+事件terminal、重连重复cursor | 事件不漏不重放副作用；whole-thread不提前关闭；账本先完成、composer解除锁 | SSE受控故障 + 浏览器 |
| C10 / U2,U4 | Python大量小文件、多文件/fallocate、超时/取消、附件输入无输出 | 磁盘/inode/process受硬限，workspace清理后才还slot；输入不变成新artifact | 实际Linux隔离 + 浏览器 |
| C11 / U3 | Responses/Chat-only多轮、stream断连、工具参数超限、慢消费者 | 合法流完整；有界内存/背压；断连abort；唯一失败终态；unknown不重试 | adapter/CLI + 真实两wire |
| C12 / U3,U4 | Assistant聊天/刷新、Agent Studio预览/版本、审批拒绝/同意/取消 | 与旧公开OpenAPI和SSE兼容；错误可处理，409有明确指引，无UI假成功 | OpenAPI + SDK + 实际Web |
| C13 / U3,U4 | KB上传/解析/索引/混合召回/引用；坏配置/embedding失败/目录分页 | 内容可用且引用正确；失败保留可用旧generation；201库/10001段无静默截断 | KB direct/PG + 真实provider/UI |
| C14 / U3,U5 | Eval同tenant用户、foreign/inactive用户、失效claim回写、正常与低预算失败case | 身份正确、owner fencing、真实model span/token/cost；失败如实显示；无虚拟主体越权 | PG + HTTP + 对比页 |
| C15 / U3,U6 | Python/TS/Java/Dart SSE；native CLI独立home/provider，Gateway不可用 | 支持客户端可读旧/新事件；独立CLI不依赖Gateway；npm制品含ELF/许可证/来源 | 各语言测试 + 真实native执行 |
| C16 / U4,U6 | 完整旧→新→旧→新、历史session/active写dispatch、DB epoch边界 | 镜像与DB兼容；旧历史可读；执行账本无重复写；无材料时阻断相应发布 | 冻结release unit演练 |
| C17 / U5 | 固定模型/采样/任务的冷/热配对性能与质量集 | 达到U0冻结指标；报告分布、成本、错误、资源；证据不足不提升baseline | 重复实际任务、正式Eval |
| C18 / U6 | fresh-machine安装、Linux amd64/arm64、已承诺native发行目标 | 可拉取或按公开source-build路径构建；digest/架构正确；旧客户端可连 | 独立机器 + registry + smoke |
| C19 / U2,U3 | 所有目标feature/config默认变化；新Guardian/插件/hook入口 | 未授权能力不可用；已授权能力不丢；Guardian不能覆盖拒绝/租户政策；不得读取宿主凭据 | 差异清单 + 容器隔离负测 |
| C20 / U1,U6 | hosted fmt/check/实际changed crates；旧源schema与目标schema对比 | 真正编译并执行目标及平台crate；新增生命周期事件逐项映射或显式拒绝 | hosted Rust CI + schema fixture |

Responses和Chat-only分别覆盖正向文本、工具与错误终态；权限边界覆盖至少两个租户、两个用户、root/child/fork，以及有权限、无权限、失效授权。不能只测试默认Qwen短回答后宣称完整兼容。

原生新增thread attachment、Guardian自动审查、Apps、hooks、远端Agent backend等并非现有公开产品承诺；默认不开放但必须证明升级未改变现有行为。若在实现中决定开放，其对应正常、权限、历史、故障和费用用例立即变成必需项，不允许借“可选”跳过已开放能力。

## 2. 复用门禁及执行环境

以下是现有入口，执行前按Makefile及脚本核对所需变量；该列表不声称现有测试已经覆盖上面全部新情形。缺失case补到对应owner与命名位置，Playwright统一放 `web/e2e/`。

| 范围 | 命令 | 限制与PASS含义 |
| --- | --- | --- |
| 文档 | `make harness-check`、`git diff --check` | 仅结构/链接/格式 |
| 源码材料 | `make agent-runtime-source-contract` | lock/receipt/manifest及负测；不是运行镜像证明 |
| U2 source-only材料 | `python3 scripts/harness/agent_runtime_supply_chain.py validate --repo-root . --lock deploy/agent-runtime-source/lock.json` | `local_source_locked`且无可运行镜像；不加`--require-artifact`。U4成对构建后才用上一行完整门禁 |
| Runtime/Worker合同 | `make agent-runtime-release-gate` | 离线组合门禁，不是live/R100 |
| Rust目标源码 | `make rust-changed-crate-gate BASE_SHA=<U0平台基线>` | 仅hosted CI；必须列出实际selected crates，含新增平台crate及上游受影响闭包 |
| 租户/状态/模型/事件 | `uv run --all-packages --extra test pytest -q --no-cov tests/services/agent_runtime tests/api/test_agent_runtime_api.py tests/api/test_agent_runtime_idempotency.py tests/services/eval` | 补新版本case；DB相关不能用mock替代 |
| 架构/公开合同 | `make architecture-boundary-gate`、`make core-boundary-gate`、`make single-instance-guard`、`make verify-openapi-contract`、`make hygiene-check`、`make loc-no-growth-gate` | 所有required结果必须为success；不能重置baseline掩盖新增问题 |
| Web/SDK/CLI | `make web-quality-gate`、`pnpm -C web build`、`make sdk-sse-contract`、`make independent-cli-gate` | 发行环境Java/Dart等必需运行，不接受缺工具skip |
| Studio/Eval/KB | `make verify-agent-studio`、`make agent-eval-core-gate`、`make eval-e1-gate`、`make kb-unit-gate`、`make kb-migration-gate` | fixture证明fixture；真实质量/权限用额外PG/HTTP证据 |
| 构建候选 | `AI_PLATFORM_AGENT_RUNTIME_SOURCE=<只读上游源> bash scripts/rust/build-update.sh --artifact all` | 实现更新完lock/overlay后Docker串行编译；构建产物不是部署 |
| 隔离活体验证 | `make agent-runtime-smoke`、`make agent-capability-worker-smoke`、`make agent-thread-store-contract`、`make agent-runtime-text-gate` | 按脚本提供候选镜像/source/env；必须为本候选，不是旧栈 |
| 当前栈 | `make validate`、`make status`；浏览器执行C05/C09/C10/C12/C13/C14 | 先读runtime-and-secrets、核对Compose owner、实际source/image/DB；不是只检查health |
| 完整回滚 | `make agent-runtime-rollback-rehearsal` | 完整冻结物料、无跨epoch非法回退；缺材料记BLOCKED，不能以pair-only代替 |

宿主不运行Cargo/rustc/rustfmt/clippy。所有Docker、migrations、E2E/provider全局串行；受控本地验收可复用专用E2E账户，凭据仅执行时读取且不进入报告。公共发行和共享数据操作另按执行授权处理。

## 3. 结果与停止条件

每个C项的receipt字段：`requirement_id`、`candidate_identity`、`command`、`exit_code`、`case_ids`、`expected`、`observed`、`pass/fail/skip/blocked`、`scope`、`artifact_refs`。恢复/副作用case附前后账本计数及唯一键，性能case附原始测量与计算方法；公开报告只保留合成输入和脱敏摘要。

- C80：U1～U4直接/隔离通过，C01～C14及C19核心live通过，且有实证可执行回滚。不能因为外部发行矩阵暂缺而推迟第一个可运行候选；不能因此跳过其安全与数据门槛。
- R100：C01～C20在已承诺支持范围内全部证据齐全，包括U5性能质量、真实两wire/CLI、目标架构、fresh-machine及完整回滚。Darwin/Windows若尚未承诺发行，保留后续计划并限制对外声明；若本次要宣布支持，就必须增加并通过对应发行矩阵。
- 任一租户越界、权限放宽、历史损失、重复副作用、无界资源、unknown当success或目标制品身份不符，停止放量并修复；不通过提示词降低任务要求来过关。
- 基线CI失败、历史缺失物料和新目标回归分别记账。本次设计检查通过不能将queued工作包改成verified。
