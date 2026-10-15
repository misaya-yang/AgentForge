# R2 当前交接：KNO-04 / KNO-05（2026-09-27）

`loop-state.json` 是状态权威。本轮 R2-A 安全增量在 `codex/r1-final-boundaries` 开发并热更新本地 Docker；此前本地 `main` 安全增量为 `bc57e429`。13 份 RP04 未跟踪 JSON 保留。完整 R2 与 J09～J13/J30 仍未通过。

已修：标准文本 `indexing` 用固定配置续接，短文本候选正文、切片、版本和 `completed` 终态同 PG 事务公开；历史恢复在成功前隐藏候选，原上传不会覆盖目标正文。API producer 与 Worker 共用准入守卫；扫描、层级和持久图片回执的已有/部分旧切片或 Qdrant 点拒绝重放，版本恢复也拒绝图片回执。文档级来源授权覆盖助手继承事件、会话、事件流、产物、测验和分享；viewer 文档/切片列表与引用原文只读 active 行，畸形 context 来源拒绝。

当前验证：`make kb-unit-gate` 1923 passed/1 skipped；真实临时 PostgreSQL 36 passed；Gateway 相关范围 81 passed；Web TS/lint/i18n/build、改动文件 Ruff、harness 与架构边界检查通过。当前 Docker 热更新后健康且已核对应源码哈希。内置浏览器在合成文档上恢复 #4 至当前 #8，1 个 500 CNY 切片；刷新后版本和切片保留，PG execution 数未增加。先前 Worker 停启验收的是**排队后重启**，不是执行中硬崩溃。实际 500（API producer 缺共享准入）已修并在浏览器复测。具体层级和截图见 `reports/product/r2-knowledge-progress-2026-09-27.md`。

剩余 P0：`HierarchicalIndexer` L1/L2/L3 与 `VisionPDFProcessor` 图片行/点分批直接发布，尚无跨 collection 候选、持久回执和原子版本切换；active BM25 v2 与非纯文本 postpublish 失败窗、完成文档后 execution ledger 仍 running 的崩溃窗口未关闭。需对 PG/Qdrant/图片对象存储各崩溃点做恢复和无重放验证。KNO-05/AR-06 已补文档级守卫，但历史全文稳定版本、受控内部分享、双租户双角色实际撤权矩阵未完成。KNO-01/06～12、EV-01 待审核样本及 J09～J13/J30 按 PRD 继续；旧 Confluence 类未注册当前 Runtime，不能计入真实同步能力。

下一步先完成多 collection 与 active BM25 v2 的持久发布/回执，再做执行中硬崩溃矩阵；随后补稳定引用与剩余 R2 用户旅程。代码改动后重跑受影响检查并热更新 Docker。Docker/E2E 前读 `docs/harness/runtime-and-secrets.md`、核 Compose owner。`make harness-check` 与最终 Git 状态以本轮收口证据为准。
