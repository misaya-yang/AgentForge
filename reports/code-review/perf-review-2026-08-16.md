# 全库性能审查报告

- 日期：2026-08-16
- 基线：`main@a49eac3`(工作树未提交改动不影响本次只读审查)
- 方法：15 个只读静态分析代理按热路径分区并行审查 + 1 个覆盖批评代理;全程未修改文件、未运行基准
- 产出：149 个热点(6 Critical / 39 High / 60 Medium / 44 Low)+ 7 个覆盖缺口 + 7 个交叉维度
- 性质声明：**所有延迟/吞吐数字均为代码结构推断的估算,不是实测值**。修复前必须先建基线(见 §8),不能拿估算当验收标准。

## 0. 审查分区

| 编号 | 区域 | 编号 | 区域 |
| --- | --- | --- | --- |
| p1 | 网关入口→代理转发 | p9 | 知识摄入管线 |
| p2 | 网关 DB 热路径 | p10 | 知识检索热路径 |
| p3 | 配额/用量计量 | p11 | 共享存储/事件 |
| p4 | Agent loop 每轮成本 | p12 | Web 聊天渲染 |
| p5 | 记忆热路径(H8) | p13 | Web 大页面 |
| p6 | 流式出口 | p14 | Web 打包/启动 |
| p7 | 工具执行 | p15 | 运行时配置 |
| p8 | 子代理 + MCP | — | 覆盖批评 |

## 1. Critical(6)

### C1 每轮请求内同步阻塞读记忆源 + 全量哈希(事件循环停顿)
`apps/assistant-service/src/assistant_service/core/agent/middlewares/runtime_memory.py:148`
- 机制：每个模型调用前 `load_memory_context` → `read_recent_sources()` 在事件循环上做阻塞 `os.open/os.read`(最多 ~8 个源),然后每个源再 `index_source` → manifest 查询 + advisory lock + 全文件 md5/sha256。H8 的 content-hash 短路只跳过嵌入,不跳过读取/查询/哈希。
- 频率：每轮请求。成本估算：多源 × 全量字节读取 + 哈希 + 每源 2 次 DB 往返,大文件时可秒级停顿事件循环。
- 方向：按 (st_mtime_ns, size) 进程内缓存跳过未变化源;读取放 `asyncio.to_thread`;把短路提升到文件读取层。

### C2 每个流式事件 3 次串行 DB 往返写 trace(每轮 150–900 次)
`apps/assistant-service/src/assistant_service/core/agent/execution_lifecycle.py:721`
- 机制：每个 text_delta/thinking_delta/tool 事件(文本按 ~120 字符切块)都触发后台任务执行 upsert trace root + upsert lifecycle span + insert event,共 3 次串行 DB 语句;最多 256 个并发后台任务与 checkpoint、消息持久化、SSE 争抢连接池。
- 频率：每个流式事件(工具密集轮 50–300+ 事件)。成本估算：~150–900 次 DB 往返/轮。
- 方向：按 run 缓冲批量 flush(每 25 事件或 finish 时);trace root 每 run 写一次;text_delta 存聚合计数或采样。

### C3 H8 核心:每轮全量重切块 + 重嵌入整个累积日记文件
`apps/assistant-service/src/assistant_service/core/runtime/memory/indexer.py:479`
- 机制：日记文件每轮追加,`_replace_source_derivatives` 删除全部 SQL chunks → 对整个累积文件(增长到 4MB 上限)重新 chunk_markdown → 重新嵌入每个 chunk(估 ~750 chunks)→ 删除并重插全部向量点。content-hash 短路对日记文件永不生效(内容每轮变化)。
- 频率：每完成一轮。成本估算：O(文件大小) 切块 + N_chunks 次嵌入 API 调用 + 2–4 Qdrant + 8–10 DB 往返,随会话长度线性增长。
- 方向：增量索引——日记是纯追加结构,按字节水位只切块/嵌入新增条目,延续 chunk_index;仅真正非追加修改才全量重建。

### C4 多模态嵌入逐 chunk 串行调用 DashScope,无超时无重试
`apps/knowledge-service/src/knowledge_service/services/knowledge/embedding.py:1816`
- 机制：`for text in texts: await self._call_api([{'text': s}])` —— 每 chunk 一次 HTTP 调用,严格串行;`_call_api` 无 `asyncio.wait_for`;API 本身接受多元素列表(同文件 embed_image_with_context 已证明),代码未用。
- 频率：多模态数据集每次摄入的全部变更 chunk。成本估算：500-chunk 文档 ≈ 500 次串行调用 ≈ 100+s;外层 90s 超时触发后整批从头重试。
- 方向：一次多元素调用(每批 10–50),加单调用超时与有界重试。

### C5 流式渲染每帧全量重解析 Markdown(O(n²)),声称的块级 memo 未实现
`web/src/components/StreamOutput.tsx:404`
- 机制：每次 RAF flush(~60/s)对**整个累积文本**重跑 17 正则泄漏过滤、RTL 检测、ReactMarkdown(remark+GFM+math+katex)全量解析;文件头注释声称有块级 memoization,实际不存在。
- 频率：每个流式帧。成本估算：20k 字符响应在尾部帧每帧解析 20KB + katex(2–8ms/帧),整体 O(n²)。
- 方向：按 `\n\n` 切块,已完成块 memo 输出,只渲染尾部未完成块;泄漏过滤/RTL 只跑变化尾部;帧率可降到 20–30fps。

### C6 antd manualChunks 把全站 antd 打进 1.39MB 预加载 chunk,包括 /login 与公开页
`web/vite.config.ts:52`
- 机制：manualChunks 把所有 antd 导入并入单一 `ui` chunk(1,394,910 B min / 419,088 B gzip),`App.tsx` 入口引用 ConfigProvider 导致 `dist/index.html` modulepreload 到**每个路由**(含未认证 /login、公开 /share /quiz);vendor 规则在 rolldown-vite 8 下只兑现一半(react-dom 进了 ui chunk,vendor 是 435 字节 stub)。即已知 >500kB chunk 警告的根因(构建日志 as-07-frontend-build.log:250)。
- 频率：每次页面加载。成本：~65% 的首屏关键路径(~640KB gzip)。
- 方向：移除 antd manualChunks 或迁移到 output.codeSplitting(构建日志自身建议);修复 vendor;加 CI 包体预算门禁。

## 2. High(39,按主题分组)

### 2.1 网关入口每请求 Redis 往返放大(p1)
| 位置 | 问题 | 方向 |
| --- | --- | --- |
| `src/core/gateway/admission.py:516` | CapacityAdmissionController 每共享预算 3 次**非管道串行** Redis 等待(zremrangebyscore→zcard→zadd),2–3 个预算 ≈ 每请求 12–18 RTT(估 15–50ms) | 每预算一个 Lua 脚本(1 RTT);或至少合并管道;per-request logger.info 降 DEBUG |
| `src/core/middleware/_streaming/rate_limit.py:118` | 两层互相重叠的 Redis 滑窗限流(中间件 3 检查 + 路由级 4–5 检查)≈ 每请求 7–9 RTT,预算语义无单一权威 | 合并为一个限流点,所有维度一次管道往返 |

### 2.2 用量计量 flush 路径(p2/p3)
| 位置 | 问题 | 方向 |
| --- | --- | --- |
| `packages/ai-gateway-core/src/ai_gateway_core/metrics/usage_recorder.py:862/865` | 每条记录一次 `fetchrow(INSERT...RETURNING)` —— 100 记录/flush ≈ 100 串行 RTT;p3 确认每 5s 或满 100 条触发 | 单条多行 `VALUES(...)...ON CONFLICT...RETURNING (xmax=0)`,保留 inserted/updated 区分 |
| 同文件 `:1030`(日)/`:1112`(时) | 每个聚合键 1 UPDATE+1 INSERT(2 RTT);WHERE 里 `COALESCE(user_id,'')` 包裹 NOT NULL 列,打掉唯一索引;另有与 00:30 聚合任务的唯一冲突竞态 | 单条 `INSERT...ON CONFLICT DO UPDATE`;去掉 COALESCE |
| 同文件 `:1224`(及 1359/1520/1572/1813) | Dashboard 查询 `created_at::date >= $2` 非 sargable,打掉 `idx_usage_tenant_date`,退化为租户全量扫描;且 usage_records **无任何保留清理**,docstring 声称的 30 天保留无代码实现 | 改半开区间 `created_at >= $2::timestamptz AND < $3+1day`;加保留任务 |
| `src/services/billing/quota_service.py:300` | 每个 LLM 请求 `SELECT * FROM user_quotas`(首次 3 RTT),零缓存 | 进程内短 TTL 缓存 + 显式列 + 首次 `INSERT...ON CONFLICT...RETURNING` |
| `src/services/billing/quota_service.py:250` | 所有租户的 RPM 检查共享**一把全局 asyncio.Lock**;`_rpm_requests` 按 (tenant,user) 永不驱逐 | 按哈希分片锁(16–32);Redis INCR/EXPIRE(同时解决多 worker 正确性) |

### 2.3 会话 JSONB 全量加载(p2)
| 位置 | 问题 | 方向 |
| --- | --- | --- |
| `packages/ai-gateway-core/src/ai_gateway_core/session/database_manager.py:136` | 每轮多次 `SELECT *` 加载整行(history 为增长 JSONB,迁移 049 注释称 6.9MB),每次全量反序列化;add_message 后清缓存导致下一次又全量重读 | 标题检查改 `SELECT metadata->'title'`;加轻量 tail 查询;追加后原地更新缓存而非删除 |
| 同文件 `:44` | Redis 未启用时 `_memory_cache` 为无上限 plain dict,只读会话永不驱逐 | LRU/大小上限 + TTL(仿 database.py:163 权限缓存模式) |

### 2.4 Agent loop 每轮上下文重序列化(p4/p6)
| 位置 | 问题 | 方向 |
| --- | --- | --- |
| `.../core/agent/agent_model_turn.py:121` | 每模型迭代 bind_model_boundary 全上下文 ~8–10 次序列化(deepcopy、后缀逐位 canonical-JSON、3 次全量 token 估算各自 deepcopy+json.dumps、packet digest、json.loads、failover 无条件估算) | ContextPacket 存 per-message token 估算一次算好处处复用;后缀匹配改哈希比较 |
| `.../core/agent/agent_turn_lifecycle.py:943` | 每次 checkpoint save 全消息列表哈希**两次**(_message_state_hash + _checkpoint_receipt 各跑 _message_state_digest),且 DB INSERT 在流式生成器内 awaited;默认开启(ASSISTANT_GATEWAY_ENABLED=true) | 每 save 算一次 digest 两处复用;per-tool checkpoint 改非阻塞任务;model_turn_started 降频 |

### 2.5 记忆热路径细节(p5)
| 位置 | 问题 | 方向 |
| --- | --- | --- |
| `.../core/runtime/compat/runtime_adapter.py:321` | 请求路径对每个近期源再跑一遍 index_source(advisory lock + manifest + 全文件哈希 + chunk 预扫描),即使上一轮 turn_sync 刚索引过;后台 sync 被跳过时全量重嵌入进请求 TTFB | 按 mtime vs manifest updated_at 一跳新鲜度检查;请求路径只验证不重建 |
| `.../core/runtime/memory/indexer.py:274` | `expected_chunks = len(chunk_markdown(...))` 在 content-hash 短路**之前**执行——未变化源每轮仍付全量 token 估算扫描(逐字符 CJK 计数) | 短路判断提到 chunk_markdown 之前(一行级改动) |
| `.../core/runtime/memory/source_store.py:499` | append 实现为整文件 read-modify-write:全文件逐行 normalize + 全量重写 + fsync,50 轮会话 ≈ 400MB+ 文件 I/O | O_APPEND 追加 + 仅尾部去重;fsync 按批次边界 |

### 2.6 流式出口(p6)
| 位置 | 问题 | 方向 |
| --- | --- | --- |
| `.../core/tools/builtin_tools.py:444` | KB 完整 chunk 文本塞进 tool_call_completed 的 metadata 原样过 SSE(与已压缩的 context_retrieved 重复),>64KB 触发 spill(artifact 写+读回)且 metadata 不在 _SPILLABLE_FIELDS | 出口前用 compact_context_payload 压缩/剥离 contexts;metadata 加进 spill 字段 |

### 2.7 工具执行(p7)
| 位置 | 问题 | 方向 |
| --- | --- | --- |
| `.../core/code_executor.py:840` | 每次代码执行全量容器 create/start/wait/remove + images.get,无热池复用(每轮多次调用,各付 0.5–2s 冷启动) | 按镜像小热池或 per-session 容器 docker exec 复用 |
| 同文件 `:769` | 容器无 log_config,json-file 在 daemon 侧无界增长(主机侧 2MB 读上限不约束运行期 daemon 磁盘) | `log_config={json-file, max-size 10m, max-file 1}` |
| `.../core/tools/code_executor_tool.py:381` | 最多 24MB 输出文件全部 base64(~32MB str)塞进 ToolCallResult,但 LLM 只看到文件名/大小 | 结果里删 content_base64,改 fetch-by-id 端点 |
| `.../core/tool_invoker.py:754` | 每次工具调用 fresh=True 重读 tenant policy(PG SELECT,旁路 300s TTL 缓存);MCP 调用另付 4 表授权 JOIN | 策略快照每轮固定一次;授权结果在 invoke 与 runtime 间复用 |

### 2.8 MCP(p8)
| 位置 | 问题 | 方向 |
| --- | --- | --- |
| `.../core/mcp/client.py:571` | 每次 MCP RPC 在事件循环上同步 `socket.getaddrinfo`(阻塞 DNS,全体租户受影响) | 进程内缓存 pinned 地址,TTL/连接失败才重解析 |
| `.../core/mcp/runtime.py:728` | 每次 tenant MCP 调用新建 MCPClient + 完整 initialize 握手(3 RTT + TLS + DNS),无会话复用 | 按 (tenant,connection) 有界缓存已初始化 client(仿 _connection_breakers 模式) |

### 2.9 知识摄入(p9)
| 位置 | 问题 | 方向 |
| --- | --- | --- |
| `.../services/knowledge/hierarchical_indexer.py:279` | 每 L2 段一次串行 LLM 摘要调用,无并发无缓存(50 段 ≈ 50 串行调用 ≈ 5–20+ 分钟) | 信号量并发(3–5)+ 段文本哈希不变则跳过 |
| `.../services/knowledge/worker.py:1155` | 层级/大文件路径重摄入从不查位置哈希:重下载、重抽取、重嵌入、重摘要 100% 内容 | 把标准路径的 per-position 哈希跳过移植进 index_document |

### 2.10 知识检索(p10)
| 位置 | 问题 | 方向 |
| --- | --- | --- |
| `.../services/knowledge/hierarchical_retriever.py:698` | 上下文增强逐 id 串行单行 SELECT(1 RTT/doc + 1 RTT/parent,5–20 次/请求) | 两条 `= ANY($1)` 批量查询 |
| 同文件 `:565` | 活跃状态过滤每层级跑一遍,路由层又对已过滤结果**重复过滤一次**(~7 RTT 中 2 个纯重复) | 单一过滤点(删路由层重复) |
| `.../services/knowledge/retrieval_service.py:1196` | Python BM25 对最多 80 个 FTS 候选全量 tokenize(代码自注释:200 文档 ≈ 1.7s),跑在事件循环上 | 只对 top 20–30 按 ts_rank 排序候选重打分;或按 content_hash 缓存 token 列表 |
| `.../services/knowledge/vector_store.py:258` | 每次请求 2–10 次未缓存 Qdrant get_collection RPC(search/filter 校验/preflight/ping/层级 4 个),全部串行 | 进程内按集合名缓存(TTL + 写路径失效) |

### 2.11 共享存储/事件(p11)
| 位置 | 问题 | 方向 |
| --- | --- | --- |
| `packages/ai-gateway-core/src/ai_gateway_core/events/consumer.py:271` | 每条消息 4 次串行 Redis RTT(SET-NX/HINCRBY/XACK/HDEL),32 条批 ≈ 128 RTT,跨主机 50ms RTT 时吞吐 ~5 事件/s | 管道化 HINCRBY+XACK+HDEL;批 XACK |
| `packages/ai-gateway-core/src/ai_gateway_core/metrics/usage_recorder.py:422` | 事件总线 publish(2 个 pydantic 模型 + JSON + XADD)在请求路径内 awaited(每请求 1 RTT,agent 10 步 = 10×) | 并入现有 5s flush 批处理;或 fire-and-forget |

### 2.12 Web 聊天渲染(p12)
| 位置 | 问题 | 方向 |
| --- | --- | --- |
| `web/src/pages/assistant/components/ChatMessage.tsx:332` | 未 React.memo:每帧 setMessages 重渲染最多 200 条历史消息(~12k 渲染/s) | 一行改动:React.memo(消息对象身份对非流式消息已稳定) |
| `web/src/pages/assistant/hooks/useChatSession.ts:1404` | thinking_delta/SUBAGENT 事件绕过 RAF 批处理,每 token 一次同步 setMessages + 全量 prev.map(3k-token 思考块 ≈ 3000 次渲染) | 走文本同款 RAF 累加器 |

### 2.13 Web 大页面(p13)
| 位置 | 问题 | 方向 |
| --- | --- | --- |
| `web/src/hooks/useKnowledge.ts:33` | 全量文档列表(后端硬编码 limit=200)每 2s 轮询,任何 tab 常开(~1800 请求/小时/页面) | 仅处理中状态或激活 tab 轮询;改无效化驱动 |
| 同文件 `:43` | 选中文档时全量段列表(SELECT * LIMIT 100 含全文)每 5s 轮询(~720 请求/小时) | 仅任务进行中轮询;分页/仅元数据 |
| `web/src/pages/knowledge/DatasetDetail.tsx:1060` | QA 流式每 delta:全消息数组 map + 全量 ReactMarkdown 重解析 + smooth scrollIntoView,O(n²) | RAF 缓冲到 10–20 次/s;块级 memo;streaming 时 auto 滚动 |

### 2.14 Web 打包(p14)
| 位置 | 问题 | 方向 |
| --- | --- | --- |
| `web/src/i18n/index.ts:4` | 6 个 locale JSON 静态导入进预加载 api chunk:**两种语言**都发给每个用户(~104KB gzip,占 api chunk 的 ~80%) | 静态只打包当前语言,切换时动态 import 另一种 |

### 2.15 运行时配置(p15)
| 位置 | 问题 | 方向 |
| --- | --- | --- |
| `Dockerfile:142` | 三个应用服务全部单 uvicorn worker = 全平台 CPU 天花板 1 核;多 worker 被 `INTERNAL_COMM_STATE_BACKEND=memory`、`INTERNAL_IDEMPOTENCY_BACKEND=memory`、进程内限流兜底**静默破坏** | GATEWAY_UVICORN_WORKERS 等显式配置面,默认 2;workers>1 时 validate-env 强制 INTERNAL_*_BACKEND=redis |

## 3. Medium / Low 汇总(按区域压缩)

<details><summary>p1 网关入口(M:4 / L:4)</summary>

| sev | 位置 | 一句话 |
| --- | --- | --- |
| M | `src/core/auth/service_access_resolver.py:130` | 每认证请求 2 次未缓存 SELECT(get_tenant/get_user),TTL 仅 30s |
| M | `src/core/middleware/_streaming/logging.py:62` | 每请求后台任务写 ~10–15 条 Redis 指标命令,未用现有 100 条/5s 批模式 |
| M | `src/proxy/billing_stream.py:152` | 每 SSE chunk 全 buffer decode+CRLF 替换+全文扫描,O(buffer)/chunk |
| M | `src/proxy/transparent_proxy.py:478` | 上游不可用期间**每个请求**串行 3 次 HTTP 探测,无 single-flight |
| L | `src/core/gateway/multi_dimension_rate_limiter.py:201` | Redis 故障时进程内兜底 dict 无界且不过期 |
| L | `src/proxy/config_loader.py:175` | services.name 无索引,配置缓存未命中时 seq scan |
| L | `src/proxy/transparent_proxy.py:1718` | 35 条操作类型正则每请求 2–3 次线性匹配,未预编译 |
| L | `transparent_proxy.py:764` | 每请求 8–12 条急切 f-string INFO TIMING 日志(~3KB 格式化工作) |

</details>

<details><summary>p2 网关 DB(M:2 / L:2)</summary>

| sev | 位置 | 一句话 |
| --- | --- | --- |
| M | `.../metrics/usage_recorder.py:686` | 每次 flush 每租户一次 24h PERCENTILE_CONT 扫描,60s TTL 反复触发 |
| M | `.../persistence/database.py:4268` | 语义缓存每次命中额外 UPDATE hit_count:读热路径写放大 |
| L | `apps/knowledge-service/.../persistence/database.py:1` | knowledge-service 携带 ~5000 行 DatabaseStorage 分叉副本,网关侧 perf 修复永远传不到 |
| L | `.../persistence/database.py:4001` | security_event 传 None 进 NOT NULL DEFAULT '' 列(新装 schema 报错被吞) |

</details>

<details><summary>p3 配额计量(M:3 / L:4)</summary>

| sev | 位置 | 一句话 |
| --- | --- | --- |
| M | `.../metrics/usage_recorder.py:373` | 事件总线启用时每 record 一次 awaited XADD(与 DB 批路径矛盾) |
| M | `src/proxy/billing_interceptor.py:194` | 拦截器逐条 flush(每条 3 阶段 × 3 重试),与 recorder 自带 buffer 双缓冲;flush 任务无上限 |
| M | `.../metrics/realtime_metrics.py:229` | 流路径首 chunk 与完成各 2 次 awaited Redis RTT |
| L | `.../metrics/usage_recorder.py:512` | 定价缓存 300s 过期时并发 stampede(无 single-flight) |
| L | 同文件 `:1108` | usage_hourly_aggregates 无清理(注释称保留 7 天,无代码) |
| L | `src/services/billing/aggregation_task.py:197` | 每小时配额重置 UPDATE 无索引全表扫描 |
| L | `.../persistence/database.py:5861` | 权限重算 4 次串行查询,60s TTL 进程内缓存多 worker 各自失效 |

</details>

<details><summary>p4 Agent loop(M:3 / L:2)</summary>

| sev | 位置 | 一句话 |
| --- | --- | --- |
| M | `.../agent/agent_context_lifecycle.py:237` | 预算内早退路径仍付 2 次全历史序列化(estimate + context_hash) |
| M | 同文件 `:579` | 压缩触发时全历史 ~6–8 次序列化 + 3 次 deepcopy |
| M | `.../agent/streaming_preparation.py:1158` | 每轮算完 context token 估算**随即丢弃**(默认路径用 cost_detail) |
| L | `.../agent/agent_context_lifecycle.py:1394` | 每轮从 DB 全量重载技能目录(无缓存/LIMIT) |
| L | `.../agent/agent_turn_lifecycle.py:559` | failover 禁用时仍无条件跑全上下文 token 估算 |

</details>

<details><summary>p5 记忆(M:4 / L:2)</summary>

| sev | 位置 | 一句话 |
| --- | --- | --- |
| M | `.../memory/compressor.py:352` | staged 压缩每 ~7000 字符段**串行** LLM 调用(100k-token ≈ 57+1 次串行) |
| M | `.../memory/governance_cleanup.py:401` | 每 principal 重建源 manifest 4 次(全库 JOIN + rglob + 逐文件哈希) |
| M | 同文件 `:196` | Qdrant scroll 带 `with_vector:true` 全量拉向量(100k 点 ≈ 600MB+/次),每 run 两次;每请求新建 httpx client |
| M | `.../memory/indexer.py:157` | advisory lock 持有整个嵌入过程(池上限 20,并发耗尽风险) |
| L | 同文件 `:497` | 每 chunk 重复 json.dumps 同一 metadata dict;manifest 逐 chunk 行 + 无效去重 |
| L | `.../memory/source_store.py:276` | 每轮每用户 ~12–18 次无谓 chmod+lstat(路径助手重复 _ensure_user_dirs) |

</details>

<details><summary>p6 流式出口(M:2 / L:3)</summary>

| sev | 位置 | 一句话 |
| --- | --- | --- |
| M | `.../sse_event_transport.py:56` | 每个 SSE 事件序列化两次(sort_keys 尺寸检查 + 路由构建帧),仅 /chat/stream 与 /agent-runtime 路径 |
| M | `packages/ai-gateway-core/src/ai_gateway_core/proxy/sse_heartbeat.py:75` | 无界 Queue 移除生产者背压,慢客户端时整响应驻留内存 |
| L | `.../api/routes/chat.py:44` | E2E stub 用户 dict 无驱逐(test-only) |
| L | `.../agent/agent_model_turn.py:212` | 三处 `str +=` 累积流式文本,O(n²) 拷贝 |
| L | `.../agent/streaming_tool_execution.py:380` | KB 工具结果全部物化后才开始发事件(30s+ 搜索期间客户端只见心跳) |

</details>

<details><summary>p7 工具执行(M:4 / L:3)</summary>

| sev | 位置 | 一句话 |
| --- | --- | --- |
| M | `.../core/code_executor.py:1220` | 工作区文件写/读(至 24MB)/rmtree 为事件循环上的同步 I/O |
| M | `.../files/file_processor.py:752` | 图片无大小上限,全量 base64 序列化进 Redis(20MB 图 ≈ 27MB Redis 值) |
| M | 同文件 `:1177` | 多文件上传串行处理(应 asyncio.gather + 信号量) |
| M | 同文件 `:1068` | PDF 转图有 20 页上限但无字节上限,全部页 base64 驻内存 |
| L | `.../core/code_executor.py:844` | 超时执行占死默认线程池线程(专用有界执行器) |
| L | `.../core/tool_invoker.py:189` | 结果缓存按条数(200)不按字节,K B 结果可驻留数百 MB |
| L | `.../files/file_processor.py:969` | 文档结构分析每行 ~10 个未预编译正则 |

</details>

<details><summary>p8 子代理 + MCP(M:3 / L:2)</summary>

| sev | 位置 | 一句话 |
| --- | --- | --- |
| M | `.../agent/subagent_manager.py:1183` | 每个模型流 chunk 包一层 ensure_future+asyncio.wait+取消收尾(每 chunk 2 任务创建) |
| M | 同文件 `:967` | 每个子代理各自重解析全工具目录 + fresh 策略(5 child 批 = 5×同查询) |
| M | `.../core/tool_invoker.py:721` | MCP 绑定授权每调用跑两次(invoke 与 runtime 各一次) |
| L | `.../agent/subagent_manager.py:397` | spawn_parallel 无界 fan-in Queue |
| L | `.../mcp/manager.py:248` | 多 resource_link 下载串行(应 gather) |

</details>

<details><summary>p9 知识摄入(M:4 / L:2)</summary>

| sev | 位置 | 一句话 |
| --- | --- | --- |
| M | `.../services/knowledge/embedding.py:653` | DashScope 批超时后逐条重试(5 次退避),最坏 50 串行调用再整批重试 |
| M | `.../services/knowledge/ingestion_service.py:1301` | 图片段逐行 INSERT(文本段已用 executemany) |
| M | `.../services/knowledge/vector_store.py:2013` | 每 sub-batch 2 次 get_collection + 1 次 DB 身份读;大批量降到 4 点/批 → 1000-chunk ≈ 750 额外 RTT |
| M | 同文件 `:2259` | bm25_v2 shadow 模式逐点 set_payload RPC(应每 sub-batch 一次) |
| L | `.../services/knowledge/worker.py:1537` | 扫描 PDF 每页一次 awaited 进度 UPDATE(300 页 ≈ 300 次) |
| L | `.../services/knowledge/chunking.py:1757` | tiktoken 模式全文被编码 ~3–4× |

</details>

<details><summary>p10 知识检索(M:4 / L:4)</summary>

| sev | 位置 | 一句话 |
| --- | --- | --- |
| M | `.../services/knowledge/hierarchical_retriever.py:738` | 层级路径用 embed_documents 绕过查询嵌入缓存(每请求 1 次远端嵌入调用) |
| M | `.../services/knowledge/retrieval_service.py:2116` | MMR 滚动带 with_payload=True 全量载荷(只需向量)+ Python O(n²) 余弦(2000 候选 ≈ 4G+ 浮点运算) |
| M | 同文件 `:1769` | 展示用 text-match 对全部 candidate_k(至 2000)计算,只用到 top_k |
| M | `.../services/knowledge/hierarchical_retriever.py:575` | 每个命中递归遍历整个 payload 查 multimodal 标记(每请求 ~25 次全量 walk) |
| L | `.../services/knowledge/knowledge_service.py:157` | 检索结果缓存默认 ttl=0(整体禁用),指纹机制闲置 |
| L | `.../services/knowledge/retrieval_service.py:2512` | 每请求数据集行读两次(入口 + 代数检查) |
| L | `.../services/knowledge/vector_store.py:1785` | 原生混合首次调用 2 次 count RPC(未预热) |

</details>

<details><summary>p11 共享存储/事件(M:6 / L:3)</summary>

| sev | 位置 | 一句话 |
| --- | --- | --- |
| M | `.../api/routes/images.py:1709` | 图片任务回退路径重复 find_variant SELECT |
| M | `.../runtime/memory/indexer.py:274` | 短路前先跑全量 chunk_markdown(与 C3/H8 同根) |
| M | `.../comm/idempotency.py:269` | 幂等中间件默认内存后端缓存全请求/响应体 24h(≤50MB/请求),且破坏 upload_file_streaming |
| M | `.../events/envelope.py:171` | 每个事件信封 JSON 解析两次 |
| M | `.../storage/artifact_storage.py:691` | 会话 artifact 删除 2N+1 查询 + N 串行存储往返 |
| M | `.../storage/artifact_storage.py:620` | 预签名 URL 变体回退链逐级 find_variant(最多 3 串行 SELECT) |
| L | `.../storage/image_storage.py:507` | 所有下载后端整对象 read() 进内存(已有流式 download_to_path 未用) |
| L | `.../storage/artifact_storage.py:412` | artifact SELECT * 拖 prompt/metadata 大列 |
| L | `.../storage/image_storage.py:876/391` | OSS/本地 delete_prefix 逐对象串行(OSS 应 batch_delete_objects) |

</details>

<details><summary>p12 Web 聊天(M:3 / L:2)</summary>

| sev | 位置 | 一句话 |
| --- | --- | --- |
| M | `web/src/pages/assistant/hooks/useChatSession.ts:2730` | sendMessage deps 含 messages/workingMemory → 每帧身份变化 → 输入区/活动面板每帧重渲染 |
| M | 同文件 `:1244` | 每帧无条件 JSON.parse 全部工具参数(reducer 另再 parse 至 3×) |
| M | `web/src/pages/assistant/index.tsx:715` | 每帧 smooth 滚动重启动画 + 强制布局 |
| L | `.../components/ChatMessage.tsx:342` | 流式消息 500ms 间隔时钟重渲染 |
| L | 同文件 `:1055` | 200 条消息会话恢复时一次性 mount 解析全部 Markdown,无虚拟化 |

</details>

<details><summary>p13 Web 大页面(M:5 / L:4)</summary>

| sev | 位置 | 一句话 |
| --- | --- | --- |
| M | `web/src/pages/dashboard/DashboardLayout.tsx:111` | 每 30s 刷新 tick 同时重发 17–25 个查询(全部 keyed on lastRefresh.getTime()) |
| M | `.../hooks/useDashboardEntityLabels.ts:177` | userOptions 无 useMemo,每次渲染重建 |
| M | `web/src/pages/knowledge/DatasetDetail.tsx:1582` | 段搜索无 debounce,每击键触发未索引 ILIKE seq scan |
| M | `.../detail/SegmentList.tsx:226` | 文档/段列表无虚拟化(200/100 行有界但每行重) |
| M | `web/src/components/ProviderStatusCard.tsx:561` | 固定 60s 轮询与 dashboard 刷新周期叠加 |
| L | `web/src/lib/sse.ts:176` | buffer += decode 每 chunk 重扫整 buffer O(n²) |
| L | `.../DatasetDetail.tsx:3476` | documentStats 每渲染 4 次 filter(应单次 reduce + useMemo) |
| L | 同文件 `:374` | qaMessages/qaHistory 无界增长 |
| L | 同文件 `:745` | 重建索引后 1s 延迟重复 invalidate |

</details>

<details><summary>p14 Web 打包(M:4 / L:2)</summary>

| sev | 位置 | 一句话 |
| --- | --- | --- |
| M | `web/src/pages/Login.tsx:4` | 登录页(未认证入口)拖入 framer-motion 39KB gzip |
| M | `web/src/pages/dashboard/DashboardLayout.tsx:19` | dashboard chunk 497KB(496.27kB min,贴线 500KB 警告),recharts 全量 |
| M | `web/src/router.tsx:41` | /agents 桶文件加载全部 4 个 agent 页(125KB) |
| M | 同文件 `:36` | /knowledge 列表路由加载 DatasetDetail(212KB) |
| L | `web/src/components/StreamOutput.tsx:5` | katex/react-markdown(86KB gzip)每个聊天面都加载,无数学也加载 |
| L | `web/src/main.tsx:6` | 入口 CSS 278KB(36KB gzip),tailwind 全量面在关键路径 |

</details>

<details><summary>p15 运行时配置(M:2 / L:3)</summary>

| sev | 位置 | 一句话 |
| --- | --- | --- |
| M | `docker-compose.yml:331` | 全服务无 docker 日志轮转,每请求 ~3 行日志无界增长 |
| M | `scripts/new/redis-entrypoint.sh:42` | 192mb maxmemory + allkeys-lru 可驱逐会话键;AOF fork 在 256m cap 内余量 <64mb |
| L | `docker-compose.yml:234` | GATEWAY_KNOWLEDGE__WORKER_CONCURRENCY=2 为死配置(网关侧 worker 已禁用) |
| L | `.../comm/idempotency.py:22` | 幂等内存存储 24h TTL 响应体驻留 |
| L | `src/core/middleware/rate_limit_http.py:120` | 每请求 4 命令 Redis 管道 + 指标任务第二条管道 |
| L | `src/main.py:417` | /health/ready 每次探测新建 httpx.AsyncClient |

</details>

## 4. Quick Wins(最小改动、最高收益,Top 15)

1. **ChatMessage 加 React.memo**(`web/src/pages/assistant/components/ChatMessage.tsx:332`)——一行消除每帧 200 条消息重渲染风暴。
2. **indexer 短路提前**(`.../runtime/memory/indexer.py:274`)——content-hash 比较移到 chunk_markdown 之前,未变化源每轮成本趋零。
3. **checkpoint digest 复用**(`execution_state.py:232/287`)——每 save 算一次 digest 两处共用,砍半每轮最大 CPU 项。
4. **KB 上下文出站剥离**(`streaming_tool_execution.py:1172`)——tool_call_completed 的 contexts 用 compact_context_payload 压缩,消除最大 SSE 帧与 64KB spill。
5. **usage_recorder 多行 INSERT**(`usage_recorder.py:862/865`)——flush 从 ~100 RTT 降到 1 条语句。
6. **`created_at::date` 改半开区间**(5 处:1224/1359/1520/1572/1813)——现有索引立即生效,Dashboard 告别租户全量扫描。
7. **去掉 COALESCE 包裹**(`usage_recorder.py:1056-1059/1139-1142`)——唯一索引恢复命中,每键省 1 RTT。
8. **admission Lua 脚本**(`src/core/gateway/admission.py:516`)——每共享预算 3 RTT → 1 RTT。
9. **i18n 按语言打包**(`web/src/i18n/index.ts:4`)——每用户首载省 ~52KB gzip,api chunk 缩小 ~80%。
10. **移除 antd manualChunks**(`web/vite.config.ts:52`)——修复已知 >500kB chunk 警告的根因。
11. **thinking_delta 走 RAF 批处理**(`useChatSession.ts:1404`)——复用已有文本通道,消灭每 token 全量 setState。
12. **MCP DNS pin 缓存**(`.../core/mcp/client.py:571`)——~5 行,事件循环不再每 RPC 阻塞 DNS。
13. **MCP client 复用**(`.../core/mcp/runtime.py:728`)——每调用 3 RTT + TLS 握手 → 1 RTT。
14. **代码沙箱 log_config 上限**(`code_executor.py:769`)——一个 dict 项,封死 daemon 侧日志无界增长。
15. **容器内日志轮转 + Redis 内存上调**(`docker-compose.yml:331`、`redis-entrypoint.sh:42`)——纯配置改动,消除无界磁盘增长与 session 驱逐/AOF fork 风险。

## 5. 覆盖缺口(批评代理,按风险排序)

| # | 缺口 | 位置 |
| --- | --- | --- |
| G1 | **local-node DirectoryWatcher 每 250ms 全量重读 + SHA-256 每个授权目录的每个文件**(最大 8MB/文件,无 mtime/size 快速路径)——设备侧持续 I/O+CPU 燃烧 | `apps/local-node/src/local_node/watcher.py:56-70` |
| G2 | 网关可观测性栈未覆盖:~4 条 INFO 日志/请求、OTel 100% 采样、每请求 asyncio 任务 + 独立 Redis 管道(6 INCR/EXPIRE + ZADD + ZREMRANGEBYRANK) | `src/main.py:477`;`src/services/metrics/metrics_recorder.py:114-166` |
| G3 | **mcp-docgen-server 全部 CPU 密集渲染(pptx/docx/pdf/xlsx + LLM 规划 + 视觉批评)直接在 async 事件循环上跑**,零 run_in_executor;作为 in-process MCP 工具接入 assistant-service 时,一次大 deck 渲染冻结所有并发 agent 会话 | `packages/mcp-docgen-server/src/docgen/pipeline.py:86-128` |
| G4 | agent_traces 6+ btree + 3 pg_trgm GIN 索引:每轮 trace 摄入付 GIN 写放大,且未见 trgm 索引被查询使用的证据 | `database/migrations/060-062`;`src/api/v1/eval.py:324` |
| G5 | 网关自有流端点 `src/api/v1/stream.py` 每 chunk Pydantic schema + model_dump_json,与 assistant 投影器重复;三个流实现(stream.py / responses / eval)无共享契约测试(首字节/心跳/背压/chunk 预算) | `src/api/v1/stream.py:56-101` |
| G6 | 迁移运行器无 advisory lock;016/031 编号碰撞 ×2;051-054/063+ 空档 | `database/migrate_per_service.py:101-104` |
| G7 | 网关其余服务模块未覆盖:模型 failover(3 次重试链)、注册表健康轮询、单并发任务队列、DB session manager | `src/services/llm/`、`src/services/registry/`、`src/services/task/` |

## 6. 交叉维度建议(后续独立工作项)

1. **可观测性开销预算**:实测指标/追踪/日志占 Redis 操作、日志行、事件循环时间的百分比;设采样率;去重两个日志中间件。
2. **SSE 契约一致性**:对 gateway stream.py / assistant responses+chat / eval 三个流实现做单次负载测试(首字节延迟、心跳、背压、chunk 序列化预算)。
3. **local-node 设备负载画像**:watcher 先 mtime/size 快速路径,自适应轮询,>max_read_bytes 排除;10k 文件目录基准。
4. **Trace 摄入写放大与保留**:实测 3 个 trgm GIN 的每插入成本;验证 trgm 索引是否真被查询使用;确认 trace_retention_scheduler 生效。
5. **Redis 键空间/TTL 审计**:验证指标/服务/任务键的扇出上限与 maxmemory 策略;检查 assistant image_task_store 与 run-state 键。
6. **后台 worker 审计**:eval outbox(2s 轮询)、trace 保留、注册表健康监视、任务 worker——确保轮询间隔与扫描规模不随规模漂移。
7. **迁移纪律**:加 advisory lock、解决编号碰撞、CI 门禁检查新表索引。

## 7. 性能路线建议(下一步)

性能是下一阶段核心,建议按"度量先行 + 快速收益 + 热路径 + 架构"推进:

**P0 — 建立度量基线(先于一切修复)**
- 补一个可复现的负载工具:gateway 代理请求(含流式)、KB 检索、agent 多工具轮、前端首屏(Playwright performance trace)。
- 对 C3(H8)、C1、C2、C5 各取"修复前"基线,修完同场景复测——本报告数字全部是结构估算,不能直接当验收依据。

**P1 — Quick Wins(§4,每个改动 <1 天)**
预计覆盖 15 项中最痛的:每轮记忆短路、checkpoint digest、ChatMessage memo、KB 上下文剥离、usage_recorder 批写、MCP client 复用。风险低、可单独验证。

**P2 — 结构热路径(1-3 天/项)**
- H8 增量索引(C3 + runtime_adapter 新鲜度门):把每轮全量重嵌入改成追加水位增量,预计是单点最大收益。
- trace 事件批量 flush(C2):每轮 DB 往返从数百降到个位数。
- 网关 Redis 往返整合(admission Lua + 单一限流层 + 指标批写):每请求 ~20-30 RTT → 个位数。
- Web 打包与渲染(antd codeSplitting、i18n 按语言、块级 memo、列表虚拟化)。
- 知识摄入批量化(多元素嵌入调用、层级重摄入增量、Qdrant 批上限提升)。

**P3 — 架构性(需要设计评审)**
- uvicorn workers 配置面 + INTERNAL_*_BACKEND 一致性门禁(多核能力)。
- 观察栈采样与去重(G2)。
- docgen 渲染移出事件循环(G3)。
- agent_traces 索引瘦身(G4)。

## 8. 与已知项交叉引用

- **H8(每轮记忆全量重索引)**：本报告确认并精确定位到 `indexer.py:479`(C3)、`runtime_adapter.py:321`、`indexer.py:274`,且前端 2s 轮询是其 UI 侧镜像(`useKnowledge.ts:33`)——修复 H8 应三处联动(增量索引 + 请求路径新鲜度门 + 推送替代轮询)。
- **>500kB web chunk 警告**：确认为 `ui` chunk 1,394.91 kB(C6)+ dashboard 496.27 kB(p14),根因是 manualChunks 与桶文件,不是 nginx(服务侧 gzip/缓存已正确)。
- 与 2026-08-16 上午的 assistant 模块报告互补:该报告覆盖正确性/契约闭环,本报告只覆盖性能;两者结论不冲突。

---

*生成方式：15 个只读子代理并行静态分析 + 1 覆盖批评代理;报告为人工合成。所有性能数字为结构估算(已标注依据),未实测;修复前请先跑 P0 基线。*

---

## 9. 复核结论(2026-08-16 · 8 个验证代理 · 70 条抽样,已对照工作树真实代码逐条裁决)

### 9.1 总览

| 分区 | 条目 | CONFIRMED | CORRECTED | REJECTED |
| --- | --- | --- | --- | --- |
| v1 循环/流式 | 6 | 5 | 1 | 0 |
| v2 H8 记忆 | 5 | 5 | 0 | 0 |
| v3 知识服务 | 7 | 7 | 0 | 0 |
| v4 网关/用量/会话 | 9 | 8 | 1 | 0 |
| v5 工具/MCP | 6 | 6 | 0 | 0 |
| v6 存储/Web | 7 | 5 | 2 | 0 |
| v7 打包/基建/缺口 | 10 | 6 | 4 | 0 |
| v8 中低危抽样 | 20 | 16 | 2 | 1 |
| **合计** | **70** | **58 (83%)** | **10 (14%)** | **1 (1.4%)** |

行号引用命中率 ~95%(偏差均 ≤20 行且已给出修正行)。**Codex 实施必须以本节为准:REJECTED 不要做,CORRECTED 按修正做。**

### 9.2 REJECTED(1 条,不要实施)

- **M12"检索结果缓存 ttl=0 禁用"** —— 误报。生产唯一实例化路径 `main.py:301` 经 `_SettingsCompat`(main.py:196-232)把 `retrieval_cache_ttl_seconds` 映射为 300s,且 `retrieval_service.py:2947-2954` 实际使用该缓存。"缓存整体禁用/指纹机制闲置"不成立;按原文"启用缓存"实施无效果。

### 9.3 CORRECTED(10 条,按修正实施)

| 条目 | 修正 |
| --- | --- |
| H2.6a | 热路径实例在 `builtin_tools.py:495`(成功分支);原引用的 :444 是 KB_SEARCH_FAILED 错误分支的同构代码 |
| H2.2c | "无保留清理"不成立:`aggregation_task.py:107 cleanup_old_records` + 01:00 调度已实现 30 天保留。**只实施半开区间改写;不要重复造保留任务** |
| H2.12b | 锚点 1404→**1417**(THINKING_DELTA 分支;1404 是 THINKING_START) |
| H2.13b | 段列表有效 LIMIT 为 **500** 非 100(`document_service.py:694` 恒传 500) |
| G3 | ai-docgen 经 **stdio 子进程**接入,不冻结 assistant-service 会话;修复应落在 docgen server 内部(to_thread/进程池),不要动 assistant-service MCP 客户端 |
| G4 | input_preview/output_preview 两个 trgm GIN 被 `agent_trace_repository.py:441-442` 的 ILIKE **实际使用**;只有复合表达式 GIN(062:17-26)是死索引。保留两个单列 GIN,只评估删复合索引 |
| G5 | `eval.py` 无流式端点;对照系应为 stream.py ↔ `agent_runtime.py:1112` ↔ assistant responses.py(§6.2 的"eval"需改) |
| G6 | 063-084 **全部存在**;空档仅 051-054(外加 040);另有第三处编号碰撞 `per_service/assistant/002` ×2 |
| M2 | 锚点 152→**196**(`_parse_events` 才是全 buffer 扫描;152-156 只处理当次 chunk) |
| M13 | 删除"破坏 upload_file_streaming"子论断(幂等中间件只挂 assistant-service,该函数只在网关 src 使用);内存驻留主体成立 |

### 9.4 复核新挖出的正确性 bug(已交叉并入全局审查报告)

- **M4 升级**:迁移 030 后 `security_event_daily_aggregates.user_id/service_id` 为 NOT NULL DEFAULT '',而 `database.py:4001-4002` 仍传 None → INSERT 必失败,网关 `src/api/v1/proxy.py:91-104` try/except pass 吞掉 → **所有安全事件(认证失败/限流/配额超限)在已迁移库上静默丢失**。KB 分叉(database.py:5011)同缺陷。修复:传 `user_id or ''`,并解除吞异常。

### 9.5 Codex 实施注意事项(交叉验证代理输出的硬约束)

1. **H8 四条目必须联动**(C1/H2.5a/H2.5b/C3 同根):mtime/size 新鲜度门 + 短路提前 + 增量索引一起设计;H2.5c 改 O_APPEND 会丢失 `_atomic_write` 的 O_NOFOLLOW/symlink 防护,须显式保留。
2. **C2 批量 flush** 必须尊重 `trace_writer.resume_sequence`(:745-775)的 drain 屏障,否则断点续跑拿到陈旧 sequence_no。
3. **报告内重复条目先去重**:indexer.py:274 出现 3 处、usage_recorder.py:373 出现 2 处、logging.py:62 出现 2 处——同一改动不要做多遍。
4. **C4 批量化前置验证**:`_call_api` 硬编码 `expected_count=1`(embedding.py:1759),且 `embed_image_with_context` 传 2 元素却依赖"多输入返回 1 向量"语义——批量化前必须实测 API 真实输出形状。
5. **M11(get_collection 缓存)受约束**:`vector_store.py:2010-2012` 注释表明每 sub-batch 刷新是刻意的 bm25_v2 副本感知设计;缓存须带写路径失效或按集合名+版本键。
6. **M19/M20 改 Redis** 需同时改 `REDIS_MAXMEMORY`(compose:65)与 `REDIS_MEMORY_LIMIT`(compose:82)两个变量。
7. **H2.15a 多 worker 化** 与 p3 的 RPM 全局锁、进程内限流兜底必须**同批 Redis 化**,否则 workers>1 时配额语义分裂。
8. **H2.10b 删除重复过滤**时保留路由层 fail-closed 权威校验(ValidationFailedError 语义),只去重复 DB 往返。
9. 依赖方向:全部修复落在各自服务内,无 web→src→apps→packages 违规;除 G4 索引瘦身需新迁移 085 外无 schema 变更。

### 9.6 总体评价

抽样覆盖 70/149(47%),含全部 6 条 Critical、39 条 High 中的 39 条。**问题本体无一虚假**:10 条 CORRECTED 全是细节级修正(行号/数字/子论断),1 条 REJECTED 是配置映射误判;反过来复核还新挖出 1 个安全事件静默丢失的正确性 bug。报告可作为 Codex 实施依据,前提是携带本节修正与注意事项。
