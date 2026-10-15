# AS-06 独立 Critic 判定 — Iteration 3

- Phase：AS-06
- Feature：AS-F007
- 日期：2026-07-19
- Verdict：`changes_requested`

## 阻塞结论

- **C06 已关闭。** 数据库模型授权分支现在会在事务内、持有模型与 Provider 行锁期间，强制调用当前 readiness revalidator；缺失、撤销或不匹配都会在发布写入前失败。
- **C07（high）仍阻塞发布。** Publish 读取 Eval manifest 时只锁定 `eval_datasets` 头行，没有锁定或版本化 `eval_examples` 内容。独立 PostgreSQL 并发复现中，Publish 完成 manifest 读取后，第二连接更新样例成功（`UPDATE 1`），随后确认 `manifest_changed=true`；Publish 仍提交，Version/Publication/event/request 为 `1/1/1/1`。因此发布可引用已经失效的评测证据，违反精确 Dataset 身份与原子 promotion 要求。

修复必须建立数据库协调的不可变 Dataset revision/content identity：样例新增、导入、更新、删除与评测完成、Publish 必须共享冲突锁或不可变修订边界。并发 UPDATE 与 INSERT/import 漂移均须 fail closed，保持 Version/Publication/event/request 为 `0/0/0/0` 且指针不变；仅锁现有 example 行不足以阻止新增 phantom。

## 独立校验回执

- Required gate 1：exit 0，`33 passed`，零 skip。
- Required gate 2：exit 0，candidate `13 passed`；golden `16/16`；Eval `41/116/35/17`，零 skip。
- Required gate 3：静态门禁全部 exit 0；Playwright `10 passed`，零 skip。
- Required gate 4：AHR `33/77/8/98`；isolation `6 passed`，零 skip。
- 补充 OSS 回归：`41 passed`，零 skip。
- 源码指纹：`13/13` 匹配。
- 最终运行态：Compose `8/8` healthy，owner 正确，Gateway/Assistant `stub=false`，容器源码与主机匹配，约 `770 MiB`。

## Phase 判定

绿色门禁没有覆盖 C07 的事务竞争窗口，不能支持完成声明。AS-F007 保持 `failing`，AS-07 不解锁；修复 C07 并通过新的独立 Critic 前，AS-06 不批准。
