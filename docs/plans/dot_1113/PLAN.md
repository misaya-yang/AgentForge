# dot_1113 执行计划（里程碑制）

> 基线 `main@4451118`（**不能编译、main 从未有过绿色 CI**，见 `/workspace/af/logs/BASELINE-4451118.md`）。目标分支 `dot_1113`。
> 节奏：**每个里程碑 = 一次大提交**。云端 Agent 在 `dot_1113` 上开 `m<N>-<slug>` 分支，一个 PR；若同一里程碑需两路并行（a/b 分支），先合到 `m<N>` 集成分支，再 **squash 成一个提交** 进 `dot_1113`。我在本机按每个里程碑的"验收命令"复跑，全部通过才合并，然后开下一个。
> 预算：用户允许今晚约 40% 周配额，按 M0→M5 串行推进，M3/M4 可并行。

## 通用约定（每个 brief 开头原样粘贴）

```
仓库 misaya-yang/AgentForge，基分支 dot_1113。先读 AGENTS.md 和 docs/plans/dot_1113/PRD.md（M0 之后存在）中与本里程碑相关的章节，不要通读 docs/。仓库很大（约 50 万行 Rust、11 万行 TS），只打开本 brief 列出的 crate 和路径。
红线：不改上游 Codex 源码（只改 rust/agent-runtime-overlay 自有部分）；数据库结构只通过 database/ 的 successor 链 + platform-migration-authority 变更，不改冻结基线和已有指纹；不读 .env*、不把任何凭据写进代码/日志/报告；不弱化、不跳过测试来让门禁变绿（确实无法在本环境运行的，标记 SKIPPED 并写原因）；不引入 GPL/AGPL 依赖（BlockNote 的 @blocknote/xl-* 禁用）；不新增常驻服务（ADR-008）。
环境：Rust 1.99.0（rust-toolchain.toml）、Node 26.11.1、pnpm 12.11.2。cargo 统一加 --locked；export CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0。
前端工作（凡改 web/ 都适用）：开工前在仓库根运行 `DISABLE_TELEMETRY=1 npx -y modern-web-guidance@latest install`（交互向导一路回车，装到 .agents/skills/modern-web-guidance，该目录已被 .gitignore），每实现一个 UI 能力前先 `npx -y modern-web-guidance@latest search "<2-5 个英文词的能力描述>"` 再 `retrieve <id>`，优先原生平台能力（<dialog>/popover/View Transitions/content-visibility 等）；收尾时运行 `npx -y modern-web-guidance@latest legacy-scan web/src --json`，新增代码不得引入新发现。新增文案必须同时加 zh-CN 与 en-US（pnpm i18n:check）。web 调用的每个接口必须在 sdk/typescript/generated.ts 中有定义（由 rust/platform-native/crates/platform-openapi 生成），否则 pnpm build 的 check-api-contract 会失败。
交付：一个 PR，标题 "M<N>: <标题>"，PR 描述包含：改动清单（文件:行号级别的要点）、每条验收命令最后 10 行真实输出、SKIPPED 项及原因、遗留问题。进度只更新 deploy/runbooks/native-enterprise-upgrade/loop-state.json，不另建报告文件（性能报告除外，放 reports/）。
```

## 依赖与并行图

```
M0 恢复绿色基线 + 文档入库
 └─ M1 资料空间：后端 + 外壳/列表页（a 后端 / b 前端 可并行，squash 成一提交）
     └─ M2 资料空间：编辑器 + 页面树 + 版本 + RAG 索引 + Yjs 协作
         ├─ M3 连接器目录 + 平台 MCP Server + skills/插件兼容      ┐ 可并行（文件基本不重叠，
         └─ M4 Codex harness 兼容 + SDK/协议适配 + 垂直 Agent 示例 ┘ 冲突点见各自说明）
             └─ M5 前端全面现代化 + 性能 + 自动化补齐
```

---

## M0：恢复绿色基线 + PRD/PLAN 入库（阻塞一切）

**目标**：`dot_1113` 能编译、能测试、CI 诚实且在能绿的 job 上为绿，后端能原生跑起来做健康检查和登录冒烟。顺带提交 PRD/PLAN。

**范围（精确路径）**
1. 文档：`docs/plans/dot_1113/PRD.md`、`docs/plans/dot_1113/PLAN.md`（内容取自本次交付），`docs/README.md` 的 Plans 表加一行链接。
2. Cargo：`rust/platform-native/crates/platform-gateway-foundation/Cargo.toml` 补 `tracing`（及 dev 的 `tracing-subscriber`），`rust/platform-native/Cargo.lock` 同步（`cargo metadata --locked` 通过）。
3. 编译错误（逐个 crate，保持业务语义，不删功能）：
   - `crates/platform-local-node/src/lib.rs`：重复 `mod` 声明（approvals/files/identity/ledger/outbox/processes/runner/signing/transport/workspace/jwks），`file_dispatch.rs:19` 的 `dyn PairingSigner` trait bound。
   - `crates/platform-auth/src/`：`TokenCodec`/jwks 相关错误（`lib.rs`、`cedar.rs:10`、`browser.rs:18`、`http.rs`）。
   - `crates/platform-migration-authority/src/`：`ApplicationPrincipal::GatewayLocalNode` 未覆盖的 match 分支；`source_tools/next_successor_02{2,3}.rs:14`；lib test 16 个错误。
   - `crates/platform-knowledge-domain/src/migration_knowledge_ingest/{service.rs:13-16,vision.rs:13,mod.rs:58}`、`migration_knowledge_query/{hierarchy.rs:11,migration.rs:15}`：参数个数不匹配。
   - `crates/platform-tools`：`buildx_resource` 相关 bin/test 错误。
   - `crates/platform-gateway`：`migration_content` 与 `skills.rs:659` 的旧错误。
   - `crates/platform-eval/src/parity_support/validators.rs`、`crates/platform-openapi`：clippy `else if` 缺 `else` 等致命 lint。
4. 前端（`web/`）：补完半合并的 `SSEHttpError`/`httpErrorMode`（`src/lib/sse.ts`、`src/lib/sse.test.ts`）；`src/api/knowledge.ts:431` 的 `PUT /api/v1/knowledge/{datasetId}/documents/{documentId}/content` 要么在 platform-openapi 中正式发布并重新生成 `sdk/typescript/generated.ts`，要么改用已发布的 `.../source` 接口（以知识服务真实路由为准）；`src/pages/knowledge/detail/QATab.tsx:200-203` 的 unknown 错误收窄；修复无效 ICU 文案；`web/build/precompression.ts` 被 `.gitignore` 忽略但被 import——改 `.gitignore` 例外并提交该源文件（或移到 `web/scripts/`）。
5. `sdk/cli/package-lock.json` 与 `package.json` 同步（`npm ci` 通过）。
6. CI 诚实化：`.github/workflows/*.yml` 中所有 `| tee` 管道加 `set -o pipefail`（或 `shell: bash` 默认带 `-eo pipefail`）；确实依赖上游 Codex 源码/buildx 的 overlay job 改为显式条件执行并在 summary 写 SKIPPED，而不是假绿。
7. `platform-tools` 的 11 个 make 门禁：能修的修（`make harness-check`、`architecture-boundary-gate`、`gateway-kb-boundary-gate`、`core-boundary-gate` 等），依赖 Docker/上游源码的列入 SKIPPED 清单并写原因。
8. 原生启动冒烟脚本：`scripts/dev/native-smoke.sh`（新增，单文件）：起 PG16（docker 或本机）、`database-authority init-fresh`、启动 gateway + knowledge API（`deploy/platform-native/native-options.example.json` 派生的本地配置，凭据由环境变量注入）、curl `/health/ready` 与登录接口、退出清理。

**验收命令（我本地复跑）**
```bash
cargo metadata --locked --format-version 1 --manifest-path rust/platform-native/Cargo.toml >/dev/null
cargo check --locked --manifest-path rust/platform-native/Cargo.toml --workspace --all-targets
cargo clippy --locked --manifest-path rust/platform-native/Cargo.toml --workspace --all-targets -- -D warnings   # 允许以 workspace lint 配置豁免既有告警类别，但不得出现 error
cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-auth -p ai-platform-local-node -p ai-platform-knowledge-domain -p ai-platform-migration-authority -p ai-platform-gateway-foundation --lib
cd web && pnpm install --frozen-lockfile && pnpm type-check && pnpm lint && pnpm test && pnpm i18n:check && pnpm build && pnpm e2e:opensource
cd sdk/cli && npm ci && npm test
make harness-check architecture-boundary-gate gateway-kb-boundary-gate core-boundary-gate
bash scripts/dev/native-smoke.sh   # 输出 READY + LOGIN_OK
```
**完成定义**：以上全部 exit 0（或在 PR 中逐条列出 SKIPPED 及原因）；GitHub Actions 在 `dot_1113` 的 ci.yml 上为绿（不能绿的 job 明确标 skipped）。
**冲突说明**：触及面广，**必须单独先合**；期间不并行任何其他里程碑。

---

## M1：资料空间——后端模型/API/迁移 + 前端外壳与列表页（对齐截图）

**并行**：M1a 后端（Rust + database + openapi）、M1b 前端（web），合并时先 a 后 b，squash 成一个提交。M1b 先对着 PRD §4.1 的接口草案用本地 mock（`web/e2e/fixtures/` 的路由拦截）开发，a 合入后切到真实合同。

### M1a 后端
- **Schema**：新 successor `030_knowledge_spaces`：`database/migrations/2026_08_post_kb_v1/030_knowledge_spaces.sql` + `030_verification.json` + `manifest.yml` 追加条目（照 029 的格式：preconditions/postconditions/最小 ACL：仅 Knowledge API 角色获得新表 SELECT/INSERT/UPDATE，Worker 只读需要的列）；表：`knowledge.spaces`、`space_items`、`space_item_revisions`、`space_members`、`space_item_shares`、`space_item_stars`、`space_item_activity`（字段见 PRD §4.1）。先放 `docs/design/knowledge-spaces-030.sql.proposal` 再落地，按 `docs/design/canonical-schema-successor-chain.md` 跑 `check-successor-proposals`；如环境有 PG16，跑 `measure-schema-successors` 生成 `SUCCESSOR_030_MEASUREMENT.json`，否则标 SKIPPED，由我本地测量。同步知识服务/网关的 epoch 准入窗口上限到 30。
- **领域与 API**：`crates/platform-knowledge-domain/src/spaces/`（新模块：model、repository(sqlx)、authorize）、`crates/platform-knowledge-service/src/spaces_http.rs`（新）并在 `http.rs` 挂载。路由（全部经 Gateway 现有 `/api/v1/knowledge/{path}` 代理，**Gateway 不建表不写 SQL**）：
  - `GET/POST /api/v1/knowledge/spaces`，`GET/PATCH/DELETE /api/v1/knowledge/spaces/{space_id}`，`GET/PUT /api/v1/knowledge/spaces/{space_id}/members`
  - `GET /api/v1/knowledge/space-items?view=all|recent|starred|mine|shared&kind=page|slide|site|image|file&q=&space_id=&cursor=`（左栏+Tab+搜索的统一查询）
  - `POST /api/v1/knowledge/spaces/{space_id}/items`，`GET/PATCH/DELETE /api/v1/knowledge/space-items/{item_id}`（PATCH 支持重命名/移动/position；DELETE 软删进回收站），`POST .../restore`，`PUT/DELETE .../star`，`PUT .../shares`
  - `GET/PUT /api/v1/knowledge/space-items/{item_id}/content`（块 JSON + If-Match revision；M1 只存储，M2 接索引）
  - `GET /api/v1/knowledge/space-trash`
  - 注意 axum 静态段优先于 `{dataset_id}` 捕获，加测试证明 `spaces` 不被当成 dataset。
- 权限点：`knowledge:space:{view,create,edit,share,admin}` 加入 `crates/platform-auth` 的权限目录与默认角色映射。
- OpenAPI：在 `crates/platform-openapi`（`kb_boundary.rs`/`broad_contract.rs`）登记全部新操作，重新生成 `sdk/typescript/generated.ts`、`sdk/openapi.json`。
- 测试：`crates/platform-knowledge-service/tests/spaces_api.rs`（连 PG 的集成测试，用 `QA`-风格环境变量 `AI_GATEWAY_TEST_DATABASE_URL`，无 PG 时 ignored 并在 PR 注明）：创建空间→建页→列表各视图→收藏→分享给另一用户后对方"与你共享"可见、无权用户 404→软删→回收站→恢复；跨租户隔离。

### M1b 前端
- 路由 `/spaces`、`/spaces/:spaceId`、`/spaces/item/:itemId`（M1 打开页面显示只读 markdown 预览/占位）在 `web/src/router.tsx` 注册，导航加"资料空间"入口（`web/src/layouts/AppLayout.tsx`），权限 `knowledge:space:view`。
- 新目录 `web/src/pages/spaces/`：`SpacesLayout.tsx`（左栏：全部/页面/幻灯片/站点/图像/Google Drive（M3 连接器前显示"连接"引导）/最近/空间列表/回收站）、`SpacesHome.tsx`（Tab：推荐/收藏/你的条目/与你共享；搜索；"创建"下拉：页面、空间、上传文件、导入 Markdown；列表/网格切换；列：名称+类型图标+空间徽标、来源、上次活动）、`TrashView.tsx`；API 客户端 `web/src/api/spaces.ts`（只用 generated.ts 中的类型）；TanStack Query 缓存键 `['spaces', ...]`；i18n 命名空间 `spaces-zh-CN.json`/`spaces-en-US.json`。
- 体验：列表 1 万条用 `content-visibility`/虚拟化；"创建"下拉与重命名对话框用原生 `<dialog>`/popover（按 MWG 指南）；空状态、加载骨架、错误重试；键盘可达、WCAG 2.2 AA。
- e2e：`web/e2e/spaces.spec.ts`（路由拦截 mock 与真实后端两种模式），并把 `/spaces` 加入 `all-routes-accessibility.spec.ts`。

**验收命令**
```bash
cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-knowledge-domain -p ai-platform-knowledge-service -p ai-platform-openapi
AI_GATEWAY_TEST_DATABASE_URL=postgres://... cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-knowledge-service --test spaces_api -- --include-ignored
cargo run --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-migration-authority --bin check-successor-proposals -- --root .
make gateway-kb-boundary-gate verify-native-openapi-contract
cd web && pnpm type-check && pnpm lint && pnpm test && pnpm i18n:check && pnpm build && npx playwright test --config playwright.opensource.config.ts e2e/spaces.spec.ts e2e/all-routes-accessibility.spec.ts
npx -y modern-web-guidance@latest legacy-scan web/src/pages/spaces --json   # totalFindings 0
bash scripts/dev/native-smoke.sh && bash scripts/dev/spaces-smoke.sh        # 新增：真实服务上 curl 走一遍建空间→建页→列表
```
**冲突说明**：M1a 只碰 knowledge-* crate、platform-openapi、platform-auth 权限表、database/、sdk/；M1b 只碰 web/。共同点是 `sdk/typescript/generated.ts`——只由 M1a 生成，M1b 不手改。

---

## M2：资料空间——块编辑器、页面树、版本、RAG 索引、实时协作

- **编辑器**：`@blocknote/core|react|shadcn@0.57`（MPL-2.0），`web/src/pages/spaces/editor/`，按路由懒加载为独立 chunk；slash 菜单、拖拽、嵌套、表格、代码块、图片（上传走现有文件接口）；中文 locale；AI 菜单（改写/续写/总结/翻译/问这个空间）调用网关 `/v1/chat/completions`（流式），不直连供应商。
- **页面树**：侧栏树（拖拽改父级/排序，fractional index），面包屑，反向链接（`[[页面]]` 提及）。
- **自动保存与版本**：去抖 1.5s 保存，`If-Match` 冲突提示；版本列表/对比/恢复（`space_item_revisions`）。
- **RAG 索引**：知识服务内保存后去抖 ≥5s 生成 markdown 投影（Rust：BlockNote JSON→Markdown，`crates/platform-knowledge-domain/src/spaces/markdown.rs`），内容哈希不变则跳过，调用现有文本文档创建/源替换路径写入空间绑定数据集；软删→文档归档；权限变化→文档 ACL 更新；检索结果带 `space_id/item_id/revision`。
- **实时协作**：知识服务内 `yrs 0.28` 实现 Hocuspocus 兼容 WebSocket（`/api/v1/knowledge/space-items/{id}/collab`，经 Gateway WebSocket 代理鉴权），awareness 光标；持久化 `ydoc_state`（若需新列，作为 successor 031）；客户端 `@hocuspocus/provider@4` + BlockNote collaboration。
- **导入/导出**：md、html 导入导出（服务端），zip 批量导入为页面树。
- **验收**：
```bash
cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-knowledge-domain spaces:: && cargo test ... -p ai-platform-knowledge-service --test spaces_api --test spaces_collab -- --include-ignored
cd web && pnpm type-check && pnpm lint && pnpm test && pnpm build && npx playwright test --config playwright.opensource.config.ts e2e/spaces-editor.spec.ts e2e/spaces-collab.spec.ts
pnpm licenses list --prod | grep -Ei 'GPL|AGPL' && exit 1 || true
bash scripts/dev/spaces-smoke.sh --rag   # 新增：建页写入唯一短语 → 等待索引 → /retrieve 召回该页面且带 item_id；无权用户检索不到
```
  e2e 要求：两个浏览器上下文同时编辑同一页，最终文档一致；断网重连后合并；编辑器 chunk 不进首屏（bundle 预算脚本通过）。
- **冲突说明**：只碰 knowledge-* crate、web/src/pages/spaces、web/package.json（新增 blocknote/yjs 依赖）。

---

## M3：连接器目录 + 平台 MCP Server + skills/插件兼容（对齐"连接应用"弹窗）

- **目录（Gateway Control）**：`crates/platform-gateway-foundation/src/catalog/`（新）：条目模型（connector/mcp_server/skill/plugin/bot_template，见 PRD §4.2），内置清单 `config/catalog/*.json`（Google Calendar、Notion、Outlook、Slack、GitHub、GitLab、Jira、Confluence、飞书、钉钉、Google Drive、AgentMail 等，图标放 `web/public/catalog/`），租户私有条目，可选从官方 MCP Registry 同步（只读，带 sha）。API：`GET /api/v1/catalog?q=&category=&kind=`、`GET /api/v1/catalog/{id}`。
- **连接**：`/api/v1/connections`（个人/共享/空间级）、`POST /api/v1/connections/{entry}/oauth/start` → 回调 `/api/v1/connections/oauth/callback`（OAuth 2.1 + PKCE、state 绑定会话、RFC 9207 issuer 校验、优先 CIMD 其次 DCR、resource indicator；令牌加密存于现有 provider_cipher 体系，绝不回传浏览器），API key/PAT 表单；状态机 available/connecting/connected/needs_reauth/error/disabled_by_policy/revoked。复用现有 MCP 注册表（`mcp_registry_api.rs`）而不是另起一套。若需新表，作为 successor 032（Gateway 角色）。
- **Agent 打通**：连接成功的 MCP 服务器工具进入内核 `tool_search/tool_describe/tool_call`；按调用用户解析凭据；写工具走审批。
- **平台 MCP Server**：Gateway 暴露 `/mcp`（Streamable HTTP，2026-07-28 无状态 + 2025-11-25 兼容，`server/discover`，Protected Resource Metadata，Bearer = 平台 API key 或 OAuth），工具：`space_list`、`space_search`、`space_read_page`、`space_create_page`、`space_append_blocks`、`knowledge_search`；同一组工具加入 Capability Worker 目录（`rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-worker/src/platform_catalog_v1.json` + 对应 broker，改动后重算 overlay manifest）。
- **Skills/插件兼容**：`crates/platform-agent-plugins` 读取 `.codex-plugin/plugin.json`、`.claude-plugin/plugin.json`、`.agents/plugins/marketplace.json`、`.mcp.json`；SKILL.md 按 agentskills.io 校验（name/description 约束、allowed-tools）；技能和插件出现在同一目录搜索里；新增内置技能 `agent-plugins/ai-docgen/skills/` 旁的 `agent-plugins/spaces/skills/space-writer/SKILL.md`。
- **前端**：`web/src/components/connect/ConnectAppsDialog.tsx`（原生 `<dialog>`）：搜索"搜索应用和技能"、分类 chip（全部/本机/办公协作/沟通/设计/开发/数据/密码管理/更多）、"已连接 N"+图标、"Bot 模板"链接、"为你推荐"网格（图标、名称、描述、连接按钮，连接中转圈）、"这台电脑"分组（Local Node 在线时显示文件/日历/提醒/通讯录等）；设置页 `ConnectorsSettings.tsx` 改为同一目录的管理视图（共享连接、策略）。
- **验收**：
```bash
cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-gateway-foundation catalog:: mcp_server:: connections:: && cargo test ... -p ai-platform-agent-plugins
bash scripts/dev/mcp-smoke.sh   # 新增：用 @modelcontextprotocol/sdk 客户端（legacy 与 2026-07-28 两种模式）连 /mcp，discover → tools/list → 调 space_search 返回 M2 建的页面；无 token 401 且带 resource_metadata
bash scripts/dev/oauth-mock-smoke.sh   # 新增：本地 mock 授权服务器完成 PKCE 连接，连接状态 connected，数据库里只有密文
cd web && pnpm type-check && pnpm lint && pnpm test && pnpm build && npx playwright test --config playwright.opensource.config.ts e2e/connect-apps.spec.ts
codex mcp add agentforge --url http://127.0.0.1:<port>/mcp   # 我本地用 Codex CLI 实测列工具（可选）
```
- **冲突说明**：与 M4 共同可能碰 overlay（capability catalog）。规则：**M3 拥有 `platform_catalog_v1.json` 与 overlay manifest**，M4 不改它们；M3 拥有 `platform-gateway-foundation/src/{catalog,mcp_server,connections}`，M4 拥有 `.../invocation_http/{ag_ui,a2a}`。

---

## M4：Codex harness 兼容 + SDK/协议适配 + 垂直 Agent 示例 + 评测

- **Codex 追踪**：按 `docs/harness/codex-upstream-upgrade.md` 把 `deploy/agent-runtime-source/lock.json` 升到上游最新稳定 tag（若 overlay 冲突过大，只做差异报告不升级）；生成 app-server v2 schema diff 报告 `reports/codex-app-server-diff-<date>.md`；补 `turn/steer`、`thread/fork` 等平台尚未投影的方法（在 overlay 与 Gateway 投影层）。环境缺上游源码/buildx 时：只跑 `make agent-runtime-source-contract` 等源码级门禁，镜像构建由我本地补。
- **协议适配（Gateway）**：AG-UI 1.0 事件流 `POST /api/v1/agui/runs`（Thread/Turn/Item → AG-UI 事件）；Vercel AI SDK UI Message Stream 适配 `POST /api/v1/ai-sdk/chat`；A2A 1.0：已发布 Agent 的 `/.well-known/agent-card.json` + send/stream/get task；Responses 兼容补齐 `previous_response_id`/工具调用的流式事件。
- **垂直 Agent 示例** `examples/vertical-agents/`：`langgraph-py`（LangChain 1.x `create_agent` + 平台 MCP）、`openai-agents-ts`（@openai/agents）、`claude-agent-ts`（@anthropic-ai/claude-agent-sdk，经 `/v1/messages`）、`adk-ts`（@google/adk）、`ai-sdk-ts`（`ai` 7.x + AG-UI/UI stream）。每个只配置 `BASE_URL`+`API_KEY`+`MCP_URL`，带 README 和一个 `npm test`/`pytest` 冒烟（默认对本地 mock 模型，`LIVE=1` 时对真实网关）。
- **评测**：`crates/platform-eval` 新增 trajectory 用例：空间工具（引用正确、无越权、写操作需审批）、连接器工具（只读默认）；OTel GenAI agent span 字段；`make eval-regression-gate` 纳入。
- **验收**：
```bash
cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-gateway-foundation ag_ui:: a2a:: -p ai-platform-eval
make agent-runtime-source-contract eval-regression-gate
for d in examples/vertical-agents/*/; do (cd $d && make test); done     # mock 模式全绿；LIVE=1 由我本地用阿里云新加坡 qwen3.8-flash 跑
npx @a2a-js/sdk validate http://127.0.0.1:<port>/.well-known/agent-card.json   # 或等价校验脚本
```
- **冲突说明**：不碰 web/（M5 再做 UI）；不碰 M3 拥有的文件。

---

## M5：前端全面现代化 + 性能与各服务优化 + 自动化补齐

- **前端现代化（MWG 全量）**：`legacy-scan web/src --json` 当前 8 条（`ime-safe-enter-submit` ×6、`autofill-sign-in-form` ×1、`form-fields-automatically-fit-contents` ×1）全部处理或写明保留理由；聊天输入 IME 安全回车；模态统一原生 `<dialog>`+`closedby`；View Transitions 路由切换；长列表 `content-visibility`；`fetchpriority`/预加载关键资源；收敛 antd/Radix 双体系的新增使用；修复 a11y spec 失败项。
- **性能**：Playwright 里度量 CWV（LCP<2.5s、INP<200ms、CLS<0.1）关键路由：`/workbench`、`/assistant`、`/knowledge`、`/spaces`；首屏 JS 不超过 `check-bundle-budget.mjs` 预算并下调 10%。Gateway `/v1/chat/completions` 额外延迟 p95<15ms（mock 上游，oha 压测）；Knowledge 检索 p95<300ms（不含 rerank）；空间列表 1 万条接口 p95<150ms。报告 `reports/perf-dot_1113-<date>.md`，含前后对比。
- **自动化**：定时任务支持 cron + 时区；Webhook 触发（HMAC 签名、重放保护）；事件触发（空间页面变更、连接器事件）；动作：运行 Agent/技能、写空间页面、发通知；"用自然语言创建自动化"（Agent 生成定时任务草稿，用户确认）。
- **验收**：
```bash
npx -y modern-web-guidance@latest legacy-scan web/src --json | jq .totalFindings   # 0，或 PR 中逐条说明保留理由
cd web && pnpm build && pnpm bundle:budget && npx playwright test --config playwright.opensource.config.ts e2e/all-routes-accessibility.spec.ts e2e/perf-cwv.spec.ts
cargo bench / oha 脚本：scripts/perf/run.sh（新增）输出与报告一致
cargo test --locked ... -p ai-platform-gateway scheduled_tasks:: webhooks:: && bash scripts/dev/automation-smoke.sh
```

## 本地验收流程（我执行）
每个 PR：拉分支到 `/workspace/af/wt_m<N>` → 跑该里程碑验收命令 → 原生启动冒烟 + 浏览器截图（存 `/workspace/af/evidence/`）→ 通过后 squash 合入 `dot_1113` → 记录 loop-state → 开下一个里程碑。失败则把具体输出贴回 PR 让同一云端 Agent 修。

## 今晚做不到 / 风险
- M0 的修复量未知（55 个编译错误 + 前端半合并 + 11 个门禁）；若 M0 超过 3–4 小时，M2 之后的里程碑顺延到明天。
- successor 030/031/032 的隔离 PG 测量回执若云端环境无 PG，由我本地补测，可能拖慢合并。
- 真实 OAuth 连接需要各厂商开发者应用；今晚只保证 mock 授权服务器端到端。
- Runtime overlay 镜像构建需要上游 Codex 源码与 buildx，云端可能只能做源码级验证。
- 多人协作的规模化与 HA 不在本期。
