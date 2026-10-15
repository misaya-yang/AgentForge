# KB 升级 T5 前端切片 → 后端依赖交付清单

- 日期：2026-08-29
- 分支：`worktree-kb-frontend`（基线 `worktree-kb-rag-upgrade@7a7fc3a`）
- 计划：`~/.claude/plans/iridescent-tickling-kahn.md`；进度账：
  `deploy/runbooks/kb-rag-ui-t5/loop-state.json`
- PRD：`docs/plans/rag-upgrade-prd-2026-08.md`（后端 worktree 未提交）

T5 前端切片（C1–C9）在本分支收口。以下依赖需要后端
（`worktree-kb-rag-upgrade`）落地；落地前前端均按"如实降级"处理，
不虚报能力。合并顺序按用户裁决：后端先合入 → 本分支 rebase 新
main → 修 API 对接 → 完整 KB+UI 实机回归（串行窗口）→ 合入本分支。

## 阻塞级

### D1 — `list_documents` SELECT 补列（阻塞 §5-#9/#4 全绿）

- 现状：列表 SQL 不取 `enabled, archived, parsing_started_at,
  splitting_started_at, indexing_started_at`（后端在途
  `database.py:2131` 一带）。
- 影响：列表行的启用/禁用/归档徽章与阶段耗时在 D1 前只对"刚操作过
  的行"准确（变更响应带全字段，前端已做缓存合并）。
- 前端兜底（已上线）：`resolveDisplayStatus` 双保险——有
  `display_status` 戳用戳，无则客户端按
  `archived > error > paused > completed(enabled?) > waiting > else`
  推导，未知态 fail-closed 到 indexing（`src/types/knowledge.ts`，
  单测锁全分支）；条件轮询也用同一解析器，D1 前后行为一致。
- 点亮方式：SELECT 补列即可，前端零改动；补上
  `display_status` 戳更佳（镜像 `derive_document_display_status`）。
- 验证：`e2e/knowledge-forward-contract.spec.ts` 的 fixture 就是 D1
  后的列表形状，mock 25/25 已绿，活栈复跑同一套件即可。

### D6 — ChunkingConfigSchema 字段集缺口

- 现状：`ChunkingConfigSchema`（extra=forbid）字段集 < `chunking.py`
  运行时字段集（`extract_metadata`、`metadata_fields`、
  `normalize_whitespace`、`strip_html`、`page_marker`、
  `segmentation` 等）——PUT /config 无法往返运行时字段。
- 影响：元数据抽取设置无法持久化；前端若原样回发会 422 整个配置更新。
- 前端兜底（C3 已上线）：`CHUNKING_CONFIG_API_FIELDS` allow-list 镜像
  schema 字段集（负向单测探针锁定），编辑保存只往返 schema 可接受
  字段；元数据抽取控件降级为"仅警告不可持久化"。
- 点亮方式：schema 扩宽后，前端同步放宽 allow-list（一次对齐即可）。

## 能力级

### D2 — `archived_reason` 列宽

- 现状：DB 列 VARCHAR(255)，schema 文档声称 2000。
- 前端处理：输入按 255 限流（`DOCUMENT_ARCHIVE_REASON_LIMIT`，
  Textarea maxLength+计数器），等后端加宽列或改文档后同步常量。

### D3 — 反馈表 + 端点（§5-#25）

- 现状：无表无端点。前端未做 👍/👎 UI，等端点后按 C 桶登记开工。

### D4 — 文档/段落列表分页参数

- 现状：后端无分页参数，硬上限 200/500。前端文案如实展示上限，
  等分页参数后接游标/页码。

### D5 — 迁移 100 遥测表的查询端点（§5-#26）

- 现状：无读端点。查询日志面板登记为等后端。

### D7 — 评测集用例持久删除（已接通）

- 后端：`DELETE /api/v1/eval/datasets/{dataset_id}/examples/{example_id}`；
  要求 eval run 权限，按 tenant + dataset + example 三重过滤。成功返回 204，
  缺失、重复删除与跨租户目标统一返回 404。
- 前端：工作台只删除用户在当前页面明确移除的已持久用例；新增/更新成功后
  串行执行删除，删除最后一个用例后保存按钮仍可用，刷新后不再恢复。
- 回归：`tests/api/test_eval_traces.py`、
  `tests/packages/ai_gateway_core/test_import_examples_dedup.py`、
  `web/src/pages/knowledge/detail/evalCaseStore.test.ts` 与
  `web/e2e/knowledge-eval-cases.spec.ts`。

### D8 — QA/hit-test 响应不暴露 `trace_id`

- 现状：KB 检索响应无 `trace_id`。
- 前端处理（C7 已上线）：一键送评测集走 `importEvalExamples`
  （skip_duplicates）+ 确定性 case_id（`kb-hit-{dataset}-{hash8}`，
  FNV-1a 空白归一），hit-test 与 QA 共用 id 空间、先送者胜。
- 点亮方式：响应暴露 `trace_id` 后，优先切
  `createEvalExampleFromTrace`（`kbEvalDataset.ts` 注释已固化）。

## 前端合同面（后端对接时可直接引用）

- 客户端：`src/api/knowledge.ts`——reindex/setDocumentEnabled/
  setDocumentArchived/batchReindex（`BatchReindexResult` 真实合同）/
  segments batch enable（≤500）/updateSegment 全字段（text/answer/
  keywords）/updateDatasetConfig（chunking REPLACE 全量、retrieval
  深补丁）。
- 前向渲染合同（C8/C9）：`display_status` 显示词表（7 值）、阶段
  时间戳（`parsing/splitting/indexing_started_at` →
  `documentStages.ts` 纯函数）、`hit_count` 文档/分块徽章
  （写入方属后端 T2，前端有值即显）。
- e2e 合同台架（全路由 mock，`pnpm dev` 即可跑）：
  `web/e2e/knowledge-{rag-eval,segment-ops,document-lifecycle,
  eval-cases,forward-contract}.spec.ts`，25/25 绿
  （`pnpm exec playwright test --config playwright.opensource.config.ts
  --grep @mock`）。
- 已知挂账：真实 `tsc -p tsconfig.app.json` 全库存量错误非本切片
  引入（详见 loop-state `type_check_gate_broken`）；`pnpm type-check`
  脚本本身检查 0 个文件，属共享基建，交用户裁决。
