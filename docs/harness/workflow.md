# Working Agreement

**Schema:** `harness/workflow/v2`

[AGENTS.md](../../AGENTS.md) 是唯一项目约定；[task harness](README.md) 说明如何聚焦实现、选择验证及复用结果。

## 1. Cold start

读取当前任务和现有执行账本，再定位相关源码。只按需补读契约或命令，不重新盘点项目。

## 2. The task loop

完成模块及调用接线 → 一次集中检查 → 修复实际失败 → 交付。验证归属和重跑条件见 task harness；
不再维护另一套步骤清单。

## 3. Definition of done

当前任务结果实现、对应必要检查通过，且未验证事项说明清楚。编译、模拟测试和真实服务验收分开记录。
原计划的必需验收保留，但不是每个小任务的前置条件。

## 4. Programs and large changes

沿用 `deploy/runbooks/<program>/loop-state.json` 和原工作包 ID。模块完成、验证结果或阻塞变化时，
更新结果、剩余工作及下一步；不另建 handoff、resume、冻结或逐次审计文件。历史记录不重写为当前证据。

## 5. Worktrees and shared resources

复用现有工作区并保护已有改动；共享文件一次一个写入者，构建和有状态联调串行。
源代码任务无需刷新运行环境。实际操作时按需查阅 [runtime-and-secrets.md](runtime-and-secrets.md)。

## 6. Encoding feedback

只保留会改变后续执行的必要规则。普通问题修复或测试归入所属模块，不自动增加全局规则和门禁。

## 7. Git

按用户授权提交、推送。只暂存任务范围，检查暂存差异，不覆盖其他改动或强制推送。
一个逻辑改动可一起提交，无需为格式、移动或状态更新单独提交。

## 8. Repository quality

清理仅覆盖已确认且属于当前任务的无用内容；保留数据、凭据、许可证、上游源码和真实历史证据。
不因为行数、日期或风格制造新的任务。
