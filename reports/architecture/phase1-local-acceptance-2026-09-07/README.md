# 一期本地集成验收

当前验证对象为 `codex/agent-platform-vnext-phase1` 的 P1-00～P1-10 集成代码。用户最新指令要求完成代码后统一 Docker 更新、内置浏览器验证、修复实际问题，再合并 main；本目录只记录实际执行证据，不能作为公开发行、跨平台或全历史回滚证书。

## 已验证结果

- Python 集成回归：3600 passed、8 skipped、0 failed。跳过项逐条保留在 `python-tests.log`；其中一期双前缀真实 PostgreSQL epoch 2 权限矩阵另行启用并通过 2 项。
- 真实 PostgreSQL Eval：过期续租拒绝、owner B 领取、旧 owner 四类写入拒绝、新 owner 成功终态。使用隔离合成库和实际 Gateway 角色。
- CLI：72 项 Node/受控 HTTP 与 stream 测试通过；Web app/node TS、完整 lint 和 production build 通过。宿主 Node 24 与项目声明 Node 22 的提示保留，不称为 Node 22 验收。
- 架构/共享包/单实例/OpenAPI 门禁通过，599 Python 文件无跨服务 import 例外，core 133 模块无增长。
- Runtime/Worker 从同一固定 upstream+overlay 在 Docker 内构建并成对运行；宿主未运行 Cargo。真实 Linux 隔离探针 7 项通过，覆盖只读 input、output 导出、磁盘/inode/fallocate 边界。
- 内置浏览器：登录、仪表盘、Qwen 流式回答和刷新恢复通过；审批刷新恢复、Python 输出 42、长 Python 取消、进程回收及后续执行槽复用通过。
- 知识库：通过浏览器创建合成库、上传文本并完成索引；混合召回命中该文档；Qwen 3.7 Plus 返回正确验收代码及一个引用。

## 现场修复

1. 旧迁移容器的清单不认识 epoch 001/002；同步 migration source 后发现新 schema contract 未打入 migrator。正式 Dockerfile 已补包，实际迁移服务通过。
2. 长运行租约时长被计入普通 HTTP p99，取消后下一轮被误熔断。已按预算维度分开延迟样本及熔断判断，保留全部并发、租户配额和普通 HTTP 保护；针对性反例和修复后完整回归通过，浏览器取消后续跑通过。

3. 平台内部Eval先因未配置外部凭证失败，随后虚拟subject被真实capability catalog拒绝。最终从当前fenced job关联持久run创建者，校验同租户active用户后生成5分钟delegated JWT；act字段记录job/run，仍走同凭证`/me`和tenant precondition。按创建者当前授权执行，空permissions claim不是权限上限；工具审批保持，judge工具禁用。真实PG拒绝foreign/inactive/missing三类主体，HTTP候选运行成功。

4. 在已有会话切换到不同工具配置的模型时，Runtime按合同返回409；前端原先误报网络流中断。已改为明确的新建对话指引；新会话Qwen3.7也实测正确回复marker，避免自动重试未知副作用或暗改线程能力。

## 证据边界与后续发行义务

`results.json` 保存各项观察；同目录日志与截图对应具体执行。源码热更新后的容器由 `live-source-identity.json` 比对确认，不能仅凭旧基础镜像 tag 认定源码版本。完整 frozen release unit/数据库回切、独立 fresh machine、公开 registry、外部 Rust CI 及其他 OS/架构发行仍未验证。当前数据库 epoch floor/ceiling 为 2；禁止直接回退到恢复旧 ACL 或 Knowledge 用户表权限的旧二进制。

原始本地凭据、`.env`、provider key 均不进入本目录。Linux arm64 native CLI已在Docker内构建并执行`--version`及`exec --help`，来源、可执行格式、许可证/NOTICE/SBOM材料校验通过；真实native exec另通过本地受控Responses端点返回`P1_NATIVE_CLI_OK`与`turn.completed`（`native-cli-exec.json`），npm pack预览13文件包含ELF与全部材料；未跑原生CLI真实provider任务，也未运行发布目标Linux x64矩阵。真实Eval HTTP已完成候选运行和终态，详见`eval-http.json`；后续修复回归631 passed、3 skipped，详见`repair-regression.log`。实现提交`5f36974df3a9`已快进合并到本地`main`，未push；随后仅补存本次验收证据。


## 最后评测闭环

正常交互配置（temperature 0.7、max_tokens 32768）下，原实验/原case/原Qwen3.7/原marker运行通过：case状态`passed`，输出`EVAL_HTTP_BF15B93A`，实际model_ref五字段与冻结值一致，13517输入/77输出token，真实model_invocation span成功。源码修复了模型账本到Eval的漏采和evaluator_id UUID序列化，旧trace详情GET也已200。

另两次temperature 0/max_tokens 512运行未通过marker质量断言，仍保留为失败；其输入完整，原因未归结为模型随机，也未以正常配置通过覆盖它。评测任务处理完成、样本质量通过、发行gate通过是不同状态。本轮证明平台能执行、捕获并如实区分这些结果，不保证任意模型/采样预算都满足质量目标。

最终容器上的内置浏览器对比页已验证，显示失败基线0分与正常配置候选1分；回归Gate仍为fail，原因包括原完整prompt/tool指纹缺失、采样变化和仅一对样本证据不足。未点击提升Baseline，也不把单样本正向smoke冒充正式质量发布门禁。该完整对比证据属于后续发行要求，见`eval-comparison.png`。

## Git收尾

实现提交：`5f36974df3a9a58ca6b4fe907389399c32e8a537`。分支`codex/agent-platform-vnext-phase1`已快进合并本地`main`；未推送。已有`docs/plans/agent-runtime-post-upgrade-core-fixes-2026-08.md`保持原始字节，作为原有未跟踪草稿保留，不纳入本轮提交。
