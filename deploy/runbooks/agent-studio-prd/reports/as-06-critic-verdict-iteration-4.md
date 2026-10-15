# AS-06 独立 Critic 判定 — Iteration 4

- Phase：AS-06
- Feature：AS-F007
- 日期：2026-07-19
- Verdict：`approved`

## 结论

C-07 已关闭，未发现新的 material blocker。

Publish 在同一事务中先以 `tenant_id + dataset_id` 精确锁定 Eval Dataset
父行 `FOR UPDATE`，再对同一 tenant、同一 Dataset 的全部 manifest example
执行 `FOR SHARE`：

- 现有 example 的 UPDATE/DELETE 与 `FOR SHARE` 冲突，必须等待；
- 新 INSERT 以及现有 import 实现最终都写入 `eval_examples`，其
  `dataset_id -> eval_datasets(dataset_id)` 外键检查取得父行 key-share 锁，
  与父行 `FOR UPDATE` 冲突，不能形成 phantom；
- 若内容变更先提交，已有 manifest-hash stale gate 会在任何发布写入前失败；
  若 Publish 先取得锁，变更只能在 Publish 提交后继续，因此发布提交点使用的
  manifest 仍是精确、稳定的评测身份。

锁定查询保留既有 Owner 授权边界，并同时限定 `tenant_id`、`dataset_id`；
未扩大 Dataset 可见性、角色权限或跨 tenant 访问。C-01 至 C-06 的测试和
事务路径仍在最终源码 required gate 中通过，未见本次两文件改动造成回退。

## 独立校验回执

- 源码指纹：独立重算 `13/13`，全部匹配
  `reports/agent-studio/as-06-publish-atomicity.json` 的最终源码记录。
- Ruff：exit 0，`All checks passed!`。
- C-07 聚焦 PostgreSQL：收集 17、选中 2，`2 passed, 15 deselected`，
  0 skip，exit 0；UPDATE 与 INSERT 两种变更均等待 Publish 事务提交。
- Required gate 1：收集 35，`35 passed`，0 failed，0 skipped，exit 0
  （API 14 + PostgreSQL 17 + release gate 4）。
- 按本轮提速边界不重复执行与两文件锁修复无关的昂贵门禁；已核对冻结的
  最终源码 Actor 回执：candidate 13、golden 16/16、Eval 41/116/35/17；
  frontend lint/type/i18n/build 与 Playwright 10；AHR 33/77/8/98、golden
  与最终 current-source isolation 6/6，均为 exit 0、零 required skip。
  首次 isolation 2/6 被明确保留为失败，未计入通过；最终 provider-free
  current-source 6/6 后，正式 Gateway/Assistant 已恢复 `stub=false`。

## Phase 判定

AS-06 的 revision-bound Eval、不可变且幂等的 Version、原子 promotion、
安全 rollback、Studio 真值与 Assistant 兼容性证据满足 Phase 06 contract。
独立 Critic 批准 AS-F007；Oracle 状态转换及 supported non-legacy claim check
仍由 orchestrator 执行，本判定不自称 AS-07 或 AS-09 完成。
