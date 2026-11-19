# R2 特殊发布后续安全增量：验收与收口（2026-09-28）

**结论：本批局部安全增量已作为 `eeff2f35` 快进合入本地 main；完整 R2 不通过。** 基线为本地 `main@37a519a5`，开发分支 `codex/r2-special-completion`。本批保留其他未跟踪工作，未推送。

## 改动与缺口

| 范围 | 本批结果 |
| --- | --- |
| KNO-03/04 层级任务 | Worker 将无横线执行 UUID 规范化为同一持久 generation，准备完成后进入 `indexing` 再发布；修复真实任务先后遇到的 manifest 不匹配、文档状态不允许发布。 |
| 历史普通文本恢复 | 在已有特殊发布栅栏内准备普通文本候选，核对历史正文、hash、版本和固定配置，原子切换后清理旧多 collection 点；恢复点不带层级 `level`。旧位置若由操作员禁用，新向量保持禁用。无版本保护的普通重处理、缺来源回执的更早普通版本恢复均安全拒绝，避免保留过期来源回执。 |
| AR-06 来源读取 | Gateway 流事件续接前重查撤权，游标跳过来源事件时重读持久来源；旧历史从切片恢复库 ID；无法表示文档来源的匿名 Quiz 分享拒绝；版本字段严格校验。 |

| 需求/旅程 | 状态与证据 |
| --- | --- |
| KNO-03/04、J10 中层级重处理/版本恢复 | **本批通过已执行切片**：真实 DashScope embedding、两代发布、同类旧版恢复；PG 版本/切片与 Qdrant 活动点一致。物理执行中硬崩溃、失败保旧的真实 provider 路径未验证。 |
| KNO-02 扫描/PDF 与跨类文本恢复 | **未完成验收**：当前公开上传路由显式拒绝 `scanned`/`multimodal`，按 PRD §5 应验证拒绝说明和无副作用，开放新解析器不是首轮前置。该报告写作时 text→special→text 只有候选与故障注入测试；后续受控夹具实机证据另见当前 R2 执行记录。 |
| KNO-05、AR-06、J30 | **局部修复，未完成双租户验收**：来源撤权及不确定来源的 Gateway 自动化通过；真实双角色、双租户、分享/历史全矩阵未跑。 |
| KNO-01/06～12、EV-01、J09/J11～J13 | **未完成本轮验收**；保留 R2 程序待办，不以本批合入代替通过。 |

## 实际执行证据

- **真实 provider 与同任务跨存储**：专用私有库 `kb_34d201bc655345e3b204e94f7c8d38ec` 中合成文本 `r2-special-hierarchy-live.txt` 使用 DashScope `text-embedding-v4`。前两次有意重试暴露 manifest UUID 和状态缺陷，分别修复后第三次完成；IAB 切片页显示父 section、子 text 各一条。一次 UI 重处理完成后 PG `current_version=2`、2 条活动切片，Qdrant base 和 `_sections` 各一条，点 ID 与 PG 完全一致，无旧点。另用已有 E2E 账号经本地 API 恢复版本 #1，任务完成后 PG 当前版本 #4（含恢复前快照 #3）、2 条活动切片，两个 Qdrant 集合各一条且 ID 与 PG 一致。执行账本为一次初始失败、一次重试失败、一次重试成功、一次重处理成功、一次恢复成功；没有刷新自动新增执行。原始上传因 IAB 原生文件选择器不可用，使用认证本地 API；恢复也经本地 API，不能称为浏览器点击通过。
- **Codex 内置浏览器**：真实库在处理后及恢复后刷新，均仍显示文档“已完成”；成功切片截图 `tmp/r2-knowledge/hierarchy-live-success-slices.png`，二代和恢复后截图分别为 `tmp/r2-knowledge/hierarchy-second-generation-refresh.png`、`tmp/r2-knowledge/hierarchy-restore-refresh.png`。当前 Mac 锁屏导致后续标签与版本弹窗交互不稳定，召回测试、版本弹窗没有通过浏览器验收。
- **受控故障注入 + 真实 PG/Qdrant/本地对象存储**：`KB_SPECIAL_CROSS_STORE_TEST=1 QDRANT_URL=http://127.0.0.1:6333 uv run --all-packages --extra test pytest -q --no-cov tests/database/test_special_publication_cross_store.py` 测两代发布，以及 preparing、Qdrant upsert、PG commit 后、旧点部分清理处中断恢复；故障为注入，不是物理杀 Worker，也不是 provider E2E。
- **自动化**：最终代码 `make kb-unit-gate` **1993 passed、1 skipped**（跳过项缺 `KB_BACKFILL_POSTGRES_TEST_DSN`）；Gateway `tests/api/test_assistant_source_access.py` **46 passed**；`make verify-assistant-runtime-dev` **5 组通过**；真实跨存储故障注入 **1 passed**；文本恢复/特殊发布定向 **45 passed**。改动文件 Ruff、`make architecture-boundary-gate`、`make harness-check`、`git diff --check` 均通过。上一轮 1976/1 的数据不替代本批最终测试。
- **Docker**：先读 `docs/harness/runtime-and-secrets.md`，核对 Compose `working_dir` 为本仓库；Knowledge/Gateway 以 `make hot-update` 更新现有容器，不安装依赖、不重建镜像。Gateway 来源文件、Knowledge API Worker 和新文本候选/数据库文件的 host/container SHA256 对齐；`make status` 全部健康。

## 独立 review

独立只读 reviewer 发现普通文本候选误写 `level=3`，会被特殊点检测误分类；还发现操作员禁用的同位置段在 PG 保持禁用时 Qdrant 新点可能仍启用。已分别移除该层级字段、在发布租约内读取禁用位置并同步向量 payload，新增定向测试。reviewer 又指出普通重处理和旧式无回执版本恢复可能留下过期特殊来源回执，已在无安全版本路径时拒绝这两种操作；其完整正常路径仍属 R2 后续工作。reviewer 对三处修复再次只读复核，**未发现新的确定阻断项**；其定向复核 149 单测及 Ruff 通过，未执行真实跨存储禁用位置验收。

## 未验证与限制

公开扫描上传当前显式关闭，尚需验证拒绝说明和无副作用；物理 Worker 崩溃续接、真实 provider 失败保旧、真实 text→special→text、双租户撤权、J09～J13/J30 全矩阵在**本报告当时**均未通过。Mac 锁屏后浏览器按钮交互未能继续；最终 Knowledge 更新后的额外刷新也超时，前述刷新和截图均发生在最终两处准入修正之前，不能冒充最终页面复验。后续实机证据另行记录，完整 R2 状态继续保持 `in_progress`。
