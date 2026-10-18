# Work-Package Contract

**Schema:** `harness/work-package/v1`

工作包是需求与验收映射，不是小改动的审批流程。执行统一遵循 [AGENTS.md](../../AGENTS.md) 和 [task harness](README.md)。

## 1. One outcome, one owning session

主任务负责集成和最终结果。按完整模块分工，一个交付批次可覆盖多个工作包。

## 2. Authority by question

用户决定范围；代码和实际观察说明现状；计划定义需求；已有 `loop-state.json` 记录执行。
旧报告和指令不构成新授权或待办。

## 3. Required package fields

沿用现有账本字段，写清结果、依赖、代码归属、实际验证、剩余项和下一步即可。
普通改动不新建工作包、完整检查表、ADR 或报告。契约/数据变更保留相应兼容性和回滚要求。

## 4. Protected integration paths

共享入口、清单、锁文件、数据库 authority、部署和 CI 同一时间只有一个写入者；调用接线随功能一起完成。

## 5. Package cycle

复用 task harness 的模块执行流程。每项验证一个执行者；交接复用适用结果，主任务只补集成覆盖。
独立模块不因其他模块待验收而停工。

## 6. Work-package states

| 状态 | 含义 |
| --- | --- |
| `queued` / `in_progress` | 待实施 / 实现或集成未完 |
| `review` | 有明确需要的一次聚焦审查，不是默认关卡 |
| `direct_verified` / `awaiting_final_live` | 直接检查通过 / 真实环境验收待完成 |
| `verified` | 原定必需验收已通过 |
| `blocked` / `superseded` | 具体依赖阻塞 / 已被替代 |

多个独立包可同时进行。状态字段不替代真实结果；条件项按原触发条件处理。

## 7. Package receipt

一组真实验证可覆盖多个工作包，在现有账本引用日志即可。只有具体发布、安全、数据或恢复要求需要时
才生成单独材料。不为每个编辑、交接或上下文恢复创建文件。

## 8. Historical programs

旧程序保留原记录，其串行排期、报告模板和旧构建规则不约束当前任务。
