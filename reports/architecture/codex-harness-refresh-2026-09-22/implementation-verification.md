# 2026-09-22 Codex 内核更新：实现与本地构建验收

目标内核的 Runtime、Capability Worker 已在 Docker 内编译、生成本地镜像并启动；最终 Gateway 热更新和源码身份复核已完成。**这份记录证明构建、制品身份、离线合同和本地运行健康，不代表全部兼容矩阵或 RELEASE_100 已完成。** LOC no-growth 门禁仍未通过；浏览器与真实 API 的结果见文末。

本报告读取实际构建日志、最终 [source lock](../../../deploy/agent-runtime-source/lock.json)、[source receipt](../../../deploy/agent-runtime-source/source-receipt.json) 和 [live-source-probe.json](live-source-probe.json)。源码探针时间为 `2026-09-22T16:26:45.584362+00:00`。中间 checkpoint 内的旧 overlay/image digest 不作为最终身份。原始日志保留于本地 `tmp/codex-refresh-integration/`；以下每个通过结论均限定在对应日志实际执行的范围。

## 最终源码与制品身份

| 项目 | 最终实际值 |
| --- | --- |
| Upstream commit | `279ba894152b2c01c5294cc0723b463b209bdca4` |
| Upstream Git tree | `0d32f51cd9fd27839dcaf93e435e855a6880f154` |
| Overlay | 119 文件；`5dc36c43638a48bc63e7069d4513ccc3c4f45ad9d930092c88c4a4a75c3b2e85` |
| Overlay manifest | `ai-platform/agent-runtime-overlay/v2`；`4de951a5d56c9aa386bfb35bfc46daeb77382e793d25beb329bddc8fe7af9880` |
| Docker 解析后 Cargo.lock | `ca24c78377d16f522d7c0233a81045f880fe981f6c1127f779d08432c7e0fea8` |
| App Server schema | 1,054 文件，4,017,931 bytes；`0bbd1819a6f910bd40f4143022897a502156d26011736e9f0455476b6d2492ab` |
| Capability schema | `b2b6a1ae71f054387b16daecc0325faf0acb09be7a6119adbfe3227cbfb945a7` |
| Docker Cargo | `cargo 1.95.0 (f2d3ce0bd 2026-03-21)` |
| Docker rustc | `rustc 1.95.0 (59807616e 2026-04-14)` |
| Source receipt SHA-256 | `238f8e1d5082baf8643567beda66c6192d87ca94b935e3e96e40ace0758f6f73` |
| Release state / 构建 profile | `local_image_locked` / `dev-small`，平台 `linux/arm64` |

| 制品 | 本地 tag | Lock 中的 image digest / 启动资格 |
| --- | --- | --- |
| Runtime | `ai-gateway-agent-runtime:local-279ba894152b-5dc36c43638a` | `sha256:1555865da7494318adfb12eecde2682e606c891d1162b684450608657ef2b3ca`；`candidate_start_allowed=true` |
| Capability Worker | `ai-gateway-agent-capability-worker:local-279ba894152b-5dc36c43638a` | `sha256:2152148bf14fb8716bb10a0a86a525ac425199276bb9b942f9790224a878ba13`；`candidate_start_allowed=true` |
| 独立 App Server 制品 | 未记录镜像 | digest/ref 均为 `null`；`candidate_start_allowed=false`，不能借用 Runtime 的通过结论 |

最终 [Runtime image 日志](../../../tmp/codex-refresh-integration/runtime-image-final.log) 与 [Worker image 日志](../../../tmp/codex-refresh-integration/worker-image-final.log) 的 export digest、tag 与上表一致，均结束于 `agent-runtime-source-contract: ok`。这些是本地构建身份，不是推送、跨架构发行或独立 CLI 制品验收。

## Docker 编译与生成证据

| 执行范围 | 实际结果与证据 |
| --- | --- |
| Runtime 二进制 | [runtime-compile-03.log](../../../tmp/codex-refresh-integration/runtime-compile-03.log) 执行 `cargo build --locked --profile dev-small -p ai-platform-agent-runtime --bin ai-platform-agent-runtime`，日志出现 `Finished`，后续 source-evidence export 完成。最终 image build 复用对应 builder 缓存。 |
| Worker 及辅助二进制 | [worker-compile-02.log](../../../tmp/codex-refresh-integration/worker-compile-02.log) 执行 `cargo build --locked --profile dev-small -p ai-platform-capability-worker`，包含 worker、health、python-supervisor 三个 bin；`Finished` 后复制三个产物完成。 |
| 真实协议 schema 与依赖闭包 | [source-evidence-final.log](../../../tmp/codex-refresh-integration/source-evidence-final.log) 使用生产 `Dockerfile.runtime` 的 `source-evidence-export`，执行编译后的 `ai_platform_export_schema` example、`cargo metadata --locked --format-version 1`，导出 schema、Cargo.lock 和工具链版本。 |
| 生成物来源 | [source receipt](../../../deploy/agent-runtime-source/source-receipt.json) 记录 `runtime_compiled=true`、`schema_generated_by_compiled_protocol_library=true`；metadata 文件 SHA-256 为 `73be523d1fe7c339f3c2e82d79932ce275a5efb2644ab852fe3d0e8964dca782`。导出的 Cargo.lock 与最终 overlay lock 的 hash 相同。 |
| SBOM | 同一份 Docker metadata 生成总 SBOM 1,626 components，hash `525cb5fc3f271e6760e1a5da96d55140d2ea5aba016e940bd1ae77b5eb678a94`；Worker SBOM 619 components，hash `5e42d03814730efa95f0f37b07d08af6f6cbf726fc46528f95eecef302c11702`。 |

这些 Rust 步骤均在 Docker 内执行，日志固定 `CARGO_BUILD_JOBS=1`、`CARGO_INCREMENTAL=0` 和 `--locked`。最终日志有缓存命中，不能将其中的短耗时解释为冷构建性能或优化收益。编译通过也不证明整个 upstream workspace 的 Rust 单元/集成测试已跑完；本报告未取得该项通过证据。

## 实际门禁结果

| 门禁 / 命令范围 | 结果 | 原始证据与限定 |
| --- | --- | --- |
| 供应链、changed-crate、构建工具合同 | **PASS：77 passed** | [integration-build-contract-tests.log](../../../tmp/codex-refresh-integration/integration-build-contract-tests.log)，三份 pytest 文件，18.77s。包含 fixture/mock，不能替代真实 Cargo workspace tests。 |
| 最终供应链合同复跑 | **PASS：18 passed** | [final-supply-tests.log](../../../tmp/codex-refresh-integration/final-supply-tests.log)，`tests/harness/test_agent_runtime_supply_chain.py`，0.06s；不是额外 18 个不重叠用例。 |
| `make harness-check` | **PASS** | [harness.log](../../../tmp/codex-refresh-integration/harness.log)：`Harness contract OK`；保留既有 `kb-rag-ui-t5` loop-state schema 提示。 |
| `make architecture-boundary-gate` | **PASS** | [architecture-boundary.log](../../../tmp/codex-refresh-integration/architecture-boundary.log)：负向自测通过；扫描 601 文件，0 violation、0 allowlist exception。 |
| `make core-boundary-gate` | **PASS** | [core-boundary.log](../../../tmp/codex-refresh-integration/core-boundary.log)：11 contracts、133 core modules，knowledge→core 为 13，与原基线相同。 |
| 离线 OpenAPI 合同 | **PASS：2 passed** | [openapi.log](../../../tmp/codex-refresh-integration/openapi.log)：兼容 schema 测试通过，保留 13 warnings；没有据此声称 live OpenAPI 验收。 |
| Web type-check / lint / unit / i18n | **PASS：137 tests，0 failed/skipped** | [web-quality.log](../../../tmp/codex-refresh-integration/web-quality.log)：app/node 两个 TS 项目、ESLint、Node tests、i18n 完成；执行环境 Node `v24.14.0`，存在项目要求 Node 22 的 engine warning，不能据此声明 Node 22 已验证。 |
| Web build 与 bundle budget | **PASS** | [web-build.log](../../../tmp/codex-refresh-integration/web-build.log)：Vite build 完成，budget 检查 `oversizedRoutes=[]`；保留大 chunk 提示。最终 hot-update 也重新构建并拷贝了 web/dist。 |
| `make loc-no-growth-gate` | **FAIL / 未通过** | [loc-gate.log](../../../tmp/codex-refresh-integration/loc-gate.log)：负向自测通过，但正式扫描 1,317 Python、404 TypeScript 文件，27 violations，`make ... Error 1`。不重置 baseline，不改成 skipped 或 PASS。 |

77 项执行范围为：

```bash
uv run --all-packages --extra test pytest -q --no-cov \
  tests/harness/test_agent_runtime_supply_chain.py \
  tests/harness/test_rust_changed_crate_gate.py \
  tests/scripts/test_rust_build_tooling.py
```

LOC 失败含既有超长文件增长、新超阈值文件，以及本轮修改的 `scripts/harness/agent_runtime_supply_chain.py`（日志口径 `1013 → 1104`）。它不是全部旧债，也不由本次 Docker 编译通过自动豁免。完整 27 项以日志为准；本次文档工作没有修改任何 LOC 基线。

## 部署后源码失配与修复

初次源码探针为 **FAIL**：实际导入根是 `/app/src`，两份本轮新增 Gateway 文件缺失。此前先创建容器并热拷贝源码，之后的 [candidate-start.log](../../../tmp/codex-refresh-integration/candidate-start.log) 明确记录 `ai-gateway-backend Recreate/Recreated`；后一次容器重建抹掉了可写层中的热拷贝。具体触发 recreate 的 Compose hash/input 字段未被日志捕获，不能声称“同配置启动一定保留热拷贝”。初次健康检查通过没有证明新代码已在容器内。

主代理随后对最终运行中的容器执行 `make hot-update ARGS="--all"`。[hot-update-running-final.log](../../../tmp/codex-refresh-integration/hot-update-running-final.log) 记录 Gateway `src/config/database`、共享包、Knowledge 服务/worker、capability catalog 和 Web 静态资源复制、服务重启及健康检查完成。后续 [live-source-probe.json](live-source-probe.json) 为 **PASS**：

- 仓库 `src` 为 331 文件，加上明确单独复制的 capability catalog，预期与容器实际均为 **332 文件**；排除 Python bytecode/cache。
- 预期与容器目录摘要均为 `af2043fa24f4991aa7d46c09c203f019d53f0fb2b6cd6640095119aa9303df2c`；`missing/changed/extra` 均为空。
- 9 个容器均 running/healthy，Compose working directory 指向本仓库。
- [make validate 最终日志](../../../tmp/codex-refresh-integration/validate-final.log) 确认配置、依赖、模型/provider alignment 与 runtime validation 成功；保留 1 条本地 bootstrap 默认配置提示。
- [make status 最终日志](../../../tmp/codex-refresh-integration/status-final.log) 的 9 个容器和 11 项健康检查全部 Healthy。

`validate/status` 原始 shell exit code 未独立持久化，探针将二者标记为 `PASS_LOG_CONFIRMED`，依据实际成功终句和健康列表。文件哈希证明容器磁盘上的源码身份；请求是否执行相应代码路径、provider 行为与浏览器旅程仍须由实机证据证明。

## 私有备份与回滚限制

[backup.log](../../../tmp/codex-refresh-integration/backup.log) 记录升级期间已创建私有 PostgreSQL 备份：`/Users/yang/.local/state/ai-gateway/backups/codex-refresh-20260922/gateway_20260922_120635.sql.gz`（命令完成后移动到本次专用目录），日志大小 90M。此处仅保存位置引用；本次报告整理没有读取、复制或发布备份内容。备份创建日志本身不证明恢复演练成功。[升级前镜像快照](pre-upgrade-local-artifacts.json) 也只记录当时可用的本地制品，不能当作完整已验证恢复包。

同目录还保留 `runtime-home-before-upgrade.tar.gz`，由 Runtime home 命名卷只读备份；未执行恢复测试。

回滚分类为 **restore-required**。[Runtime 兼容文档](../../../rust/agent-runtime-overlay/kernel-rs/ai-platform-agent-runtime/COMPATIBILITY.md) 明确目标原生历史新增 `token_usage_record`、`retained_context`，旧 `94cbbdda` decoder 无法读取。因此不能在已写入目标数据的同一数据库上，仅切回旧 Runtime/Worker 镜像并声称无损兼容。应先停写、保留目标数据库，再把冻结基线恢复到隔离数据库，验证完整服务与数据恢复。**本报告没有完整 restore rehearsal 的通过证据**。

## 浏览器与真实 API 验收

本次目标为用户要求的本地升级收口，以下只记录实际覆盖，不提升为完整发布矩阵。内置浏览器使用 `http://127.0.0.1:8081`、现有专用 E2E 账号和 Qwen 3.7 Plus。

- 登录、仪表盘、Assistant 正常渲染；文本标记精确返回，刷新后保留，下一轮能记住标记。
- Python `print(6*7)` 经 UI 通过审批后返回42；[PG回执](live-ui-tools-pg-20260922.json)证明仅一次成功执行、exit0；拒绝另一次调用后执行与dispatch均为0。
- 长Python任务在 UI 停止后 run 为 cancelled、活动模型租约为0。Worker 已发出的执行保守记录为 side_effect_unknown；不把这一回执说成已证明进程即时清理或零副作用。
- 取消后首次续聊遇到 provider ConnectError，run正确失败，历史仍保留；[脱敏诊断](live-ui-recovery-connect-error-20260922.json)。最小修正仅涉及 Gateway共用模型流包装：首帧前稳定502、首帧后唯一failed终态、无重试、无错误详情泄露。作者56项相关测试通过；[独立reviewer](final-review.json)复跑4项新增用例通过，无阻断finding。
- 修正通过 `make hot-update ARGS="--gateway"` 更新，validate/status均exit0；[最终源码核对](live-source-after-transport-fix.json)332/332一致。Runtime/Worker镜像身份未改变。
- 知识库旧合成资料向量与BM25各命中1，真实Qwen问答返回 `P1-ORION-739` 并有正确的引用原文。未对本次未改动的入库全流程、全量Eval或跨平台发行作通过声明。
- Agent Studio现有草稿、版本摘要与编辑/预览入口正常渲染；未执行发布操作。

辅助API的[首次失败](api-acceptance-initial-failed.json)完整保留。[诊断](api-acceptance-diagnosis.json)证实历史测试错误使用generic session接口，应使用UI相同的assistant session接口；todo_read在升级前就被generic Assistant隐藏，测试应使用实际snapshot存在的只读工具。没有修改工具权限来使测试通过。[最后一轮记录](api-acceptance-final.json)逐项保留PASS/FAIL；不以总命令退出失败掩盖成功项，也不把未通过的精确输出断言改写为PASS。

截图保留于本地 `tmp/codex-refresh-integration/iab-python-pass.png`、`iab-kb-retrieval-pass.png`、`iab-kb-qa-pass.png`，持久证据以本目录结构化回执为准。

权限三档单独完成[现状研究](permission-controls-research.md)，按用户收口要求未纳入本次功能扩展。

最后一轮 API 的文本、多轮、历史、取消与关键词知识检索共11项PASS；只读工具项仍FAIL。其[单独诊断](api-acceptance-final-diagnosis.json)明确本次 local_node_catalog 需要 device_id，输入缺失导致没有工具调用；不是模型完成后多输出文字。该辅助案例保留失败，不冒充工具成功。Python真实执行/拒绝的独立证据已覆盖本次工具兼容主路径。

Runtime重启后同会话复测另发现一个实际恢复回归：首次verify发送空resume配置，让上游以默认OpenAI加载旧线程；后续再传Gateway配置时已被loaded缓存忽略。此问题独立于先前的provider ConnectError，必须修复并再次验证，不能归类成外部网络失败。修正限定到Gateway的首次resume路由参数，不扩展Rust或权限功能。最终已热更新并重启Runtime；[PG复证](live-ui-cold-resume-final.json)确认provider=ai-platform-gateway、dashscope/qwen3.7-plus、1条completed模型调用、活动租约0、唯一成功终态。在同一会话精确得到 `COLD_RESUME_QWEN_OK`，6.13秒。独立reviewer新增两项合同复测通过；[IAB回执](iab-acceptance.json)、[最终332文件源码核对](live-source-closeout.json)与[最终review](final-review.json)均保留。

## 收口决定

按用户最新要求，本次以“目标内核升级、现有主流程本地实测、复现问题最小修正、独立reviewer复测”为完成范围，结论为 **completed_local**。最终 `make validate`、`make status` 均exit0。本地验收收口时未新增权限功能、提交/push、公共镜像发布或生产部署。

遗留义务单列，不解释为全绿：LOC门禁27项未通过；辅助local_node_catalog用例缺设备参数而未执行；已dispatch取消的Worker回执为unknown，未证明即时进程清理；完整恢复演练、hosted Rust/跨架构/新机器与全C01–C20矩阵未完成。这些保留为后续工作，本次不通过扩大重构或修改验收基线来消除它们。

## 合入 main 前复核

用户随后明确授权最终复核、合入 main 并删除已合并分支。[本轮复核](premerge-review.json)记录71项相关测试、独立6项复测、18项供应链测试及配置/文档检查通过。容器已在本轮之前停止，因此本轮运行检查未完成，未重启环境；上一轮实机记录仍按原身份保留。分支采用正常提交、fast-forward合并和普通push，不改写历史。
