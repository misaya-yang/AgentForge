# R2-A 特殊文档候选发布：本地合入证据（2026-09-28）

**判定：已验证的安全增量以 `cfff6306` 快进合入本地 `main`；R2-A 和完整 R2 尚未通过全部验收。** 本记录区分自动化、真实 PostgreSQL、真实 Qdrant、本地 Docker 浏览器和未验证项。基线为本地 `main@ca564f5e`，候选分支 `codex/r2-special-publication`。13 份未跟踪 RP04 JSON 保留，未纳入提交；未推送。

## 范围和结果

| 范围 | 当前结果 |
| --- | --- |
| KNO-02/03/04 特殊处理 | 层级与 VisionPDF 先准备独立 generation 的向量、行和图片对象，再在负 revision 栅栏下发布正文、切片、版本及执行清单；成功后按冻结的旧点 ID 清理。扫描候选使用持久 collection 名。处理前对象清单覆盖上传中崩溃。 |
| 同类特殊版本恢复 | 历史版本保存类型、原始来源 hash、逐页文本/对象 hash 与处理配置；hierarchy 从历史正文准备，Vision 从校验原文件与固定页文本准备。API、Worker 和发布器均核对候选版本。缺证据的旧版返回 409，不伪装为成功。 |
| 崩溃恢复/并发 | 唯一运行 execution 与文档 generation marker 绑定负 revision；migration 076 的 revision 漂移可定位原 owner；prepared 阶段尚未建集合可清理；旧 tenantless 点按文档+数据集+精确 ID 清理并回读；非阻塞 publication 锁及池容量门禁避免已确认的等待环。 |
| 来源与助手 | 检索命中仅对可证的当前版本附带版本号/hash，手工编辑切片不继承旧全文版本；Gateway 历史、分享、Quiz、图片与 SSE 的文档来源权限重新校验；Web 旧引用不拿最新版冒充历史原文。内部来源字段受用户元数据写入保护。 |

## 已运行验证

- **自动化：** `make kb-unit-gate` 为 **1976 passed、1 skipped**；跳过项缺 `KB_BACKFILL_POSTGRES_TEST_DSN`。受影响 Gateway、真实 PostgreSQL 测试组合 **180 passed**；特殊恢复、队列、候选计划定向 **33 passed**。新来源引用 Node 单测 **3 passed**。改动 Python 文件 Ruff、Web app/node TypeScript、Web lint、i18n、build、`make harness-check`、`make architecture-boundary-gate` 与 `git diff --check` 均通过。Web 构建所用 Node 24.14.0 高于项目指定 22，出现 engine warning。
- **真实 PostgreSQL + 受控向量故障：** `tests/database/test_kb_dual_verb_queue.py::test_special_publication_pg_commit_and_crash_recovery` 使用隔离 PG schema，向量侧受控注入“PG 已提交、清旧点前中断”；重新取唯一 owner 后沿原 execution 清旧点、开正 revision，正文/版本/summary/账本一致。`test_hierarchy_summary_writes_jsonb_on_live_postgres` 验证 L1 JSONB 写入。BM25 Tier-B 的触发器漂移和唯一 owner 定向组 **4 passed**。这里的向量故障是注入，不是物理杀进程。
- **真实 Qdrant：** `tests/knowledge/test_qdrant_integration_smoke.py::test_special_cleanup_deletes_legacy_tenantless_point_and_skips_missing_collection` **1 passed**，使用一次性集合验证旧无 tenant 点、异文档 ID 隔离和未创建集合跳过；测试后移除一次性集合。这与上一条 PG 测试分别执行，尚非同一任务的跨存储 E2E。
- **本地 Docker：** 先核实所有 `ai-gateway-*` Compose 工作目录标签为本仓库，再运行 `make hot-update ARGS="--all"`，无依赖安装或镜像重建；`make status` 全部健康。Knowledge API/Worker、Gateway 来源代码和 Frontend `index.html` 的 host/container SHA256 逐一相同。
- **Codex 内置浏览器真实页面：** 使用已有专用 E2E 账号登录本地 `127.0.0.1:8081`。知识库列表刷新后仍为 3 文档；失败 PDF 显示“最后记录阶段：解析”、手动重试指引及文档 ID；恢复过的文本 `r2-policy.txt` 历史仍是当前版本 #8、1 条 500 CNY 切片。只读 PG 核对该文档仍为 4 条 execution（3 次有意 reprocess），刷新未新增。`/assistant` 能加载，默认显示 Qwen 3.8 Flash，浏览器 console 无 error/warn。截图在 `tmp/r2-knowledge/r2-special-final-*.png`。此次浏览器回看是既有文本任务，不算新特殊 provider 实测。

## 独立 review 与未完范围

`r2_independent_review` 只读检查了最终候选的状态恢复、触发器漂移、权限、元数据、锁与清理。其发现的旧 tenantless 点残留、prepared 未建集合卡死、旧版本内容不符、初次 version_count 偏差、特殊 marker 可改写、不同数据集并发池等待环，以及旧 Vision 缺页回执被改写，均有最小修正和对应定向回归；reviewer 对这些修正作了静态复核，没有亲自跑 Docker。

- **R2-A 尚未通过：** 真实扫描 OCR/图像 embedding 与层级 provider 的 UI 成功重处理、失败保旧和物理硬崩溃端到端未执行；现有真实 PG 与 Qdrant 证据分层记录，不能合称跨存储 provider 通过。特殊文档回到更早普通 text 版本仍安全拒绝 409，缺专用多集合清理后发布路径。
- **长期栅栏边界：** migration 076 的普通内容触发器仍会推进负 revision。本批将预留量从 10 万提高到 10 亿并以唯一 owner + 文档 marker 定位，覆盖常见恢复窗口；这仍是有界栅栏，持续异常写入的极端情况需要后续迁移为保负触发器。未宣称数学意义的永久栅栏。
- **R2 其余需求：** KNO-01/05～12、AR-06 完整双角色/双租户撤权、EV-01 待审核样本，以及 J09～J13/J30 全矩阵未在本次收口完成。R3～R5 未启动。本地合入仅保存本批已验证增量，不改变这些验收状态。
