# PC 开发接续：2026-10-16

本次交付分支：`codex/native-workflows-20261016`。`main` 保留仓库整理检查点
`22d9ce0ff9cba51b130f525e7b1add5cc3853744`；后续功能从交付分支另开短期分支，
经测试和 PR 合并，不在同一工作区混入多个未完成功能。

## 在个人 PC 获取代码

```bash
git clone --branch codex/native-workflows-20261016 git@github.com:misaya-yang/AgentForge.git
cd AgentForge
git status --short
git log -1 --oneline
```

使用仓库根目录，不使用原先嵌套的 `AI--Platfform` 或 Downloads 导入目录。
运行前阅读 `AGENTS.md` 和 `docs/harness/runtime-and-secrets.md`。

平台服务与运维程序均为 Rust，前端为 React/TypeScript；Python 仅是可选用户代码沙箱。
本机密钥、测试登录态、数据库卷、编译目标、截图和下载的 Codex 上游不在 Git 内，
不能把当前 Mac 的私有绝对路径或本地镜像标签当作 PC 的运行依赖。

## 工具与环境

- Native Rust 1.99.0，Runtime/Worker 在受控 Docker 构建中使用 Rust 1.95.0。
- Node 26.11.1、pnpm 12.11.2、Docker Compose v2；Windows 建议在 WSL2 内开发。
- 依照 `docs/harness/commands.md` 安装和检查依赖，前端执行
  `pnpm -C web install --frozen-lockfile`。
- 使用公开配置示例重新生成本机忽略的环境文件，安全注入自己的测试密钥。
  不复制或提交当前 Mac 的 `.env`、Keychain 项或会话 token。
- 每台机器只使用本仓库的一个 Compose 项目。`make dev-setup` / `make dev-start`
  管理本机环境；`make dev-compose` 构建并运行本机 native 服务。先运行 `make doctor`，
  首次部署步骤和真实参数以命令目录及各 Make target 为准。
- Runtime 源构建需要独立、干净的官方 Codex 上游 checkout，固定 commit 由
  `deploy/agent-runtime-source/lock.json` 给出。按
  `deploy/agent-runtime-source/README.md` 组合本仓库 overlay；不能复用别的版本镜像。

基础编译和契约检查：

```bash
export CARGO_TARGET_DIR="$HOME/.cache/agentforge/targets/dots-native-1.99.0"
export CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
make native-check
make harness-check
pnpm -C web type-check
```

数据库由 native migration authority 写入；本次支持并实机验证到 epoch 024。
001–024 的冻结 SQL 和包身份不应为绕过错误而改写。新机器采用正常 fresh 初始化，
不需要不可用的旧平台 Git 历史。

## 本次边界与证据

当前准确状态以 `reports/native-current-acceptance-20261016/` 和
`deploy/runbooks/native-enterprise-upgrade/loop-state.json` 为准。
知识库文本/PDF发布、文本编辑与历史版本恢复、Qwen 检索与重排、问答引用、
嵌入迁移 cutover/rollback/cancel/abort 已有真实 API 和浏览器证据。
正式 SDK、LocalNode 和最后集成检查的收尾结果记录在同目录最终检查点报告中。

用户要求收尾后，未完成的 OIDC/SCIM、025 RLS/audit、增强 sandbox、online-eval
实验源码已从交付树移出；本次没有激活 025。JWT keyring 的完整实现与测试保留，
当前本机服务显式使用兼容的 legacy JWT 模式。不能将这份检查点称为全部企业需求验收完成。

后续优先按 Qoder 的 `docs/research/enterprise-platform-gap-analysis-2026-10.md`
逐项开分支：身份联邦与 SCIM、租户 RLS 与审计、guardrails/密钥轮换、完整可观测性、
HA/滚动升级、签名供应链、评测与 canary、可选沙箱隔离。每个分支交付完整流程和实际验收，
避免再次把多个未完成模块接入同一工作区。

已知限制：个人 PC 尚未实际启动；真实企业 IdP、双实例升级、公开签名镜像及完整企业合规
尚未验收。Main 的 Flash 记忆召回有失败记录，原因未确定；qwen3.7-plus 的对应流程已有
真实成功证据。Provider 未公开计算中断/账单确认，不能把本地取消当作该类保证。

## 最终收尾状态

用户已关闭 Docker；本轮未重启 Docker，自有 Gateway、Knowledge、SDK Worker、Vite、OTLP 接收器及 LocalNode 测试进程已停止，数据库卷和设备身份状态保留。

最终 native 全工作区 all-targets 检查通过；选定 Rust 库 525 项、迁移 authority 74 项、部署契约 44 项测试通过；Runtime 101 / Worker 86 项通过（分别 3 / 1 项忽略）。前端 2050 项测试、类型检查、i18n 和生产构建通过。编译与测试不等于全部实机验收。

- 标准 OpenAI SDK：之前完整运行通过，最终重跑 models/chat/SSE 通过，embedding 再次返回 HTTP 502，原因未解决。usage service ID 长度问题已修复并有回归测试，真实 usage 入库尚未复验。
- LocalNode：配对、授权同步和重连通过；最后 file.list 停在审批，未执行本地操作。read/search/revoke/deny 的最终验收未完成。保留状态，接续时不要重放过期或结果不确定的请求。
- Main 浏览器：V2 创建 thread 的权限错误已修复并实测 201；一个模型回合完成但拒绝 synthetic marker，后续自然项目名回合停止前未核实最终结果，不能称浏览器记忆验收通过。

这些限制记录在 `reports/native-current-acceptance-20261016/closing-checkpoint.json`。后续优先修复和验收上述流程，再按需求升级文档逐项推进。
