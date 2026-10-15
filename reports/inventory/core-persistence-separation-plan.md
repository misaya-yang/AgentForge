# ARC-04 persistence god classes:跨域 SQL 盘点与分离方案

状态:**本轮只出清单与方案,不移动代码**(决定理由见 §6)。
机械证据:`reports/inventory/core-persistence-sql-inventory.json`
(schema `arc04-persistence-sql/v1`,重生成:`uv run python scripts/core_boundary/inventory_persistence_sql.py`)。

## 1. 方法

- 只扫描 `ai_gateway_core` 内 SQL **字符串常量**(AST 常量节点);运行时拼表名不可见,属已知盲区。
- 已知表集合来自 `database/schema.sql` + 全部 migrations(含 `per_service/**`)的
  `CREATE TABLE`,并跟随 `ALTER TABLE … SET SCHEMA` 迁移(含
  `per_service/_global/002_move_tables.sql` 的 JSONB 动态迁移清单),共 **252** 张已知表。
  不在集合内的读目标(CTE 名、正则噪声)标 `unresolved`,不计入跨域统计。
- 表 → writer-owner 域映射在 `inventory_persistence_sql.py` 的 `TABLE_OWNER`
  中,是**文档性声明**,供 ARC-03 收紧角色前复核,不代表当前数据库授权状态。

## 2. 跨域写入发现(5 个模块)

| 模块(home 域) | 跨域写入的域 | 涉及表 |
| --- | --- | --- |
| `persistence.database`(platform) | auth、memory、agent-runtime | users/user_roles/user_permissions/role_permissions/rbac_roles/auth_config/login_audit/api_keys/audit_logs、session_memory/user_memory、assistant.sessions |
| `persistence.repositories.agent_repository`(agent-studio) | agent-runtime、auth、eval-trace、mcp-connector、memory、platform | assistant.sessions、assistant_runs、assistant_run_checkpoints、agent_runtime_{attachments,feedback,idempotency}、audit_logs、agent_traces、mcp_connections、mcp_channel_grants、connector_credential_principals、session_memory、user_memory、assistant_memory_reflections、semantic_cache |
| `metrics.usage_recorder`(metrics) | platform | usage_records、usage_{daily,hourly}_aggregates、user_quotas、billing_events、request_traces |
| `metrics.context_metrics`(metrics) | platform | usage_statistics |
| `skills.builder`(skills) | agent-runtime | assistant_audit_events |

只读跨域(不计入分离优先级,但 ARC-03 授权时需保留读):
`agent_trace_repository` 读 agent-studio 表;`usage_recorder` 读 api_keys/users;
`agent_repository` 读 skills 表(assistant_skill_versions 等)。

## 3. god classes 现状

| 模块 | LOC | 问题 |
| --- | --- | --- |
| `persistence/database.py`(DatabaseStorage) | 4150 | 单类横跨 platform/auth/memory/agent-runtime 写 21 表 |
| `persistence/repositories/agent_repository.py` | 7255 | agent-studio 仓库写 6 个外域表组,含会话/记忆/trace/mcp |
| `persistence/repositories/agent_trace_repository.py` | 3417 | 本域干净(12 张 eval-trace 表),仅跨域读 |
| `persistence/repositories/mcp_repository.py` | 1655 | 本域干净(7 写 8 读,全在 mcp-connector) |
| `metrics/usage_recorder.py` | 2261 | 计量域整组写 platform 计费表 |

参照系:`agent_trace_repository`、`mcp_repository` 是"按表 owner 对齐"的完成态样板 ——
分离目标就是让 `database.py`/`agent_repository.py` 达到同样状态。

## 4. 分阶段分离方案(按 table writer owner)

原则:每次只把一个外域表组从一个 god class 中移出;每步不改行为(同 SQL、同事务边界、
同连接池);移动的是方法归属与调用点,不重写查询。

**阶段 0(本包已完成)**:机械盘点 + owner 映射 + 门禁基线
(`check_core_boundary` 冻结 core 模块集合,knowledge→core 依赖数 13 不增长)。

**阶段 1 — metrics → platform 计费仓库**(最小、最独立):
`usage_recorder`/`context_metrics` 的 6+1 张 platform 表写入收拢为独立
`usage_repository`(仍在 core 内,先对齐域,再论位置)。消费者只有 metrics 自身与
Gateway 装配点;无跨服务消费者变更。

**阶段 2 — database.py 的 auth 表组移出**:
users/roles/permissions/auth_config/login_audit 写路径归 `user_repository`/
`api_key_repository`(已存在,补齐写路径)。DatabaseStorage 保留连接管理。

**阶段 3 — agent_repository 的会话/记忆表组移出**:
assistant.sessions、assistant_runs、session_memory、user_memory、
assistant_memory_reflections 写路径归 `session_repository` 与 memory 域仓库。
这是体量最大的一步,建议再按表拆成子批次。

**阶段 4 — 残余外域边**:
agent_repository → mcp_* 表归 `mcp_repository`;→ agent_traces 归
`agent_trace_repository`;skills.builder → assistant_audit_events 随 skills
领域迁移(见 `core-domain-ownership-inventory.json`)。

**阶段 5 — 移交 ARC-03**:
每个域仓库只持有本域表后,ARC-03 按仓库→表清单生成角色授权、收紧
search_path/SECURITY DEFINER,并出回滚方案。PRD §ARC-04 目标 7 明确
"先消除跨域 SQL,再让 ARC-03 收紧角色"。

## 5. 已知盲区

- 运行时拼接的表名不在扫描范围;阶段实施前需对目标模块人工复核一遍。
- `TABLE_OWNER` 中个别归属待 ARC-03 定夺(如 version_retention_policies 现记
  platform、语义更接近 knowledge;artifacts 已单独列为 artifact 域)。
- 扫描只看 core;`src/` 侧 `src/services/storage/artifact_storage.py` 等是
  Phase 5f 既有 shim,写同表的唯一实现已在 core。

## 6. 本轮不执行代码移动的决定

PRD 允许"完全不改行为时做第一个具名小批次"。本轮判定**不做**,理由:

1. 同树并行多个工作包(ARC-00B/ARC-01/ARC-02/ARC-03 等)正在改动 `src/`、
   `database/`、测试装配,任何 persistence 层移动都无法在本轮内完成
   "改前改后同一测试基线"的零漂移验证;
2. 阶段 1(最小批次)的装配消费者位于 `src/`(不在本包 owned 路径),
   单独完成需要跨包协作,违反"每个提交只迁一个领域"的可回滚性;
3. 数据库角色收紧归 ARC-03(task #90),分离批次必须与其授权方案同批评审。

因此本包交付:机械盘点(§2-3)+ 分阶段方案(§4)+ 门禁冻结基线,
把第一个可执行批次(阶段 1)的入口条件写清楚,留给后续单独工作包。
