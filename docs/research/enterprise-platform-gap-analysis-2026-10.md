# 企业级 AI 平台差距分析与升级路线（2026-10）

**定位**：架构评审。以 2025–2026 业界实践为基线，评估 AI Gateway / AgentForge 的现状、差距与升级路径。
这是源码评审，不是部署验收声明；任何"已实现"仅指源码存在并通过其自带测试，不代表实机接受。

**元数据**：日期 2026-10-16｜基线 `22d9ce0` + 当日工作树｜范围：`rust/platform-native`、`rust/agent-runtime-overlay`、
`web/`、`database/`、`deploy/`、`.github/`、`docs/`｜方法：四路并行只读源码调研 → 主评审人对关键结论逐条
`grep`/`sed` 复核（见 §10）→ 71 次外部检索，一手规范优先（见 §9）｜状态：**active**。

**执行归属**：本文不是第二个状态权威，也不创建新 roadmap。所有建议按 §5 的 scope 列挂入
[`native-enterprise-upgrade/loop-state.json`](../../deploy/runbooks/native-enterprise-upgrade/loop-state.json)
（E1–E8、UX），由该程序排期与验收。文中结论在程序推进后会过期，复用前须按 §10 的命令重新核实。

---

## 0. 一页结论

**心智模型：内核硬、边缘薄。** 地基与承重结构（数据权威、持久执行、能力租约、计费账本）按高标准浇筑；
门窗（协议入口）、门禁（企业身份）、消防（安全护栏）、监控（遥测导出）、电梯冗余（HA）尚未达到商用验收。

1. 工程纪律处于业界前列：不可变基线 + 112 条有序迁移 + 指纹锁；HMAC 签名能力租约（TTL ≤ 120 s）；
   PostgreSQL 追加式事件日志 + 游标重连；9 份 ADR；约 1,900 个 Rust 测试 + 477 个覆盖层测试；边界门禁。
2. 但"Gateway"不说行业通用语：没有服务端 `/v1/chat/completions`、`/v1/embeddings`；推理只接受 OpenAI 兼容
   wire；跨供应商 failover 只做配置组装，执行交给遗留 LangGraph 适配器。
3. 企业采购清单上的硬项缺失：OIDC/SAML SSO、SCIM、Postgres RLS、密钥轮换/KMS、护栏/PII、Gateway 结构化
   日志与 OTel 指标导出、多实例 HA、合规文档。
4. Python 移植残留明显：12 个 `migration_*` 模块目录；51 个非测试文件引用 `langgraph`；`architecture.md`
   仍称"Retained Python `src/`"而 `src/` 已无 `.py`；Helm 仍含 `langgraph-agent.yaml`；前端 antd 6 与
   Radix 双体系并存，`@langchain/*` 依赖零引用。

---

## 1. 评审方法与证据边界

| 来源 | 内容 | 可信度 |
| --- | --- | --- |
| 四路只读源码调研 | Gateway/供应商平面；身份/安全/租户；可观测/部署/CI；Agent/RAG/评测 | 二手；关键结论已由主评审人复核 |
| 主评审人复核 | §10 列出的全部 `grep`/`sed`/`find` 命令与结果 | 一手 |
| 外部检索 | 规范、官方文档、OWASP/NIST/EU 一手页面；博客仅作景观参考 | 见 §9，含不确定项 |

不主张：任何实机、浏览器、供应商或生产就绪结论；任何未运行的测试通过。

---

## 2. 现状画像

**规模**（`wc -l`，2026-10-16）

| 区域 | 文件 | 行数 |
| --- | --- | --- |
| `rust/platform-native`（19 crates） | 1,025 `.rs` | 363,819 |
| 其中 `platform-gateway` / `-foundation` / `-integrations` | — | 97,085 / 98,471 / 52,107 |
| `rust/agent-runtime-overlay/kernel-rs` | 129 `.rs` | 91,311（37 个上游改动文件 + 111 个新文件，上游 Codex `rust-v0.162.1` 构建期合成） |
| `web/src` | — | 124,808 TS/TSX，151 个测试文件 |
| 数据库 | 112 条迁移，基线 `2026_08_post_kb_v1`，当前 epoch 22 | — |

**已达到或超过业界基线的能力**

| 能力 | 证据 |
| --- | --- |
| 数据库迁移权威 | [`database/authority`](../../database/authority/README.md)、`platform-migration-authority`：不可变基线、SHA-256 包锁、`startup-check`、作用域角色 |
| 持久执行 | `ai-platform-agent-runtime/src/postgres_store/`：同步提交追加日志、sticky failure fence；`http_service/events.rs` `after_sequence` 游标重放 |
| 能力执行契约 | `ai-platform-capability-contract/src/lib.rs`：`capability-descriptor/v2`、`runtime-capability-lease/v1`、`SideEffectUnknown` 屏障；审批决定行锁 + 回执 |
| 准入与配额 | `platform-gateway-foundation/src/capacity_admission.rs`（Redis Lua 租约）、`rate_limits.rs`（滑窗多维限流）、`request_governance.rs`（进程级准入 + 截止） |
| 计费账本 | `usage_recorder.rs`（微分成本）、`pricing_receipt.rs`（SHA-256 价格回执）、`billing.rs:414-474` `OverageStrategy::DowngradeModel`（即业界"budget fallback"） |
| RAG 检索 | `migration_knowledge_query/qdrant.rs:625` dense + BM25 稀疏 + RRF；`migration_knowledge_ingest/chunking.rs` 父子分块；OCR；`document_versions` 版本化发布 |
| 评测进 CI | `platform-eval`（79 文件）：recall@k/MRR/nDCG 门禁、trajectory 金标、LLM judge（DeepSeek，与生成模型分家）；`.github/workflows/ci.yml:130` |

---

## 3. 评分卡（1–5）

| 领域 | 分 | 现状摘要 | 关键证据 |
| --- | --- | --- | --- |
| 数据/迁移权威 | 5 | 见 §2 | `database/authority` |
| Agent 内核/能力执行 | 4.5 | 见 §2；seccomp BPF 沙箱 | `ai-platform-capability-worker/src/python_supervisor.rs` |
| 准入/配额/计费 | 4 | 见 §2；缺 TPM 限流、团队/项目级虚拟密钥 | `quota_store.rs:103-159` |
| 知识/RAG | 3.5 | hybrid+RRF、版本化；**reranker 默认关闭** | `platform-knowledge-service/src/langgraph_tools.rs:64,84,530` |
| 评测 | 3.5 | 离线门禁有；无线上评测、无 prompt/AgentSpec 金丝雀 | `platform-eval/src/eval_operations.rs` |
| 模型供应商平面 | 2.5 | 单 wire；Anthropic/Gemini 仅健康探针；无 Azure/Bedrock；无 hedging；"semantic_cache" 实为精确哈希 | `migration_agent/model.rs:640-663`、`provider_service.rs:466-487`、`migration_compat/response_cache.rs:1` |
| 身份与访问 | 2.5 | JWT HS256 单密钥、无 `kid`/轮换；RBAC 分散在 handler；无策略引擎；无 OIDC/SAML/SCIM | `platform-auth/src/lib.rs:153-221`、`rbac.rs:7-116` |
| 安全护栏/数据保护 | 1.5 | 无内容审核/PII/注入防御；Fernet 解密**明文直通且解密失败回退原值**；审计表无不可篡改约束 | `provider_cipher.rs:55-71` |
| 可观测性 | 2.5 | 仅 OTel trace；native Gateway **无 `tracing-subscriber`**（覆盖层有）；GenAI 遥测只落 PG；Grafana 规则引用不存在的指标 | `platform-gateway-foundation/Cargo.toml:9-11`、`distributed_tracing.rs`、`docker/monitoring/` |
| HA/部署 | 2 | Gateway+Runtime 强制单实例；本地卷状态；Helm 遗留；备份仅 `pg_dump` | `platform-tools/src/guard.rs:187-193`、`deploy/helm/ai-gateway/templates/langgraph-agent.yaml`、`scripts/new/backup.sh` |
| 供应链/CI | 2.5 | 无 clippy、无 cargo-deny/audit、fmt 仅 2 crate、无 cosign | `.github/workflows/ci.yml:40`、`docker-publish.yml:87-88` |
| 前端控制台 | 3 | React 19 + TanStack Query + zustand + i18n 合理；双 UI 库；死依赖 | `web/package.json` |
| 文档一致性 | 3 | ADR 优秀；`architecture.md`/`DEPLOY.md`/`RELEASE.md` 漂移 | `docs/harness/architecture.md:24,33` |

---

## 4. 业界基线对照（2025–2026）

| 领域 | 业界基线 | 本项目 | 判定 |
| --- | --- | --- | --- |
| Gateway 入口 | 一个 OpenAI 兼容端点覆盖 100+ 供应商；虚拟密钥带模型白名单/预算/RPM+TPM；模型名虚拟化 + 跨供应商 fallback [S1][S3] | 无 `/v1/chat/completions`；单 wire；failover 仅配置；有 RPM/RPD 无 TPM | **缺失** |
| 预算回退 | 预算触发降级到便宜模型 [S2] | `OverageStrategy::DowngradeModel` | 对齐 |
| Gateway 形态 | Envoy AI Gateway 2026-09 更名 Agent Router 并入 Linux Foundation AAIF；网关演进为 **MCP/A2A 代理** [S4][S8] | Gateway 已为 Worker 代理 MCP（内部），未对外暴露 | 部分，有先发位置 |
| 语义缓存 | Portkey 有；Envoy 仍在设计阶段 [S5] | 精确哈希 | 部分（非紧急） |
| 可观测性 | OTel GenAI semconv **仍为 Development**；`invoke_agent`/`execute_tool` span、`gen_ai.client.token.usage`、`gen_ai.client.operation.duration`；线上评测挂 trace [S9][S10][S11] | 仅 trace 导出；GenAI 数据只落 PG；Gateway 无结构化日志 | **缺失** |
| MCP | 2025-11-25：OAuth 2.1 + OIDC 发现、Tasks、URL elicitation；2026-07-28 RC：**无状态核心**、Tasks/Apps 变 Extension、**废弃 Roots/Sampling** [S13][S14] | 仅 client、仅 streamable-http、有 OAuth audience 校验 | 部分，方向正确 |
| A2A / ACP / Skills | A2A v1.0（LF，150+ 组织，可签名 Agent Card）[S15][S16]；ACP 已被 Zed/JetBrains/Copilot 采用 [S17][S18]；Agent Skills `SKILL.md` 开放标准 [S19] | 三者均无；`skills-lock.json` 仅锁设计技能 | **缺失** |
| 持久执行 | 每步 checkpoint + 确定性重放、幂等副作用回执、多日审批挂起为版本化事件 [S22] | 事件日志、`side_effect_unknown`、行锁审批回执 | **对齐，强项** |
| 沙箱 | "容器不是安全边界"：gVisor/Kata/Firecracker；K8s `agent-sandbox` CRD [S23][S24] | Docker `--network none` + seccomp BPF | 部分 |
| 安全框架 | OWASP LLM Top 10 **2026**（2026-08-03；LLM08 → Hidden Context Exposure）[S26][S27]；OWASP **Agentic Top 10**（ASI01–10）[S28] | 无 LLM01/LLM02 控制；审批/租约/工具白名单覆盖 ASI02/ASI03 | 部分 |
| 身份 | OIDC/SAML SSO + **SCIM** 为采购硬项 [S1] | 无 | **缺失** |
| 租户隔离 | RLS 作为纵深；`FORCE ROW LEVEL SECURITY` + 事务内 `SET LOCAL`；**连接池须重置租户上下文** [S40][S41] | 仅应用层谓词；角色已 `NOBYPASSRLS`，无 policy | 部分 |
| 合规 | EU AI Act：2026-08-02 Art.50 透明义务 + 执法生效；Annex III 高风险**推迟到 2027-12-02** [S29][S30]；ISO/IEC 42001 + NIST AI RMF [S31]；SBOM 含 AI/ML-BOM（CycloneDX 1.7）[S33] | 有 CycloneDX SBOM；无 AI-BOM/模型卡/合规映射 | 部分 |
| 评测 | 从生产 trace 策展 golden、CI 门禁、红队（Promptfoo 已被 OpenAI 收购 [S12]；备选 Garak/PyRIT）、judge ≠ 生成模型家族 [S35] | 离线门禁有；judge 已分家；无线上评测 | 部分 |
| RAG | hybrid + RRF + cross-encoder rerank 为默认；contextual retrieval [S36]；Docling/MinerU 解析 [S38]；专用向量库在十亿级或 p99 < 20 ms 才必要 [S37] | hybrid+RRF 已有，rerank 默认关；Qdrant 外置 | 对齐度高 |
| 企业通道 | Feishu Lark Channel SDK 流式卡片 [S43]；DingTalk Stream [S44]；WeCom 长连接机器人 [S45]；Slack Agents & AI Apps [S46]；**Teams Bot Framework 已退役，用 Microsoft 365 Agents SDK** [S47][S48] | 均无 | 缺失（产品法已列为优先 surface） |
| FinOps | token 为计量；按团队/应用/租户分摊；先 showback 再 chargeback [S42] | 成本账本、价格回执、配额告警已有；缺分摊报表 | 部分，数据层就位 |

---

## 5. 差距清单与升级建议

每条：问题 → 建议 → 证据 → 归属 scope → 验收。按商业阻断程度分级。

### P0 — 没有这些卖不进企业

**P0-1 入口协议补齐**（E3）
- 问题：无服务端 `/v1/chat/completions`、`/v1/embeddings`、`/v1/models`；推理 wire 仅 `responses_v1` / `chat_completions`
  且 Bearer-only，否则 422 `RUNTIME_PROVIDER_WIRE_UNSUPPORTED`；failover 候选只组装不执行。
- 建议：新增 OpenAI 兼容入口路由复用现有 `invocation_http.rs` 准入/计费管线；新增原生 Anthropic Messages、
  Gemini `generateContent` `ProtocolAdapter`（trait 已在 `service_registry.rs:356-379`）；允许 `dispatcher.rs`
  在"安全重试类别"内跨 provider 候选执行 failover，hedging 作为显式配置项；补 TPM 限流维度。
- 证据：`migration_adapters/openai.rs:55-57,117`（仅出站）；`migration_agent/model.rs:640-663`；
  `model_failover.rs:7`；`dispatcher.rs:9`。
- 验收：客户用官方 OpenAI / Anthropic SDK 零改动接入并计费正确；单 provider 故障时请求在同一调用内完成切换。
- 张力：见 §7-1。

**P0-2 企业身份**（E8）
- 问题：JWT HS256 单对称密钥、无 `kid`/轮换；`password_changed_at` 吊销在
  [`tenant-account-administration.md:81-85`](../architecture/tenant-account-administration.md) 自述"required but not yet enforced"；无 OIDC/SAML/SCIM。
- 建议：OIDC（authorization code + PKCE）作为第二身份源，映射到现有 `users`/`roles`；JWT 改 RS256/EdDSA + JWKS +
  `kid` 轮换；SCIM 2.0 用户/组同步；凭证修订号真正生效。API key 的无盐 SHA-256（`api_keys.rs:91,101`）对高熵
  随机密钥是行业常规，不是漏洞，只需加前缀/last4 便于识别与轮换。
- 证据：`platform-auth/src/lib.rs:153,183,192`。
- 验收：企业 IdP 登录 → 角色映射 → 停用账号在 IdP 侧生效后 ≤ 5 min 内平台拒绝。

**P0-3 租户隔离纵深**（E8）
- 问题：隔离仅靠应用层 `tenant_id` 谓词；仓库无任何 `ROW LEVEL SECURITY` / `CREATE POLICY`；审计表可 UPDATE/DELETE。
- 建议：对租户表启用 `FORCE ROW LEVEL SECURITY` + policy；应用在事务内 `SET LOCAL app.tenant_id`，由非 owner
  角色执行（`database/bootstrap/roles.sql:32-75` 已 `NOBYPASSRLS`）；连接归还池前确保上下文随事务结束 [S40]。
  审计表 `REVOKE UPDATE, DELETE` + 行哈希链；补 `044_tenant_soft_isolation.sql:34` 的保留策略 TODO。
- 验收：跨租户 SQL 否定测试在应用谓词被人为移除时仍被 RLS 阻断；审计行篡改触发校验失败。

**P0-4 可观测性闭环**（E8）
- 问题：native 工作区无 `tracing-subscriber`（19 个 crate 的 `Cargo.toml` 均无），Gateway 以 `eprintln!` 为主；
  OTel 仅 `trace` feature；`agent_traces` 的 TTFT/token/cost 只落 PG；`/metrics` 需 RBAC 而 Prometheus scrape
  无凭证；Grafana 告警引用 `langgraph_runs_total` 等已不存在的指标。
- 建议：Gateway/Knowledge 接 `tracing-subscriber` JSON + `EnvFilter`（覆盖层 `app-server/src/lib.rs:723-732`
  已有可复用模式）；OTLP 导出 metrics/logs；按 GenAI semconv 发 `gen_ai.*`，导出层设
  `OTEL_SEMCONV_STABILITY_OPT_IN=gen_ai_latest_experimental` 并版本化 schema [S9]；`/metrics` 提供 scrape token；
  重写 `docker/monitoring/` 规则与看板。
- 验收：一次 Assistant 对话在 Tempo/Grafana 可见 Gateway→Runtime→Worker→Provider 全链路 span，含 token/cost。

**P0-5 护栏管道 + 密钥卫生**（E8）
- 问题：请求路径无 PII 脱敏、注入检测、内容审核（仅 `mcp_http.rs:793-878` 对工具描述做字串替换）；
  `provider_cipher.rs:55-71` 对非 `enc:` 值明文直通，解密失败回退原值（fail-open）；主密钥在 env，无信封加密/KMS。
- 建议：Gateway 请求/响应两侧可插拔钩子（PII、注入、审核 provider 可选 NeMo Guardrails / Llama Guard [S32]），
  按 OWASP LLM 2026 的 LLM01/LLM02/LLM10 与 Agentic ASI01/ASI06 做威胁映射 [S26][S28]；`decrypt` 对非 `enc:`
  与解密失败一律报错，配一次性迁移；引入信封加密与 KMS/Vault 接口，支持轮换。
- 验收：注入样本集被拦截并留审计；明文凭证启动即失败；密钥轮换不中断服务。

### P1 — 从"能用"到"专业"

| # | 项 | 建议 | 证据 | scope |
| --- | --- | --- | --- | --- |
| P1-1 | HA | Gateway 无状态化（`gateway-data` 等本地卷外置 PG/S3）；Runtime 多实例 owner-fence 的 claim/recovery 门禁证据；解除 `guard.rs` 的 `replicas: 1` 硬约束 | `docker-compose.yml:709-719`、`harness.yml:166` | E8 |
| P1-2 | CI 门禁 | `clippy -D warnings`、cargo-deny（advisories/licenses/bans）、全工作区 fmt、cosign 镜像签名、Dependabot；约 800 处 `.unwrap()`（含内联测试）由 clippy 收敛 | `ci.yml` | E8 |
| P1-3 | 文档/部署一致性 | 见 §8 | — | E8 |
| P1-4 | RAG | reranker 默认开启；真语义缓存（embedding 相似度）；contextual retrieval；Docling 级解析；Qdrant 备份与租户分区核实 | `langgraph_tools.rs:64,84` | E5 |
| P1-5 | 评测 | 线上采样评测挂 trace；AgentSpec/prompt 金丝雀发布；红队套件进 CI（Garak/PyRIT） | `eval_operations.rs` | E8 |
| P1-6 | 沙箱 | `execute_python_code` 从 Docker+seccomp 升到 microVM（Firecracker/gVisor）；K8s 用 `agent-sandbox` CRD [S23] | `coding_sandbox.rs:319-435` | E1 |
| P1-7 | 互操作 | MCP server 对外暴露（按 2026-07-28 RC 无状态核心设计，不实现 Roots/Sampling [S14]）；A2A v1.0 Agent Card（可签名，与 `agent_versions.spec_hash` 对应）[S16]；ACP 适配 [S17]；Skill 直接采用 `SKILL.md` [S19] | `mcp_registry_store.rs` | E2/E7 |
| P1-8 | 企业通道 | Feishu（Lark Channel SDK 流式卡片）、DingTalk Stream、WeCom 长连接、Slack AI Apps、Teams 基于 Microsoft 365 Agents SDK [S43–S48] | — | E4/UX |

### P2 — 卓越

| # | 项 | 建议 |
| --- | --- | --- |
| P2-1 | 去移植化 | `migration_*` → 领域命名；清理 51 个 `langgraph` 引用（保留为可选 adapter，不渗透 failover）；前端统一 UI 体系、删 `@langchain/*` |
| P2-2 | FinOps | showback/chargeback 报表与导出；预算告警推送到通道；成本感知路由 |
| P2-3 | 合规资产 | 威胁模型；SOC 2 控制映射；ISO/IEC 42001 / NIST AI RMF 对照；EU AI Act 分期义务清单（2026-08 透明、2027-12 高风险）；AI-BOM + 模型卡 |

---

## 6. 路线图与验收定义

| 阶段 | 周期 | 内容 | 验收 |
| --- | --- | --- | --- |
| 0 当前 | 1–2 周 | 关闭 `loop-state.json` 四个 blocker，完成 checkpoint；把 §5 条目作为 E3/E8/E5/UX 子门禁挂入现有程序 | 干净工作树；Assistant / Knowledge / Local Node 三条 E2E 旅程通过 |
| 1 企业可售 | 6–8 周 | P0 全部 | 官方 SDK 零改动接入；IdP 登录；RLS 否定测试；全链路 trace+metrics+log；护栏可演示；明文凭证启动失败 |
| 2 专业化 | 8–12 周 | P1 | 双实例 Gateway 滚动升级零丢请求；Helm 一键部署含 Runtime/Worker；CI 全门禁绿；reranker 默认开 |
| 3 领先 | 持续 | P2 | 合规文档包可交付客户审计；至少两个企业通道实机 |

**企业级 Definition of Done（可测量）**
1. 外部客户使用 OpenAI 或 Anthropic 官方 SDK 零改动接入，计费与配额正确。
2. 企业 IdP（OIDC）登录与 SCIM 同步；停用在 ≤ 5 min 内生效。
3. 移除应用层租户谓词后，RLS 仍阻断跨租户读写（否定测试进 CI）。
4. 任一对话可在 OTel 后端追溯到 provider 调用，含 token/cost/TTFT。
5. 注入/PII 样本集拦截率有基线并进 CI；凭证明文零容忍。
6. Gateway ≥ 2 实例滚动升级零丢请求；RPO/RTO 有文档并演练。
7. CI：fmt + clippy + cargo-deny + 全工作区测试 + 签名镜像。
8. 合规文档包：威胁模型、控制映射、AI-BOM、模型卡。

---

## 7. 架构张力与需要 ADR 的决策

1. **Gateway 执行 failover 与产品法 L5**。[`platform-architecture.md`](../harness/platform-architecture.md) L5 规定
   Gateway 只管控制策略不管执行语义。建议 ADR 明确："供应商层 failover/hedging 属于 provider adaptation
   （L5 已列入 Gateway 职责），不属于 Agent 执行语义（模型/工具迭代、取消、审批）"，并限定在安全重试类别内。
2. **RLS 与应用层谓词的关系**。RLS 是纵深，不替代谓词、对象授权和 owner fence（与
   [`architecture.md`](../harness/architecture.md) §4 "A role never substitutes for tenant predicates" 一致）。
3. **MCP 目标版本**。以 2025-11-25 为稳定基线，按 2026-07-28 RC 的无状态核心设计；RC 是否已定稿需在实施前复核 [S14]。
4. **沙箱边界**。容器 + seccomp 是否足以承载企业用户代码；若否，microVM 的运维成本如何进入 `deploy/`。
5. **Qdrant vs pgvector**。现状 Qdrant 带 BM25 稀疏向量是合理选择；代价是多一个 HA/备份对象。暂不迁移，
   但需补备份与租户分区证据。

---

## 8. 文档与部署漂移修正清单

| 文件 | 漂移 | 修正 |
| --- | --- | --- |
| `docs/harness/architecture.md:24,33` | "Retained Python `src/`"、"default Dockerfile/Compose commands still select Python Gateway" | `src/` 已无 `.py`，`Dockerfile` 全 Rust；改为当前拓扑 |
| `DEPLOY.md:170-171,179-185` | 称 compose "retain[s] Python Gateway"；Helm 为 "retained Python profile" | 同上；Helm 重建后更新 |
| `RELEASE.md` | 引用 `uv run pytest`、`pyproject.toml`、`islamic-content-service` 镜像 | 按 Rust 发布流程重写 |
| `deploy/helm/ai-gateway/` | `langgraph-agent.yaml`、`langgraph-db-init-job.yaml`；无 agent-runtime / capability-worker | 重建 chart |
| `docker/monitoring/` | 告警与看板引用 `langgraph_runs_total`、`event_dlq_depth`；scrape 无凭证；`postgres:15` | 对齐 `migration_host/metrics.rs` 实际指标 |
| `web/package.json` | `@langchain/core`、`@langchain/langgraph-sdk` 零引用 | 删除 |

---

## 9. 来源

Gateway
- [S1] LiteLLM Proxy 功能基线 — https://docs.litellm.ai/docs/simple_proxy
- [S2] LiteLLM Budget fallbacks — https://docs.litellm.ai/docs/proxy/budget_fallbacks
- [S3] Agent Router 0.7 token 用量限流（CEL） — https://theagentrouter.ai/docs/0.7/capabilities/traffic/usage-based-ratelimiting
- [S4] Envoy AI Gateway 更名 Agent Router（AAIF） — https://theagentrouter.ai/blog/envoy-ai-gateway-is-now-agent-router/
- [S5] Envoy AI Gateway 语义缓存议题 — https://github.com/envoyproxy/ai-gateway/issues/30
- [S6] agentgateway（MCP/A2A 数据面） — https://agentgateway.dev/
- [S7] LiteLLM vs AI gateways 景观 — https://contabo.com/blog/litellm-vs-ai-gateways/
- [S8] Agentic AI Foundation — https://aaif.io/ ；成立公告 https://www.linuxfoundation.org/press/linux-foundation-announces-the-formation-of-the-agentic-ai-foundation

可观测性
- [S9] OTel GenAI 属性注册表 — https://opentelemetry.io/docs/specs/semconv/registry/attributes/gen-ai/
- [S10] semantic-conventions-genai 仓库（状态 Development） — https://github.com/open-telemetry/semantic-conventions-genai
- [S11] Agent observability 2026 — https://www.braintrust.dev/articles/agent-observability-complete-guide-2026
- [S12] OpenAI 收购 Promptfoo — https://openai.com/index/openai-to-acquire-promptfoo/

Agent 互操作与运行时
- [S13] MCP 2025-11-25 changelog — https://modelcontextprotocol.info/specification/2025-11-25/changelog/
- [S14] MCP 2026-07-28 release candidate — https://blog.modelcontextprotocol.io/posts/2026-07-28-release-candidate/
- [S15] A2A 150+ 组织 — https://www.linuxfoundation.org/press/a2a-protocol-surpasses-150-organizations-lands-in-major-cloud-platforms-and-sees-enterprise-production-use-in-first-year
- [S16] A2A 规范 — https://github.com/a2aproject/A2A/blob/main/docs/specification.md
- [S17] ACP — https://agentclientprotocol.com/get-started/agents
- [S18] JetBrains ACP — https://www.jetbrains.com/acp/
- [S19] Agent Skills 开放标准 — https://github.com/agentskills/agentskills
- [S22] Durable execution for agent runtimes — https://zylos.ai/research/2026-04-24-durable-execution-agent-runtimes/
- [S23] kubernetes-sigs/agent-sandbox — https://github.com/kubernetes-sigs/agent-sandbox
- [S24] Firecracker vs gVisor — https://fly.io/learn/firecracker-vs-gvisor/
- [S25] OpenAI Agents SDK 迁移指南 — https://developers.openai.com/cookbook/examples/agents_sdk/migrate-from-claude-agent-sdk/readme

安全与合规
- [S26] OWASP GenAI LLM Top 10 2026 — https://genai.owasp.org/resource/owasp-genai-llm-top-10-2026/
- [S27] OWASP LLM Top 10 2026 变化解读 — https://www.imperva.com/blog/owasp-llm-top-10-2026-what-changed/
- [S28] OWASP Top 10 for Agentic Applications — https://genai.owasp.org/resource/owasp-top-10-for-agentic-applications-for-2026/
- [S29] EU AI Act 实施时间线 — https://ai-act-service-desk.ec.europa.eu/en/ai-act/timeline/timeline-implementation-eu-ai-act
- [S30] Digital Omnibus 推迟高风险期限 — https://www.gibsondunn.com/eu-ai-act-omnibus-agreement-postponed-high-risk-deadlines-and-other-key-changes/
- [S31] NIST AI RMF — https://www.nist.gov/itl/ai-risk-management-framework
- [S32] NeMo Guardrails — https://github.com/NVIDIA-NeMo/Guardrails
- [S33] CycloneDX — https://cyclonedx.org/

评测与 RAG
- [S35] LLM-as-judge 偏差 — https://www.alphaxiv.org/abs/2410.21819 ；https://arxiv.org/html/2604.22891v4
- [S36] Contextual retrieval — https://www.anthropic.com/news/contextual-retrieval
- [S37] Postgres 向量检索扩展性 — https://clickhouse.com/resources/engineering/scale-vector-search-postgres
- [S38] RAG 文档解析器对比 2026 — https://agentscamp.com/guides/comparisons/best-document-parsers-for-rag-2026

多租户与 FinOps
- [S40] RLS 连接池泄漏 — https://stackoverflow.com/questions/80003232/postgres-row-level-security-leaks-across-tenants-when-using-a-pooled-connection
- [S41] PostgreSQL RLS 实践 — https://rivestack.io/blog/postgresql-row-level-security
- [S42] FinOps for AI — https://www.finops.org/wg/finops-for-ai-overview/

企业通道
- [S43] Feishu 通道接入 — https://open.feishu.cn/document/mcp_open_tools/integrating-agents-with-feishu/integrate-feishu-channel
- [S44] DingTalk Stream 模式 — https://open.dingtalk.com/document/resourcedownload/Introduction-to-stream-mode
- [S45] WeCom 智能机器人 — https://work.weixin.qq.com/nl/index/openclaw
- [S46] Slack AI apps — https://docs.slack.dev/ai/
- [S47] Bot Framework 退役 — https://learn.microsoft.com/en-us/azure/bot-service/what-is-new?view=azure-bot-service-4.0
- [S48] Microsoft 365 Agents SDK 迁移 — https://learn.microsoft.com/en-us/microsoft-365/agents-sdk/bf-migration-guidance

**不确定项**：MCP 2026-07-28 RC 是否已定稿；OTel GenAI semconv 稳定时间；DingTalk AI 卡片细节；Agent Skills 采用方名单。

---

## 10. 复核记录（2026-10-16，主评审人一手）

| 结论 | 命令 / 位置 | 结果 |
| --- | --- | --- |
| 无服务端 `/v1/chat/completions` | `rg '"/v1/chat/completions"\|/v1/embeddings\|/v1/messages' rust/platform-native/crates --type rust` | 仅 `migration_adapters/openai.rs:55-57,117`（出站）、`embedding.rs:862-866`、`model.rs:659`、`provider_service.rs:475`（健康探针） |
| 推理 wire 仅两种 | `sed -n 640,663p .../migration_agent/model.rs` | `wire != "chat_completions"` → 422 `RUNTIME_PROVIDER_WIRE_UNSUPPORTED` |
| failover 不执行 | `sed -n 1,10p .../model_failover.rs`；`sed -n 1,18p .../dispatcher.rs` | "no retry execution or provider traffic here"；"retries never select a different provider" |
| native 无 `tracing-subscriber` | `grep -rln tracing-subscriber rust/platform-native/crates/*/Cargo.toml` | 空；覆盖层 `app-server/src/lib.rs:723-732`、`agent-runtime/src/main.rs:82` 有 |
| JWT HS256 | `grep -n 'HS256\|Algorithm::' platform-auth/src/lib.rs` | `:153` "supports HS256 only"，`:183,192` |
| Fernet fail-open | `sed -n 50,72p .../provider_cipher.rs` | 非 `enc:` 直通；`unwrap_or_else(\|\| value.to_owned())` |
| 单实例硬约束 | `sed -n 185,195p platform-tools/src/guard.rs` | `replicas != ["1"]` → failure |
| reranker 默认关 | `grep -n rerank platform-knowledge-service/src/langgraph_tools.rs` | `:64,84` `rerank: false`；`:530` `unwrap_or(false)` |
| CI 无 clippy / cargo-deny | `grep -c clippy .github/workflows/*.yml`；`grep -ci 'cargo-deny\|cargo audit'` | 全部 0 |
| Python 已清空 | `find src packages -name '*.py' \| wc -l` | 0（`src/` 26 个文件均为 fixture/catalog） |
| `migration_*` 残留 | `find rust/platform-native/crates -maxdepth 3 -type d -name 'migration_*' \| wc -l` | 12 |
| langgraph 渗透 | `grep -rl langgraph rust/platform-native/crates --include='*.rs' \| grep -v test \| wc -l` | 51 |
| 前端双 UI / 死依赖 | `grep -rl "from 'antd'" web/src \| wc -l`；`@radix-ui/`；`@langchain/` | 45 / 11 / 0 |
| Helm 遗留 | `ls deploy/helm/ai-gateway/templates` | 含 `langgraph-agent.yaml`、`langgraph-db-init-job.yaml`，无 runtime/worker |
| 规模 | `find ... -name '*.rs' \| xargs wc -l` | §2 表 |
