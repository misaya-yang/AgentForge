# 设计交付与下一步

设计基线main `305ef0c1`，目标upstream `279ba894152b2c01c5294cc0723b463b209bdca4`；当前运行源码锁仍为`94cbbdda`。本次仅源码调查和计划编写，未升级、编译、部署、迁移或调用模型。

开始实现时先读README与主计划，再按work-packages执行U0。不能跳过现有LOC/Rust测试失败归因，也不能因其阻塞hosted编译而声称Rust gate已通过。保留忽略的旧本地草稿，不作为执行入口。

源码差异：107个overlay文件，23个双边变化、10个文本冲突；目标有ToolPolicy、工具command-start、代理控制、ThreadStore和feature默认变化。详细证据在reports/architecture/codex-harness-refresh-2026-09-22。

当前loop-state为queued，不继承任何旧的自动执行next_action。实施授权后由唯一主writer更新active_package；每个C项按实际结果记账。C80可见候选与R100完整发行分开，但不能通过改名缩小最终验收范围。

## 实施进行中（2026-09-22）

用户已明确授权多代理并行代码更新/优化/review，代码完成后统一Docker更新及内置浏览器实测；随后要求以完整上游Rust核心直接复制为准，平台保留最小必要宿主接缝。source target仍279ba894；不追新HEAD。

当前分支codex/harness-refresh-plan-2026-09-22，未commit/push。六个子代理由root管理：upstream_host_update已完成首次Rust host迁移（23旧文件+8新增目标文件，tmp/upstream-host-refresh/residual-inventory.json）；platform_runtime_update正在收尾PG、ToolPolicy、子代理动态工具/root lease映射；gateway_compat_update在收尾tool ceiling/per-turn、wire、Eval；client_compat_update已完成CLI/SSE及交叉review；build_gate_update已完成59项门禁测试；upgrade_security_review已输出具体风险并交给实现代理。

root已三方合并workspace Cargo.toml，以target Cargo.lock为暂存起点（必须Docker解析后更新）。三种image builder已接入rust_gate_identity.verify_overlay_upstream_base；supply_chain已支持v2 manifest hash/schema在lock/receipt中锁定，18项测试通过。最终v2 manifest/upstream_blobs/receipt/source lock仍待代码冻结后生成，当前旧source lock未更新，不能启动候选。

Docker daemon运行但所有ai-gateway容器已停止。Compose owner是本仓库。Docker约3GiB，host盘约41GiB；所有Rust在Docker串行。旧镜像/容器身份已保存reports/architecture/codex-harness-refresh-2026-09-22/pre-upgrade-local-artifacts.json；DB备份/实际回滚尚未做。新raw rollout含token_usage_record/retained_context，旧内核不能读取，禁止仅切N-1镜像在同DB降级；保留原生历史，采用restore-required且保留目标数据的隔离备份恢复验证，完整发行证据不能冒称已满足。

临时完整target源码在tmp/codex-refresh-integration/source。metadata Docker第1次probe用libgit2停于crossterm，已明确取消（session15707 exit130）；为纳入最新crate manifest并使用有超时的Git CLI/HTTP1.1重新构建，当前exec session65174，日志tmp/codex-refresh-integration/metadata-build-retry.log，导出目录tmp/codex-refresh-integration/metadata，Dockerfile.metadata。先确认该handle状态，不盲目重启。root下一步：完成metadata锁文件导出，复制最终overlay并用生产Dockerfile.runtime builder真实编译，把错误交给owner；源schema/工具链/SBOM从实际容器输出生成；成对构建后按授权更新栈、内置浏览器验收、修复并做回归。不得host Cargo，不得输出凭据，专用E2E账户可执行时复用。


## 2026-09-22 最终收口

本次用户将执行范围收敛到本地升级、实际兼容测试、最小修正及独立reviewer复测，程序已 `completed_local`，不继续执行旧编译handle或扩大权限三档工作。最终状态以loop-state.json和reports/architecture/codex-harness-refresh-2026-09-22/implementation-verification.md为准。

最终upstream279ba894、overlay5dc36c43638a；Runtime/Worker使用lock.json内同一对制品。Gateway追加传输失败规范化与cold verify路由两项最小修正，已hot-update；同旧会话重启后Qwen精确返回COLD_RESUME_QWEN_OK。此前fallback/ConnectError及脚本失败证据全部保留。无新commit/push。权限三档研究见同证据目录permission-controls-research.md，未实现。发布矩阵、LOC及取消Worker即时清理证明仍在follow-up中。
