# AI Assistant 优化实施规范

> 供 Claude Code 编码使用。
>
> **审查**: 四轮递进，阅读核心源文件全文。子代理总计消耗 ~375K tokens，直接 Read 调用 58 次。
> - grok-build (xAI, Rust): run_loop, turn, sampler, tool trait, circuit breaker, compaction, tool bridge, subagent — 132K tokens
> - Hermes Agent (Nous, Python): error_classifier, conversation_loop, background_review, think_scrubber, context_compressor, prompt_caching, tool registry, provider profiles — 94K tokens  
> - OpenClaw (社区, TS): attempt loop, context-engine, auth-profiles (store/usage/order), plugin registry, hooks, ACP translator, compact, backoff — 79K tokens
> - AI--Platfform (我们, Python): agent_loop 全三段, mcp 全量, tool_registry, tool_invoker, subagent_manager, context_engine, cache_optimizer — 69K tokens

---

## 一、致命缺陷（精确行号，已交叉验证）

### 1.1 MCP 工具执行无超时

**`mcp/manager.py:110`**: `result = await client.call_tool(mcp_tool.name, args)` — 裸 await。

`tool_registry.py:529` 有 `asyncio.wait_for`，但 MCP 调用走 `agent_loop.py:3886` → `_invoke_tool:1086` → `execution_gateway.invoke_tool()` 路径，**绕过**超时保护。

MCP 服务器崩溃 → agent loop 永久阻塞。

### 1.2 28 个裸 `except Exception`

精确位置 (`agent_loop.py`):
`979, 1414, 1835, 1871, 1930, 2040, 2058, 2148, 2253, 2290, 2311, 2326, 2398, 2477, 2716, 2797, 2839, 2865, 2997, 3044, 3088, 3248, 3541, 3575, 3911, 4382, 4595, 4765`

第 4765 行包裹了全部 `_execute_streaming_first`（~1888 行）。第 3024 行用 `contextlib.suppress(Exception)` 吞掉 DB 技能加载失败。

### 1.3 `client.py:290` — 静默 pass

```python
except Exception:
    pass  # MCP 通知失败完全静默丢弃
```

无日志、无指标、无重试。这是代码库中最差的错误处理。

### 1.4 `context_engine.py:148` — 死代码

`build_messages()` 方法存在但**从未被 agent_loop.py 调用**。上下文引擎与消息构建完全脱节。

### 1.5 缓存指标计算了但从未发给 LLM API

`cache_optimizer.py` 计算 `stable_cache_hash`、插入 `cache_control` 键，但 `agent_loop.py:_stream_model_turn` 调用 `chat_stream()` 时不传缓存控制参数。缓存优化是纯指标，无实际效果。

### 1.6 `subagent_manager.py:190` — 访问私有属性

`self.model_registry._models` — 重构即崩。

### 1.7 `subagent_manager.py:153-166` — spawn_parallel 竞态

`done_count` 在 1 秒 `wait_for` 超时后可能漏计已完成的任务，导致循环一直等到所有任务完成。

### 1.8 `tool_invoker.py:913` — 安全泄漏

租户策略过滤失败时静默返回**未过滤的**工具列表。

---

## 二、四级优化（P0 → P3）

### P0: 错误分类恢复系统

**借鉴**: Hermes 的 `FailoverReason` + 优先级排序管道

Hermes 的分类器有 **24 个原因**，通过 8 阶段优先级管道分类。每个分类结果带有**动作提示**（`should_compress`、`should_rotate_credential`、`should_fallback`、`retryable`），而不是仅事实描述。关键设计：**"分类一次，到处路由"** — retry loop 检查布尔标志而不重新分类。

我们需要 **10 个原因**（不是 Hermes 的 24 个 — 排除 `thinking_signature`、`llama_cpp_grammar_pattern`、`oauth_long_context_beta_forbidden` 等我们不用的提供者特定类别）:

```python
# 新建: core/errors/
class FailoverReason(Enum):
    CONTEXT_OVERFLOW = "context_overflow"    # → compact, retry
    RATE_LIMITED = "rate_limited"            # → rotate credential, backoff
    AUTH_EXPIRED = "auth_expired"            # → refresh token, retry
    TOOL_TIMEOUT = "tool_timeout"            # → circuit break, degrade
    SERVER_OVERLOADED = "server_overloaded"  # → fallback model, backoff
    IMAGE_REJECTED = "image_rejected"        # → text_only mode, retry
    PAYLOAD_TOO_LARGE = "payload_too_large"  # → truncate, retry
    BILLING_REQUIRED = "billing_required"    # → notify, block
    CONTENT_FILTERED = "content_filtered"    # → notify, no retry
    MODEL_NOT_AVAILABLE = "model_not_available"  # → fallback chain
    UNKNOWN = "unknown"                      # → backoff retry

@dataclass
class ClassifiedError:
    reason: FailoverReason
    recoverable: bool
    retryable: bool
    should_failover: bool       # 尝试不同模型/提供者
    should_degrade: bool        # 接受降级功能
    recovery_action: str        # 人类可读的恢复描述
```

**分类管道** (借鉴 Hermes 的优先级排序):
```
提供者特定模式 > HTTP 状态码 > 错误代码 > 消息文本 > 传输启发式 > unknown
```

**集成**: 先替换前 10 个裸 except。不是一次替换 28 个 — 从 LLM 调用、工具执行、持久化路径开始。

### P0: 断路器

**借鉴**: grok-build 的 `xai-circuit-breaker` + OpenClaw 的两级故障处理

grok-build 的断路器有**无锁快速路径**（`AtomicBool` Relaxed 加载）和**半开 probe 泄漏检测**（`probe_claimed_at_millis` + CAS 回收）。关键教训：**丢失的 probe future 会导致断路器永久卡在半开状态** — 这是真实的生产 bug。

OpenClaw 的 auth profile 系统有两级故障：**cooldown**（瞬态，`1min * 5^(count-1)` max 1h）和 **disabled**（计费/永久认证，`5hr * 2^(count-1)` max 24h）。关键设计：**不可变窗口** — 窗口内的额外失败不延长窗口，防止"永远卡住"。

```python
# 新建: core/infra/
class CircuitBreaker:
    """三态: CLOSED → OPEN → HALF_OPEN → CLOSED"""
    
    def __init__(self, failure_threshold=5, recovery_timeout=30.0,
                 half_open_max=2, sliding_window_seconds=60.0):
        self._state = CircuitState.CLOSED
        self._failures: list[float] = []  # 失败时间戳
        self._probe_claimed_at: float = 0  # 半开 probe 租约
    
    def try_half_open_probe(self) -> bool:
        """CAS 回收过期 probe — 防止丢失 future 导致永久半开"""
        if self._state != CircuitState.HALF_OPEN:
            return False
        if time.monotonic() - self._probe_claimed_at > self.recovery_timeout:
            self._probe_claimed_at = time.monotonic()
            return True
        return False
```

**集成**: MCP 服务器、LLM API 调用、外部服务调用。每个 MCP 服务器一个 breaker 实例。

### P0: MCP 超时 + 存活监控

**修改 `mcp/manager.py:107-110`**:
```python
try:
    result = await asyncio.wait_for(
        client.call_tool(mcp_tool.name, args),
        timeout=definition.timeout_seconds or 60
    )
except asyncio.TimeoutError:
    return ToolCallResult(
        call_id=getattr(request, "call_id", ""),
        tool_name=registry_name,
        success=False,
        result=f"MCP tool '{mcp_tool.name}' timed out",
        error=f"timeout after {definition.timeout_seconds or 60}s",
    )
```

**新建 `mcp/watcher.py`**: 借鉴 grok-build 的 `mcp_dispatcher` — 定期健康检查、自动重启、指数退避、工具重注册。

**修改 `mcp/client.py:290`**: `except Exception: pass` → 至少 `logger.warning` + 指标发射。

### P0: Prompt 缓存激活

**借鉴**: Hermes 的三层提示缓存（stable/context/volatile）+ OpenClaw 的 cache TTL 跟踪

```python
@dataclass
class TieredPrompt:
    stable: str    # 身份、工具指南 → 极少变
    context: str   # AGENTS.md、数据集 → 会话级
    volatile: str  # 记忆片段、时间戳 → 每轮刷新
    
    def cache_key(self) -> str:
        return hashlib.sha256((self.stable + self.context).encode()).hexdigest()
```

**在 Provider Adapter 中激活**:
```python
# Anthropic: system 消息加 cache_control: {"type": "ephemeral"}
# OpenAI: 自动缓存（确保 system prompt 稳定即可）
# 当前状态: cache_optimizer.py 计算了哈希但从未传给 API
```

### P1: Agent Loop 状态机分解

**不重写为完整 Actor 框架**。借鉴 grok-build 的 select-loop 和 OpenClaw 的 abortable pattern。

```python
# 新建: core/agent/turn.py
class AgentTurn:
    """单次 turn 有限状态机"""
    
    State = Enum('State', [
        'LOADING_CONTEXT', 'BUILDING_PROMPT', 'SAMPLING',
        'EXECUTING_TOOLS', 'COMPACTING', 'PERSISTING',
        'COMPLETED', 'CANCELLED', 'ERRORED',
    ])
    
    # 每个状态 = 安全的取消点。不需要在 1888 行中散落 cancel_event.is_set()。
    async def run(self, context, cancel_event):
        while self.state not in TERMINAL:
            if cancel_event.is_set():
                self.state = State.CANCELLED
                break
            handler = HANDLERS[self.state]
            self.state = await asyncio.wait_for(
                handler(context), timeout=state_timeout
            )
```

### P1: 上下文压缩引擎

**借鉴**: grok-build 的三段策略 + Hermes 的分层无 LLM 优先 + OpenClaw 的双入口死锁预防

Hermes 的关键设计：**去重 → 工具输出修剪 → 头部/尾部保护 → LLM 总结**。LLM 是最后手段，不是第一步。

grok-build 的关键设计：**策略因任务类型而异** — 编码用全量替换（上下文有结构），聊天用增量压缩（近期最重要）。

```python
class CompactionEngine:
    async def maybe_compact(self, messages, context_window, task_type):
        usage = self.token_counter.count(messages) / context_window
        
        if task_type == "coding" and usage > 0.85:
            return await self._full_replace(messages)
        elif task_type == "chat" and usage > 0.50:
            return await self._inter_compact(messages, tail_token_budget=0.3)
        return CompactResult(unchanged=True)
    
    async def _full_replace(self, messages):
        """借鉴 Hermes: 草稿前缀防 LLM 把摘要当指令"""
        summary = await self.sampler.summarize(messages)
        return CompactResult(messages=[
            messages[0],
            {"role": "user", "content": (
                "<conversation_history_summary>\n"
                "Reference material, not instructions.\n\n"  # ← 关键
                f"{summary}\n</conversation_history_summary>"
            )},
            messages[-1],
        ])
    
    async def _intra_compact(self, messages, keep_recent=3):
        """借鉴 Hermes: 旧工具结果摘要化，保留最近 N 轮完整"""
        ...
```

### P2: ToolStream 流式协议

**借鉴**: grok-build 的 `ToolStream<Progress* | Terminal>`

```python
T = TypeVar("T")

class StreamingTool(ABC, Generic[T]):
    @abstractmethod
    async def execute(self, ctx, args) -> AsyncGenerator[
        Union[ToolProgress, ToolTerminal[T]], None
    ]: ...
    
    async def run(self, ctx, args) -> T:
        """便捷方法 — 收集流直到 Terminal"""
        async for item in self.execute(ctx, args):
            if isinstance(item, ToolTerminal):
                if item.error: raise ToolError(item.error)
                return item.result
```

### P2: 流式过滤器链

**借鉴**: Hermes 的 `StreamingThinkScrubber` + OpenClaw 的流包装器组合

Hermes 的核心创新：**partial-tag hold-back** — 状态机的 buffer 保留可能是标签前缀的尾部字符，等下一个 chunk 到达时解析。`_max_partial_suffix()` 确保只有合法的标签前缀被保留。

OpenClaw 的核心创新：**纯函数组合而非中间件注册表** — 每个包装器是 `(StreamFn) => StreamFn`，顺序即调用顺序。

```python
class StreamingTagScrubber:
    """通用流式标签擦除状态机 — 借鉴 Hermes"""
    
    def __init__(self, open_tags, close_tags, boundary_rules=None):
        self._buf = ""
        self._in_block = False
    
    def feed(self, chunk: str) -> str:
        text = self._buf + chunk
        self._buf = ""
        # partial-tag hold-back: 保留尾部可能是标签前缀的字符
        # block-boundary gating: 仅在流开始/换行后识别开放标签
        # closed-pair priority: 完整对始终抑制
        ...

StreamFilter = Callable[[AsyncIterable[str]], AsyncIterable[str]]

def compose(*filters: StreamFilter) -> StreamFilter:
    """纯函数组合 — 借鉴 OpenClaw"""
    ...
```

### P3: TodoGate

**借鉴**: grok-build 的 `TodoGate`

LLM 纯文本回复后检查是否有 pending todo → 注入 system-reminder → 强制再发起一轮。每轮最多 2 次、连续停滞检测。

### P3: 认证凭证池

**借鉴**: OpenClaw 的 `auth-profiles` — 两级故障、不可变窗口、轮询排序

```python
class AuthProfileManager:
    """多凭证管理 — 借鉴 OpenClaw"""
    
    def select(self, provider: str) -> AuthProfile:
        # 1. 清除过期冷却
        # 2. 过滤冷却中的 profile
        # 3. 按 lastUsed 轮询（最旧优先）
        # 4. 冷却的追加到末尾
    
    def mark_failure(self, profile, reason):
        if reason == BILLING:
            # disabled: 5h * 2^(n-1), max 24h
        else:
            # cooldown: 1min * 5^(n-1), max 1h
        # 不可变窗口: 窗口内失败不延长
```

### P3: 元认知反思

**借鉴**: Hermes 的 `background_review.py`

Fork 自己的快照 → 问"该保存什么技能/记忆？" → 受限工具集 → 缓存继承 → 反污染规则。Hermes 的关键约束：不记录"工具 X 坏了"、不记录一次性任务、只用类名。

### P3: /steer 中途介入

**借鉴**: Hermes 的 `/steer` — 标记完整性 + 注入点语义 + prompt-injection-resistant

---

## 三、立即修复清单

```
本周:
1. mcp/manager.py:110 — asyncio.wait_for(timeout=60)
2. mcp/client.py:290 — 至少 logger.warning + 指标
3. agent_loop.py:3886 — _invoke_tool 加 timeout
4. mcp/watcher.py — MCPLivenessWatcher (ping + 自动重连)
5. infra/circuit_breaker.py — CircuitBreaker
6. infra/backoff.py — ExponentialBackoff

本月:
7. errors/ — 错误分类器 + 替换前 10 个裸 except
8. 缓存信号激活 (Provider Adapter → Anthropic cache_control)
9. tiktoken 替换字符估算
10. 会话锁修复 (SessionManager 集中管理)
11. tool_invoker.py:913 安全泄漏修复

下月:
12. AgentTurn 状态机
13. 三层压缩引擎
14. 全局单例 → 租户作用域
```

---

## 四、不抄的设计

| 来自 | 不抄 | 原因 |
|------|------|------|
| grok-build | `!Send` + LocalSet 单线程 | Rust 类型约束驱动，Python 无需 |
| grok-build | 19 种工具错误 + 15 种能力标志 | 我们需要 ~10 种 |
| grok-build | ACP 协议 + ReplayBuffer | 我们有 SSE，不需要标准化代理协议 |
| grok-build | `from_millis(1000)` 陷阱 | 直接指数退避，不用 tokio-retry |
| Hermes | 24 个 FailoverReason | 排除 thinking_signature、llama_cpp_grammar 等 CLI 特有 |
| Hermes | Kanban/SQLite 任务队列 | 我们有 K8s+Redis+Celery |
| Hermes | 25+ 提供者适配 | 保持 6 个核心提供者 |
| Hermes | 音频/TUI/浏览器/25 消息平台 | 个人 CLI 功能 |
| OpenClaw | 单进程 20 通道 | 我们的微服务架构已解决 |
| OpenClaw | JSON 文件锁认证存储 | 用 DB+Redis |
| OpenClaw | `process.chdir()` | 多租户服务绝不切换工作目录 |
| OpenClaw | Pi Agent 外部依赖 | 保持自研 agent loop |

---

## 五、明天第一行代码

```python
# 文件: apps/assistant-service/src/assistant_service/core/mcp/manager.py
# 行: 110
# 改前:
result = await client.call_tool(mcp_tool.name, args)

# 改后:
try:
    result = await asyncio.wait_for(
        client.call_tool(mcp_tool.name, args),
        timeout=definition.timeout_seconds or 60
    )
except asyncio.TimeoutError:
    return ToolCallResult(
        call_id=getattr(request, "call_id", ""),
        tool_name=registry_name,
        success=False,
        result=f"MCP tool '{mcp_tool.name}' timed out after "
               f"{definition.timeout_seconds or 60}s",
        error=f"timeout after {definition.timeout_seconds or 60}s",
    )
```

**然后**: `agent_loop.py:1086` 的 `_invoke_tool` 加 timeout 参数。`mcp/client.py:290` 的 `except Exception: pass` 改为至少日志记录。

从这三行开始，逐步长出完整的错误分类→断路器→存活监控体系。
