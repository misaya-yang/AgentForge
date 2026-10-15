# i18n 全面审查报告 — 2026-08-19

**范围**: 全部 locale 文件（en-US/zh-CN 主 bundle + eval/agents 命名空间，共 6 个 JSON）+ `web/src` 全部页面/组件/布局
**方法**: 12 个并行 agent（5 个英文质量审查 + 6 个硬编码扫描 + 1 个静态 key 审计），230 条发现（32 high / 110 medium / 88 low）
**验证**: high 发现已逐条抽查文件与行号复核；en/zh key 对账（`i18n:check`）通过、bundle 间无 key 冲突、locale 切换链路基本健全

---

## 总体结论

i18n 基建（初始化、懒加载、切换队列）是健全的，en/zh 两个文件的 key 集合完全同步。但 **"页面都生效"这一目标没有达成**：问题集中在**新增功能没有同步添加 locale 条目** —— 大量 `t()` 调用把中文（或英文）写死在 defaultValue 里，导致默认 en-US 用户看到中文、zh-CN 用户看到英文。另有少数英文翻译存在误译和机翻腔。

**核心数字**：
- **227 个 key 在所有 bundle 中缺失**，靠 defaultValue 渲染（中英互漏）
- **72 处硬编码字符串**（连 `t()` 都没走）
- 主 bundle **846 个 dead keys**（31%），其中 `confluence.*` 278 个疑似已删除功能残留
- **99 处** `t(key, {count})` 缺少 `_one/_other` 复数形式 → en-US 下 "1 rows" 类错误

---

## P0 — 默认 en-US 下用户看到中文（必须修）

### 1. 完全未接入 i18n 的组件

| 文件 | 问题 |
| --- | --- |
| `pages/assistant/components/ConnectorsPanel.tsx` | 整个组件零 `useTranslation`，连接器面板全部中文硬编码（标题、aria-label、加载中、空态、激活/断开按钮、确认弹窗、错误回退） |

### 2. 缺失 key + 中文 defaultValue（新增功能未写 locale）

| 缺失 key 段 | 数量 | 影响 |
| --- | --- | --- |
| `assistant.quiz.*`（Quiz 流程） | 44 | Quiz 作答/提交/结果/分享全流程：en 用户看到中文（kbd 提示、错误 toast、状态徽章），zh 用户看到英文按钮（Previous/Next/Submit）——两侧都漏 |
| `knowledge.eval.*`（检索评测工作台） | 44 | 整个工作台中文默认值，en 用户看全中文；`markRelevant` 默认值还是中文 |
| `eval.workbench.*` | 19 | 评测结果面板在 zh 下显示英文默认值 |
| `assistant.activity.*`（活动时间线） | 13 | zh 下显示英文默认值 |
| `services.*`（ProviderStatusCard / ServiceConfigDialog / Services 概览） | 26 | 服务状态卡片、配置弹窗、概览条中英互漏 |
| `tasks.ribbon.*` + 硬编码值 | 4+4 | 任务页概览条：标签走 `t()` 但 key 缺失，值（正常/待处理任务/就绪/Worker 线程）完全硬编码 |
| `services.overview.*` + 硬编码值 | 4+6 | 服务页概览条：`${n} 在线`、`100% 可用`、`已接入`、`活跃模型`、`降级保障` 硬编码 |
| 其他零散 | ~60 | `common.debug`（调试按钮）、`knowledge.detail.tabEval`（评测 tab）、`knowledge.detail.retrievalPreset` + 硬编码 `· API 投影`、`knowledge.detail.image`、`dashboard.trend.*` 8 个、`playground.thinking.*` 5 个等 |

### 3. 硬编码中文（无 `t()` 路径）

- `pages/knowledge/DatasetDetail.tsx:3124` `{m.dimension}维`（embedding 模型选项的维度单位）
- `pages/knowledge/detail/DatasetUploadDialog.tsx:153` `PDF、Word、TXT、MD`；`:896` `{model.dimension}维`
- `pages/knowledge/detail/useDatasetUploadController.ts:31` `badge: "推荐"`（配置数据里的渲染文案）
- `pages/tasks/index.tsx`、`pages/Services.tsx` 概览条值（见上表）

### 4. eval 命名空间未在 /knowledge 路由预加载

`pages/knowledge/DatasetDetail.tsx:1881` 使用 `t(`eval.ragas.metrics.${metric}`)`，但 `loadTranslationNamespace('eval')` 只在 `/eval` 路由调用。直接打开知识库数据集详情（未先访问评测页）时，指标标签渲染为原始 token（如 `faithfulness`）。

---

## P1 — 英文翻译质量（流利度/误译）

**high（3 条）**：
- `settings.connectors.failed: "Request failed: {message}"` — 单大括号不插值，用户看到字面 `{message}`
- `playground.emptyStateHint` — 误译：zh"仍会保持同步" vs en"will stay available"（意思变了）
- `knowledge.create.useCaseRichText` — 误译：zh"图文并茂回复" vs en"Rich Text Response"（富文本 ≠ 图文并茂）

**代表性问题（medium/low，共 40+ 条流利度 + 20 条误译）**：
- Login hero 文案生硬（`brandTitle`"Operate models, knowledge, and delivery"、`brandDescription`"following every request through production"、`secureAccess`"Protected enterprise access"）
- dashboard 标签语义漂移：`performance.avg`"Average"（丢了"延迟"）、`requestTrace.detailHint`"safe metadata"（安全元数据误成"无害的元数据"）、`security.rateLimitedCount`"{{count}} rate limited"（缺名词）、`security.topUsers`"Top users"（zh 是"高频用户"）
- `tasks.states.initial`"Enter task ID to start query"（语法错误）
- 冠词缺失若干（"Please enter knowledge base name" 等）
- zh-CN 文件残留英文值 3 处（`playground.modelFailover` 等）

---

## P2 — 卫生与基建（可延后）

- **846 个 dead keys**（主 bundle 2718 个中占 31%）：`confluence.*` 278、`knowledge.*` 173、`tasks.*` 115、`assistant.*` 72……；eval bundle 18 个、agents bundle 17 个
- **99 处 count 复数缺失**：`t(key, {count})` 无 `_one/_other` → en-US "1 rows" 类错误
- `i18n/index.ts` 4 个小问题：初始化 promise 失败后不可恢复（需手动刷新）；`loadTranslationNamespace` 在 bundle 加载成功前就把命名空间标记为 active；`changeAppLanguage` 有冗余二次加载；`escapeValue: false` 全局关闭（目前安全，但需 lint 规则防回归）

---

## 修复建议（分批）

1. **P0-a（结构性，一次性）**：为 227 个缺失 key 补全 en/zh 条目（按上表 8 个 key 段集中添加），并把 3 处硬编码组件接入 `t()`。约 15 个文件，纯增量改动。
2. **P0-b**：DatasetDetail 挂载时调用 `loadTranslationNamespace('eval')`。
3. **P1**：按附录建议逐条修订英文文案（约 90 条，含 3 条 high）。
4. **P2**：删除 846 个 dead keys（先跑一次引用扫描确认）、补 99 处复数形式、修 `i18n/index.ts` 4 个点。
5. **防回归门禁**：给 `web/scripts/check-i18n-keys.mjs` 增加"代码引用 key ∉ locale 文件"和"`t()` 无 defaultValue 且 key 缺失"的检查，纳入 CI。

---

## 修复状态（2026-08-19 已执行，全部批次完成）

### Batch 1 — P0：key 补全 + 组件接线 ✅

- **277 个 key** 写入 6 个 locale 文件（230 个缺失 key + 47 个接线用新 key），就地按字母序插入
- 组件接线（3 个并行 agent）：
  - `ConnectorsPanel.tsx` 全组件接入 `useTranslation`（17 处中文硬编码 → `connectors.*`）
  - `QuizIdle.tsx` / `QuizResult.tsx`：isZh 三元 hack 移除、硬编码 `：` → `wrongCountSep`、8 处休眠中文默认值清除
  - `DatasetDetail.tsx` / `DatasetUploadDialog.tsx` / `useDatasetUploadController.ts`：`{n}维`、`PDF、Word、TXT、MD`、`推荐` badge、`· API 投影`、`：${err}` 拼接全部走 `t()`
  - `tasks/index.tsx` / `Services.tsx`：概览条 10 处值硬编码 → `tasks.ribbon.*` / `services.overview.*`
  - `ProviderStatusCard.tsx`：11 处语言三元 → `t()`（含模块级 helper 签名转换、空态诊断块、死变量 `isZh` 移除）
  - `UserManagement.tsx`：batchDelete 传 `{count}` 插值
- **eval 预加载**：`router.tsx` KnowledgeDatasetDetailPage 改用 `lazyNamedWithNamespace(..., "eval")`

### Batch 2 — P1：英文质量修订 ✅

- **119 条** en 值修订（3 high：`{message}` 单大括号、`emptyStateHint` 语义、`useCaseRichText` 误译）+ zh 残留英文 18 处翻译 + AppLayout 中文 fallback 清理。断言式替换（old 值不匹配则跳过），0 冲突。

### Batch 3 — P2：卫生 + 门禁 ✅

- **58 个** count 复数 base 补 `_one/_other`（en 区分单复数，zh 同值）→ "1 rows" 类错误根治
- **967 个 dead keys 删除**（1934 条 en/zh 条目；61 个候选被"全库字符串搜索"安全网救回——三元/config 数组引用的 key 正确保留）；空父对象一并剪除
- `i18n/index.ts` 4 修：初始化失败可恢复（`.catch` 重置缓存）、命名空间加载成功后才标记 active、`changeAppLanguage` 冗余二次 ensure 移除、`escapeValue:false` 加注释
- **防回归门禁**：`web/scripts/check-i18n-keys.mjs` 新增"代码引用 key ∉ locale 文件"检查（已接入 CI，`pnpm -C web i18n:check`），首跑即通过

### 验证

- `pnpm i18n:check` ✅（parity + 代码引用门禁）
- `pnpm type-check`（tsc --noEmit）✅
- `pnpm lint`（eslint . 全量）✅ 零告警
- `pnpm build` ✅（vite build + bundle budget，`oversizedRoutes: []`）
- 接线文件 CJK 残留终扫 ✅：仅注释与休眠三元默认参数（key 已存在，永不渲染）

**遗留（有意保留）**：`ProviderStatusCard.tsx` 中 783/803/851-858/928 等处的 `t(key, 语言三元默认值)` 模式——key 已存在、默认值休眠，功能正确，属风格问题，未改动。

---

## 附录

完整 230 条发现（按 agent 分组、按严重度排序）见下文。

## web/src/i18n/locales/en-US.json (lines 1-1100) vs zh-CN.json

Reviewed lines 1-1100 of en-US.json against the key-matched zh-CN.json values. Overall English quality is good: no Chinese punctuation or full-width characters in en values, no mixed-language leaks, and placeholder names match between locales everywhere except one single-brace defect. Findings are concentrated in login hero copy (awkward 'delivery' phrasing), a few dashboard labels that drift from the zh meaning (average latency, security metadata, rate-limited count, top users), one broken interpolation that will render a literal '{message}', and several missing-article/missing-"the" fluency slips. 18 findings total: 1 high, 5 medium, 12 low.
### 🔴 HIGH

- **`settings.connectors.failed`** · placeholder-mismatch · web/src/i18n/locales/en-US.json:1024
- 问题: Single-brace "{message}" will not be interpolated — i18next is initialized with the default {{}} prefix/suffix (web/src/i18n/index.ts:95), so users see the literal token "Request failed: {message}".
- 建议: Request failed: {{message}}

### 🟠 MEDIUM

- **`dashboard.security.rateLimitedCount`** · fluency · web/src/i18n/locales/en-US.json:459
- 问题: "{{count}} rate limited" is missing a noun — "3 rate limited" is visibly broken English, unlike its sibling "{{count}} auth failures" (zh: {{count}} 次限流).
- 建议: {{count}} rate-limited requests

### 🟠 MEDIUM

- **`login.brandTitle`** · mistranslation · web/src/i18n/locales/en-US.json:251
- 问题: "Operate models, knowledge, and delivery from one console." is awkward verb-object copy and drops the zh nuance "交付质量" (delivery quality; zh: 统一管理模型服务、知识资产与交付质量); "models" should be "model services".
- 建议: Manage model services, knowledge assets, and delivery quality from one console.

### 🟠 MEDIUM

- **`dashboard.performance.avg`** · mistranslation · web/src/i18n/locales/en-US.json:435
- 问题: "Average" is ambiguous as a metric label — the zh value is 平均延迟 (average latency), so the English dropped the quantity being averaged.
- 建议: Avg latency

### 🟠 MEDIUM

- **`dashboard.requestTrace.detailHint`** · mistranslation · web/src/i18n/locales/en-US.json:508
- 问题: "safe metadata" misrenders zh 安全元数据 (security metadata) — "safe" implies harmless rather than security-related, which changes the meaning.
- 建议: Stage latency, tokens, cost and security metadata

### 🟠 MEDIUM

- **`tasks.states.initial`** · fluency · web/src/i18n/locales/en-US.json:712
- 问题: "Enter task ID to start query" uses a bare noun after "start" — ungrammatical; should be "to start querying" (zh: 输入任务 ID 开始查询).
- 建议: Enter a task ID to start querying

### 🟡 LOW

- **`login.highlights.observability.title`** · mistranslation · web/src/i18n/locales/en-US.json:281
- 问题: "Traceable delivery" is an odd abstract phrase — "delivery" of what? The zh is 全程可追踪 (traceable end-to-end), which the English does not convey.
- 建议: End-to-end traceability

### 🟡 LOW

- **`login.brandDescription`** · fluency · web/src/i18n/locales/en-US.json:252
- 问题: "following every request through production" is awkward — you track/monitor requests in production; zh is 追踪每一次生产请求 (track every production request).
- 建议: A focused workspace for configuring services, maintaining trusted knowledge, and tracking every request in production.

### 🟡 LOW

- **`login.secureAccess`** · fluency · web/src/i18n/locales/en-US.json:253
- 问题: "Protected enterprise access" is stilted — zh 企业级安全访问 means enterprise-grade secure access.
- 建议: Enterprise-grade secure access

### 🟡 LOW

- **`settings.apiKeys.newKeyWarning`** · punctuation · web/src/i18n/locales/en-US.json:963
- 问题: Comma splice joining two independent clauses, and mid-sentence "API Key" is unnecessarily capitalized.
- 建议: Please save this API key — it won't be shown again!

### 🟡 LOW

- **`knowledge.create.nameRequired`** · fluency · web/src/i18n/locales/en-US.json:1084
- 问题: Missing article: "Please enter knowledge base name" should be "the knowledge base name" (zh: 请输入知识库名称).
- 建议: Please enter the knowledge base name

### 🟡 LOW

- **`users.dialogs.resetPasswordMessage`** · fluency · web/src/i18n/locales/en-US.json:678
- 问题: Missing article: "reset password for user" should be "reset the password for user".
- 建议: Are you sure you want to reset the password for user "{{name}}"?

### 🟡 LOW

- **`tasks.confluence.pageManage.hint`** · fluency · web/src/i18n/locales/en-US.json:852
- 问题: Missing article: "during next sync" should be "during the next sync".
- 建议: Disabled pages will be skipped during the next sync without affecting existing synced content

### 🟡 LOW

- **`user.departments.tech`** · mistranslation · web/src/i18n/locales/en-US.json:244
- 问题: "Engineering" for zh 技术 (Technology/Technical) is a meaning drift in the department name — the zh says "Technical", not "Engineering".
- 建议: Technology

### 🟡 LOW

- **`dashboard.security.topUsers`** · mistranslation · web/src/i18n/locales/en-US.json:461
- 问题: "Top users" is vague about the ranking criterion; zh 高频用户 means high-frequency users (those repeatedly triggering auth/rate-limit events).
- 建议: Frequent users

### 🟡 LOW

- **`login.highlights.knowledge.description`** · mistranslation · web/src/i18n/locales/en-US.json:278
- 问题: "with visible retrieval controls" drifts from zh 清晰掌控检索配置 (with clear control over retrieval configuration) — visibility is not the same as control.
- 建议: Maintain datasets and connected sources with clear control over retrieval settings.

### 🟡 LOW

- **`settings.auth.jwt.description`** · fluency · web/src/i18n/locales/en-US.json:897
- 问题: "JWT Token" is redundant (JWT = JSON Web Token) and inconsistent with the sibling key settings.auth.jwt.title "JWT Authentication".
- 建议: Authenticate using JWT tokens

### 🟡 LOW

- **`settings.connectors.field.modeHint`** · fluency · web/src/i18n/locales/en-US.json:1000
- 问题: "both = both" is circular and reads as a placeholder; zh is "both = 两者" (both live and ingest).
- 建议: live = agent tool calls; ingest = KB sync source; both = live and ingest


## web/src/i18n/locales/en-US.json lines 1101-2200 (knowledge, playground, help, confluence sections) vs zh-CN.json

Reviewed lines 1101-2200 of en-US.json against zh-CN.json. Overall the English is solid, but found 2 clear mistranslations (emptyStateHint, useCaseRichText), several visibly broken/MT-like strings (confirmReindex "Confirm rebuild index?", urlTitlePlaceholder "auto use URL", chunkHeadingDesc, chunkRecursiveHint, rollbackDesc), a recurring "{n} success, {n} failed" noun/verb mismatch in upload toasts, literal translations ("Auto extract...", "Question Prefix" vs zh 问题标识符=identifier), a comma-splice family in perm*Hint, one label inconsistency (hierarchical "Hierarchical" vs "Parent-Child" elsewhere), British spelling "cancelled" in an en-US locale, and untranslated English values in the zh-CN file (playground.modelFailover/errors). Placeholder names all match between locales; no Chinese punctuation or double spaces found in the en values.
### 🔴 HIGH

- **`playground.emptyStateHint`** · mistranslation · web/src/i18n/locales/en-US.json:2003
- 问题: zh 刷新页面后，对话历史和当前草稿仍会保持同步 means 'will stay in sync', but the en value says 'will stay available', a wrong meaning (available vs. in sync).
- 建议: Conversation history and your current draft will stay in sync after a refresh.

### 🔴 HIGH

- **`knowledge.create.useCaseRichText`** · mistranslation · web/src/i18n/locales/en-US.json:1124
- 问题: zh 图文并茂回复 means 'response combining text and images', but 'Rich Text Response' describes formatted text and misleads users choosing a use-case type.
- 建议: Response with Text & Images

### 🟠 MEDIUM

- **`knowledge.detail.documentRow.confirmReindex`** · fluency · web/src/i18n/locales/en-US.json:1725
- 问题: 'Confirm rebuild index?' is ungrammatical (bare verb after 'Confirm'); zh is 确认重新构建索引？.
- 建议: Confirm index rebuild?

### 🟠 MEDIUM

- **`knowledge.detail.urlTitlePlaceholder`** · fluency · web/src/i18n/locales/en-US.json:1662
- 问题: 'Leave empty to auto use URL as title' uses 'auto use' (non-native) and missing articles.
- 建议: Leave empty to automatically use the URL as the title

### 🟠 MEDIUM

- **`knowledge.create.chunkHeadingDesc`** · fluency · web/src/i18n/locales/en-US.json:1142
- 问题: 'requires content under different heading levels not to mix' reads like machine translation (zh: 要求不同级标题下的内容不会混杂).
- 建议: Suitable for documents where headings divide independent topics, provided that content under different heading levels does not mix

### 🟠 MEDIUM

- **`knowledge.versionHistory.rollbackDesc`** · fluency · web/src/i18n/locales/en-US.json:1241
- 问题: 'to version #{{version}} content' is a missing-article/awkward possessive; a native speaker would write 'to the content of version #{{version}}'.
- 建议: This will restore document "{{title}}" to the content of version #{{version}}.

### 🟠 MEDIUM

- **`knowledge.detail.chunkRecursiveHint`** · fluency · web/src/i18n/locales/en-US.json:1627
- 问题: 'splits by paragraph, then sentence progressively, ensuring each block stays within limit' drops the parallel 'by' and the article before 'limit' (zh: 按段落、句子逐级细分，确保每个块不超过限制).
- 建议: Recursive chunking splits by paragraph, then by sentence, progressively narrowing until each block is within the size limit

### 🟠 MEDIUM

- **`knowledge.detail.imageUploadDone / uploadDone (1695)`** · fluency · web/src/i18n/locales/en-US.json:1691
- 问题: '{{success}} success, {{failed}} failed' misuses the noun 'success' as a count adjective (zh: {{success}} 成功, {{failed}} 失败); '2 success, 1 failed' is visibly wrong in a toast.
- 建议: Image upload complete: {{success}} succeeded, {{failed}} failed  (same fix for uploadDone: 'Upload complete: {{success}} succeeded, {{failed}} failed')

### 🟠 MEDIUM

- **`knowledge.versionHistory.changeRestored`** · mistranslation · web/src/i18n/locales/en-US.json:1237
- 问题: zh 回滚 means 'rollback', not 'restore'; 'Restored' drifts from the source and clashes with the rollback terminology used by sibling keys (rollback, confirmRollback, confirmRollbackBtn).
- 建议: Rolled Back

### 🟠 MEDIUM

- **`knowledge.detail.questionPrefix / answerPrefix (1630)`** · mistranslation · web/src/i18n/locales/en-US.json:1629
- 问题: zh 问题标识符/答案标识符 means 'Question/Answer identifier', so 'Prefix' is a meaning drift for the QA chunking markers.
- 建议: Question Identifier  /  Answer Identifier

### 🟠 MEDIUM

- **`knowledge.create.rerankModelHint`** · fluency · web/src/i18n/locales/en-US.json:1164
- 问题: 'Use rerank model to optimize relevance ranking' is missing the article and 'relevance ranking' needs 'the'.
- 建议: Use a rerank model to optimize the relevance ranking of retrieval results

### 🟠 MEDIUM

- **`zh-CN.json playground.modelFailover + playground.errors (zh lines 2022-2041)`** · mixed-language · web/src/i18n/locales/en-US.json:2022
- 问题: The zh-CN file leaves playground.modelFailover.* and playground.errors.* entirely in English ('Model fallback is active...', 'Request was cancelled.'...), so Chinese users see English error messages; the en file is fine here.
- 建议: Translate the modelFailover/errors block in zh-CN.json (e.g. selected: '模型回退已生效。网关已将该响应切换到 {{provider}} / {{model}}。', cancelled: '请求已取消。'); en values need no change.

### 🟡 LOW

- **`knowledge.detail.segment.truncated`** · fluency · web/src/i18n/locales/en-US.json:1750
- 问题: 'click expand to view all' is missing 'to' after 'click' (zh: 点击展开查看全部).
- 建议: {{count}} chars total, click to expand and view all

### 🟡 LOW

- **`knowledge.detail.permPrivateHint (also permTenantHint 1551, permPublicHint 1552)`** · fluency · web/src/i18n/locales/en-US.json:1550
- 问题: Comma splice without a conjunction and missing article: 'Private knowledge base is only accessible to the creator, can be used in...'; the sibling permTenantHint/permPublicHint have the same construction.
- 建议: A private knowledge base is only accessible to the creator and can be used in the AI assistant and LangGraph.  (parallel fix for 1551: '...same tenant and can be used...', 1552: '...all users, and anyone can use it...')

### 🟡 LOW

- **`knowledge.create.chunkFixedSizeDesc`** · fluency · web/src/i18n/locales/en-US.json:1138
- 问题: 'models with smaller context length' is awkward; native usage is 'shorter context windows' (zh: 上下文长度较小的模型).
- 建议: Suitable for scenarios with strict token count requirements, e.g., when using models with shorter context windows

### 🟡 LOW

- **`knowledge.detail.qaStartHint`** · fluency · web/src/i18n/locales/en-US.json:1378
- 问题: 'trigger retrieval and model answer, supports streaming' is a comma splice with a missing article ('a model answer').
- 建议: Enter a question to trigger retrieval and a model answer, with support for streaming and reference segments

### 🟡 LOW

- **`knowledge.detail.extractTitle (also extractSummary 1635, extractEntities 1637, detectLanguage 1638)`** · fluency · web/src/i18n/locales/en-US.json:1634
- 问题: 'Auto extract document title' / 'Auto generate summary' use 'auto' as an adverb, which is non-native; native English uses 'Automatically...'.
- 建议: Automatically extract the document title  (1635: 'Automatically generate a summary', 1637: 'Automatically identify named entities', 1638: 'Automatically detect language')

### 🟡 LOW

- **`knowledge.detail.regexHint`** · fluency · web/src/i18n/locales/en-US.json:1620
- 问题: 'use regular pattern to delete matched content' is missing the article and is choppy (zh: 使用普通模式则删除匹配内容).
- 建议: Use a lookahead (?=...) to keep the matched content, or a regular pattern to remove it

### 🟡 LOW

- **`knowledge.detail.chunkModeLabels.hierarchical`** · inconsistent · web/src/i18n/locales/en-US.json:1435
- 问题: The hierarchical mode is labeled 'Hierarchical' here but 'Parent-Child' everywhere else (chunkHierarchical 1145, chunkHierarchicalHint 1404, hierarchical 1414, uploadChunkModes.hierarchical 1589); zh consistently uses 父子 for it.
- 建议: Parent-Child

### 🟡 LOW

- **`playground.activity.statusCancelled`** · inconsistent · web/src/i18n/locales/en-US.json:1961
- 问题: 'cancelled' uses British spelling in an en-US locale ('canceled' is the US form).
- 建议: canceled


## web/src/i18n/locales/en-US.json lines 2201–3258 (confluence, assistant, agent, artifact, services, models, providers, llm, quota)

Reviewed 1058 lines of en-US.json (confluence through quota sections) against zh-CN.json. Overall quality is good: no placeholder mismatches, no Chinese characters or full-width punctuation in en values. The issues cluster around (a) service-registration copy with comma splices and missing articles, (b) a few clipped/literal labels, and (c) terminology inconsistency for the same zh concepts (sync mode/interval rendered 3 different ways). No high-severity (meaning-changing) defects found; 6 medium and 13 low findings below.
### 🟠 MEDIUM

- **`services.langgraph.description`** · fluency · web/src/i18n/locales/en-US.json:2844
- 问题: Comma splice with missing articles: "Fill in only two required fields, gateway will auto-configure transparent proxy" reads as machine-translated copy on a visible registration card.
- 建议: Fill in only two required fields; the gateway will auto-configure a transparent proxy.

### 🟠 MEDIUM

- **`services.page.noServices`** · fluency · web/src/i18n/locales/en-US.json:3063
- 问题: "Click button above to add." is missing the definite article — visibly awkward for native speakers (same problem at noProviders, line 3064).
- 建议: No services found. Click the button above to add.

### 🟠 MEDIUM

- **`confluence.bind.selectAllRoots`** · fluency · web/src/i18n/locales/en-US.json:2413
- 问题: Button label "Select All Root" is ungrammatical/incomplete; zh 全选顶级 means "select all top-level (pages)".
- 建议: Select All Root Pages

### 🟠 MEDIUM

- **`confluence.syncConfig.pollingDesc`** · fluency · web/src/i18n/locales/en-US.json:2250
- 问题: Missing article and dropped nuance: zh 定时检测 (scheduled/periodic detection) is lost in "System automatically detects Confluence changes and syncs".
- 建议: The system automatically detects Confluence changes on a schedule and syncs them.

### 🟠 MEDIUM

- **`llm.model.catalogModelHint`** · fluency · web/src/i18n/locales/en-US.json:3183
- 问题: "Choosing a catalog model fills capability, context, and pricing defaults" — "fills capability" is non-native; a catalog model populates/fills in defaults.
- 建议: Choosing a catalog model fills in the default capability, context, and pricing values.

### 🟠 MEDIUM

- **`llm.model.templateProviderHint`** · fluency · web/src/i18n/locales/en-US.json:3188
- 问题: "before using it in runtime" — "in runtime" is a direct non-native calque; English uses "at runtime".
- 建议: Template entry. Save the provider credentials before using it at runtime.

### 🟡 LOW

- **`services.langgraph.deploymentUrlDesc`** · fluency · web/src/i18n/locales/en-US.json:2860
- 问题: Sentence fragment "LangGraph service deployment address, local or cloud" — the apposition reads clipped.
- 建议: Deployment address of the LangGraph service, either local or cloud.

### 🟡 LOW

- **`services.langgraph.graphIdDesc`** · fluency · web/src/i18n/locales/en-US.json:2862
- 问题: "Graph name or Assistant ID, auto-injected when calling" — dangling participial phrase; zh 用于调用时自动注入 is smoother.
- 建议: Graph name or Assistant ID, injected automatically on each call.

### 🟡 LOW

- **`services.langgraph.yaml.loadStrategy`** · fluency · web/src/i18n/locales/en-US.json:2861
- 问题: "Load balance strategy" should be "Load balancing strategy"; also inconsistent with "Load Balancing" used at configDialog.basic.loadBalanceStrategy (line 2936) for the same zh 负载均衡.
- 建议: Load balancing strategy

### 🟡 LOW

- **`services.configDialog.capacity.queueTimeoutHint`** · fluency · web/src/i18n/locales/en-US.json:2992
- 问题: "How long a queued request may wait before 503" — truncated; the 503 needs a verb.
- 建议: How long a queued request may wait before returning 503.

### 🟡 LOW

- **`confluence.syncConfig.manualDesc`** · fluency · web/src/i18n/locales/en-US.json:2249
- 问题: Comma splice: "Trigger sync manually when needed, full control over timing".
- 建议: Trigger sync manually when needed for full control over timing.

### 🟡 LOW

- **`services.langgraph.langsmithApiKeyDesc`** · fluency · web/src/i18n/locales/en-US.json:2850
- 问题: Fragment "Not required for local deployment, only needed for cloud LangGraph" reads clipped for a paragraph-level hint.
- 建议: Not required for local deployments; only needed for cloud LangGraph.

### 🟡 LOW

- **`assistant.welcomeWhy`** · fluency · web/src/i18n/locales/en-US.json:2542
- 问题: "This assistant talks to the model services" — "talks to" is an odd word choice for connecting to a backend service (zh 连接).
- 建议: This assistant connects to the model services you configure in the console.

### 🟡 LOW

- **`assistant.outline.goal`** · fluency · web/src/i18n/locales/en-US.json:2728
- 问题: "Generate {{format}} document" lacks the article; also reads awkwardly when the format is a proper noun (e.g. "Generate PDF document").
- 建议: Generate a {{format}} document.

### 🟡 LOW

- **`assistant.kbActive`** · fluency · web/src/i18n/locales/en-US.json:2539
- 问题: "knowledge base(s) active" — the parenthesized "(s)" plural hack looks like a placeholder left in a user-facing string.
- 建议: knowledge bases active

### 🟡 LOW

- **`assistant.localFilesLoading`** · punctuation · web/src/i18n/locales/en-US.json:2580
- 问题: Uses the single-character ellipsis "…" while every other loading string in the file uses three dots ("..."), creating inconsistent typography.
- 建议: Local files...

### 🟡 LOW

- **`confluence.bind.maxImageSize`** · inconsistent · web/src/i18n/locales/en-US.json:2436
- 问题: "Max image size" uses sentence case while the sibling options in the same list are Title Case ("Include Attachments", "Sync Images", "Include Comments").
- 建议: Max Image Size

### 🟡 LOW

- **`confluence.syncConfig.interval`** · inconsistent · web/src/i18n/locales/en-US.json:2251
- 问题: The same zh concept 同步间隔 is rendered three ways: "Sync Interval" here, "Polling Interval" at create.pollingInterval (line 2362), and "Polling Interval (minutes)" at form.pollingInterval (line 2325).
- 建议: Standardize on "Sync Interval" (and "Sync Interval (minutes)") across all three keys.

### 🟡 LOW

- **`confluence.syncMode.polling`** · inconsistent · web/src/i18n/locales/en-US.json:2240
- 问题: The same polling/scheduled sync mode is named "Scheduled Polling" here, "Auto Sync" at syncConfig.polling (line 2248), and "Automatic" at create.syncModeAuto (line 2360).
- 建议: Standardize on one term, e.g. "Auto Sync" everywhere (zh 自动同步/定时轮询).


## eval locale — web/src/i18n/locales/eval-en-US.json (vs eval-zh-CN.json)

Reviewed all 560 lines of eval-en-US.json against eval-zh-CN.json. No high-severity defects: all {{placeholders}} match between locales, no Chinese characters/punctuation in English values, no mixed-language leakage. Found 4 medium issues (wrong concept word "bounded" for zh 脱敏预览 "redacted previews"; ungrammatical "Scored in view"; "Failures" losing zh 阻断原因 "blocking reasons"; missing article in the ragas no-evaluator sentence) and 10 low issues (en-US spelling of "Cancelled", dropped "rate"/"type"/"credibility" nuances, the "(s)"/"(es)" plural hack, and trailing-punctuation/capitalization inconsistencies).
### 🟠 MEDIUM

- **`description`** · mistranslation · /Users/yang/projects/AI--Platfform/web/src/i18n/locales/eval-en-US.json:4
- 问题: zh 脱敏预览 means "redacted previews", but the en value says "bounded previews" — "bounded" (有界) is a different concept used elsewhere in this file (runtime.schema "bounded runtime evidence"), and the en value also drops zh's 回归沉淀 (regression accumulation) while adding "human scoring" that zh does not mention.
- 建议: "Review assistant, LangGraph proxy, and RAG traces with redacted previews, human scoring, and regression cases."

### 🟠 MEDIUM

- **`ragas.summary.scoredTraces`** · fluency · /Users/yang/projects/AI--Platfform/web/src/i18n/locales/eval-en-US.json:335
- 问题: "Scored in view" is a broken noun phrase (zh 当前视图已评分 = "scored in the current view"); a native speaker would write "Scored in this view".
- 建议: "Scored in this view"

### 🟠 MEDIUM

- **`comparison.failures`** · mistranslation · /Users/yang/projects/AI--Platfform/web/src/i18n/locales/eval-en-US.json:286
- 问题: zh 阻断原因 means "blocking reasons", but the en value is the generic "Failures" (also duplicated from workbench.failures 失败项), losing both the "blocking" and "reasons" nuance of what this section lists.
- 建议: "Blocking reasons"

### 🟠 MEDIUM

- **`ragas.noEvaluatorDescription`** · fluency · /Users/yang/projects/AI--Platfform/web/src/i18n/locales/eval-en-US.json:330
- 问题: "online sampling on trace family rag" reads unnaturally — the definite article and the word "trace" are missing (zh: 开启 trace family 为 rag 的在线采样).
- 建议: "Create a ragas evaluator with online sampling on the rag trace family to score retrieval automatically."

### 🟡 LOW

- **`status.cancelled`** · inconsistent · /Users/yang/projects/AI--Platfform/web/src/i18n/locales/eval-en-US.json:423
- 问题: The default locale is en-US, where the standard spelling is "Canceled" (single l); "Cancelled" is en-GB.
- 建议: "Canceled"

### 🟡 LOW

- **`workbench.executionErrorRate`** · mistranslation · /Users/yang/projects/AI--Platfform/web/src/i18n/locales/eval-en-US.json:219
- 问题: zh 执行错误率 is "execution error rate", but the en value "Execution errors" drops "rate", reading as a raw count, and is inconsistent with comparison.metrics.execution_error_rate rendered as "Error rate" for the same metric.
- 建议: "Execution error rate"

### 🟡 LOW

- **`workbench.trajectoryPass / criticalPass (119) / behaviorPassRate (214)`** · inconsistent · /Users/yang/projects/AI--Platfform/web/src/i18n/locales/eval-en-US.json:60
- 问题: zh renders these as ...通过率 (pass rate) for all three, but en mixes forms: "Trajectory pass" and "Critical pass" drop "rate" while "Behavior pass" also omits it — all three are percentage metrics and should be parallel.
- 建议: "Trajectory pass rate" / "Critical pass rate" / "Behavior pass rate"

### 🟡 LOW

- **`toolChanges, ragChanges (294), batchQueued (360), caseCount (541), batchPlan (542), validationFailed (546), validationPassed (547), importSuccess (555)`** · fluency · /Users/yang/projects/AI--Platfform/web/src/i18n/locales/eval-en-US.json:293
- 问题: The "{{count}} ...(s)" / "(es)" pattern is translation-ese a native writer would not produce; i18next plural keys (_one/_other) are the standard fix, e.g. "1 tool change" vs "3 tool changes".
- 建议: Use i18next plural suffixes, e.g. "{{count}} tool change" / "{{count}} tool changes"; "Queued {{queued}} traces, skipped {{skipped}}"; "{{count}} cases parsed"

### 🟡 LOW

- **`comparison.attribution`** · mistranslation · /Users/yang/projects/AI--Platfform/web/src/i18n/locales/eval-en-US.json:278
- 问题: zh 归因可信度 is "attribution credibility/confidence", but en "Attribution" alone reads as attribution mapping and drops the credibility nuance.
- 建议: "Attribution credibility"

### 🟡 LOW

- **`behavior.toolMode`** · mistranslation · /Users/yang/projects/AI--Platfform/web/src/i18n/locales/eval-en-US.json:239
- 问题: zh 预期类型 is "expectation type", but en "Expectation" alone is vague as a field label for the Required/Forbidden mode picker.
- 建议: "Expectation type"

### 🟡 LOW

- **`detail.loading`** · inconsistent · /Users/yang/projects/AI--Platfform/web/src/i18n/locales/eval-en-US.json:429
- 问题: "Loading trace detail" lacks the trailing ellipsis used by its siblings loadingRun "Loading run…" (209) and ragas.loadingDetail "Loading retrieval trace detail…" (320).
- 建议: "Loading trace detail…"

### 🟡 LOW

- **`score.form.scoreNameRequired`** · punctuation · /Users/yang/projects/AI--Platfform/web/src/i18n/locales/eval-en-US.json:509
- 问题: Validation messages inconsistently end with a period: "Case ID is required." (263) and "A runnable user message is required." (264) have one, but "Score name is required" (509) and "Dataset ID is required for batch scoring" (359) do not.
- 建议: "Score name is required." (and "Dataset ID is required for batch scoring.")

### 🟡 LOW

- **`detail.runtime.resumeReady`** · mistranslation · /Users/yang/projects/AI--Platfform/web/src/i18n/locales/eval-en-US.json:463
- 问题: zh 可恢复 means "resumable" (a property of the trace), but en "Resume ready" describes a readiness state and sounds like an imperative.
- 建议: "Resumable"

### 🟡 LOW

- **`workbench.promoteToGolden / addToReview (78) / createFailureCase (79)`** · inconsistent · /Users/yang/projects/AI--Platfform/web/src/i18n/locales/eval-en-US.json:77
- 问题: These three action labels use Title Case ("Promote to Golden", "Add to Review", "Create Failure Case") while every sibling action uses sentence case ("Add trace to dataset", "Create dataset", "Queue evaluator"), breaking the label capitalization system.
- 建议: "Promote to golden" / "Add to review" / "Create failure case" (keep Title Case only if these are rendered as styled buttons and the others are not)


## agents locale (agents-en-US.json vs agents-zh-CN.json)

Reviewed all 655 lines of agents-en-US.json against agents-zh-CN.json. No placeholder mismatches, Chinese punctuation, or full-width characters found; the English is mostly fluent. Main issues: (1) the analytics page title drops "governance" that the zh title explicitly carries; (2) a few machine-translation-style phrasings ("No publication is disabled in AS-05", "Read through the explicit Preview credential principal"); (3) dropped nuance in several keys (positiveFeedback missing "rate", noDatasetTitle missing "evaluation", retentionDescription "Durable" vs "immutable"); (4) recurring capitalization/terminology inconsistency for the same concepts (Workspace dataset vs Workspace Dataset, All Versions vs All channels, choices vs selections, versions vs Versions). 5 medium, 10 low findings; none high.
### 🟠 MEDIUM

- **`analytics.title`** · mistranslation · /Users/yang/projects/AI--Platfform/web/src/i18n/locales/agents-en-US.json:526
- 问题: Title reads 'Analytics' but the zh value is '分析与治理' (Analytics and governance), silently dropping a domain the zh title promises even though the subtitle still lists data governance.
- 建议: Analytics & Governance

### 🟠 MEDIUM

- **`list.archiveConfirmDescription`** · fluency · /Users/yang/projects/AI--Platfform/web/src/i18n/locales/agents-en-US.json:63
- 问题: The second sentence 'No publication is disabled in AS-05.' is a literal passive rendering of 'AS-05 不会停用任何发布' that reads like spec-checklist English in a user-facing confirmation dialog.
- 建议: The Agent becomes read-only and remains available for audit and rollback. AS-05 does not disable any publication.

### 🟠 MEDIUM

- **`create.catalogs.connectorReady`** · fluency · /Users/yang/projects/AI--Platfform/web/src/i18n/locales/agents-en-US.json:152
- 问题: 'Read through the explicit Preview credential principal' reads as 'peruse the principal' rather than 'access happens via this principal' (zh: 通过明确的预览凭证主体读取); same phrase recurs at line 261.
- 建议: Reads via the explicit Preview credential principal

### 🟠 MEDIUM

- **`analytics.positiveFeedback`** · mistranslation · /Users/yang/projects/AI--Platfform/web/src/i18n/locales/agents-en-US.json:551
- 问题: zh is '正向反馈率' (positive feedback RATE); the en drops 'rate' and is inconsistent with its sibling metric labels (Success rate, Tool success rate, Knowledge hit rate).
- 建议: Positive feedback rate

### 🟠 MEDIUM

- **`analytics.operationsDescription`** · fluency · /Users/yang/projects/AI--Platfform/web/src/i18n/locales/agents-en-US.json:596
- 问题: 'Each sensitive operation is audited and preserves immutable release history' grammatically makes the operation the subject that preserves history (zh: 每项敏感操作均写入审计，并保留不可变发布历史).
- 建议: Each sensitive operation is audited, and immutable release history is preserved.

### 🟡 LOW

- **`analytics.retentionDescription`** · mistranslation · /Users/yang/projects/AI--Platfform/web/src/i18n/locales/agents-en-US.json:576
- 问题: 'Durable history' renders '不可变历史' (immutable history), a different concept and inconsistent with the product's everywhere-else 'immutable' terminology; the zh '仅' (only) is also dropped.
- 建议: Immutable history stays intact; these settings control only runtime and derived data.

### 🟡 LOW

- **`studio.release.noDatasetTitle`** · mistranslation · /Users/yang/projects/AI--Platfform/web/src/i18n/locales/agents-en-US.json:330
- 问题: zh is '尚未配置生产评测 Dataset' (no production EVALUATION dataset configured); the en drops 'evaluation', which matters because this title heads the release-evaluation gate.
- 建议: No production evaluation dataset is configured

### 🟡 LOW

- **`list.supportTemplate`** · mistranslation · /Users/yang/projects/AI--Platfform/web/src/i18n/locales/agents-en-US.json:47
- 问题: 'Support template' drops '分流' from '支持分流模板' (triage), and line 48 'Knowledge template' drops '向导' from '知识向导模板' (guide), so the list labels no longer match the templates' own titles (Support triage / Knowledge guide).
- 建议: Support triage template (line 48: Knowledge guide template)

### 🟡 LOW

- **`create.catalogs.workspaceDataset`** · inconsistent · /Users/yang/projects/AI--Platfform/web/src/i18n/locales/agents-en-US.json:154
- 问题: Same label rendered 'Workspace dataset' here but 'Workspace Dataset' at studio.knowledge.workspaceDataset (line 278); capitalization of the 'Dataset' product concept is inconsistent between the two files.
- 建议: Workspace Dataset (unify both occurrences)

### 🟡 LOW

- **`analytics.allVersions`** · inconsistent · /Users/yang/projects/AI--Platfform/web/src/i18n/locales/agents-en-US.json:535
- 问题: In one filter list, 'All Versions' and 'All Publications' are capitalized while 'All channels' and 'All statuses' (lines 537-538) are lowercase, and 'Version'/'Publication' are elsewhere capitalized as product concepts.
- 建议: All Versions / All Publications / All Channels / All Statuses

### 🟡 LOW

- **`public.unavailable`** · mistranslation · /Users/yang/projects/AI--Platfform/web/src/i18n/locales/agents-en-US.json:630
- 问题: zh '已撤回' means withdrawn/recalled, rendered as the vaguer 'no longer available', which overlaps with 'disabled' and loses the specific meaning.
- 建议: The publication may be private, disabled, or withdrawn.

### 🟡 LOW

- **`studio.capabilities.none`** · fluency · /Users/yang/projects/AI--Platfform/web/src/i18n/locales/agents-en-US.json:269
- 问题: Status value 'No' (zh: 无) reads unnaturally as a standalone chip label; 'None' is the natural English rendering of 无 in this position.
- 建议: None

### 🟡 LOW

- **`studio.channels.description`** · inconsistent · /Users/yang/projects/AI--Platfform/web/src/i18n/locales/agents-en-US.json:462
- 问题: 'Publish immutable versions' lowercases the product concept that is consistently capitalized elsewhere ('Publish immutable Version' line 366, 'Immutable Versions' line 384).
- 建议: Publish immutable Versions, manage release channels and Runtime API tokens, and roll back to prior versions.

### 🟡 LOW

- **`create.catalogs.unavailable`** · inconsistent · /Users/yang/projects/AI--Platfform/web/src/i18n/locales/agents-en-US.json:148
- 问题: Same zh '已有选择会保留' rendered as 'Existing choices are preserved' here but 'Existing selections are preserved' at studio.degraded/degradedPlural (lines 439-440).
- 建议: {{label}} catalog is unavailable. Existing selections are preserved.

### 🟡 LOW

- **`studio.release.providerFreeTitle`** · fluency · /Users/yang/projects/AI--Platfform/web/src/i18n/locales/agents-en-US.json:333
- 问题: 'Provider-free release profile' (zh: 不调用模型的发布 profile, a profile that makes no model calls) is ambiguous - a native reader may parse 'provider-free' as 'no provider needed' rather than 'does not call a provider/model'.
- 建议: Model-free release profile (also update providerFreeScope at line 361 to match)


## web/src (assistant pages, features/chat, components chat/agent/artifacts/llm/ui, ChatWindow, ConversationSidebar, MultimodalInput, StreamOutput, ToolCallBlock, ThinkingIndicator)

Systemic i18n breakage in the assistant surface. (A) ~140 t() calls reference keys absent from ALL six locale files (main, eval, agents, en+zh) — verified by flattening every locale bundle: assistant.quiz.* (~45 keys), assistant.share* (12), assistant.activity.* (13), assistant.artifacts*/noCode/noOutput, assistant.thinking*/thinkingLevel/thinkingOff-Low-Medium-High, assistant.rename/more/delete/moveToFolder/removeFromFolder/projects/folderHint/folderPlaceholder, llm.model.catalogMismatch, llm.provider.template/guidedMode/advancedMode, providers.actions.syncModels, playground.thinking.analyzing/thinking/searching/planning/preparing, playground.stats.firstText, assistant.connectors. Since every call site passes a hardcoded defaultValue, the Quiz components render CHINESE text to en-US users (highest severity, default locale is en-US), and all the English-default sites render English to zh-CN users. (B) Whole components with no useTranslation at all: ConnectorsPanel.tsx is 100% hardcoded Chinese (dialog, alerts, confirms), and the entire local-os/ subtree plus SubAgentCard.tsx, ErrorDisplay.tsx, ProviderForm.tsx partial, ArtifactsPanel toasts/headers are 100% hardcoded English. Root cause is likely a recent feature drop (Quiz, share, folders, artifacts panel, local-os) merged without locale entries; the fix is to add the missing keys to both en-US.json and zh-CN.json. Highest-value fixes: ConnectorsPanel, Quiz family, then the missing-key batch.
### 🔴 HIGH

- **`连接器 / 连接第三方数据源，AI 可搜索和引用其中内容`** · hardcoded-string · web/src/pages/assistant/components/ConnectorsPanel.tsx:141
- 问题: Entire component is hardcoded Chinese with zero useTranslation: heading '连接器' (141), subtitle (142), aria-label '关闭连接器面板' (144), '加载中...' (154), '暂无可用连接器...' (157), 'AI 工具已激活 · N 个工具可用'/'已连接但 AI 工具未激活' (178-179), '激活 AI 工具'/'处理中...' (188), '断开' (196), '尚未连接' (202), '连接'/'跳转中...' (208), footer (223), plus alert/confirm '✅ AI 工具已激活！...' (108), '激活失败: ...' (110), '确定要断开此连接？' (116), error fallbacks (96, 127). English users see Chinese by default.
- 建议: Refactor with useTranslation. New keys (en/zh): connectors.title 'Connectors'/'连接器'; connectors.subtitle 'Connect third-party data sources for AI search and citation'/'连接第三方数据源，AI 可搜索和引用其中内容'; connectors.closeAria 'Close connectors panel'/'关闭连接器面板'; connectors.loading 'Loading...'/'加载中...'; connectors.empty 'No connectors configured. Add sources in Settings → Connectors.'/'暂无可用连接器，请先在「设置 → 连接器」配置数据源'; connectors.activated 'AI tools activated · {{count}} tool(s) available'/'AI 工具已激活 · {{count}} 个工具可用'; connectors.connectedNoTools 'Connected, AI tools not activated'/'已连接但 AI 工具未激活'; connectors.activate 'Activate AI tools'/'激活 AI 工具'; connectors.disconnect 'Disconnect'/'断开'; connectors.notConnected 'Not connected'/'尚未连接'; connectors.connect 'Connect'/'连接'; connectors.processing 'Processing...'/'处理中...'; connectors.redirecting 'Redirecting...'/'跳转中...'; connectors.disconnectConfirm 'Disconnect this connection?'/'确定要断开此连接？'; connectors.footer 'After connecting, AI can search its content via MCP tools.'/'连接后，AI 可通过 MCP 工具检索其中内容'

### 🔴 HIGH

- **`assistant.quiz.allAnsweredTitle / allAnsweredHint / questions`** · missing-key · web/src/pages/assistant/components/Quiz/QuizCard.tsx:584
- 问题: t('assistant.quiz.allAnsweredTitle', '你已完成') / t('assistant.quiz.questions', '题') / t('assistant.quiz.allAnsweredHint', '可点击下方任意题号跳回检查，或直接提交。') — keys verified absent from both en-US.json and zh-CN.json, so every user (including default en-US) sees the hardcoded Chinese fallback. Part of ~45 missing assistant.quiz.* keys across QuizCard/QuizIdle/QuizResult/QuizShareDialog (e.g. startQuiz, prev, next, submit, submitErrorTitle, submitErrorGeneric, reviewAnswers, retry, backToEdit, reviewAll/reviewWrong/reviewUnanswered, reviewEmpty, backToResults, draftSaved, shareQuiz, requireName, generateLink, copied, copyLink, correctOf, correct, wrongDetail, wrongCountChip, countQ, retake, viewHistory, attemptHistory, kbdSelect/kbdNav/kbdNext/kbdPrev/kbdSubmit, chipAnswering/chipReadyToSubmit/chipSubmitting/chipSubmitError/chipResult/chipReviewWrong/chipReviewUnanswered/chipReview).
- 建议: Add all assistant.quiz.* keys to both locale files, e.g.: allAnsweredTitle en 'You answered all {{count}} questions' zh '你已完成'; questions en 'questions' zh '题'; allAnsweredHint en 'Click any question number below to review, or submit directly.' zh '可点击下方任意题号跳回检查，或直接提交。'; startQuiz en 'Start Quiz' zh '开始作答'; submitErrorGeneric en 'An unknown error occurred while submitting. Please retry.' zh '提交时发生未知错误，请重试'

### 🔴 HIGH

- **`assistant.quiz.submitErrorGeneric = '提交时发生未知错误，请重试'`** · missing-key · web/src/pages/assistant/components/Quiz/QuizCard.tsx:322
- 问题: Submit-error toast uses Chinese defaultValue on a missing key — shown to en-US users verbatim. Same at 694-700 (submitErrorTitle '提交失败' + submitErrorGeneric).
- 建议: Add assistant.quiz.submitErrorTitle en 'Submission failed' zh '提交失败'; assistant.quiz.submitErrorGeneric en 'An unknown error occurred while submitting. Please retry.' zh '提交时发生未知错误，请重试'

### 🔴 HIGH

- **`assistant.quiz.chipAnswering/chipReadyToSubmit/chipSubmitting/chipSubmitError/chipResult/chipReviewWrong/chipReviewUnanswered/chipReview`** · missing-key · web/src/pages/assistant/components/Quiz/QuizCard.tsx:926
- 问题: Status-chip labels use Chinese defaultValues on missing keys: '作答中 · {{done}}/{{total}}', '全部已答 · 待提交', '提交中…', '提交失败', '已提交 · {{pct}}%', '审阅 · 错题', '审阅 · 未答', '审阅模式' — rendered as Chinese in the default en-US locale.
- 建议: Add chip keys to both locales, e.g. chipAnswering en 'Answering · {{done}}/{{total}}' zh '作答中 · {{done}}/{{total}}'; chipReadyToSubmit en 'All answered · Ready to submit' zh '全部已答 · 待提交'; chipSubmitting en 'Submitting…' zh '提交中…'; chipResult en 'Submitted · {{pct}}%' zh '已提交 · {{pct}}%'; chipReviewWrong en 'Review · Wrong answers' zh '审阅 · 错题'

### 🔴 HIGH

- **`assistant.quiz.startQuiz = '开始作答'`** · missing-key · web/src/pages/assistant/components/Quiz/QuizIdle.tsx:98
- 问题: Primary CTA uses Chinese defaultValue on missing key — en-US users see '开始作答'. Keyboard hint labels at 108-129 use the same pattern: kbdSelect '选择', kbdNext '下一题', kbdPrev '上一题', kbdSubmit '提交'. Line 73 also branches on locale directly: {isZh ? '题' : t('assistant.quiz.questions', 'questions')} instead of using t() alone (locale-switch hack).
- 建议: Add assistant.quiz.startQuiz en 'Start Quiz' zh '开始作答'; kbdSelect en 'Select' zh '选择'; kbdNext en 'Next' zh '下一题'; kbdPrev en 'Previous' zh '上一题'; kbdSubmit en 'Submit' zh '提交'. Replace the isZh ternary at 73 with t('assistant.quiz.questions', { count: total }).

### 🔴 HIGH

- **`assistant.quiz.correctOf = '答对 {{correct}}/{{total}} 题'`** · missing-key · web/src/pages/assistant/components/Quiz/QuizResult.tsx:104
- 问题: Result screen uses Chinese defaultValues on missing keys: correctOf '答对 {{correct}}/{{total}} 题' (104), wrongCountChip '错题' and countQ '道' (151-152), plus reviewAnswers/retake/viewHistory/attemptHistory/correct/wrongDetail at 132-200 — all Chinese in default en-US rendering.
- 建议: Add assistant.quiz.correctOf en 'Correct {{correct}}/{{total}}' zh '答对 {{correct}}/{{total}} 题'; wrongCountChip en 'Wrong' zh '错题'; countQ en '' (or drop) zh '道'; correct en 'Correct' zh '正确'; wrongDetail en 'Wrong (you: {{user}}, answer: {{correct}})' zh '答错（你：{{user}}，答案：{{correct}}）'; retake en 'Retake' zh '重做'; viewHistory en 'View history' zh '查看历史'; attemptHistory en 'Attempt history' zh '作答历史'

### 🔴 HIGH

- **`assistant.connectors = '连接器'`** · missing-key · web/src/pages/assistant/components/QuickActionsMenu.tsx:196
- 问题: t('assistant.connectors', '连接器') — key absent from locale files, so the Connectors menu item renders as Chinese '连接器' in the default en-US UI.
- 建议: Add assistant.connectors en 'Connectors' zh '连接器'

### 🟠 MEDIUM

- **`'Share Link' / 'Anyone with this link can take the quiz (name required).'`** · hardcoded-string · web/src/pages/assistant/components/Quiz/QuizShareDialog.tsx:113
- 问题: Hardcoded English labels bypassing t(): 'Share Link' (113) and 'Anyone with this link can take the quiz{{suffix}}.' (125-127). Additionally all t('assistant.quiz.shareQuiz/requireName/generateLink/copied/copyLink', English defaults) at 74-156 reference keys absent from both locale files — zh-CN users see English.
- 建议: Use t() with new keys: quiz.shareLinkLabel en 'Share Link' zh '分享链接'; quiz.shareLinkHint en 'Anyone with this link can take the quiz{{suffix}}.' zh '任何拥有此链接的人都可以作答{{suffix}}。' (suffix en ' (name required)' zh '（需填写姓名）'); and add assistant.quiz.shareQuiz en 'Share Quiz' zh '分享测验'; requireName en 'Require name before taking' zh '作答前需填写姓名'; generateLink en 'Generate Share Link' zh '生成分享链接'; copied en 'Copied!' zh '已复制'; copyLink en 'Copy Link' zh '复制链接'

### 🟠 MEDIUM

- **`assistant.rename / assistant.more / assistant.delete / assistant.moveToFolder / assistant.removeFromFolder / assistant.projects / assistant.folderHint / assistant.folderPlaceholder`** · missing-key · web/src/components/ConversationSidebar.tsx:287
- 问题: Folder/rename/delete UI (79-94, 287-316, 515) calls t() with keys verified absent from both locale files; English defaultValues mean zh-CN users always see English ('Rename', 'More', 'Move to folder', 'Remove from folder', 'Delete', 'Projects', 'Enter a folder name or select an existing one', 'e.g. Product Research').
- 建议: Add keys to both locales: assistant.rename en 'Rename' zh '重命名'; assistant.more en 'More' zh '更多'; assistant.delete en 'Delete' zh '删除'; assistant.moveToFolder en 'Move to folder' zh '移动到文件夹'; assistant.removeFromFolder en 'Remove from folder' zh '移出文件夹'; assistant.projects en 'Projects' zh '项目'; assistant.folderHint en 'Enter a folder name or select an existing one' zh '输入文件夹名称或选择已有文件夹'; assistant.folderPlaceholder en 'e.g. Product Research' zh '例如：产品研究'

### 🟠 MEDIUM

- **`assistant.artifacts / assistant.artifactsEmptySubtitle`** · missing-key · web/src/components/artifacts/ArtifactsPanel.tsx:519
- 问题: t('assistant.artifacts', 'Artifacts') (519) and t('assistant.artifactsEmptySubtitle', 'No files yet') (536) reference missing keys — zh users see English. Line 533 also hardcodes the plural via template literal: `${totalCount} ${totalCount === 1 ? 'file' : 'files'}` — bypasses t() and is wrong for zh anyway.
- 建议: Add assistant.artifacts en 'Artifacts' zh '产物'; assistant.artifactsEmptySubtitle en 'No files yet' zh '暂无文件'; add count with plural: t('assistant.artifactsFileCount', { count: totalCount }) with en '{{count}} file(s)' zh '{{count}} 个文件'

### 🟠 MEDIUM

- **`'Copied to clipboard' / 'Failed to copy' / 'Download started'`** · hardcoded-string · web/src/components/artifacts/ArtifactsPanel.tsx:544
- 问题: Toast notifications hardcoded in English at 544-563 (handleCopy/handleDownload) — no translation path; zh-CN users see English toasts.
- 建议: Add toast keys: artifacts.copySuccess en 'Copied to clipboard' zh '已复制到剪贴板'; artifacts.copyFailed en 'Failed to copy' zh '复制失败'; artifacts.downloadStarted en 'Download started' zh '开始下载'

### 🟠 MEDIUM

- **`'No files yet' / 'Images (N)' / 'Files' / assistant.artifactsEmpty / artifactsEmptyHint / noCode / noOutput`** · hardcoded-string · web/src/components/artifacts/ArtifactsPanel.tsx:748
- 问题: Empty states use missing keys with English defaults (assistant.artifactsEmpty 748/799, artifactsEmptyHint 751, noCode 824, noOutput 847); section headers 'Images ({{n}})' (~774) and 'Files' (~795) are hardcoded English JSX text.
- 建议: Add assistant.artifactsEmpty en 'No files yet' zh '暂无文件'; artifactsEmptyHint en 'Generated files will appear here.' zh '生成的文件将显示在这里'; noCode en 'No code available' zh '暂无代码'; noOutput en 'No output available' zh '暂无输出'; artifacts.imagesSection en 'Images ({{count}})' zh '图片（{{count}}）'; artifacts.filesSection en 'Files' zh '文件'

### 🟠 MEDIUM

- **`assistant.activity.approvalRequired / queueState / contextUsedWindow / contextUsed / historyDropped / contextCompactedDropped / contextCompacted`** · missing-key · web/src/pages/assistant/components/buildTimeline.ts:333
- 问题: Timeline body builders use 7 missing assistant.activity.* keys with English defaultValues (333-389) — zh-CN users see English status text in the activity timeline.
- 建议: Add to both locales: assistant.activity.approvalRequired en 'Approval required' zh '需要审批'; queueState en 'Queue: {{state}}' zh '队列：{{state}}'; contextUsedWindow en 'Context used: {{used}} / {{window}}' zh '已用上下文：{{used}} / {{window}}'; contextUsed en 'Context used: {{used}}' zh '已用上下文：{{used}}'; historyDropped en 'dropped {{count}} history messages' zh '丢弃 {{count}} 条历史消息'; contextCompactedDropped en 'Context compacted · dropped {{count}} history messages' zh '上下文已压缩 · 丢弃 {{count}} 条历史消息'; contextCompacted en 'Context compacted' zh '上下文已压缩'

### 🟠 MEDIUM

- **`assistant.activity.contextState / retrievedContext / retrievedContextBody / generatedArtifact`** · missing-key · web/src/pages/assistant/components/buildTimeline.ts:558
- 问题: Timeline context/artifact entries use 4 more missing assistant.activity.* keys with English defaultValues (558-592) — zh users see English ('Context', 'Retrieved N chunks', etc.).
- 建议: Add: assistant.activity.contextState en 'Context' zh '上下文'; retrievedContext en 'Retrieved context' zh '检索上下文'; retrievedContextBody en '{{count}} chunks from {{datasets}}' zh '来自 {{datasets}} 的 {{count}} 个片段'; generatedArtifact en 'Generated artifact' zh '生成产物'. Note: playground.activity.foundResults at 635 is fine (resolves foundResults_one/_other plural).

### 🟠 MEDIUM

- **`playground.thinking.analyzing / thinking / searching / planning / preparing`** · missing-key · web/src/components/ThinkingIndicator.tsx:43
- 问题: Five rotating phase messages call missing playground.thinking.* keys with English defaultValues ('Analyzing your request...', 'Thinking...', 'Searching knowledge base...', 'Planning response...', 'Preparing answer...') — zh-CN users always see English in the thinking indicator.
- 建议: Existing key playground.thinking.label ('Thinking...'/'思考中...') already covers the 'thinking' phase. For the others add: playground.thinking.analyzing en 'Analyzing your request...' zh '正在分析您的请求...'; searching en 'Searching knowledge base...' zh '正在搜索知识库...'; planning en 'Planning response...' zh '正在规划回复...'; preparing en 'Preparing answer...' zh '正在准备回答...'

### 🟠 MEDIUM

- **`assistant.thinkingLevel / thinkingOff / thinkingLow / thinkingMedium / thinkingHigh`** · missing-key · web/src/pages/assistant/components/ChatInputArea.tsx:203
- 问题: Thinking-level aria-label and select options use 5 missing keys with English defaults ('Thinking', 'Think off', 'Think low', 'Think mid', 'Think high') — zh users see English; 'Think mid' is also poor English.
- 建议: Add: assistant.thinkingLevel en 'Thinking level' zh '思考程度'; thinkingOff en 'Off' zh '关闭'; thinkingLow en 'Low' zh '低'; thinkingMedium en 'Medium' zh '中'; thinkingHigh en 'High' zh '高'

### 🟠 MEDIUM

- **`assistant.thinkingInProgress / thoughtFor / thoughtProcess`** · missing-key · web/src/pages/assistant/components/ThinkingPanel.tsx:90
- 问题: Thinking panel toggle text uses 3 missing keys with English defaults ('Thinking...', 'Thought for {{time}}', 'Thought process') — zh users see English.
- 建议: Add: assistant.thinkingInProgress en 'Thinking...' zh '思考中...'; thoughtFor en 'Thought for {{time}}' zh '思考了 {{time}}'; thoughtProcess en 'Thought process' zh '思考过程'

### 🟠 MEDIUM

- **`assistant.shareCreated / shareFailed / linkCopied / shareConversation / messages / artifacts / includeArtifacts / shareNote / creating / createShareLink / shareLinkReady`** · missing-key · web/src/pages/assistant/components/ShareDialog.tsx:29
- 问题: Entire share dialog uses 11 keys verified absent from both locale files, with English defaultValues (29-164) — zh-CN users see English for all share UI/toasts.
- 建议: Add: assistant.shareCreated en 'Share link created!' zh '分享链接已创建！'; shareFailed en 'Failed to create share link' zh '创建分享链接失败'; linkCopied en 'Link copied to clipboard' zh '链接已复制到剪贴板'; shareConversation en 'Share Conversation' zh '分享对话'; messages en 'Messages' zh '消息'; includeArtifacts en 'Include artifacts' zh '包含产物'; shareNote en 'Anyone with this link can view the conversation.' zh '任何拥有此链接的人都可以查看对话。'; creating en 'Creating...' zh '创建中...'; createShareLink en 'Create share link' zh '创建分享链接'; shareLinkReady en 'Share link ready' zh '分享链接已就绪'

### 🟠 MEDIUM

- **`assistant.share / assistant.shareConversation / assistant.artifacts`** · missing-key · web/src/pages/assistant/index.tsx:921
- 问题: Top-bar share button aria-label/tooltip (921, 926) and artifacts chips (1000, 1351) use missing keys with English defaults — zh users see English.
- 建议: Add assistant.share en 'Share' zh '分享' (plus assistant.shareConversation and assistant.artifacts as suggested for ShareDialog/ArtifactsPanel).

### 🟠 MEDIUM

- **`llm.model.catalogMismatch / llm.model.enableAdvancedOverride`** · missing-key · web/src/components/llm/ModelForm.tsx:560
- 问题: Catalog-mismatch warning banner and its override button use 2 missing llm.model.* keys with English defaults (560-569) — zh users see English in the model form.
- 建议: Add: llm.model.catalogMismatch en 'This model belongs to a known provider catalog. Switch to the matching provider or enable advanced override.' zh '该模型属于已知提供商目录。请切换到匹配的提供商，或启用高级覆盖。'; llm.model.enableAdvancedOverride en 'Enable advanced override' zh '启用高级覆盖'

### 🟠 MEDIUM

- **`llm.provider.template / llm.provider.guidedMode / llm.provider.advancedMode / providers.actions.syncModels`** · missing-key · web/src/components/llm/ProviderForm.tsx:254
- 问题: Provider template label and guided/advanced toggle use 3 missing llm.provider.* keys with English defaults (254-263); ProviderCard.tsx:154-155 uses missing providers.actions.syncModels for the sync button title/aria-label — zh users see English.
- 建议: Add: llm.provider.template en 'Provider template' zh '提供商模板'; llm.provider.guidedMode en 'Guided' zh '引导模式'; llm.provider.advancedMode en 'Advanced' zh '高级模式'; providers.actions.syncModels en 'Sync models' zh '同步模型'

### 🟠 MEDIUM

- **`assistant.activity.approvalRequired / approve / reject`** · missing-key · web/src/pages/assistant/components/ActivityPanel.tsx:83
- 问题: Approval-required status (83, 235) and Approve/Reject buttons (261, 282) use 3 missing keys with English defaults — zh users see English in the activity panel. (Line 90 playground.activity.steps resolves via steps_one/steps_other plurals — OK.)
- 建议: Add: assistant.activity.approvalRequired en 'Approval required' zh '需要审批'; assistant.activity.approve en 'Approve' zh '批准'; assistant.activity.reject en 'Reject' zh '拒绝'

### 🟠 MEDIUM

- **`playground.stats.firstText / '% cached'`** · missing-key · web/src/pages/assistant/components/ChatMessage.tsx:221
- 问题: Stats line uses missing key playground.stats.firstText with default 'text' — renders as 'text 1.2s' for every user; line 231 hardcodes '{{pct}}% cached' via template literal with no translation path.
- 建议: Add playground.stats.firstText en 'text' zh '文本'; add stats.cached en '{{pct}}% cached' zh '缓存命中 {{pct}}%'

### 🟠 MEDIUM

- **`'Local files' / 'Close Local files panel' / 'Enable local file capabilities for this chat session' / 'Pairing challenge created'`** · hardcoded-string · web/src/pages/assistant/local-os/LocalOSPanel.tsx:69
- 问题: The entire local-os/ subtree has ZERO useTranslation. LocalOSPanel.tsx: 'Local files' heading (69), aria-labels (88, 120), 'Pairing challenge created' (138); LocalOSControlSurface.tsx tab labels 'Control'/'Permissions'/'Receipts' (83-85); OfflineDegradationNotice.tsx 'Unavailable' (29). All English, shown to zh-CN users.
- 建议: Add useTranslation and new keys, e.g. localOS.title en 'Local files' zh '本地文件'; localOS.closeAria en 'Close Local files panel' zh '关闭本地文件面板'; localOS.enableAria en 'Enable local file capabilities for this chat session' zh '为当前会话启用本地文件能力'; localOS.pairingChallenge en 'Pairing challenge created' zh '配对挑战已创建'; localOS.tabs.control/permissions/receipts en 'Control'/'Permissions'/'Receipts' zh '控制'/'权限'/'回执'; localOS.unavailable en 'Unavailable' zh '不可用'

### 🟠 MEDIUM

- **`'Assigned task' / 'Result' / 'Evidence' / 'Limitations' / 'No host-provided task summary.'`** · hardcoded-string · web/src/pages/assistant/components/SubAgentCard.tsx:188
- 问题: Sub-agent detail sections hardcode English headings (188 'Assigned task', 202 'Result', 211 'Evidence', 231 'Limitations') and empty-state text (190 'No host-provided task summary.'); no useTranslation in the file — zh users see English.
- 建议: Add useTranslation and keys: subagent.assignedTask en 'Assigned task' zh '分配的任务'; subagent.result en 'Result' zh '结果'; subagent.evidence en 'Evidence' zh '证据'; subagent.limitations en 'Limitations' zh '限制'; subagent.noTaskSummary en 'No host-provided task summary.' zh '主机未提供任务摘要。' (Also apply same treatment to ErrorDisplay.tsx 'Recoverable'/'Phase:' at 163/186, ContextDisplay.tsx 'Source' at 112, CitationDisplay.tsx 'Preview:' at 157, and ProviderForm.tsx 'Project ID'/'Location' at 369/380.)


## web/src/pages/dashboard + web/src/components/SafeResponsiveChart.tsx

Scanned all 22 files under web/src/pages/dashboard plus SafeResponsiveChart.tsx. No high-severity hardcoded Chinese was found in live code paths — every hardcoded Chinese literal in mounted components is a t() fallback whose key exists in both locale files. The most serious issues are (a) missing keys with wrong-language fallbacks in live components (dashboard.reliability.sloNotConfigured, dashboard.dataStatus.live/stale), (b) hardcoded aria-labels and stat titles in live components, and (c) a cluster of 7 missing dashboard.trend.* keys with Chinese fallbacks plus a raw 暂无数据 literal in SummaryCharts.tsx — that file (and KPICards.tsx) is currently dead code (not imported anywhere), so those are capped at medium. All dashboard.* keys referenced via eval.*/agents.* namespaces: none used. No files were edited.
### 🟠 MEDIUM

- **`暂无数据`** · hardcoded-string · web/src/pages/dashboard/components/SummaryCharts.tsx:203
- 问题: Raw hardcoded Chinese literal rendered as the SVG no-data text inside LineChart — no t() at all. If this chart is ever mounted, default en-US users see Chinese with no translation path. (File is currently dead code — SummaryCharts is not imported by any other module — so not high.)
- 建议: Replace with existing key t("common.noData") (en: "No data", zh: "暂无数据") — verified present in both en-US.json and zh-CN.json.

### 🟠 MEDIUM

- **`dashboard.trend.requestTrend / count / current / previous / latencyTrend / costComposition / totalCost`** · missing-key · web/src/pages/dashboard/components/SummaryCharts.tsx:328
- 问题: Seven t() calls (lines 328-329, 333-334, 406, 411-412, 478, 495) reference keys under dashboard.trend that exist in NEITHER en-US.json nor zh-CN.json (verified: both files only have up/down/vsPrevious/noBaseline). The Chinese fallbacks ("请求趋势", "次", "本期", "上期", "延迟趋势", "成本构成", "总成本") therefore render in both locales — en-US users would see Chinese. File is currently unreferenced (dead code), hence medium.
- 建议: Add the keys to both locale files: dashboard.trend.requestTrend (en "Request Trend"/zh "请求趋势"), dashboard.trend.count ("count"/"次"), dashboard.trend.current ("Current"/"本期"), dashboard.trend.previous ("Previous"/"上期"), dashboard.trend.latencyTrend ("Latency Trend"/"延迟趋势"), dashboard.trend.costComposition ("Cost Composition"/"成本构成"), dashboard.trend.totalCost ("Total Cost"/"总成本").

### 🟠 MEDIUM

- **`dashboard.reliability.sloNotConfigured`** · missing-key · web/src/pages/dashboard/DashboardLayout.tsx:304
- 问题: t("dashboard.reliability.sloNotConfigured", "Not configured") — key is absent from both locale files (verified: no dashboard.reliability subtree at all). The English fallback "Not configured" renders for zh-CN users in the live Reliability workspace signal chip. Adjacent line 303 hardcodes the "SLO" label with no t() (low).
- 建议: Add dashboard.reliability.sloNotConfigured (en "Not configured"/zh "未配置") to both files; also add dashboard.reliability.slo (en "SLO"/zh "SLO") and use t() for the line 303 label.

### 🟠 MEDIUM

- **`dashboard.dataStatus.live / dashboard.dataStatus.stale`** · missing-key · web/src/pages/dashboard/components/DataStatusBadge.tsx:15
- 问题: STATUS_CONFIG (lines 15-16) references dashboard.dataStatus.live and dashboard.dataStatus.stale, which do not exist in either locale file (verified: dataStatus only has ok/delayed/empty/error/freshnessMinutes/unknown). t(config.label, status) then falls back to the raw status literal, so the tooltip shows "live"/"stale" in BOTH locales. Additionally line 73 renders hardcoded English badge text "LIVE"/"STALE"/"N/A" visible to Chinese users. This badge is live (PanelWrapper renders it for SecurityEventsPanel).
- 建议: Add dashboard.dataStatus.live (en "Live"/zh "实时") and dashboard.dataStatus.stale (en "Stale"/zh "延迟") to both files; render the badge text through t() with the same keys ("N/A" -> t("dashboard.dataStatus.empty") or t("common.unknown")).

### 🟠 MEDIUM

- **`aria-label="refresh" / aria-label="fullscreen"`** · hardcoded-string · web/src/pages/dashboard/index.tsx:227
- 问题: Two icon buttons in the live dashboard filter bar (lines 227 and 231) have hardcoded English aria-labels. Chinese screen-reader users hear "refresh"/"fullscreen" regardless of locale; existing keys are already available.
- 建议: Use existing keys: aria-label={t("dashboard.refresh.now")} (en "Refresh now"/zh "立即刷新") and aria-label={t("dashboard.fullscreen.enter")} (en "Enter fullscreen"/zh "进入全屏") — both verified present.

### 🟠 MEDIUM

- **`Retrieval / Tool Calls / Overhead`** · hardcoded-string · web/src/pages/dashboard/components/panels/PerformancePanel.tsx:137
- 问题: Hardcoded English Statistic titles at lines 137 ("Retrieval"), 147 ("Tool Calls"), 155 ("Overhead") in the live Performance panel. Chinese users see untranslated English; P50/P95/P99/TTFB/LLM are acronyms and acceptable, but these are full English words.
- 建议: Add keys to both files: dashboard.performance.retrieval (en "Retrieval"/zh "检索"), dashboard.performance.toolCalls (en "Tool Calls"/zh "工具调用"), dashboard.performance.overhead (en "Overhead"/zh "开销").

### 🟡 LOW

- **`Loading chart...`** · hardcoded-string · web/src/components/SafeResponsiveChart.tsx:119
- 问题: Hardcoded English placeholder text shown while chart dimensions are measured on mount — visible to Chinese users across every panel that uses this wrapper (brief, but user-visible). Component has no useTranslation hook.
- 建议: Add useTranslation and use t("common.loading") (en "Loading..."/zh "加载中...", verified present) or add a dedicated key common.chartLoading (en "Loading chart..."/zh "图表加载中...").

### 🟡 LOW

- **`API Key · {{name}} / API Key · {{suffix}}`** · hardcoded-string · web/src/pages/dashboard/hooks/useDashboardEntityLabels.ts:26
- 问题: User-filter dropdown labels for API-key-derived users are assembled via template literals (lines 26, 119, 123) bypassing t(): "API Key · " is hardcoded English and shows in the live dashboard user filter for Chinese users.
- 建议: Add dashboard.filters.apiKeyLabel (en "API Key · {{name}}"/zh "API Key · {{name}}") and use t("dashboard.filters.apiKeyLabel", { name }) in all three places (API Key may stay as a brand term in zh).

### 🟡 LOW

- **`UNATTRIBUTED`** · hardcoded-string · web/src/pages/dashboard/components/panels/TokenUsagePanel.tsx:233
- 问题: Hardcoded English "UNATTRIBUTED" label rendered in the live provider-breakdown list when an item has no provider (the zh branch of the ternary is hardcoded English).
- 建议: Add dashboard.tokenUsage.unattributed (en "UNATTRIBUTED"/zh "未归因") to both locale files and use t().

### 🟡 LOW

- **`NEW`** · hardcoded-string · web/src/pages/dashboard/components/KPICards.tsx:265
- 问题: Hardcoded English "NEW" badge shown next to the trend delta when a KPI has no previous-period baseline. Chinese users see English. (File is currently dead code — KPICards is not imported anywhere.)
- 建议: Add dashboard.trend.new (en "NEW"/zh "新增") and render through t(); also worth wiring KPICards back into the layout or deleting it.

### 🟡 LOW

- **`i18n.language.startsWith("zh") ? "其他" : "Others"`** · locale-switch-bug · web/src/pages/dashboard/components/SummaryCharts.tsx:466
- 问题: Manual locale sniffing bypasses t(): the "other providers" donut label is hardcoded "其他"/"Others" based on i18n.language. Works for zh/en today but is fragile (breaks on any other locale and is inconsistent with the rest of the file's t() usage).
- 建议: Add dashboard.trend.others (en "Others"/zh "其他") to both locale files and replace the ternary with t("dashboard.trend.others").

### 🟡 LOW

- **`Retrieval / Tool / trace_id`** · hardcoded-string · web/src/pages/dashboard/components/panels/RequestTracePanel.tsx:128
- 问题: Hardcoded English stat labels in the live trace detail view: "Retrieval" (line 128), "Tool" (line 132), and "trace_id" (line 260). Chinese users see untranslated labels; TTFB/LLM (lines 120/124) are standard acronyms and acceptable.
- 建议: Add dashboard.requestTrace.retrieval (en "Retrieval"/zh "检索"), dashboard.requestTrace.tool (en "Tool"/zh "工具"), dashboard.requestTrace.traceId (en "Trace ID"/zh "Trace ID") to both locale files and use t().


## pages/knowledge

Scanned all 22 files under web/src/pages/knowledge (DatasetDetail.tsx, Datasets.tsx, DatasetCreate*, detail/* components, create/*, useDatasetUploadController) and verified all ~660 static t() keys against the locale files (en-US.json/zh-CN.json main namespace; eval-*-US.json, agents-*-US.json). Root cause: the ~45-key family `knowledge.eval.*` (plus `knowledge.detail.tabEval`, `knowledge.detail.retrievalPreset`, `knowledge.detail.image`) exists in NEITHER en-US.json NOR zh-CN.json (verified: grep -c "knowledge.eval" = 0 in both), and every call site passes a Chinese default fallback — so the entire retrieval eval workbench, the dataset-detail preset panel, and several badges render Chinese in the default en-US locale (HIGH). Second cluster: hardcoded Chinese literals (维 dimension suffix x2, "推荐" badge, "PDF、Word、TXT、MD", "· API 投影") visible in en-US. Third cluster: hardcoded English literals (Rerank/MMR labels x8, Max Tokens, Top K x2, Provider/Collection/Dimension, Dense/BM25/Hybrid, "LLM …ms"/"Tokens …", "chars"/"tokens" units x10, "Segment image" alt, metric labels in the workbench) that break zh-CN. Plus 5 keys missing from locale files with English defaults (common.loadFailed, knowledge.datasets.copyFailed/loadFailed/loadFailedDesc, knowledge.create.stepProgress) — low impact. Clean files: Datasets.tsx, DatasetCreate*.tsx, documentRow/versionHistory/StatusBadge/SourcesTab/index. Suggest adding the missing keys to both locale files (en + zh values provided below) and replacing literals with t() keys.
### 🔴 HIGH

- **`-`** · missing-key · web/src/pages/knowledge/detail/RetrievalEvalWorkbench.tsx:505
- 问题: Whole workbench renders Chinese in default en-US: every t('knowledge.eval.*') key (title/subtitle/scopeNote/testSet/queryLabel/queryPlaceholder/add/empty/annotate/removeCase/relevantSegmentIds/markRelevant/loadingPresets/presetsFailed/retry/presetA/presetB/selectPreset/gateK/cancel/run/waitForPresets/chooseDistinctPresets/labelEveryCase/staleTestSet/resultAt/pass/fail/metric, ~40 keys) is MISSING from en-US.json AND zh-CN.json (grep -c 'knowledge.eval' = 0 in both), so the Chinese defaults (e.g. '检索评测工作台', '输入一个测试问题，例如：报销流程需要什么材料？', '指标对比', '通过'/'未通过') render for all users. Also L212 noPresets, L335 noCandidates, L447 cancelled, L488-496 metricRows (hitRate/recall/precision/mrr/ndcg/map), L609-612 placeholder, L645-648, L817-819, L833-836.
- 建议: Add the full knowledge.eval.* set to both locale files. Examples: knowledge.eval.title: en 'Retrieval Evaluation Workbench' / zh '检索评测工作台'; knowledge.eval.queryPlaceholder: en 'Enter a test query, e.g.: What materials are required for the reimbursement process?' / zh '输入一个测试问题，例如：报销流程需要什么材料？'; knowledge.eval.pass: en 'Pass' / zh '通过'; knowledge.eval.fail: en 'Fail' / zh '未通过'; knowledge.eval.resultAt: en 'Metric comparison' / zh '指标对比'. Existing metric keys can be reused where present (eval.ragas.* exists).

### 🔴 HIGH

- **`-`** · missing-key · web/src/pages/knowledge/DatasetDetail.tsx:1252
- 问题: t('knowledge.detail.tabEval', '评测') — key missing from both locale files; the dataset detail page shows a Chinese '评测' tab label in the default en-US locale.
- 建议: Add to en-US.json: 'knowledge.detail.tabEval': 'Evaluation' and to zh-CN.json: 'knowledge.detail.tabEval': '评测'.

### 🔴 HIGH

- **`-`** · missing-key · web/src/pages/knowledge/DatasetDetail.tsx:1625
- 问题: t('knowledge.detail.retrievalPreset', '检索预设') + hardcoded '· API 投影' (L1625, aria-label duplicated at L1635): key missing from locale files, and the '· API 投影' suffix is a hardcoded Chinese string appended outside t().
- 建议: Add knowledge.detail.retrievalPreset: en 'Retrieval preset' / zh '检索预设' to both files; replace '· API 投影' with a t() call, e.g. new key knowledge.detail.apiProjection: en '· API projection' / zh '· API 投影'.

### 🔴 HIGH

- **`-`** · missing-key · web/src/pages/knowledge/DatasetDetail.tsx:1640
- 问题: Preset panel states use missing keys with Chinese fallbacks: L1640-1643 t('knowledge.eval.loadingPresets','正在加载检索预设…'), t('knowledge.eval.selectPreset','选择预设'); L1657-1658 t('knowledge.eval.presetsFailed','检索预设加载失败') + template `` ：${retrievalPresetError}`` (Chinese full-width colon); L1667 t('knowledge.eval.retry','重试').
- 建议: Add to both locale files: knowledge.eval.loadingPresets: en 'Loading retrieval presets…' / zh '正在加载检索预设…'; knowledge.eval.selectPreset: en 'Select preset' / zh '选择预设'; knowledge.eval.presetsFailed: en 'Failed to load retrieval presets' / zh '检索预设加载失败'; knowledge.eval.retry: en 'Retry' / zh '重试'. Use key-based interpolation ({{error}}) instead of the Chinese-colon concatenation.

### 🔴 HIGH

- **`-`** · missing-key · web/src/pages/knowledge/DatasetDetail.tsx:1673
- 问题: t('knowledge.eval.projectionHint', '仅应用当前检索 API 可执行字段。') and t('knowledge.eval.presetOptional', '可选：应用预设会填充请求参数；手动调整将切回自定义配置。') at L1673-1680 — keys missing from both locale files, Chinese text shown in en-US.
- 建议: Add: knowledge.eval.projectionHint: en 'Only fields executable by the current retrieval API are applied.' / zh '仅应用当前检索 API 可执行字段。'; knowledge.eval.presetOptional: en 'Optional: applying a preset fills the request parameters; manual adjustments switch back to custom configuration.' / zh '可选：应用预设会填充请求参数；手动调整将切回自定义配置。'.

### 🔴 HIGH

- **`-`** · missing-key · web/src/pages/knowledge/DatasetDetail.tsx:1805
- 问题: aria-label={t('knowledge.detail.image', '图片')} — key missing from both locale files; screen readers get the Chinese default in en-US.
- 建议: Add knowledge.detail.image: en 'Image' / zh '图片' to both locale files.

### 🔴 HIGH

- **`-`** · missing-key · web/src/pages/knowledge/DatasetDetail.tsx:1938
- 问题: Status badges L1938 t('knowledge.eval.executed','已执行') and L1940 t('knowledge.eval.requestedUnverified','已请求（执行未确认）') — keys missing from both locale files; Chinese badges shown in default en-US.
- 建议: Add: knowledge.eval.executed: en 'Executed' / zh '已执行'; knowledge.eval.requestedUnverified: en 'Requested (execution not confirmed)' / zh '已请求（执行未确认）' to both locale files.

### 🔴 HIGH

- **`-`** · hardcoded-string · web/src/pages/knowledge/DatasetDetail.tsx:3124
- 问题: {m.dimension}维 — hardcoded Chinese unit suffix '维' appended to the dimension number in the embedding model select options; visible in en-US.
- 建议: Use a t() call with interpolation, e.g. t('knowledge.detail.dimensionValue', { dimension: m.dimension }) with en '{{dimension}} dimensions' / zh '{{dimension}}维'.

### 🔴 HIGH

- **`-`** · hardcoded-string · web/src/pages/knowledge/detail/DatasetUploadDialog.tsx:153
- 问题: <p>PDF、Word、TXT、MD</p> — hardcoded Chinese string with Chinese enumeration comma 、; shown in en-US upload dialog.
- 建议: Replace with t('knowledge.upload.supportedFormats', { defaultValue: 'PDF, Word, TXT, MD' }) with zh-CN value 'PDF、Word、TXT、MD'.

### 🔴 HIGH

- **`-`** · hardcoded-string · web/src/pages/knowledge/detail/DatasetUploadDialog.tsx:896
- 问题: {model.dimension}维 — hardcoded Chinese '维' suffix in the embedding model option list inside the upload dialog; visible in en-US.
- 建议: Same fix as DatasetDetail L3124: t('knowledge.detail.dimensionValue', { dimension: model.dimension }) with en '{{dimension}} dimensions' / zh '{{dimension}}维'.

### 🔴 HIGH

- **`-`** · hardcoded-string · web/src/pages/knowledge/detail/useDatasetUploadController.ts:31
- 问题: badge: '推荐' — hardcoded Chinese badge text in the DATASET_EMBEDDING_MODELS config, rendered as a blue badge next to the dashscope text-embedding-v4 option in the upload dialog; visible in en-US.
- 建议: Replace the literal with a t() key (badge: t('knowledge.upload.recommended', 'Recommended')) — if the module cannot use the hook, map to key at render: en 'Recommended' / zh '推荐'.

### 🟠 MEDIUM

- **`-`** · hardcoded-string · web/src/pages/knowledge/DatasetDetail.tsx:1823
- 问题: Hardcoded English labels 'Rerank' / 'MMR' spans (L1823, 1833, 1929, 1945, 2245, 2255, 2983, 2997) — visible to zh-CN users.
- 建议: Add keys knowledge.detail.rerank: en 'Rerank' / zh '重排' and knowledge.detail.mmr: en 'MMR' / zh 'MMR' (or reuse existing knowledge.detail labels if present) and use t() at all 8 sites.

### 🟠 MEDIUM

- **`-`** · hardcoded-string · web/src/pages/knowledge/DatasetDetail.tsx:2112
- 问题: Hardcoded English field labels 'Max Tokens' (L2112) and 'Top K' (L2136, L2942) in the retrieval/chunking config forms — untranslated for zh-CN users.
- 建议: Add knowledge.detail.maxTokens: en 'Max Tokens' / zh '最大 Token 数' and knowledge.detail.topK: en 'Top K' / zh 'Top K' (or reuse knowledge.detail.topK if it exists) and use t().

### 🟠 MEDIUM

- **`-`** · hardcoded-string · web/src/pages/knowledge/DatasetDetail.tsx:2435
- 问题: Hardcoded English 'LLM {msg.response.timing.llm_ms}ms' (L2435) and 'Tokens {msg.response.tokens_used}' (L2438) summary lines — untranslated for zh-CN users.
- 建议: Use t() with interpolation: knowledge.detail.llmLatency: en 'LLM {{ms}}ms' / zh 'LLM {{ms}}ms' and knowledge.detail.tokensUsed: en 'Tokens {{count}}' / zh 'Token 数 {{count}}'.

### 🟠 MEDIUM

- **`-`** · hardcoded-string · web/src/pages/knowledge/DatasetDetail.tsx:2935
- 问题: Hardcoded English mode labels: L2935 {{vector:'Dense', dense:'Dense', keyword:'BM25', bm25:'BM25', hybrid:'Hybrid'}...} map and L2959 'BM25 {...}%' — visible to zh-CN users.
- 建议: Use t() for each mode label: knowledge.detail.retrievalMode.dense: en 'Dense' / zh '稠密' (if a retrievalModes.* map already exists under knowledge.detail.retrievalModes, extend it) and knowledge.detail.bm25Score: en 'BM25 {{pct}}%' / zh 'BM25 {{pct}}%'.

### 🟠 MEDIUM

- **`-`** · hardcoded-string · web/src/pages/knowledge/DatasetDetail.tsx:3081
- 问题: Hardcoded English config labels 'Provider' (L3081), 'Collection' (L3087), 'Dimension' (L3142) in the embedding-config section — untranslated for zh-CN users.
- 建议: Add keys knowledge.detail.provider: en 'Provider' / zh '服务商', knowledge.detail.collection: en 'Collection' / zh '集合', knowledge.detail.dimension: en 'Dimension' / zh '维度' and use t().

### 🟠 MEDIUM

- **`-`** · hardcoded-string · web/src/pages/knowledge/DatasetDetail.tsx:2500
- 问题: Hardcoded 'TopK {topK} · {mode}' summary line (L2500) — English tokens with a middle dot, untranslated for zh-CN users; mode value comes from the untranslated map at L2935.
- 建议: Compose from t() keys, e.g. t('knowledge.detail.topKMode', { topK, mode: t(...modeLabel...) }) with en 'TopK {{topK}} · {{mode}}' / zh 'TopK {{topK}} · {{mode}}'.

### 🟠 MEDIUM

- **`-`** · hardcoded-string · web/src/pages/knowledge/DatasetDetail.tsx:2693
- 问题: Hardcoded 'chars' unit (DatasetDetail L2693/2705, DatasetCreateIndexStep L161) — untranslated for zh-CN users; a localized key already exists for the same unit.
- 建议: Reuse existing key knowledge.segment.chars (used by ChunkCard L99/199/287) via t('knowledge.segment.chars', { count: ... }) instead of the literal 'chars'.

### 🟠 MEDIUM

- **`-`** · hardcoded-string · web/src/pages/knowledge/detail/SegmentCard.tsx:166
- 问题: Hardcoded '~{segment.token_count} tokens' — English unit; same pattern at ChunkCard L101/202/290, HierarchicalSegmentCard L142/293/486, SegmentList L189 '(~{...} tokens)', DatasetDetail L3244 (10+ sites) — untranslated for zh-CN users.
- 建议: Introduce knowledge.segment.tokens: en '~{{count}} tokens' / zh '~{{count}} 个 token' and use t('knowledge.segment.tokens', { count }) at all sites.

### 🟠 MEDIUM

- **`-`** · hardcoded-string · web/src/pages/knowledge/detail/SegmentCard.tsx:102
- 问题: alt={segment.image_filename || 'Segment image'} — hardcoded English alt text; screen-reader users of zh-CN get English.
- 建议: Use t('knowledge.segment.imageAlt', { defaultValue: 'Segment image' }) with zh-CN value '分段图片'.

### 🟠 MEDIUM

- **`-`** · hardcoded-string · web/src/pages/knowledge/detail/RetrievalEvalWorkbench.tsx:773
- 问题: Hardcoded English metric labels in the gate display: 'nDCG ≥' / 'Recall ≥' / 'MRR ≥' (L773) and aria-label='K' (L758) — untranslated for zh-CN users.
- 建议: Use t() per metric, e.g. knowledge.eval.metric.nDCG: en 'nDCG ≥' / zh 'nDCG ≥' (metric names are acronyms; keep zh identical or provide zh 'nDCG ≥') and replace aria-label='K' with t('knowledge.eval.gateK', 'K') with zh 'K'.

### 🟡 LOW

- **`-`** · missing-key · web/src/pages/knowledge/DatasetDetail.tsx:1284
- 问题: Keys missing from BOTH locale files but with English defaults, so the fallback renders acceptably in en-US yet blocks translation: common.loadFailed (DatasetDetail L1284, 'Unable to load...'), knowledge.datasets.copyFailed (Datasets L123), knowledge.datasets.loadFailed/loadFailedDesc (Datasets L860/863), knowledge.create.stepProgress (DatasetCreate L340).
- 建议: Add each key with English + Chinese values, e.g. common.loadFailed: en 'Unable to load data. Please try again.' / zh '加载失败，请重试'; knowledge.create.stepProgress: en 'Step {{current}} of {{total}}' / zh '第 {{current}} / {{total}} 步'.

### 🟡 LOW

- **`-`** · hardcoded-string · web/src/pages/knowledge/detail/RetrievalResultCard.tsx:235
- 问题: Hardcoded English metadata debug line 'MMR: score=..., relevance=..., max_sim=...' (L235-240) and hardcoded 'BM25' badge text (L160, proper noun) — untranslated for zh-CN users.
- 建议: For L235-240, build with t() keys: knowledge.retrieval.mmrDetail: en 'MMR: score={{score}}, relevance={{relevance}}, max_sim={{maxSim}}' / zh 'MMR：score={{score}}、relevance={{relevance}}、max_sim={{maxSim}}'; 'BM25' at L160 is a proper noun and may stay, or route through a key for consistency.

### 🟡 LOW

- **`-`** · hardcoded-string · web/src/pages/knowledge/create/DatasetCreateIndexStep.tsx:69
- 问题: Hardcoded English model names 'GTE-ReRank', 'GTE-ReRank v2', 'BGE Reranker v2-m3' (L69-72, proper nouns — low) and hardcoded English sample text in the chunk-preview textarea (L85-93, demo content) — the sample text is user-visible in zh-CN.
- 建议: Model names are proper nouns and acceptable; localize the preview sample via a key, e.g. knowledge.create.chunkPreviewSample: en (existing sample) / zh (translated sample).

### 🟡 LOW

- **`-`** · hardcoded-string · web/src/pages/knowledge/detail/RetrievalEvalWorkbench.tsx:887
- 问题: Minor hardcoded UI tokens: 'Δ (B−A)' column header (L887-890), 'Q:'/'A:' QA placeholders in DatasetUploadDialog (L654/664), and '维' hardcoded unit in SegmentList if any. Low impact but not localizable.
- 建议: Add knowledge.eval.delta: en 'Δ (B−A)' / zh 'Δ (B−A)' (symbols, keep both) and knowledge.upload.qaLabel: en 'Q:' / zh '问：' / 'A:' / '答：' if QA fields are user-facing.


## web/src/pages/agents + web/src/pages/agent-public (i18n compliance)

Scanned all 7 TSX files under pages/agents and pages/agent-public in full. No high-severity findings: zero hardcoded Chinese in the default en-US rendering, and every literal t() key (verified programmatically against agents-en-US/zh-CN and en-US/zh-CN) exists in both locales, including all dynamic families (statuses, roles, sections, memory modes, deletion states, release statuses/auth/operations — "promote"/"rollback" per the AgentPublication type, scopes, confirm dialogs). The agents namespace is correctly lazy-loaded for all these routes in web/src/router.tsx, including AgentHostedPage. The medium-severity cluster is one recurring pattern: raw, untranslated API values (channel names, run/audit/publication statuses) rendered as user-visible labels in the analytics page and release panel — these break Chinese users (English words stay in the zh-CN UI) and also look inconsistent in en-US (lowercase "preview"/"builtin"/"timeout" next to properly cased labels). The remainder are low-severity hardcoded literals (placeholder example URLs, "r"/"v" version prefixes, unit suffixes, a t() default value, raw backend error text on the public page) and one defensive missing-key gap in the dynamic diffSections family (instructions/channels variants absent).
### 🟠 MEDIUM

- **`channel filter Select options: ["preview","hosted","embed","api","builtin"].map((value) => ({ value, label: value }))`** · hardcoded-string · web/src/pages/agents/AgentAnalyticsPage.tsx:309
- 问题: Channel filter dropdown labels are the raw lowercase API values ("preview", "hosted", "embed", "api", "builtin") with no t() call — English-only words remain in the zh-CN UI, and lowercase raw values look inconsistent with the rest of the UI. Note the same channels are properly translated in AgentReleasePanel.tsx via agents.studio.release.channels.*.
- 建议: Map to existing keys where possible and add the missing ones: use t("agents.studio.release.channels.hosted") (en "Hosted", zh "Hosted"), t("agents.studio.release.channels.embed") (en "Embed", zh "Embed"), t("agents.studio.release.channels.api") (en "Runtime API", zh "Runtime API"); add new keys agents.analytics.channels.preview (en "Preview", zh "预览") and agents.analytics.channels.builtin (en "Built-in", zh "内置") to agents-en-US.json / agents-zh-CN.json.

### 🟠 MEDIUM

- **`status filter Select options: ["succeeded","failed","timeout"].map((value) => ({ value, label: value }))`** · hardcoded-string · web/src/pages/agents/AgentAnalyticsPage.tsx:310
- 问题: Run-status filter dropdown renders raw English status values as labels; Chinese users see "succeeded"/"failed"/"timeout" untranslated.
- 建议: Add keys to agents-en-US.json / agents-zh-CN.json: agents.analytics.statuses.succeeded (en "Succeeded", zh "成功"), agents.analytics.statuses.failed (en "Failed", zh "失败"), agents.analytics.statuses.timeout (en "Timeout", zh "超时"), then render label: t(`agents.analytics.statuses.${value}`).

### 🟠 MEDIUM

- **`render: (value: string) => <Tag color={value === "succeeded" ? "green" : "red"}>{value}</Tag> (also line 344 {trace.status} in trace cards and line 274 audit status <Tag>{value}</Tag>)`** · hardcoded-string · web/src/pages/agents/AgentAnalyticsPage.tsx:263
- 问题: Traces table/card and audit table render raw status strings ("succeeded", "failed", "timeout") directly into Tags — untranslated English visible in zh-CN.
- 建议: Render through t(`agents.analytics.statuses.${value}`) using the new keys from the status-filter finding (agents.analytics.statuses.succeeded/failed/timeout, en "Succeeded"/"Failed"/"Timeout", zh "成功"/"失败"/"超时").

### 🟠 MEDIUM

- **`String(entry.channel || "unknown")`** · hardcoded-string · web/src/pages/agents/AgentAnalyticsPage.tsx:331
- 问题: Channel breakdown tags show raw channel values and a hardcoded English fallback "unknown" (also line 262 renders raw channel values in the traces table Tag).
- 建议: Use the channel keys from the filter finding (agents.studio.release.channels.* for hosted/embed/api, new agents.analytics.channels.preview/builtin) and replace "unknown" with a new key agents.analytics.unknownChannel (en "unknown", zh "未知").

### 🟠 MEDIUM

- **`{publication.status} (also line 633 {publication.channel}, line 221 and line 558 {evaluation.channel.toUpperCase()})`** · hardcoded-string · web/src/pages/agents/AgentReleasePanel.tsx:602
- 问题: Publication cards, version-history tags and eval cards render raw English status/channel values ("active", "hosted", "embed", "api", "HOSTED"...) directly — untranslated in zh-CN and inconsistent with the localized channel labels used elsewhere in the same panel (channelOptions uses t()).
- 建议: For channels use t(`agents.studio.release.channels.${channel}`) (hosted/embed/api already exist) before uppercasing; for publication status add keys agents.studio.release.publicationStatus.active (en "Active", zh "已发布") and agents.studio.release.publicationStatus.inactive (en "Inactive", zh "已停用") to agents-en-US.json / agents-zh-CN.json.

### 🟡 LOW

- **`render: (value: number | null) => `r${value ?? 0}` (also AgentAnalyticsPage.tsx:307 `v${version.version_number}` and AgentReleasePanel.tsx:632 v{version.version_number})`** · hardcoded-string · web/src/pages/agents/AgentListPage.tsx:199
- 问题: Draft-revision column hardcodes an "r" prefix and version selectors hardcode a "v" prefix via template literals, bypassing t(). The locale already models this text (agents.common.draftLabel = "Draft r{{revision}}"/"草稿 r{{revision}}"), so the prefix is untranslatable user-visible text.
- 建议: Render the list cell via t("agents.common.draftLabel", { revision: value ?? 0 }) (key exists in both locales), and add agents.common.versionShort (en "v{{version}}", zh "v{{version}}") for the "v" prefixes in AgentAnalyticsPage.tsx:307 and AgentReleasePanel.tsx:632.

### 🟡 LOW

- **`placeholder="https://example.com/agent-icon.png" (identical at AgentCreatePage.tsx:227)`** · hardcoded-string · web/src/pages/agents/AgentStudioPage.tsx:462
- 问题: Icon URL input placeholder is a hardcoded literal in both the studio and create pages — visible example text that cannot be localized.
- 建议: Add a shared key agents.common.iconUrlPlaceholder (en "https://example.com/agent-icon.png", zh "https://example.com/agent-icon.png") to agents-en-US.json / agents-zh-CN.json and use t("agents.common.iconUrlPlaceholder") at both sites.

### 🟡 LOW

- **`placeholder="https://app.example.com"`** · hardcoded-string · web/src/pages/agents/AgentReleasePanel.tsx:514
- 问题: Allowed-origins input placeholder is a hardcoded literal example URL, visible to all users and not localizable.
- 建议: Add agents.studio.release.allowedOriginsPlaceholder (en "https://app.example.com", zh "例如：https://app.example.com") to agents-en-US.json / agents-zh-CN.json and use t() in the placeholder.

### 🟡 LOW

- **`t(`agents.studio.release.diffSections.${name}`)`** · missing-key · web/src/pages/agents/AgentReleasePanel.tsx:577
- 问题: Diff section names come from the server as a Record<string, ...>, but agents-en-US.json/agents-zh-CN.json only define 7 diffSections keys (identity, prompt, model, memory, capabilities, knowledge, skills). The spec also contains an instructions section and the studio has a channels section — if the server ever emits one of those names, i18next renders the raw key string ("agents.studio.release.diffSections.instructions") to the user.
- 建议: Defensively add the two plausible missing variants to agents-en-US.json / agents-zh-CN.json: agents.studio.release.diffSections.instructions (en "Instructions", zh "指令") and agents.studio.release.diffSections.channels (en "Channels", zh "渠道").

### 🟡 LOW

- **`status: String(event.status || data.status || t("agents.preview.toolStatus")) (rendered at line 345 <Tag color="green">{activity.status}</Tag>)`** · hardcoded-string · web/src/pages/agents/AgentPreviewPanel.tsx:211
- 问题: Tool activity status is taken raw from stream events when present (only the fallback is translated via agents.preview.toolStatus), so English server status words appear untranslated in the preview activity feed for zh users.
- 建议: Map known server statuses through t(), e.g. add agents.preview.toolStatusRunning (en "Running", zh "执行中") / agents.preview.toolStatusSuccess (en "Success", zh "成功") to agents-en-US.json / agents-zh-CN.json and translate before display, falling back to t("agents.preview.toolStatus").

### 🟡 LOW

- **`t("agents.list.emptyWhy", "Agents package your prompts, tools, and knowledge into a reusable runtime.")`** · hardcoded-string · web/src/pages/agents/AgentListPage.tsx:316
- 问题: The second argument is a hardcoded English fallback string embedded in code. agents.list.emptyWhy exists in both locale files (en and zh verified), so the fallback never renders — it is dead, untranslatable duplication that can drift from the locale file.
- 建议: Drop the second argument: t("agents.list.emptyWhy") — the key already has correct en ("Agents package your prompts, tools, and knowledge into a reusable runtime.") and zh ("智能体将你的提示词、工具和知识库封装为可复用的运行时。") values in the locale files.

### 🟡 LOW

- **`formatMetric(metrics.total_cost_cents, " ¢") and formatMetric(value, " ms") at lines 264/321/322/327`** · hardcoded-string · web/src/pages/agents/AgentAnalyticsPage.tsx:327
- 问题: Unit suffixes are hardcoded literals appended via string concatenation (" ms", " ¢"). "ms" is locale-neutral, but the hardcoded cent sign is a US currency display assumption applied to all locales and cannot be localized.
- 建议: Format cost with Intl: new Intl.NumberFormat(locale, { style: "currency", currency: "USD", maximumFractionDigits: 2 }).format(metrics.total_cost_cents / 100) so the symbol/position follow the active locale; the " ms" suffix may stay literal.

### 🟡 LOW

- **`setError(cause instanceof Error ? cause.message : t("agents.public.streamFailed")) (also line 200 for upload failures)`** · hardcoded-string · web/src/pages/agent-public/AgentHostedPage.tsx:159
- 问题: The public hosted chat page displays raw backend error messages (cause.message from stream/upload failures) directly in the composer error area — English server text surfaces untranslated in the zh-CN rendering of this public-facing page.
- 建议: Map known backend error codes to translated keys (e.g. agents.public.streamFailed / agents.public.uploadFailed already exist for fallback), and only surface the raw message for unknown codes, or prefix it with a translated label such as t("agents.public.errorPrefix") (en "Error: ", zh "错误：").


## web/src — pages/eval, pages/playground, pages/tasks, Services.tsx, ServiceCard.tsx, ServiceConfigDialog.tsx, ServiceForm.tsx, TaskTable.tsx, ProviderStatusCard.tsx, SystemStatusPage.tsx, HealthBadge.tsx, ConfigEditor.tsx, SetupBanner.tsx

Scanned all listed files in full against en-US.json / zh-CN.json / eval-*.json / agents-*.json. Verified every t() key resolves in the correct namespace file (eval.* and agents.* checked in their own files; the eval namespace IS lazy-loaded via router.tsx lazyNamedWithNamespace before EvalPage renders — OK). Found 22 violations: the highest-severity cluster is two newly-added \"operational signal ribbon\" strips (Services.tsx overview bar and tasks/index.tsx ribbon) where t() fallbacks and JSX children are hardcoded Chinese, rendering Chinese in the default en-US locale; plus a Chinese fallback on the ServiceCard debug button (common.debug missing). The eval workbench and ServiceConfigDialog ship ~30 keys that exist only as English fallbacks (zh-CN users see English), ProviderStatusCard contains large manual i18n.language ternaries that bypass the i18n system plus hardcoded in/out price labels, and playground/tasks have fully hardcoded English strings (alert messages, Up/Down buttons, column headers).
### 🔴 HIGH

- **`tasks.ribbon.dispatcher`** · hardcoded-string · web/src/pages/tasks/index.tsx:42
- 问题: Overview ribbon t() calls use Chinese fallbacks for keys that do not exist in any locale file: t("tasks.ribbon.dispatcher", "执行引擎"), t("tasks.ribbon.queue", "队列吞吐"), t("tasks.ribbon.cron", "定时调度"), t("tasks.ribbon.concurrency", "并发工作节点"). In the default en-US locale i18next returns the Chinese fallback, so English users see 执行引擎/队列吞吐/定时调度/并发工作节点. The whole ribbon was added without locale entries (tasks.ribbon does not exist in en-US.json or zh-CN.json).
- 建议: Add to both locale files: tasks.ribbon.dispatcher: en "Dispatch engine" / zh "执行引擎"; tasks.ribbon.queue: en "Queue throughput" / zh "队列吞吐"; tasks.ribbon.cron: en "Scheduled jobs" / zh "定时调度"; tasks.ribbon.concurrency: en "Concurrent workers" / zh "并发工作节点" (then the fallbacks become dormant).

### 🔴 HIGH

- **``** · hardcoded-string · web/src/pages/tasks/index.tsx:45
- 问题: Ribbon values are hardcoded Chinese with no t() at all: line 45 "正常" (normal), line 57 "待处理任务" (pending tasks), line 69 "就绪" (ready), line 81 "Worker 线程". These render as Chinese in the default en-US locale with no translation path.
- 建议: Replace with t() keys, e.g. tasks.ribbon.statusNormal: en "Normal" / zh "正常"; tasks.ribbon.pendingTasks: en "Pending tasks" / zh "待处理任务"; tasks.ribbon.cronReady: en "Ready" / zh "就绪"; tasks.ribbon.workerThreads: en "Worker threads" / zh "Worker 线程".

### 🔴 HIGH

- **`services.overview.gatewayState`** · hardcoded-string · web/src/pages/Services.tsx:361
- 问题: Overview strip uses t() with Chinese fallbacks for keys that do not exist in either locale (services.overview is absent from en-US.json/zh-CN.json): gatewayState "网关服务", providers "上游厂商", models "模型路由", dispatch "调度模式". en-US users see Chinese labels.
- 建议: Add to both locale files: services.overview.gatewayState: en "Gateway" / zh "网关服务"; services.overview.providers: en "Providers" / zh "上游厂商"; services.overview.models: en "Model routing" / zh "模型路由"; services.overview.dispatch: en "Dispatch mode" / zh "调度模式".

### 🔴 HIGH

- **``** · hardcoded-string · web/src/pages/Services.tsx:363
- 问题: Overview strip values are hardcoded Chinese with no t(): line 363 `${services.length} 在线` / `"0 离线"`, line 364 `100% 可用`, line 376 `已接入`, line 388 `活跃模型`, line 400 `降级保障 (Fallback)`. All render Chinese in the default en-US locale.
- 建议: Route through t() with interpolation, e.g. services.overview.onlineCount: en "{{count}} online" / zh "{{count}} 在线"; services.overview.offline: en "0 offline" / zh "0 离线"; services.overview.available: en "100% available" / zh "100% 可用"; services.overview.connected: en "connected" / zh "已接入"; services.overview.activeModels: en "active models" / zh "活跃模型"; services.overview.fallback: en "Degraded fallback" / zh "降级保障".

### 🔴 HIGH

- **`common.debug`** · missing-key · web/src/components/ServiceCard.tsx:144
- 问题: t("common.debug", "调试") — common.debug exists in NEITHER en-US.json nor zh-CN.json, and the fallback default is Chinese. The Debug button therefore renders the Chinese word "调试" in the default en-US locale with no translation path.
- 建议: Add common.debug: en "Debug" / zh "调试" to both locale files (or switch the call to an existing key such as common.configure; note common.remove/approve/review are also absent from common.* so common.* should be audited).

### 🟠 MEDIUM

- **`services.actions.debugInPlayground`** · missing-key · web/src/components/ServiceCard.tsx:141
- 问题: t("services.actions.debugInPlayground", "Debug in Playground") — the key does not exist in either locale (services.actions is absent), so zh-CN users get the English tooltip "Debug in Playground".
- 建议: Add services.actions.debugInPlayground: en "Debug in Playground" / zh "在 Playground 调试" to both locale files.

### 🟠 MEDIUM

- **`eval.workbench.runLoadFailed`** · missing-key · web/src/pages/eval/components/ExperimentRunResults.tsx:102
- 问题: 15 eval.workbench.* keys used here do not exist in eval-en-US.json or eval-zh-CN.json, so both locales render the English fallback: runLoadFailed (102), runEmpty (107), runResults (143), scoringCases/waitingEvaluator (159), resultsLoadFailed (168), averageScore (176), scored (177), needsReview (178), skipped (179), searchRunResults (196), noFailureDetail (226), openTrace (234), resultsPending (243), noMatchingCases (246). zh-CN users see English throughout the run-results panel.
- 建议: Add the 15 keys to both eval locale files with the fallback strings as en values and Chinese translations, e.g. eval.workbench.runResults: en "Run results" / zh "运行结果"; eval.workbench.averageScore: en "Average score" / zh "平均分"; eval.workbench.needsReview: en "Needs review" / zh "待审核"; eval.workbench.scored: en "Scored" / zh "已评分"; eval.workbench.skipped: en "Skipped" / zh "已跳过"; eval.workbench.openTrace: en "Open trace" / zh "打开 Trace"; eval.workbench.searchRunResults: en "Search case, failure, or trace" / zh "搜索用例、失败原因或 Trace".

### 🟠 MEDIUM

- **``** · hardcoded-string · web/src/pages/eval/components/ExperimentRunResults.tsx:187
- 问题: Segmented filter labels built by string concatenation in English: `All ${cases.length}`, `Failed N`, `Review N`, `Passed N` (lines 187-190), and aria-label="Evaluation case results" (line 203). No t() path — Chinese users see English filter labels.
- 建议: Use t() with count interpolation, e.g. eval.workbench.filterAll: en "All {{count}}" / zh "全部 {{count}}"; eval.workbench.filterFailed / filterReview / filterPassed similarly; and eval.workbench.caseResultsAria: en "Evaluation case results" / zh "评测用例结果" for the aria-label.

### 🟠 MEDIUM

- **`eval.workbench.traceVolume`** · missing-key · web/src/pages/eval/index.tsx:1125
- 问题: eval.workbench.traceVolume (line 1125 "Traces & Golden Sets"), eval.workbench.qualityGates (1147 "Quality & Gates"), eval.workbench.createOrConfigure (1587 "Create or configure an experiment"), and eval.workbench.gateNeedsRun (1803 "Select a successful run with complete gate metrics before running the gate.") do not exist in either eval locale file — zh-CN users see the English fallbacks.
- 建议: Add the 4 keys to eval-en-US.json and eval-zh-CN.json, e.g. eval.workbench.traceVolume: en "Traces & Golden Sets" / zh "Trace 与金标集"; eval.workbench.qualityGates: en "Quality & Gates" / zh "质量与门禁"; eval.workbench.createOrConfigure: en "Create or configure an experiment" / zh "创建或配置实验"; eval.workbench.gateNeedsRun: en "Select a successful run with complete gate metrics before running the gate." / zh "请先选择带有完整门禁指标的已成功运行再执行门禁。"

### 🟠 MEDIUM

- **`common.approve`** · missing-key · web/src/pages/eval/index.tsx:1749
- 问题: t("common.approve", "Approve") (1749) and t("common.review", "Needs fix") (1752) — neither key exists in the main locale files, so zh-CN users see the English button labels in the review queue.
- 建议: Add to both locale files: common.approve: en "Approve" / zh "通过"; common.review: en "Needs fix" / zh "需要修改" (keys are referenced without a namespace prefix, so common.* in the main files is correct).

### 🟠 MEDIUM

- **`services.providersStatus.col.provider`** · missing-key · web/src/components/ProviderStatusCard.tsx:851
- 问题: 8 column-header keys (services.providersStatus.col.provider/models/enabled/status/health/latency/success/actions), services.providersStatus.modelUnit, and dashboard.trend.refreshedAt do not exist in the locale files; the code compensates with inline i18n.language ternaries (lines 851-858, 783, 803), so translations live in code, are invisible to the translation pipeline, and any third locale falls back to English.
- 建议: Add the 10 keys to both locale files, e.g. services.providersStatus.col.provider: en "Provider" / zh "厂商 / 平台"; col.health: en "Health" / zh "健康度"; col.latency: en "Avg Latency (ms)" / zh "平均延迟 (ms)"; col.success: en "Success" / zh "成功率"; col.actions: en "Actions" / zh "操作"; services.providersStatus.modelUnit: en "models" / zh "个模型"; dashboard.trend.refreshedAt: en "Updated" / zh "更新于".

### 🟠 MEDIUM

- **``** · hardcoded-string · web/src/components/ProviderStatusCard.tsx:704
- 问题: Large user-visible blocks bypass the i18n system with manual isZh ternaries: lines 704-714 (checking/no-records copy), 718 ("已检测到未归因请求" / "Unattributed usage detected"), 725 ("端点诊断" / "Endpoint diagnostics"), 794 ("已配置"/"configured"), 813 ("个接入缺口"/"gaps"), 927 ("无模型"/"No models"), plus helper functions endpointStateLabel (311-317) and providerSourceLabel (319-325). Strings are hardcoded, untranslatable via locale files, and the zh/en split duplicates the i18n layer.
- 建议: Move these strings into en-US.json/zh-CN.json under services.providersStatus.* (e.g. providersStatus.emptyChecking, emptyNoRecords, emptyUnattributed, diagnosticsTitle, configuredCount, integrationGaps, noModels, sourceHealth/sourceAssistant/sourceModels/sourceProviders, endpointChecking/endpointOk/endpointEmpty/endpointUnauthorized/endpointError) and replace the ternaries with t() calls.

### 🟠 MEDIUM

- **``** · hardcoded-string · web/src/components/ProviderStatusCard.tsx:501
- 问题: Price rows render hardcoded English "in" / "out" (lines 501-502) in the model detail modal. The locale already has exact keys for this: services.providersStatus.table.input ("Input") and services.providersStatus.table.output ("Output") — Chinese users currently see English "in"/"out".
- 建议: Replace with t("services.providersStatus.table.input") and t("services.providersStatus.table.output") — keys already exist in both locale files (zh: "输入"/"输出").

### 🟠 MEDIUM

- **``** · hardcoded-string · web/src/components/ServiceConfigDialog.tsx:1203
- 问题: Fallback-candidate reorder buttons render hardcoded English "Up" (1203) and "Down" (1212) with no t(). Chinese users see English buttons.
- 建议: Use t() with new keys: common.moveUp: en "Move up" / zh "上移" and common.moveDown: en "Move down" / zh "下移".

### 🟠 MEDIUM

- **`services.configDialog.description`** · missing-key · web/src/components/ServiceConfigDialog.tsx:852
- 问题: 10 t() keys used in this dialog do not exist in either locale and fall back to English for zh-CN users: services.configDialog.description (852), services.configDialog.model.noRuntimeProviders (1026), model.failover (1080), model.failoverHint (1083), model.maxAttempts (1101), model.addFallback (1129), common.remove (1220), model.noFallbacks (1229), model.failoverInvalid (1278), model.failoverReady (1282).
- 建议: Add the keys to both locale files using the current fallbacks as en values plus Chinese translations, e.g. services.configDialog.model.failover: en "Failover" / zh "故障转移"; services.configDialog.model.addFallback: en "Add fallback" / zh "添加后备模型"; services.configDialog.model.maxAttempts: en "Max attempts" / zh "最大重试次数"; common.remove: en "Remove" / zh "移除"; services.configDialog.model.failoverReady: en "{{count}} fallback candidates" / zh "{{count}} 个后备候选"; services.configDialog.description: en "Configure service connection, model override, rate limits, authentication, cache, priority, and danger-zone settings." / zh "配置服务连接、模型覆盖、限流、鉴权、缓存、优先级及危险区设置。"

### 🟠 MEDIUM

- **``** · hardcoded-string · web/src/pages/playground/index.tsx:526
- 问题: Share-link feedback uses browser alerts with hardcoded English: alert(`Share link copied to clipboard!\n${shareUrl}`) (526) and alert(`Failed to create share link: ${message}`) (529). No translation path for either language.
- 建议: Replace with t() calls, e.g. playground.shareCopied: en "Share link copied to clipboard!" / zh "分享链接已复制到剪贴板！" and playground.shareFailed: en "Failed to create share link: {{message}}" / zh "创建分享链接失败：{{message}}"

### 🟠 MEDIUM

- **`playground.hideHistory`** · missing-key · web/src/pages/playground/index.tsx:244
- 问题: playground.hideHistory (lines 244, 341, 359) and playground.showHistory (line 360) do not exist in either locale file (the playground.* section has ~60 keys but not these two), so zh-CN users see the English aria-labels/button text "Hide history"/"Show history".
- 建议: Add to both locale files: playground.hideHistory: en "Hide history" / zh "隐藏历史"; playground.showHistory: en "Show history" / zh "显示历史".

### 🟡 LOW

- **`services.registerServiceDescription`** · missing-key · web/src/components/ServiceForm.tsx:166
- 问题: t("services.registerServiceDescription", "Register a LangGraph service by entering connection details or advanced YAML configuration.") — key missing from both locales; the sr-only dialog description (screen-reader visible) stays English for zh-CN users.
- 建议: Add services.registerServiceDescription: en "Register a LangGraph service by entering connection details or advanced YAML configuration." / zh "通过填写连接信息或高级 YAML 配置来注册 LangGraph 服务。" to both locale files.

### 🟡 LOW

- **``** · hardcoded-string · web/src/components/TaskTable.tsx:23
- 问题: Table column headers are hardcoded English — "Service" (23), "Status" (24), "Created" (25) — with no useTranslation at all in the file. The component appears unused (no imports found outside the file), but if ever rendered, Chinese users see English headers.
- 建议: Add useTranslation and map headers to keys, e.g. tasks.service / tasks.status / tasks.createdAt (tasks.service and tasks.createdAt already exist in the tasks.* locale section; add tasks.status if missing).

### 🟡 LOW

- **``** · hardcoded-string · web/src/components/ProviderStatusCard.tsx:685
- 问题: Hardcoded English aria-labels on icon-only buttons: aria-label="refresh" (685, 818), aria-label="trend" (950), aria-label="more" (960). Screen readers announce English regardless of locale.
- 建议: Route through t(): common.refresh already exists (en "Refresh" / zh "刷新"); add services.providersStatus.viewTrend: en "View trend" / zh "查看趋势" and services.providersStatus.viewMore: en "More" / zh "更多".

### 🟡 LOW

- **``** · hardcoded-string · web/src/pages/eval/components/AssistantTraceList.tsx:221
- 问题: Hardcoded English aria-labels in eval panels: "Trace filters" (AssistantTraceList.tsx:221), "Assistant trace detail" and "Trace metrics" (AssistantTraceDetail.tsx:394, 409), "Trace score records" (TraceScorePanel.tsx:175). Screen-reader users of any locale hear English.
- 建议: Add aria keys to the eval locale, e.g. eval.list.traceFiltersAria: en "Trace filters" / zh "Trace 筛选条件"; eval.detail.traceDetailAria: en "Assistant trace detail" / zh "Assistant Trace 详情"; eval.detail.metricsAria: en "Trace metrics" / zh "Trace 指标"; eval.score.recordsAria: en "Trace score records" / zh "Trace 评分记录".


## pages (Login/Quiz/Share/Settings/UserEdit/UserManagement/settings), layouts/AppLayout, router, App, components (Help/Profile/PasswordChange/ProtectedRoute/RootErrorBoundary/Logo)

20 findings. Two public pages (QuizPage, SharePage) have zero useTranslation — every string hardcoded English with no translation path for zh-CN users. UserManagement.tsx hardcodes all dialog labels even though users.* keys already exist in the locale files. PasswordChangeModal, Settings capacity tab, ProtectedRoute, RootErrorBoundary, ConnectorsSettings, and Logo alt are also hardcoded. Missing keys with English-only fallback: eval.title/eval.description and agents.list.subtitle (referenced from the default namespace, but those keys only exist in the eval/agents namespace files) plus users.actions.batchDelete/selectAll and users.pagination.navigation. No hardcoded Chinese found in scope; AppLayout.tsx:372/392 pass Chinese fallback defaults to t() (keys exist in both locales, so latent only). Files not in scope that also showed violations in a global grep (ProviderStatusCard.tsx, ServiceCard.tsx) are reported by sibling agents.
### 🟠 MEDIUM

- **`Quiz link is invalid. / Quiz not found... / Failed to load quiz. / attempt limit / Failed to submit quiz. Please try again.`** · hardcoded-string · web/src/pages/QuizPage.tsx:46
- 问题: Public page has no useTranslation at all; all error/loading strings are hardcoded English rendered to zh-CN users: line 46 'Quiz link is invalid.', 80-81 'Quiz not found, expired, or max attempts reached.'/'Failed to load quiz.', 91 'Quiz details could not be loaded...', 95 'Failed to load quiz.', 134-135 'This quiz has reached its attempt limit.'/'Failed to submit quiz...', 151 'Failed to submit quiz. Please try again.', 171 'Loading quiz…', 185 'This quiz link may have expired or been removed.', 194 fallback 'Quiz result'.
- 建议: Add useTranslation() and new keys under a quiz.* block, e.g. quiz.loading = 'Loading quiz…' / '正在加载测验…', quiz.errors.invalidLink = 'Quiz link is invalid.' / '测验链接无效。', quiz.errors.notFound = 'Quiz not found, expired, or max attempts reached.' / '测验不存在、已过期或已达尝试上限。', quiz.errors.loadFailed = 'Failed to load quiz.' / '测验加载失败。', quiz.errors.attemptLimit = 'This quiz has reached its attempt limit.' / '该测验已达作答上限。', quiz.errors.submitFailed = 'Failed to submit quiz. Please try again.' / '测验提交失败，请重试。', quiz.errors.expired = 'This quiz link may have expired or been removed.' / '该测验链接可能已过期或被移除。', quiz.resultFallback = 'Quiz result' / '测验结果'.

### 🟠 MEDIUM

- **`questions / min / Your name / Start Quiz / Quiz progress / Previous / Next / Submit / Powered by AI Platform`** · hardcoded-string · web/src/pages/QuizPage.tsx:208
- 问题: Quiz UI controls and metadata hardcoded: 208 '{questionCount} questions', 212 same, 236 '{n} questions · ~{m} min', 243 sr-only label 'Your name', 249 placeholder 'Your name', 268 'Start Quiz', 292 aria-label 'Quiz progress', 330 'Previous', 340 'Next', 359 'Submit', 384 'Powered by AI Platform'.
- 建议: New keys, e.g. quiz.questions = '{{count}} questions' / '{{count}} 道题', quiz.minutes = '~{{count}} min' / '约 {{count}} 分钟', quiz.yourName = 'Your name' / '您的姓名', quiz.start = 'Start Quiz' / '开始测验', quiz.progress = 'Quiz progress' / '测验进度', quiz.previous = 'Previous' / '上一题', quiz.next = 'Next' / '下一题', quiz.submit = 'Submit' / '提交', quiz.poweredBy = 'Powered by AI Platform' / '由 AI Platform 提供'.

### 🟠 MEDIUM

- **`Conversation link is invalid / Conversation not found or expired / Failed to load shared conversation / Loading shared conversation… / Conversation not found / This shared conversation may have expired or been removed.`** · hardcoded-string · web/src/pages/SharePage.tsx:70
- 问题: Public share page has no useTranslation; loading and error states fully hardcoded English: 70 'Conversation link is invalid', 84-85 'Conversation not found or expired'/'Failed to load shared conversation', 96 same, 112 'Loading shared conversation…', 123 {error || 'Conversation not found'}, 126 'This shared conversation may have expired or been removed.'
- 建议: Add useTranslation() and keys under share.*: share.loading = 'Loading shared conversation…' / '正在加载分享的对话…', share.errors.invalidLink = 'Conversation link is invalid' / '分享链接无效', share.errors.notFound = 'Conversation not found or expired' / '对话不存在或已过期', share.errors.loadFailed = 'Failed to load shared conversation' / '分享对话加载失败', share.errors.expired = 'This shared conversation may have expired or been removed.' / '该分享对话可能已过期或被移除。'.

### 🟠 MEDIUM

- **`AI Assistant / Shared · {n} messages / files / views / Start a new conversation / Shared from AI Platform · AI-generated content / Expires / Download`** · hardcoded-string · web/src/pages/SharePage.tsx:163
- 问题: Share page header, footer, CTA, and download buttons hardcoded English: 163 'AI Assistant{· model}', 166-168 'Shared · {n} messages' / '{n} files' / '{n} views', 257 'Start a new conversation', 263 'Shared from AI Platform · AI-generated content', 267 'Expires {date}', 317-318 title/aria-label 'Download'.
- 建议: New keys under share.*: share.header = 'AI Assistant' / 'AI 助手', share.sharedMeta = 'Shared · {{count}} messages' / '已分享 · {{count}} 条消息', share.files = '{{count}} files' / '{{count}} 个文件', share.views = '{{count}} views' / '{{count}} 次浏览', share.newConversation = 'Start a new conversation' / '开始新对话', share.footer = 'Shared from AI Platform · AI-generated content' / '来自 AI Platform 的分享 · AI 生成内容', share.expires = 'Expires {{date}}' / '{{date}} 过期', share.download = 'Download' / '下载'.

### 🟠 MEDIUM

- **`Create User / Email / Display Name / Department / Roles`** · hardcoded-string · web/src/pages/UserManagement.tsx:761
- 问题: Create-user dialog hardcoded English (761 'Create User', 768 'Email', 778 'Display Name', 786 'Department', 804 'Roles') even though the locale already defines users.createUser ('Create User'), users.editUser, and users.fields.email/displayName/department/roles — the page uses t() for departments and pagination but not for these dialogs. zh-CN users see English.
- 建议: Replace with existing keys: t('users.createUser'), t('users.fields.email'), t('users.fields.displayName'), t('users.fields.department'), t('users.fields.roles') — all present in both en-US.json and zh-CN.json.

### 🟠 MEDIUM

- **`Edit User / Display Name / Department / Status / Active / Disabled / Roles`** · hardcoded-string · web/src/pages/UserManagement.tsx:848
- 问题: Edit-user dialog hardcoded English (848 'Edit User', 855 'Display Name', 863 'Department', 881 'Status', 887-888 'Active'/'Disabled', 893 'Roles') — locale already has users.editUser, users.fields.status, users.status.active, users.status.disabled, users.fields.roles.
- 建议: Replace with existing keys: t('users.editUser'), t('users.fields.displayName'), t('users.fields.department'), t('users.fields.status'), t('users.status.active'), t('users.status.disabled'), t('users.fields.roles') — all exist in both locales.

### 🟠 MEDIUM

- **`Extra Permissions (Direct Assignment) / These permissions are assigned directly... / You do not have permission to view role list. / You do not have permission to view permissions list.`** · hardcoded-string · web/src/pages/UserManagement.tsx:922
- 问题: Permission sections hardcoded English: 922 'Extra Permissions (Direct Assignment)', 924 'These permissions are assigned directly to the user, in addition to role-based permissions.', 828 & 917 'You do not have permission to view role list...', 970 'You do not have permission to view permissions list.', plus 980 'Save' and 995/1013/1031 'Cancel' buttons.
- 建议: New keys: users.dialogs.extraPermissions = 'Extra Permissions (Direct Assignment)' / '额外权限（直接分配）', users.dialogs.extraPermissionsDesc = 'These permissions are assigned directly...' / '这些权限直接分配给用户，在基于角色的权限之外。', users.noRolePermission = 'You do not have permission to view role list.' / '您没有查看角色列表的权限。', users.noPermissionPermission = 'You do not have permission to view permissions list.' / '您没有查看权限列表的权限。'; for buttons reuse existing t('common.save') and t('common.cancel').

### 🟠 MEDIUM

- **`Delete User / Are you sure you want to delete... / Deleting... / Delete`** · hardcoded-string · web/src/pages/UserManagement.tsx:989
- 问题: Delete-user dialog hardcoded English (989 'Delete User', 990-991 'Are you sure you want to delete {email}? This action cannot be undone.', 995 'Cancel', 997 'Deleting...'/'Delete') — locale already has users.dialogs.deleteTitle ('Confirm Delete'), users.dialogs.deleteMessage with {{name}}, and users.actions.delete.
- 建议: Use existing keys: t('users.dialogs.deleteTitle'), t('users.dialogs.deleteMessage', { name: selectedUser?.email }) ('Are you sure you want to delete user "{{name}}"? This action cannot be undone.' / '确定要删除用户 "{{name}}" 吗？此操作无法撤销。'), t('common.cancel'), t('users.actions.delete'); add users.actions.deleting = 'Deleting...' / '删除中…'.

### 🟠 MEDIUM

- **`Delete {n} Users / Are you sure you want to delete {n} selected users? / Delete {n} Users (button)`** · hardcoded-string · web/src/pages/UserManagement.tsx:1007
- 问题: Batch-delete dialog fully hardcoded English (1008 'Delete {n} Users', 1009 'Are you sure you want to delete {n} selected users? This action cannot be undone.', 1015 template literal 'Delete ${n} Users'), including string concatenation that bypasses t().
- 建议: New key: users.dialogs.batchDeleteTitle = 'Delete {{count}} Users' / '删除 {{count}} 个用户', users.dialogs.batchDeleteMessage = 'Are you sure you want to delete {{count}} selected users? This action cannot be undone.' / '确定要删除选中的 {{count}} 个用户吗？此操作无法撤销。'.

### 🟠 MEDIUM

- **`Reset Password / Reset password for {email} to the configured default? / Reset Password (button)`** · hardcoded-string · web/src/pages/UserManagement.tsx:1025
- 问题: Reset-password dialog hardcoded English (1025 'Reset Password', 1026-1027 'Reset password for {email} to the configured default? The user will be required to change password on next login.', 1032 'Cancel', 1033 'Reset Password') — locale already has users.dialogs.resetPasswordTitle and users.dialogs.resetPasswordMessage.
- 建议: Use existing keys: t('users.dialogs.resetPasswordTitle'), t('users.dialogs.resetPasswordMessage', { name: selectedUser?.email }) ('Are you sure you want to reset password for user "{{name}}"?' / '确定要重置用户 "{{name}}" 的密码吗？'), t('common.cancel'), t('users.actions.resetPassword'); optionally extend resetPasswordMessage if the 'required to change on next login' sentence must be kept.

### 🟠 MEDIUM

- **`Change Password / Current Password / New Password / Confirm New Password / At least 8 characters / Passwords do not match / Failed to change password / Changing Password...`** · hardcoded-string · web/src/components/PasswordChangeModal.tsx:121
- 问题: Entire modal hardcoded English, shown on the login page (Login.tsx mounts it) and in AppLayout: 121 'Change Password', 129 'Current Password', 141 'New Password', 156 'Confirm New Password', 40-49 validation errors ('At least 8 characters', 'At least one letter', 'At least one number', 'At least one special character'), 67 'Passwords do not match', 97 'Failed to change password', 174 'Cancel', 182 'Changing Password...'/'Change Password'.
- 建议: Reuse existing t('user.changePassword') ('Change Password' / '修改密码') for the title; add passwordChange.currentLabel = 'Current Password' / '当前密码', passwordChange.newLabel = 'New Password' / '新密码', passwordChange.confirmLabel = 'Confirm New Password' / '确认新密码', passwordChange.minLength = 'At least 8 characters' / '至少 8 个字符', passwordChange.letter = 'At least one letter' / '至少包含一个字母', passwordChange.number = 'At least one number' / '至少包含一个数字', passwordChange.special = 'At least one special character' / '至少包含一个特殊字符', passwordChange.mismatch = 'Passwords do not match' / '两次输入的密码不一致', passwordChange.failed = 'Failed to change password' / '密码修改失败', passwordChange.changing = 'Changing Password...' / '修改中…'; reuse t('common.cancel').

### 🟠 MEDIUM

- **`Capacity / Gateway Capacity / Budget / Limit / In Flight / Queue / Status / not enforced`** · hardcoded-string · web/src/pages/Settings.tsx:303
- 问题: Capacity tab is the only settings section not using t(): 303 'Capacity' badge label, 324 TabsTrigger 'Capacity' (siblings all use t('settings.tabs.*')), 582 CardTitle 'Gateway Capacity', 592-596 TableHead 'Budget'/'Limit'/'In Flight'/'Queue'/'Status', 608 'not enforced' fallback for disabled budgets. zh-CN users see English for this entire section.
- 建议: Add keys mirroring the existing settings.tabs pattern: settings.tabs.capacity = 'Capacity' / '容量', settings.capacity.title = 'Gateway Capacity' / '网关容量', settings.capacity.budget = 'Budget' / '预算', settings.capacity.limit = 'Limit' / '上限', settings.capacity.inFlight = 'In Flight' / '进行中', settings.capacity.queue = 'Queue' / '队列', settings.capacity.status = 'Status' / '状态', settings.capacity.notEnforced = 'not enforced' / '未强制执行'.

### 🟠 MEDIUM

- **`eval.title / eval.description`** · missing-key · web/src/layouts/AppLayout.tsx:114
- 问题: getPageChrome() references eval.title and eval.description (lines 114-117) via the default-namespace t(), but the main en-US.json and zh-CN.json have eval: {} — these keys exist only in the eval namespace files (eval-en-US.json / eval-zh-CN.json). Result: t() always falls back to the English defaults 'Eval Console' and the English subtitle, so zh-CN users always see English in the /eval page header.
- 建议: Either add eval.title = 'Eval Console' / '评测控制台' and eval.description = 'Review assistant, LangGraph proxy, and RAG traces with bounded previews and human scoring.' / '以受限预览和人工评分审阅 assistant、LangGraph 代理和 RAG 追踪。' to the main en-US.json/zh-CN.json, or make AppLayout use the 'eval' namespace for these two keys.

### 🟠 MEDIUM

- **`agents.list.subtitle`** · missing-key · web/src/layouts/AppLayout.tsx:110
- 问题: getPageChrome() uses subtitleKey 'agents.list.subtitle' (line 110) in the default namespace, but main en-US.json/zh-CN.json have agents: {} — the key exists only in agents-en-US.json/agents-zh-CN.json. t() always falls back to English 'Create, configure, test, and publish reusable agents.' for zh-CN users on the /agents page header.
- 建议: Add agents.list.subtitle = 'Create, configure, test, and publish reusable agents.' / '创建、配置、测试并发布可复用的智能体。' to the main namespace files, or resolve the subtitle with the 'agents' namespace in AppLayout.

### 🟠 MEDIUM

- **`users.actions.batchDelete`** · missing-key · web/src/pages/UserManagement.tsx:456
- 问题: t('users.actions.batchDelete', `Delete (${selectedUserIds.size})`) — users.actions.batchDelete does not exist in either locale file (users.actions only has edit/resetPassword/enable/disable/delete), so zh-CN users always see the English fallback 'Delete (N)'.
- 建议: Add users.actions.batchDelete = 'Delete ({{count}})' / '删除（{{count}}）' and pass { count: selectedUserIds.size }; remove the hardcoded fallback so the key's zh value is used.

### 🟠 MEDIUM

- **`Loading...`** · hardcoded-string · web/src/components/ProtectedRoute.tsx:35
- 问题: Hardcoded 'Loading...' shown on every route transition while auth state hydrates — visible to all users in both locales before any page renders.
- 建议: Use existing t('common.loading') ('Loading...' / '加载中…') by importing useTranslation (the component currently has no i18n import).

### 🟠 MEDIUM

- **`Something went wrong / The app hit an unexpected error while rendering... / Reload / Reset app state`** · hardcoded-string · web/src/components/RootErrorBoundary.tsx:82
- 问题: Error panel fully hardcoded English (82 'Something went wrong', 85-88 'The app hit an unexpected error while rendering...', ~107 'Reload', 118 'Reset app state'). It is a class component so useTranslation() hooks are unavailable, but i18n.t direct import works.
- 建议: Import i18n directly and use i18n.t('errors.boundary.title', 'Something went wrong') with new keys errors.boundary.title = 'Something went wrong' / '出了点问题', errors.boundary.message = 'The app hit an unexpected error while rendering. Your data is safe — reloading usually fixes it...' / '应用渲染时遇到意外错误。您的数据是安全的——重新加载通常可以解决。如果问题持续，请尝试"重置应用状态"以清除缓存的界面状态。', errors.boundary.reload = 'Reload' / '重新加载', errors.boundary.reset = 'Reset app state' / '重置应用状态'.

### 🟠 MEDIUM

- **`Loading...`** · hardcoded-string · web/src/pages/settings/ConnectorsSettings.tsx:270
- 问题: Hardcoded 'Loading...' for the connector list loading state while the rest of the page correctly uses t() (e.g. settings.connectors.empty).
- 建议: Use existing t('common.loading') ('Loading...' / '加载中…').

### 🟡 LOW

- **`users.actions.selectAll`** · missing-key · web/src/pages/UserManagement.tsx:588
- 问题: t('users.actions.selectAll', 'Select all users') — users.actions.selectAll does not exist in either locale; the English fallback is always used (aria-label only, so screen readers for zh users read English).
- 建议: Add users.actions.selectAll = 'Select all users' / '全选用户' and drop the inline fallback.

### 🟡 LOW

- **`users.pagination.navigation`** · missing-key · web/src/pages/UserManagement.tsx:714
- 问题: t('users.pagination.navigation', 'User list pagination') — users.pagination only has showing/page/first/last/prev/next/perPage; the navigation aria-label always renders the English fallback.
- 建议: Add users.pagination.navigation = 'User list pagination' / '用户列表分页' and drop the inline fallback.

### 🟡 LOW

- **`alt="AI Platform Logo"`** · hardcoded-string · web/src/components/Logo.tsx:7
- 问题: Hardcoded alt text on the brand logo image; the component has no useTranslation. Visible to screen-reader users and as image alt fallback in both locales.
- 建议: Add useTranslation() and use t('common.logoAlt', 'AI Platform Logo') with common.logoAlt = 'AI Platform Logo' / 'AI 平台标志'.

### 🟡 LOW

- **`t("theme.mode.dark", "深色模式") / t("theme.mode.light", "浅色模式")`** · mixed-language · web/src/layouts/AppLayout.tsx:372
- 问题: Chinese fallback defaults passed to t() — if the theme.mode.dark/light keys are ever missing or renamed, en-US users would see '深色模式'/'浅色模式' instead of English. Keys currently exist in both locales, so this is latent, but the sidebar collapse label at line 392 has the same pattern ('收起侧栏'), and lines 172-174/180/267/412/426 use English fallbacks for the same keys — inconsistent style.
- 建议: Align all fallback defaults to English (matching the keys that exist): t('theme.mode.dark', 'Dark'), t('theme.mode.light', 'Light'), t('nav.collapseSidebar', 'Collapse sidebar').


## web/src i18n static audit (React + i18next, 3 bundles x 2 locales)

Audited 275 non-test TS/TSX files (2,897 literal t() keys + 45 dynamic-key sites) against 6 locale files (main/eval/agents x en-US/zh-CN) with regex extraction, brace-scoped default-value detection, flattened-locale diffing, and runtime i18next verification (missing key renders raw key; count without plural forms falls back to base string — confirmed against the repo's own i18next). Findings: (1) 0 literal keys render raw — every missing key has a fallback — BUT 227 keys are absent from ALL bundles and show hardcoded English (or Chinese) fallback text in the other locale, concentrated in newly added sections (assistant.quiz 44, knowledge.eval 44, eval.workbench 19, assistant.activity 13, services.* 26, dashboard.trend 8, playground.thinking 5, tasks.ribbon 4); (2) eval namespace is never preloaded for /knowledge routes, so eval.ragas.metrics.* / eval.ragas.labels.* dynamic labels on DatasetDetail.tsx render raw key tokens until /eval is visited once; (3) dead keys: main 846, eval 18, agents 17 (confluence.* 278 looks like a removed feature); (4) 99 count-usages lack _one/_other plural forms -> "1 rows" fluency bugs in en-US (correct for zh); (5) duplicate keys across bundles: CLEAN — main bundle has no eval/agents top-level sections, zero full-path collisions; (6) locale switch: sound — boot awaits initializeI18n() before createRoot.render, queue serializes ops, bundles pre-loaded before changeLanguage, router awaits loadTranslationNamespace before rendering lazy pages; no first-paint race; minor issues: one-shot initialization promise with no retry, namespace marked active before load completes, redundant double-load pass; (7) en/zh key-set parity is perfect (0 drift in all 3 bundles); no malformed keys, no namespace collisions. CAVEAT: several code files (Datasets.tsx, DatasetDetail.tsx, eval/index.tsx, ProviderStatusCard.tsx) were being concurrently edited during the audit (mtimes changed mid-run); key-level conclusions are stable because locale files were untouched, but cited line numbers in those files may drift by a few lines.
### 🔴 HIGH

- **`eval.ragas.metrics.* / eval.ragas.labels.*`** · missing-key · /Users/yang/projects/AI--Platfform/web/src/pages/knowledge/DatasetDetail.tsx:1881
- 问题: DatasetDetail (loaded via lazyNamed WITHOUT a namespace preload) calls t(`eval.ragas.metrics.${metric}`) and t(`eval.ragas.labels.${label}`) at lines 1881/1898, but the eval namespace is only loaded by the /eval route (router.tsx:58); a user who opens /knowledge/:id before ever visiting /eval sees the raw key token (e.g. 'faithfulness') as the metric label.
- 建议: Call loadTranslationNamespace('eval') in the knowledge dataset detail page (or move those label keys to the main bundle); e.g. fire-and-forget in a useEffect on DatasetDetail mount.

### 🔴 HIGH

- **`assistant.quiz.* (44 keys)`** · missing-key · /Users/yang/projects/AI--Platfform/web/src/pages/assistant/components/Quiz/QuizCard.tsx:925
- 问题: The entire assistant.quiz section (44 keys: questions, startQuiz, kbdSelect/kbdNext/kbdPrev/kbdSubmit, submitErrorGeneric, prev, next, submit, chipAnswering, chipResult, correctOf, wrongDetail, reviewAll/reviewWrong/reviewUnanswered, etc.) is absent from all six locale files, so the whole quiz flow renders hardcoded English defaults in the zh-CN UI.
- 建议: Add the 44 assistant.quiz.* keys to en-US.json and zh-CN.json (they are all listed in /tmp/rawkeys2.json with their call sites: QuizCard.tsx:322-952, QuizResult.tsx:104-151, QuizIdle.tsx:73-129).

### 🔴 HIGH

- **`knowledge.eval.* (44 keys)`** · missing-key · /Users/yang/projects/AI--Platfform/web/src/pages/knowledge/detail/RetrievalEvalWorkbench.tsx:645
- 问题: The whole knowledge.eval.* section (44 keys: title, subtitle, hitRate, recall, precision, mrr, ndcg, map, annotate, removeCase, run, pass, fail, metric, presetA/presetB/gateK, ...) is missing from the main bundle, so the eval workbench embedded in DatasetDetail renders hardcoded fallbacks; additionally markRelevant's defaultValue is Chinese ('标记为正确分段：{{segmentId}}'), so en-US users see Chinese text (mixed-language bug).
- 建议: Add all knowledge.eval.* keys to en-US.json and zh-CN.json (44 keys, call sites in RetrievalEvalWorkbench.tsx) and translate the markRelevant default into en-US when adding it.

### 🔴 HIGH

- **`eval.workbench.* (19 keys)`** · missing-key · /Users/yang/projects/AI--Platfform/web/src/pages/eval/components/ExperimentRunResults.tsx:102
- 问题: 19 eval.workbench.* keys are absent from eval-en-US.json/eval-zh-CN.json (runLoadFailed, runEmpty, runResults, scoringCases, waitingEvaluator, resultsLoadFailed, averageScore, scored, needsReview, skipped, searchRunResults, noFailureDetail, openTrace, resultsPending, noMatchingCases, traceVolume, qualityGates, createOrConfigure, gateNeedsRun), so the run-results panel shows English defaults in zh-CN.
- 建议: Add the 19 keys to both eval locale files (call sites: ExperimentRunResults.tsx:102-246, eval/index.tsx:1125-1803).

### 🔴 HIGH

- **`assistant.activity.* (13 keys)`** · missing-key · /Users/yang/projects/AI--Platfform/web/src/pages/assistant/components/ActivityPanel.tsx:83
- 问题: 13 assistant.activity.* keys missing from all bundles (approvalRequired, approve, reject, queueState, contextUsedWindow, contextUsed, historyDropped, contextCompactedDropped, contextCompacted, contextState, retrievedContext, retrievedContextBody, generatedArtifact) — the assistant activity/process timeline renders English defaults in zh-CN.
- 建议: Add the 13 keys to en-US.json and zh-CN.json (call sites: ActivityPanel.tsx:83-282, buildTimeline.ts:341-592).

### 🔴 HIGH

- **`services.providersStatus.* / services.configDialog.* / services.page.* / services.overview.* (26 keys)`** · missing-key · /Users/yang/projects/AI--Platfform/web/src/components/ProviderStatusCard.tsx:783
- 问题: 26 services.* keys missing from all bundles: providersStatus.modelUnit and col.* headers (ProviderStatusCard.tsx:803-858), configDialog.description/model.failover/maxAttempts/addFallback/noFallbacks/failoverInvalid/failoverReady (ServiceConfigDialog.tsx:852-1282), registerServiceDescription, page.title/subtitle, overview.* — services pages show hardcoded English in zh-CN.
- 建议: Add the 26 keys to both main locale files (key list and call sites in /tmp/rawkeys2.json).

### 🟠 MEDIUM

- **`dashboard.trend.* (8) + dashboard.reliability`** · missing-key · /Users/yang/projects/AI--Platfform/web/src/pages/dashboard/components/panels/ServiceHealthPanel.tsx:1
- 问题: dashboard.trend.refreshedAt and 7 more dashboard.trend.* keys plus dashboard.reliability are missing from all bundles; the trend panel and reliability stats show hardcoded English/zh-ternary fallbacks in the other locale.
- 建议: Add dashboard.trend.* and dashboard.reliability keys to en-US.json and zh-CN.json (call sites ProviderStatusCard.tsx:783, ServiceHealthPanel.tsx).

### 🟠 MEDIUM

- **`playground.thinking.* (5) + playground.stats/activity/hideHistory/showHistory`** · missing-key · /Users/yang/projects/AI--Platfform/web/src/components/ThinkingIndicator.tsx:43
- 问题: playground.thinking.analyzing/thinking/searching/planning/preparing (ThinkingIndicator.tsx:43-72), playground.stats.ttft, playground.activity.* and playground.hideHistory/showHistory are missing from all bundles — thinking indicator and playground labels show English defaults in zh-CN.
- 建议: Add these playground.* keys to both main locale files.

### 🟠 MEDIUM

- **`tasks.ribbon.* (4), llm.provider/model (5), knowledge.* (8), users.* (3), common.* (5)`** · missing-key · /Users/yang/projects/AI--Platfform/web/src/pages/UserManagement.tsx:456
- 问题: Remaining scattered missing keys: tasks.ribbon.concurrency/cron/dispatcher/queue (4), llm.provider.template/guidedMode/advancedMode + llm.model.catalogMismatch/enableAdvancedOverride, knowledge.detail.*/datasets.*/create.* (8 incl. knowledge.datasets.loadFailed at Datasets.tsx:824), users.actions.batchDelete + users.pagination.perPage, common.loadFailed/remove/debug/approve/review — all with hardcoded fallbacks, untranslated in the other locale.
- 建议: Add all remaining 40+ keys from /tmp/rawkeys2.json to en-US.json and zh-CN.json.

### 🟠 MEDIUM

- **`assistant.* misc (~35 keys)`** · missing-key · /Users/yang/projects/AI--Platfform/web/src/components/ConversationSidebar.tsx:79
- 问题: ~35 scattered assistant.* keys missing (artifacts/artifactsEmpty/artifactsEmptySubtitle/artifactsEmptyHint/noCode/noOutput in ArtifactsPanel.tsx, moveToFolder/folderHint/folderPlaceholder/rename/more/removeFromFolder/delete/projects in ConversationSidebar.tsx, share/shareConversation/shareLinkReady/messages/includeArtifacts/shareNote/creating/createShareLink/linkCopied/shareCreated/shareFailed in ShareDialog.tsx, thinkingLevel/thinkingOff/thinkingLow/thinkingMedium/thinkingHigh/thinkingInProgress/thoughtFor/thoughtProcess, mcpTools, connectors, citationsTitle, subagents, localFiles*), all rendering English defaults in zh-CN.
- 建议: Add all remaining assistant.* keys from /tmp/rawkeys2.json to both main locale files.

### 🟠 MEDIUM

- **`confluence.* (278), knowledge.* (173), tasks.* (115), assistant.* (72), dashboard.* (38), playground.* (27), quota.* (25), analytics.* (23), metrics.* (22), common.* (20)`** · dead-key · /Users/yang/projects/AI--Platfform/web/src/i18n/locales/en-US.json:1
- 问题: 846 of 2,718 main-bundle keys are never referenced by any code (checked including dynamic prefixes, nameKey/labelKey/titleKey/subtitleKey fields, plural bases): confluence.* 278 (looks like a removed feature), knowledge.* 173 (e.g. knowledge.datasets.typeAudioVideo, knowledge.create.embeddingBgeM3), tasks.* 115 (e.g. tasks.status.completed, tasks.tabs.confluence), assistant.* 72, dashboard.* 38, playground.* 27, quota.* 25, analytics.* 23, metrics.* 22, common.* 20 (incl. the entire common.time.* relative-time set: justNow/minutesAgo/hoursAgo/daysAgo/... — no formatRelativeTime helper exists).
- 建议: Prune or archive the 846 dead keys (largest first: confluence.*, knowledge.*, tasks.*); keep common.time.* only if a relative-time formatter is planned.

### 🟠 MEDIUM

- **`workbench.datasets, workbench.experiments, workbench.datasetsDescription, workbench.experimentsDescription, workbench.currentExperiment, workbench.target, workbench.runExperiment, workbench.listedExperiments, workbench.latestRun, workbench.comparison, workbench.thread.selected, summary.* (6), ragas.evaluatorRubric, score.title`** · dead-key · /Users/yang/projects/AI--Platfform/web/src/i18n/locales/eval-en-US.json:1
- 问题: 18 eval-bundle keys are never referenced in code (workbench 11, summary 6, ragas 1, score 1) — left over from earlier UI revisions; note eval.title and eval.description top-level keys are also unused.
- 建议: Delete the 18 unused eval keys after confirming the new workbench UI doesn't need them (summary.* look like an abandoned trace-summary strip).

### 🟠 MEDIUM

- **`studio.validation.modelRequired, studio.overview.notSet, studio.model.required/supported/unsupported, studio.eval.readiness/ready/boundary, studio.release.evalComplete, studio.degraded, studio.degradedPlural, studio.validationSummaryOne/Other, list.countOne/countOther, preview.citationOne/citationOther`** · dead-key · /Users/yang/projects/AI--Platfform/web/src/i18n/locales/agents-en-US.json:1
- 问题: 17 agents-bundle keys are never referenced (studio 13, list 2, preview 2) — including orphaned plural pairs list.countOne/countOther, preview.citationOne/citationOther and studio.degraded/degradedPlural whose singular is unused.
- 建议: Delete the 17 unused agents keys, or wire them up if the referenced UI (validation summary, degraded states) was intended.

### 🟠 MEDIUM

- **`common.rows, knowledge.detail.characters, dashboard.refresh.seconds/minutes, eval.detail.bytes, users.pagination.perPage, agents.create.characterCount, ... (99 call sites)`** · fluency · /Users/yang/projects/AI--Platfform/web/src/components/agent/TaskResultCard.tsx:525
- 问题: 99 t(key, { count }) call sites pass count where the locale defines only a base string with no _one/_other (or nested one/other) plural forms — verified at runtime that i18next falls back to the base string, so en-US renders '1 rows', '1 characters', '1 segments', '1 docs' etc. (zh-CN is unaffected; Chinese has no plural rules).
- 建议: Add _one/_other variants for en-US (e.g. common.rows_one '{{count}} row' / common.rows_other '{{count}} rows') for the 99 call sites listed in the audit output (biggest clusters: knowledge.detail.* ~40, eval.* ~30, dashboard.* ~10, agents.* ~8).

### 🟠 MEDIUM

- **`agents.analytics.confirm.${kind}Title/Description/Action, agents.analytics.deletion.${status}, eval.comparison.metrics.${metric.key}, agents.studio.memory.${mode}, settings.connectors.${MODE_LABELS[mode]}, nav.group.${row.group}`** · locale-switch-bug · /Users/yang/projects/AI--Platfform/web/src/pages/agents/AgentAnalyticsPage.tsx:240
- 问题: These template-literal keys have NO fallback argument, so any enum value not present in the locale renders the raw key: confirm.* (kinds come from delete-confirmation flows), deletion.* (backend result.status), comparison.metrics.* (metric.key from experiment config), studio.memory.*, settings.connectors.* and nav.group.* — coverage was verified present today, but any new backend/dataset value silently renders a raw key.
- 建议: Add a fallback (t(`...${x}`, x)) or a map lookup with default for these six dynamic families; document the allowed enum values.

### 🟠 MEDIUM

- **`initialization promise (module-level)`** · locale-switch-bug · /Users/yang/projects/AI--Platfform/web/src/i18n/index.ts:70
- 问题: The module-level `initialization` promise (line 13/71) is never reset: if the initial dynamic import of a locale JSON fails (network hiccup), every future initializeI18n() call returns the same rejected promise, bootstrap never renders, and recovery requires a manual reload (the boot watchdog only offers a reload button after 15s).
- 建议: Cache only successful inits: `initialization = init().catch(err => { initialization = undefined; throw err; })`.

### 🟠 MEDIUM

- **`knowledge.eval.markRelevant`** · mixed-language · /Users/yang/projects/AI--Platfform/web/src/pages/knowledge/detail/RetrievalEvalWorkbench.tsx:645
- 问题: The defaultValue for knowledge.eval.markRelevant is hardcoded Chinese ('标记为正确分段：{{segmentId}}'), so en-US users see Chinese text for this aria-label/UI string (verified inline at line 645; other knowledge.eval.* defaults are English, so the section is mixed-language).
- 建议: Once knowledge.eval.* keys are added to the bundles, remove the defaultValue entirely; meanwhile localize it by locale like the ProviderStatusCard pattern or use a language-conditional default.

### 🟡 LOW

- **`eval.status.${status}, eval.ragas.metrics.${metric}, eval.ragas.labels.${label}, eval.score.types.${type}, models.providers.${providerName}, dashboard.userQuota.strategy_labels.${strategy}`** · locale-switch-bug · /Users/yang/projects/AI--Platfform/web/src/pages/eval/components/AssistantTraceList.tsx:115
- 问题: These template-literal keys pass the raw enum value as the fallback (t(`eval.status.${status}`, status)), so unknown values render the code-level token (e.g. a new trace status from a newer backend shows 'eval.status.new_status' — actually shows the token itself) instead of a human label; also the fallback is locale-invariant English.
- 建议: Use a fixed fallback string (e.g. t(`eval.status.${s}`, s === 'succeeded' ? 'Succeeded' : s)) or a dedicated label map so unknown values degrade gracefully.

### 🟡 LOW

- **`playground.beat.${cat}`** · fluency · /Users/yang/projects/AI--Platfform/web/src/components/chat/WorkflowBeat.tsx:85
- 问题: t(`playground.beat.${cat}`, { count }) passes count to keys that have no _one/_other plural forms, so beat labels can't pluralize in en-US ('1 step(s)' style), and a new beat category renders a raw key.
- 建议: Add _one/_other forms for the five playground.beat.* keys and keep the category list in sync with the locale keys.

### 🟡 LOW

- **`activeDeferredNamespaces`** · locale-switch-bug · /Users/yang/projects/AI--Platfform/web/src/i18n/index.ts:107
- 问题: loadTranslationNamespace adds the namespace to activeDeferredNamespaces before the bundle import completes; if ensureDeferredLocale rejects, the failed namespace stays active and every subsequent changeAppLanguage re-attempts the failing import (and the lazy page chunk's Promise.all rejects, hitting the error boundary).
- 建议: Move activeDeferredNamespaces.add(namespace) after a successful load, or remove it in a catch.

### 🟡 LOW

- **`changeAppLanguage second load pass`** · locale-switch-bug · /Users/yang/projects/AI--Platfform/web/src/i18n/index.ts:128
- 问题: The second Promise.all after i18n.changeLanguage (lines 128-134) re-ensures the same locale's bundles — in practice a no-op because loadedBundles already contains both, and the earlier ensure call ran before changeLanguage; harmless but misleading dead code.
- 建议: Drop the second pass, or keep only a resolveAppLocale re-check comment documenting why it exists.

### 🟡 LOW

- **`i18n.language.startsWith('zh') ? '...' : '...' ternaries`** · hardcoded-string · /Users/yang/projects/AI--Platfform/web/src/components/ProviderStatusCard.tsx:783
- 问题: ProviderStatusCard.tsx:783-858 (10+ spots) bypasses the locale system with inline bilingual ternaries (e.g. t('services.providersStatus.modelUnit', i18n.language.startsWith('zh') ? '个模型' : 'models')) — the locale files can never translate these strings, and the zh/en split is duplicated in code.
- 建议: Add the keys to the locale files (they are already in the missing set) and call plain t(key) without ternary fallbacks.

### 🟡 LOW

- **`interpolation.escapeValue: false`** · other · /Users/yang/projects/AI--Platfform/web/src/i18n/index.ts:95
- 问题: escapeValue is disabled globally, so interpolated values are inserted unescaped; safe today because all t() output flows through React JSX (which escapes) and the only dangerouslySetInnerHTML (DocumentPreview.tsx:256) sanitizes with DOMPurify, but any future t() output into antd message/notification or document.title sinks would inject raw HTML.
- 建议: Keep escapeValue default true and escape only where React JSX already handles it, or add a lint rule banning t() output into non-JSX sinks.

### 🟡 LOW

- **`knowledge.chunkModeLabels.<mode> (doc placeholders)`** · placeholder-mismatch · /Users/yang/projects/AI--Platfform/web/src/types/knowledge.ts:567
- 问题: The deprecated helpers getChunkingModeLabel/getChunkingModeDescription/getRetrievalModeLabel carry t('knowledge.chunkModeLabels.<mode>') in comments and @deprecated tags — the '<mode>' literal is not a real key; if anyone revives these helpers they will render the raw literal.
- 建议: Update the comments to the concrete keys (e.g. knowledge.detail.chunkModeLabels.${mode}) or remove the deprecated helpers.

### 🟡 LOW

- **`t(lang.nameKey, lang.nativeName)`** · locale-switch-bug · /Users/yang/projects/AI--Platfform/web/src/layouts/AppLayout.tsx:167
- 问题: The language-menu label uses the dynamic key t(lang.nameKey) with a nativeName fallback — this works only because i18n/index.ts's languages array literals (language.zhCN/enUS) happen to exist in the bundle; the dynamic lookup is invisible to static checks and would silently regress if the array or bundle drift.
- 建议: Keep as-is but add the two language.* keys to the static allowlist used by the key-audit script (already covered in this audit).

### 🟡 LOW

- **`knowledge.datasets.loadFailed / loadFailedDesc`** · missing-key · /Users/yang/projects/AI--Platfform/web/src/pages/knowledge/Datasets.tsx:824
- 问题: knowledge.datasets.loadFailed/loadFailedDesc are absent from all bundles and rely on inline English defaults ('Couldn't load knowledge bases'), untranslated for zh-CN (this file was concurrently edited during the audit, so line numbers may drift).
- 建议: Add both keys to en-US.json and zh-CN.json (call sites around Datasets.tsx:824-827).

