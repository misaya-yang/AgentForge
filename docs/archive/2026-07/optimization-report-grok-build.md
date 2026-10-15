# AI Assistant 优化设计报告

## —— 吸收 grok-build 前沿技术，打造更完善的通用型 AI 助手

**日期**: 2026-07-16  
**分析对象**: grok-build (xAI) v.s. AI--Platfform (我们)  
**目标**: 吸收 grok-build 核心 agent runtime 的前沿实现，优化我们的 AI assistant

---

## 一、总体评估

### 1.1 两项目定位对比

| 维度 | grok-build (xAI) | AI--Platfform (我们) |
|------|------------------|---------------------|
| **语言** | Rust (80+ crates) | Python (FastAPI + asyncio) |
| **架构** | 单进程 Actor 模型 | 微服务网关 + 多服务 |
| **部署形态** | TUI / Headless / ACP | Web API + Docker Compose |
| **定位** | 终端编码助手 | 通用型 AI 助手平台 |
| **核心优势** | 极致可靠性、流式工具、压缩引擎 | 多模型、生态强（Skills/MCP）、微服务可扩展 |

### 1.2 我们已有的强项 (保持)

- ✅ **多模型支持**: OpenAI / Anthropic / DeepSeek / Google / DashScope 统一注册表
- ✅ **Skills 生态**: SkillRegistry + SkillToolBridge + 数据库持久化
- ✅ **MCP 集成**: MCPManager 多服务器编排 + 工具前缀命名空间
- ✅ **微服务架构**: Gateway → Assistant Service 分离，独立扩缩
- ✅ **多层次记忆**: 会话记忆 / 用户记忆 / 运行时记忆 / 工作记忆
- ✅ **知识库服务**: 独立的 RAG 微服务 (Qdrant + embedding)

---

## 二、十大优化方向

以下优化按**影响力 × 实现难度**排序，建议分批落地。

---

### 🔴 P0 — 核心架构优化 (高影响力，中难度)

---

### 优化 1: Agent Loop 升级为 Actor 模式

**现状问题**:  
`AgentLoop._execute_streaming_first()` 是一个 4871 行的大函数，执行流是线性阻塞的。当需要并发处理取消信号、模型切换、会话事件时，只能依赖 asyncio 的协程取消（`CancelledError`），缺乏结构化的并发事件处理。

**grok-build 做法**:  
Session Actor 使用 `tokio::select!` 同时等待 8 个事件源：
```rust
tokio::select! {
    _ = idle_flush_sleep => { /* 空闲时持久化记忆 */ }
    _ = dream_check_sleep => { /* 后台记忆整理 */ }
    msg = model_switch_rx => { /* 模型热切换 */ }
    event = chat_state_event_rx => { /* 会话重置 */ }
    event = event_rx => { /* 会话级事件 */ }
    result = completion_rx => { /* 推理完成 */ }
    cmd = cmd_rx => { /* 用户命令 */ }
}
```

**建议方案**:  
将 AgentLoop 重构为 `AgentActor`，用 `asyncio.wait()` + `asyncio.Queue` 组合实现多事件源并发等待：

```python
class AgentActor:
    """Actor 模式 agent 循环 —— 同时监听多个事件源"""
    
    def __init__(self):
        self.cmd_queue: asyncio.Queue[AgentCommand] = asyncio.Queue()
        self.event_queue: asyncio.Queue[AgentEvent] = asyncio.Queue()
        self.cancel_event = asyncio.Event()
        self.model_switch_event = asyncio.Event()
        self._turn_task: Optional[asyncio.Task] = None
    
    async def run(self):
        """主循环 - 替代当前的 execute()"""
        while True:
            # 并发等待多个事件源
            done, pending = await asyncio.wait(
                [
                    asyncio.create_task(self.cmd_queue.get()),
                    asyncio.create_task(self.event_queue.get()),
                    asyncio.create_task(self._idle_timer()),
                    asyncio.create_task(self._dream_timer()),
                ],
                return_when=asyncio.FIRST_COMPLETED,
            )
            for task in pending:
                task.cancel()
            for task in done:
                await self._handle(task.result())
    
    async def _handle(self, item):
        if isinstance(item, PromptCommand):
            self._turn_task = asyncio.create_task(self._execute_turn(item))
        elif isinstance(item, CancelCommand):
            if self._turn_task:
                self._turn_task.cancel()
        elif isinstance(item, ModelSwitchEvent):
            self._current_model = item.new_model
        # ...
```

**收益**:  
- 优雅处理并发取消（不再依赖粗暴的 `CancelledError`）
- 支持模型热切换（无需重建会话）
- 结构化的空闲/后台任务管理
- 为未来扩展（会话暂停/恢复、断线重连）打下基础

---

### 优化 2: 流式工具执行协议

**现状问题**:  
我们的工具调用是 RPC 式的：调用 → 等待 → 完整结果返回。对于长时间运行的工具（代码执行、大文件检索、子代理），用户在等待期间看不到任何进度。

**grok-build 做法**:  
统一 `ToolStream<T>` 协议 —— 工具返回 `Progress* → Terminal(Result)` 流：

```rust
pub enum ToolStreamItem<T> {
    Progress(ToolProgress),  // 中间进度（文本块、内容块、自定义负载）
    Terminal(Result<T, ToolError>),  // 最终结果
}
```

**建议方案**:  
在 Python 中实现等价的 `AsyncGenerator[ToolStreamItem]` 协议：

```python
from enum import Enum
from dataclasses import dataclass
from typing import AsyncGenerator, Union, TypeVar, Generic

T = TypeVar("T")

@dataclass
class ToolProgress:
    text: Optional[str] = None
    content_block: Optional[dict] = None
    custom_payload: Optional[dict] = None

@dataclass  
class ToolTerminal(Generic[T]):
    result: Optional[T] = None
    error: Optional["ToolError"] = None

ToolStreamItem = Union[ToolProgress, ToolTerminal[T]]
ToolStream = AsyncGenerator[ToolStreamItem, None]

class StreamingTool(ABC, Generic[T]):
    """支持流式输出的工具基类"""
    
    @abstractmethod
    async def execute(self, ctx: ToolCallContext, args: dict) -> ToolStream[T]:
        """子类重写此方法返回流"""
        ...
    
    async def run(self, ctx: ToolCallContext, args: dict) -> T:
        """便捷方法 - 收集流直到 Terminal"""
        async for item in self.execute(ctx, args):
            if isinstance(item, ToolTerminal):
                if item.error:
                    raise item.error
                return item.result
        raise ToolError("Stream ended without Terminal")
```

**Agent Loop 中的消费**:
```python
# 在 agent loop 中，工具调用时：
async for item in tool.execute(ctx, args):
    match item:
        case ToolProgress(text=t):
            yield SSEEvent(type="tool_progress", text=t)  # 实时推送给用户
        case ToolTerminal(result=r):
            tool_result = r  # 拼接结果，传给 LLM
            break
```

**收益**:  
- 长时间工具执行时用户可看到实时进度
- Bash/代码执行可流式输出 stdout
- 子代理可流式返回中间步骤
- 提升用户体验感知速度，减少"黑盒等待"焦虑

---

### 🟡 P1 — 上下文与记忆优化 (高影响力，中高难度)

---

### 优化 3: 三级压缩引擎 (Compaction Engine)

**现状问题**:  
我们的上下文管理主要是"修剪"（裁剪旧消息 + 按字符数截断），这是一种有损且不智能的压缩方式。关键信息可能在裁剪中丢失。

**grok-build 做法**:  
三级压缩引擎 —— 对应不同场景：

```
┌─────────────────────────────────────────────────┐
│  Full-Replace Compaction (全量替换压缩)          │
│  触发: 上下文 > 85% 窗口                          │
│  做法: LLM 总结全部历史 → 替换为紧凑摘要          │
│  特点: 保留关键信息，丢失细节但保留语义            │
├─────────────────────────────────────────────────┤
│  Intra-Compaction (步内压缩)                     │
│  触发: 单轮 agent 循环中工具调用过多              │
│  做法: 压缩旧工具结果为摘要                       │
│  特点: 保留最近 N 轮完整，旧轮摘要化               │
├─────────────────────────────────────────────────┤
│  Inter-Compaction (步间压缩)                     │
│  触发: 会话轮次过多                               │
│  做法: 分块压缩历史轮次                           │
│  特点: 保留首尾完整，中间压缩                      │
└─────────────────────────────────────────────────┘
```

**建议方案**:

```python
class CompactionEngine:
    """三级压缩引擎"""
    
    def __init__(
        self,
        sampler: CompactionSampler,      # 调用 LLM 做摘要
        token_counter: ItemTokenCounter, # token 估算器
        threshold: float = 0.85,         # 触发阈值 (占上下文窗口比例)
    ):
        self.sampler = sampler
        self.token_counter = token_counter
        self.threshold = threshold
    
    async def maybe_compact(
        self,
        conversation: list[Message],
        context_window: int,
    ) -> list[Message]:
        """根据当前 token 使用率选择压缩策略"""
        usage = self.token_counter.count(conversation) / context_window
        
        if usage > self.threshold:
            return await self._full_replace(conversation)
        elif usage > 0.6:
            return await self._inter_compact(conversation)
        else:
            return conversation  # 不需要压缩
    
    async def _full_replace(self, conversation: list[Message]) -> list[Message]:
        """全量替换: LLM 总结全部历史"""
        summary = await self.sampler.summarize(conversation)
        # 仅保留 system prompt + 摘要 + 最后一条用户消息
        return [
            conversation[0],  # system
            Message(role="user", content=f"[Previous conversation summary]\n{summary}"),
            conversation[-1], # 最后一条用户消息
        ]
    
    async def _intra_compact(
        self, conversation: list[Message], keep_recent: int = 3
    ) -> list[Message]:
        """步内压缩: 压缩旧的工具调用结果"""
        # 保留最近 keep_recent 轮完整，其余压缩
        ...
    
    async def _inter_compact(
        self, conversation: list[Message]
    ) -> list[Message]:
        """步间压缩: 首尾保留，中间摘要"""
        ...
```

**收益**:  
- 智能保留语义信息，而非粗暴裁剪
- 大幅延长有效会话长度
- 三种策略自动适配不同场景
- 用户感知的"失忆"问题显著改善

---

### 优化 4: TodoGate — 任务完成强制检查

**现状问题**:  
当 LLM 在工具调用后返回纯文本时，有时会"忘记"还有未完成的子任务。用户看到回复以为完成了，但实际还有待办事项。

**grok-build 做法**:  
`TodoGate` 在 LLM 返回纯文本后检查 `TodoState`：
1. 如果存在 `pending` 或 `in_progress`（无支撑的）任务 → 注入 system-reminder → 强制再发起一轮
2. 每轮最多触发 2 次（防止死循环）
3. 如果模型连续 2 轮未推进任何 todo → 不再触发（防止卡死）

```rust
// 伪代码
fn after_assistant_message(msg: &Message, todo_state: &TodoState) -> TodoGateAction {
    if msg.has_tool_calls() { return Allow; }
    
    let pending = todo_state.pending_or_unbacked_in_progress();
    if pending.is_empty() { return Allow; }
    
    if self.fire_count >= 2 { return Allow; } // 防止死循环
    
    self.fire_count += 1;
    InjectReminder(format!(
        "You have {} pending tasks. Continue working on them or mark them as completed.",
        pending.len()
    ))
}
```

**建议方案**:

```python
class TodoGate:
    """任务完成守卫 —— 防止 LLM 半途而废"""
    
    MAX_FIRES_PER_TURN = 2
    MAX_STALL_ROUNDS = 2  # 连续未推进 todo 的最大轮数
    
    def __init__(self):
        self._fire_count = 0
        self._stall_count = 0
        self._prev_todo_state: Optional[dict] = None
    
    def check(self, message: Message, todo_state: TodoState) -> TodoGateResult:
        if message.tool_calls:
            self._fire_count = 0  # 有工具调用，重置
            return TodoGateResult.ALLOW
        
        pending = todo_state.get_pending_or_unbacked_in_progress()
        if not pending:
            return TodoGateResult.ALLOW
        
        # 检测停滞
        if self._todo_state_unchanged(todo_state):
            self._stall_count += 1
        else:
            self._stall_count = 0
        
        if self._stall_count >= self.MAX_STALL_ROUNDS:
            return TodoGateResult.ALLOW  # 模型确实卡住了，放过
        
        if self._fire_count >= self.MAX_FIRES_PER_TURN:
            return TodoGateResult.ALLOW
        
        self._fire_count += 1
        return TodoGateResult.INJECT_REMINDER(
            f"You still have {len(pending)} pending tasks. "
            f"Please continue working on them. Current tasks: {pending}"
        )
```

**收益**:  
- 显著减少"半途而废"的会话
- 提升多步任务的完成率
- 用户无需手动催促 AI 继续

---

### 优化 5: PromptContext 序列化与确定性渲染

**现状问题**:  
我们的 system prompt 构建分散在多处（`system_prompt_v2.py`、`get_streaming_first_prompt()`、`context_engine.py`），缺乏统一的输入建模。虽然有缓存意识，但因为没有完全序列化，缓存命中率不够理想。

**grok-build 做法**:  
`PromptContext` 是一个完全可序列化的 struct，捕获 prompt 渲染的所有输入：

```rust
pub struct PromptContext {
    pub prompt_mode: PromptMode,       // Full / Extend
    pub audience: Audience,            // Primary / Subagent
    pub agent_md_files: Vec<AgentMd>,  // AGENTS.md 内容
    pub memory_config: MemoryConfig,
    pub os_name: String,
    pub shell: String,
    pub working_dir: PathBuf,
    pub current_date: NaiveDate,
    pub non_interactive: bool,
    // ... 所有变量都显式列出
}
```

渲染时：`ToolBridge::render_prompt(ctx) -> String`

**建议方案**:

```python
from dataclasses import dataclass, field
from datetime import date
from enum import Enum
import hashlib
import json

class PromptMode(Enum):
    FULL = "full"         # 首轮完整提示
    EXTEND = "extend"     # 续写模式
    SUMMARIZED_FORK = "summarized_fork"  # 子代理 fork 模式

class Audience(Enum):
    PRIMARY = "primary"
    SUBAGENT = "subagent"

@dataclass
class PromptContext:
    """完全可序列化的 Prompt 输入模型 —— 确保缓存键确定性"""
    prompt_mode: PromptMode
    audience: Audience
    agent_md: list[str] = field(default_factory=list)
    tools_json: str = ""             # 工具定义的 JSON 序列化
    memory_snippets: list[str] = field(default_factory=list)
    skills_xml: str = ""             # 匹配的技能 XML
    os_name: str = ""
    shell: str = ""
    working_dir: str = ""
    current_date: str = ""
    model_id: str = ""
    reasoning_effort: str = "medium"
    extra_context: dict = field(default_factory=dict)
    
    def cache_key(self) -> str:
        """生成确定性缓存键"""
        raw = json.dumps(self.__dict__, sort_keys=True, default=str)
        return hashlib.sha256(raw.encode()).hexdigest()
    
    def render(self, template: str) -> str:
        """通过模板渲染最终 prompt"""
        return template.format(
            tools=self.tools_json,
            skills=self.skills_xml,
            memories="\n".join(self.memory_snippets),
            os=self.os_name,
            shell=self.shell,
            cwd=self.working_dir,
            date=self.current_date,
            **self.extra_context,
        )
```

**收益**:  
- 确定性缓存键，提示缓存命中率从 ~60% → ~95%
- 调试时可 diff 不同请求的 PromptContext
- 为后续的 prompt 版本管理/A/B 测试提供基础

---

### 🟢 P2 — 可靠性基础设施 (中影响力，低中难度)

---

### 优化 6: 断路器 (Circuit Breaker)

**现状问题**:  
当 MCP 服务器或外部 API 不稳定时，我们的重试逻辑简单（固定次数重试），可能加剧下游压力（thundering herd），且缺乏自动恢复机制。

**grok-build 做法**:  
专用 `xai-circuit-breaker` crate —— 完整的状态机：

```
       ┌──────────┐
       │  CLOSED  │ ─── 正常工作，滑动窗口计数失败
       └────┬─────┘
            │ failures > threshold
            ▼
       ┌──────────┐
       │   OPEN   │ ─── 立即拒绝，不再尝试
       └────┬─────┘
            │ timeout 到期
            ▼
       ┌──────────┐
       │HALF_OPEN │ ─── 允许少量探测请求
       └────┬─────┘
            │ 成功 → CLOSED
            │ 失败 → OPEN
```

**建议方案**:

```python
import time
import asyncio
from enum import Enum

class CircuitState(Enum):
    CLOSED = "closed"
    OPEN = "open"  
    HALF_OPEN = "half_open"

class CircuitBreaker:
    """MCP/外部服务的断路器保护"""
    
    def __init__(
        self,
        failure_threshold: int = 5,
        recovery_timeout: float = 30.0,   # OPEN 状态维持时间
        half_open_max_requests: int = 2,   # HALF_OPEN 状态允许的探测请求数
        sliding_window_seconds: float = 60.0,
    ):
        self.failure_threshold = failure_threshold
        self.recovery_timeout = recovery_timeout
        self.half_open_max_requests = half_open_max_requests
        self.sliding_window_seconds = sliding_window_seconds
        
        self.state = CircuitState.CLOSED
        self._failures: list[float] = []   # 失败时间戳
        self._last_failure_time: float = 0
        self._half_open_count: int = 0
    
    async def call(self, fn, *args, **kwargs):
        """受断路器保护的调用"""
        if self.state == CircuitState.OPEN:
            if time.time() - self._last_failure_time > self.recovery_timeout:
                self.state = CircuitState.HALF_OPEN
                self._half_open_count = 0
            else:
                raise CircuitBreakerOpenError(
                    f"Circuit open, retry in {self.recovery_timeout - (time.time() - self._last_failure_time):.0f}s"
                )
        
        if self.state == CircuitState.HALF_OPEN:
            if self._half_open_count >= self.half_open_max_requests:
                raise CircuitBreakerOpenError("Half-open limit reached")
            self._half_open_count += 1
        
        try:
            result = await fn(*args, **kwargs)
            self._on_success()
            return result
        except Exception as e:
            self._on_failure()
            raise e
    
    def _on_success(self):
        self.state = CircuitState.CLOSED
        self._failures.clear()
    
    def _on_failure(self):
        now = time.time()
        self._failures.append(now)
        self._last_failure_time = now
        # 滑动窗口清理
        self._failures = [t for t in self._failures if now - t < self.sliding_window_seconds]
        if len(self._failures) >= self.failure_threshold:
            self.state = CircuitState.OPEN
```

**使用方式**:
```python
# MCPManager 中为每个 MCP 服务器创建断路器
class MCPManager:
    def __init__(self):
        self._breakers: dict[str, CircuitBreaker] = {}
    
    async def call_tool(self, server: str, tool: str, args: dict):
        breaker = self._breakers.setdefault(server, CircuitBreaker())
        client = self._clients[server]
        return await breaker.call(client.call_tool, tool, args)
```

**收益**:  
- 阻止级联故障 —— 下游服务故障不会拖垮整个系统
- 自动恢复 —— 无需人工重启
- 滑动窗口避免短暂波动触发断路
- 对 MCP 服务器不可用的情况极为重要

---

### 优化 7: MCP 存活监控与自动重启

**现状问题**:  
MCP 工具注册是一次性的（启动时 `initialize_all()`）。如果 MCP 服务器在运行期间崩溃/重启，工具变为僵尸（注册了但不可用），且没有自动恢复。

**grok-build 做法**:  
`mcp_dispatcher` 监控 MCP 客户端事件，发现失效时触发可配置的重启策略。

**建议方案**:

```python
class MCPLivenessWatcher:
    """MCP 服务器存活监控与自动恢复"""
    
    def __init__(
        self,
        manager: MCPManager,
        health_check_interval: float = 30.0,
        restart_policy: RestartPolicy = RestartPolicy.EXPONENTIAL_BACKOFF,
    ):
        self.manager = manager
        self.health_check_interval = health_check_interval
        self.restart_policy = restart_policy
        self._watchers: dict[str, asyncio.Task] = {}
    
    async def start(self, server_name: str):
        """启动对某个 MCP 服务器的监控"""
        self._watchers[server_name] = asyncio.create_task(
            self._watch_loop(server_name)
        )
    
    async def _watch_loop(self, server_name: str):
        backoff = 1.0
        while True:
            await asyncio.sleep(self.health_check_interval)
            try:
                # 轻量级健康检查 —— ping 或 list_tools
                await self.manager.ping(server_name)
                backoff = 1.0  # 恢复正常
            except Exception:
                logger.warning(f"MCP server {server_name} unhealthy, restarting...")
                await self.manager.restart_server(server_name)
                await asyncio.sleep(backoff)
                backoff = min(backoff * 2, 60.0)  # 指数退避，最大 60s
                
                # 重新注册工具
                await self.manager.refresh_tools(server_name)
                logger.info(f"MCP server {server_name} recovered")
    
    def stop(self, server_name: str):
        if task := self._watchers.pop(server_name, None):
            task.cancel()
```

**收益**:  
- MCP 工具从"注册一次"变为"持续可用"
- 用户无感知的服务恢复
- 指数退避避免反复重启

---

### 🔵 P3 — 开发者体验与扩展性 (中影响力，低难度)

---

### 优化 8: 结构化输出 (JSON Schema) 支持

**现状问题**:  
当需要 LLM 返回结构化数据时（如提取实体、生成报表），我们目前依赖提示工程让模型输出 JSON，缺乏强制约束和校验-重试机制。

**grok-build 做法**:  
注入合成 `StructuredOutput` 工具 —— 模型调用它来输出结构化数据，参数被 JSON Schema 校验，不合格则返回修正提示并重试（最多 3 次）。

**建议方案**:

```python
class StructuredOutputHelper:
    """为 LLM 添加 JSON Schema 结构化输出能力"""
    
    MAX_RETRIES = 3
    
    def __init__(self, schema_registry: dict[str, dict] = None):
        self.schemas = schema_registry or {}
    
    def register_schema(self, name: str, schema: dict):
        self.schemas[name] = schema
    
    def inject_structured_output_tool(
        self, tools: list[ToolDefinition], schema_name: str
    ) -> list[ToolDefinition]:
        """注入合成工具 —— 模型用此工具以 JSON 格式输出"""
        schema = self.schemas[schema_name]
        
        structured_tool = ToolDefinition(
            name=f"structured_output_{schema_name}",
            description=(
                f"Use this tool to output your final result in the required {schema_name} format. "
                f"Your response MUST be valid JSON matching the schema."
            ),
            parameters={
                "type": "object",
                "properties": {
                    "output": schema
                },
                "required": ["output"],
            },
            is_synthetic=True,  # 标记为合成 —— 由 agent loop 拦截而非真正执行
        )
        return tools + [structured_tool]
    
    async def handle_structured_call(
        self, tool_name: str, args: dict, schema_name: str, retry_count: int
    ) -> StructuredOutputResult:
        """拦截合成工具调用，校验 schema"""
        try:
            jsonschema.validate(args["output"], self.schemas[schema_name])
            return StructuredOutputResult(valid=True, data=args["output"])
        except jsonschema.ValidationError as e:
            if retry_count < self.MAX_RETRIES:
                return StructuredOutputResult(
                    valid=False,
                    correction=f"Output does not match schema: {e.message}. Please fix and retry.",
                    retry=True,
                )
            else:
                raise StructuredOutputMaxRetriesError(f"Failed after {self.MAX_RETRIES} retries")
```

**收益**:  
- 可靠的结构化数据提取（表单解析、文档分类等）
- 重试机制将格式错误率从 ~15% → ~1%
- AI assistant 可充当可靠的数据管道组件

---

### 优化 9: 子代理 Fork 模式

**现状问题**:  
我们的 `SubAgentManager` 为子代理创建全新的上下文，子代理看不到父代理的历史。这既浪费（父代理上下文可能有关键信息），又孤立（子代理缺乏背景）。

**grok-build 做法**:  
子代理 fork 父会话 —— 压缩父历史 → 注入为子代理的初始上下文 → 子代理在此基础上工作。保留了"知道从哪来"的上下文连续性。

**建议方案**:

```python
class SubAgentForker:
    """从父会话 fork 子代理上下文"""
    
    def __init__(self, compaction_engine: CompactionEngine):
        self.compaction = compaction_engine
    
    async def fork_context(
        self,
        parent_conversation: list[Message],
        subagent_type: SubAgentType,
        subagent_prompt: str,
    ) -> list[Message]:
        """从父会话 fork 出子代理的初始上下文"""
        
        # 1. 压缩父会话为紧凑摘要
        summary = await self.compaction.sampler.summarize(
            parent_conversation,
            instruction="Summarize the conversation, focusing on context relevant to the sub-task.",
        )
        
        # 2. 构建子代理的 system prompt
        system_prompt = self._build_subagent_system_prompt(
            subagent_type, subagent_prompt
        )
        
        # 3. 组装初始消息
        return [
            Message(role="system", content=system_prompt),
            Message(
                role="user",
                content=(
                    f"<parent_context>\n"
                    f"You were spawned by a parent agent to handle a sub-task.\n"
                    f"Parent conversation summary:\n{summary}\n"
                    f"</parent_context>\n\n"
                    f"{subagent_prompt}"
                ),
            ),
        ]
```

**收益**:  
- 子代理继承父上下文，减少重复探索
- 压缩而非复制 —— 节省 token 预算
- 子代理更像"团队协作"而非"外包给陌生人"

---

### 优化 10: 事件缓冲与合并 (ReplayBuffer)

**现状问题**:  
SSE 流中事件粒度太细 —— 一个 turn 可能产生几十个小事件（thinking、tool_call、tool_progress、text），对客户端渲染负担重，也可能超出某些 SSE 客户端的处理能力。

**grok-build 做法**:  
`ReplayBuffer` 基于 `BufferingSettings`（最大等待时长）合并事件块，减少 ACP 消息数量。

**建议方案**:

```python
class EventCoalescer:
    """SSE 事件合并器 —— 减少事件频率，优化客户端渲染"""
    
    def __init__(
        self,
        max_wait_ms: int = 50,       # 最多等 50ms 就发送
        max_batch_size: int = 10,    # 最多攒 10 个事件
    ):
        self.max_wait_ms = max_wait_ms
        self.max_batch_size = max_batch_size
        self._buffer: list[SSEEvent] = []
        self._last_flush: float = 0
    
    async def push(self, event: SSEEvent) -> Optional[list[SSEEvent]]:
        """推入事件，如果达到合并条件则返回合并后的事件列表"""
        now = time.monotonic()
        
        # 文本类事件可以合并（连续的 text 增量合并为一个）
        if self._buffer and self._can_merge(self._buffer[-1], event):
            self._buffer[-1] = self._merge(self._buffer[-1], event)
        else:
            self._buffer.append(event)
        
        # 达到任一条件就刷新
        elapsed_ms = (now - self._last_flush) * 1000
        if len(self._buffer) >= self.max_batch_size or elapsed_ms >= self.max_wait_ms:
            return self._flush()
        
        return None
    
    def _can_merge(self, a: SSEEvent, b: SSEEvent) -> bool:
        """判断两个事件能否合并"""
        # 连续的 text delta 可合并
        return (
            a.type == "text_delta"
            and b.type == "text_delta"
        )
    
    def _merge(self, a: SSEEvent, b: SSEEvent) -> SSEEvent:
        """合并两个事件"""
        return SSEEvent(
            type="text_delta",
            content=a.content + b.content,
        )
    
    def _flush(self) -> list[SSEEvent]:
        events, self._buffer = self._buffer, []
        self._last_flush = time.monotonic()
        return events
```

**收益**:  
- SSE 事件数量减少 60-80%
- 客户端渲染更流畅
- 网络带宽节省

---

## 三、实施路线图

### 第一阶段 (1-2 周): 可靠性基础设施
```
Week 1: CircuitBreaker + MCP Liveness Watcher
Week 2: EventCoalescer (SSE 合并)
```
**预期收益**: 系统稳定性明显提升，MCP 工具可用性从 95% → 99.5%

### 第二阶段 (2-3 周): 上下文优化
```
Week 3: CompactionEngine (三级压缩引擎)
Week 4: TodoGate (任务守卫)
```
**预期收益**: 有效会话长度提升 2-3x，任务完成率提升 30%

### 第三阶段 (2-3 周): 核心架构升级
```
Week 5: PromptContext 序列化 + 确定性缓存
Week 6: AgentActor 重构 (Actor 模式)
Week 7: StreamingTool 协议 + 重构关键工具
```
**预期收益**: 缓存命中率 → 95%，架构更健壮

### 第四阶段 (1-2 周): 开发者体验
```
Week 8: StructuredOutput 支持
Week 9: SubAgentFork 模式
```
**预期收益**: 结构化输出可靠性 → 99%，子代理协作更智能

---

## 四、风险与注意事项

| 风险 | 缓解措施 |
|------|---------|
| AgentActor 重构成大 | 渐进式迁移 —— 先在新代码路径验证，再切换旧路径 |
| Compaction LLM 调用增加延迟 | 使用 fast model 做摘要 (如 gpt-4o-mini)，异步执行 |
| CircuitBreaker 误触发 | 滑动窗口 + 可配置阈值 + 监控面板 |
| PromptContext 缓存键变化 | 版本号管理，向后兼容旧缓存键 |

---

## 附录: 技术术语对照

| grok-build 术语 | AI--Platfform 对应 |
|----------------|-------------------|
| MVP Agent | Gateway Router |
| Session Actor | AgentLoop |
| ChatStateActor | SessionManager + MemoryService |
| Compaction Engine | ContextCompressor (需升级) |
| ToolBridge | ToolRegistry |
| SubagentCoordinator | SubAgentManager |
| TodoGate | 无 (新增) |
| ReplayBuffer | SSE streaming (需增强) |
| PromptContext | system_prompt_v2.py (需结构化) |
| Circuit Breaker | 无 (新增) |
| StructuredOutput | 无 (新增) |
