# 2026-09-22 Codex 更新设计证据
权限档位的单独研究见 [permission-controls-research.md](permission-controls-research.md)；此项是现状与方案研究，尚未开放新权限。


后续实现、Docker 构建、最终制品身份与本地验收见[实现验证报告](implementation-verification.md)。本文以下保留设计阶段的只读源码调查与计划验证事实；其中的旧 pin、overlay、CI 状态及“本次未编译/部署”均指设计阶段，不代表实现后的当前状态。

## 输入身份

| 输入 | 本次观察 |
| --- | --- |
| 平台 | `305ef0c14bb22a65894bc8a2bfaa1a721ac1e2f6`，开始时main与origin/main同步、工作区干净 |
| 平台source lock | `deploy/agent-runtime-source/lock.json` → upstream `94cbbddafc1776d5e377bca1b05932c697e82238` |
| 目标仓库 | `/Users/yang/projects/opensource-harness/codex-harness`，只读；工作区干净 |
| 选定target | `279ba894152b2c01c5294cc0723b463b209bdca4`；当前本地快照，不声称远端最新 |
| 当前overlay | 107文件，锁内hash `80992c9505b06a2e9993980efc739ce33ee79c1fcc79b60914c894b2f62129ba` |
| 旧pin材料核验 | `make agent-runtime-source-contract`：exit 0，17 passed；没有构建/启动任何目标镜像 |

## 可重复核查

在提供的源码仓库运行以下只读命令：

```bash
git status --short --branch
git rev-parse HEAD
git rev-list --count 94cbbddafc1776d5e377bca1b05932c697e82238..279ba894152b2c01c5294cc0723b463b209bdca4
git diff --no-renames --name-status 94cbbddafc1776d5e377bca1b05932c697e82238 279ba894152b2c01c5294cc0723b463b209bdca4
```

提交差1159，`--no-renames`变更路径5045。默认rename检测的shortstat为5035文件，但输出提示未穷尽rename检测，故计划统一使用可重复的5045路径口径。这些数值代表源码变化量，不代表1159个用户功能或5045个平台改动。

[source-delta.json](source-delta.json)逐文件将平台`kernel-rs/<path>`对应到upstream`codex-rs/<path>`，比较旧blob、目标blob和平台文件：82项两个upstream快照均不存在（平台新增）、2项仅平台变化、23项双边变化。对双边文件执行 `git merge-file -p <platform> <old-upstream> <target-upstream>`，结果为10 conflict、13 clean。输入暂存在临时目录，输出不应用到任何源码。文本clean仍可能有签名、默认行为或运行时依赖变化。

## 源码结论与边界

| 结论 | 实际读过的来源 | 对计划的影响 |
| --- | --- | --- |
| 托管Runtime确实宿主化Codex，而非独立Python loop | `rust/agent-runtime-overlay/kernel-rs/ai-platform-agent-runtime/Cargo.toml`；`src/lib.rs:127`的`InProcessAppServerClient::start_with_host` | 保留内核和宿主扩展路线；不重写loop |
| 旧overlay可覆盖目标整文件 | `scripts/harness/build_agent_runtime_image.sh:63` archive及下一行copy；source/worker builders同类路径 | 三方重放并验证目标blob，不能只更新pin |
| HostRuntime是平台新增文件 | overlay的`app-server/src/host_runtime.rs`在旧/新upstream均不存在；source delta归为platform_added | 宿主预授权ID/store/contributor职责需要手动适配，不能误称上游标准接口 |
| 启动工具上限已有新上游接口 | target `codex-rs/ext/extension-api/src/tool_policy.rs:10`；`core/src/tools/registry.rs` | 映射不可变snapshot，resume重传，default无限制不能直接沿用 |
| 工具生命周期字段和回调改变 | target `ext/extension-api/src/contributors/tool_lifecycle.rs:118`、`:153`；旧/新git diff | originating item与executor语义纳入C04/C05 |
| 代理控制接口演进 | target `core/src/agent/api.rs:45`、`agent/control.rs`、`agent/control/spawn.rs` | 保留新调度/快照/消息语义，同时约束scope和总预算 |
| ThreadStore语义扩展 | target `thread-store/src/store.rs`、`types.rs`旧/新diff；平台`postgres_store/thread_store.rs:45` | 审核同步durability、pending metadata、附件、创建身份、删除关联状态；no-op不是已验证bug，也不是安全证明 |
| Guardian上下文默认改变 | target `features/src/lib.rs:1628`：`guardianv2.thread_context` default true；平台`http_service/thread_lifecycle.rs:169`只覆盖部分feature | 必须枚举并固化profile；不推断Guardian整体已启用 |
| Gateway已有出站授权修复 | `src/services/agent_runtime/model/native_responses.py:255`、`chat_completions.py:273`、`authorization.py` | 将这些既有修复作为迁移不变量，不重新声称旧草稿问题全都仍存在 |
| 终态控制已有根/子run逻辑 | `src/services/agent_runtime/control/event_stream.py:190`起 | 目标协议与旧V1/V2映射重新验证，不替换成简单“任何terminal都关闭” |
| 已有连接池与耗时日志 | `src/services/agent_runtime/model_plane.py:284`、`model/timing.py` | 先测量并复用，不加重复池或假设网络hop就是瓶颈 |

上游路径均相对输入仓库，平台路径相对AI--Platfform；所有推断仅用于设计，不把静态发现直接当成已复现生产故障。

## 当前基线CI与历史证据

当前基线[CI run 35730600944](https://github.com/misaya-yang/AI--Platfform/actions/runs/35730600944)已结束：Frontend、Script Contracts、Eval Contracts、Runtime Offline Contracts、Gateway Units、SDK SSE、Compose/Harness、Independent CLI、KB Migration成功；Architecture与Rust Changed Crate失败，聚合gate失败，Release Readiness跳过。

失败根因已从日志区分：Architecture内部LOC_NO_GROWTH为failure，其余五项success；Rust构建工具合同的`test_build_update_builds_before_requiring_empty_source_lock_artifacts`期待轨迹少了一次实际`validate:source`。这不是新目标Rust编译结果，前置测试失败后changed-crate没有完成。设计不擅自修改这些生产/测试文件。

9月7日一期本地证据只适用于其记录的旧pin/平台代码；Qwen、审批、取消、KB、Eval等已验收事实不直接复制成目标快照PASS。公开发行、新机器、完整冻结回滚、正式质量提升缺口仍保留，详见[一期证据](../phase1-local-acceptance-2026-09-07/README.md)。

本次没有运行目标Cargo、Docker、迁移、模型调用或浏览器；没有修改upstream checkout、source lock、生产代码、运行服务或既有程序状态。

## 设计交付检查

`make harness-check`与`git diff --check`通过；只读init脚本语法及执行通过。7个工作包必需字段、依赖顺序和实际路径检查通过，C01～C20均有owner；11条新文档本地链接可解析。设计自检结果与文件hash见[design-verification.json](design-verification.json)。这些检查只证明计划可定位、状态诚实及结构完整，不证明未来实现的兼容性。唯一Harness提示是既有kb-rag-ui-t5缺失schema声明，未在本次改动中处理。
