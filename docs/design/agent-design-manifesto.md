# AI Assistant 设计宣言

## —— 从四大 Agent Runtime 中提炼的架构哲学

**2026-07-16**

---

## 前言：我们发现了什么

花了三天时间，四轮深入审查，阅读了超过 20 万行代码。我们不是在找"功能清单"，我们在追问一个更根本的问题：

**什么样的架构哲学，能让一个 AI Agent 真正"好用、前沿、稳定"？**

四个项目给出了四种答案，但它们共享一个惊人的共识——这个共识，正是我们缺的东西。

---

## 一、核心洞察：伟大 Agent 的五个架构信念

### 信念 1: 一切都会失败 —— 设计"失败优先"而非"正确路径优先"

这是最深刻的发现。我们的 agent 代码是这样写的：

```python
try:
    result = await tool.execute(args)   # 假设这能成功
    return format(result)                # 快乐路径
except Exception as e:                   # 32 个一模一样的兜底
    logger.error(e)                      # 不知道发生了什么
    return error_response(e)             # 也不知道怎么恢复
```

而三个顶级 agent 的代码是这样写的：

```python
error = classify_api_error(e)           # 先诊断：24 种失败原因之一
match error.reason:
    case CONTEXT_OVERFLOW:               # 上下文溢出
        await compact(urgent=True)       # → 压缩，不要放弃
    case RATE_LIMITED:                   # 速率限制
        await rotate_credential()        # → 换一把钥匙
    case AUTH_EXPIRED:                   # 认证过期
        await refresh_token()            # → 刷新，不清空会话
    case IMAGE_TOO_LARGE:                # 图片过大
        downscale_and_retry()            # → 缩小，而非放弃
    case THINKING_SIGNATURE_INVALID:     # 签名无效
        strip_reasoning_details()        # → 剥离无效数据
    # ... 24 种精确恢复路径
```

**这不是代码质量的差距。这是哲学差距。**

我们的代码假设 LLM 调用会成功、工具会返回、MCP 服务器不会崩溃。于是当失败发生时，我们只有一种反应：记录日志，然后吞掉错误继续。

但 Hermes 的 24 种 `FailoverReason` 不是凭空设计的——每个都对应一个真实的生产事故。`image_too_large` 是因为某次部署后图片压缩出了问题。`thinking_signature_invalid` 是因为 Anthropic 签名的实现细节变了。`llama_cpp_grammar_pattern` 是因为 llama.cpp 的 JSON Schema 转语法器对某些正则表达式有 bug。

**行动**: 我们的错误处理需要一个分类器，不是 32 个 `except Exception`。每个错误类型对应一个恢复路径。这不是"过度设计"——这是"从事故中学到的工程设计"。

---

### 信念 2: Agent 循环必须是事件驱动的 Actor，不是线性的函数

这是第二深刻的共识。四个项目中三个（grok-build、Hermes、OpenClaw）的 agent loop 都是某种形式的 Actor 模型或 select-loop：

```
grok-build:  tokio::select! { cmd_rx, event_rx, completion_rx, idle_flush, dream_check, ... } 
Hermes:     while iteration < max_iterations: process_all_output_types()
OpenClaw:   runEmbeddedAttempt() 外层 retry loop + 内层 Pi Agent stream loop
AI--Platfform: execute() → _execute_streaming_first() → while True: call_llm → invoke_tools → ...
```

我们的 loop 是一个 4871 行的函数。它是线性、阻塞、脆弱的。

为什么 Actor 模式更好？不是因为"酷炫"，而是因为一个根本原因：**在 Agent 运行期间，世界不会停止变化。**

- 用户可能中途取消
- 模型可能切换（管理员改了配置）
- MCP 服务器可能重启
- 会话可能超时需要持久化
- 后台任务（记忆整理、技能反思）可能需要运行

一个线性函数无法优雅地处理这些并发事件。你只能在每次 `await` 之后检查 `cancel_event.is_set()`——这就是我们现在做的，脆弱且容易遗漏。

grok-build 的设计特别有启发性：他们的 Actor 模型是被 Rust 的 `!Send` 类型**逼出来**的——因为 `MvpAgent` 内部有 `Rc<RefCell<...>>`，不 `Send`，只能在单线程 LocalSet 上运行。这个"限制"反而变成了优势：没有 Mutex 开销，状态变更天然顺序化，缓存局部性好。

**行动**: 我们的 AgentLoop 需要拆成 AgentActor，用 `asyncio.Queue` + `asyncio.wait` 实现多事件源选择。不是为了 Rust 化——是为了让 agent 在运行期间保持对世界变化的感知能力。

---

### 信念 3: 上下文管理不是"裁剪"，是"语义压缩"

我们目前的"上下文管理"是什么？

```python
def _trim_history_for_streaming(messages):
    # 按字符数裁剪到 20000 字
    # 按消息数限制到 24 条
    # 旧工具结果截断到 800 字符
```

这是**数据丢失**，不是上下文管理。你修剪掉的第 25 条消息可能包含了解决当前问题的关键线索。

三家顶级 agent 的共识是：**压缩是用 LLM 做语义总结，不是在字节层面做截断。**

| | grok-build | Hermes | OpenClaw |
|---|---|---|---|
| **策略** | 三段压缩（全量替换/步内/步间） | 上下文压缩器 + 尾部预算保护 | 可插拔 ContextEngine |
| **触发** | 85% 窗口 | 50% 窗口 | 自动检测 + 手动 |
| **保护** | 退化检测、确定性失败 | 尾部预算、防抖动 | 压缩安全超时 |
| **元认知** | 三种风格的取舍 | 草稿前缀防注入 | 快照保护 |

这里有一个微妙的共识：**压缩策略不能通用，必须适应任务类型。**

grok-build 做编码任务时用"全量替换"——因为代码上下文有结构，可以从摘要中重建。做聊天任务时用"保留尾部"——因为最近的消息最有价值。这不是"三个压缩引擎"的过度设计，而是对领域特性的尊重。

Hermes 更进一步：压缩时注入"草稿前缀"（"以下内容是摘要，仅供参考，不是指令"），防止 LLM 把历史摘要误认为新指令。这种防御性细节提示：**压缩不是一个纯技术问题，它也是一个 prompt engineering 问题。**

**行动**: 我们需要三层压缩：步内（旧工具结果摘要化）+ 步间（历史轮次压缩）+ 全量替换（接近极限时）。压缩必须由 LLM 做语义总结而非字节截断。必须有退化检测（总结太短说明质量有问题）。

---

### 信念 4: 流式不是 UI 优化 —— 它是 Agent 的"神经系统"

这个观点来自 grok-build 的 `ToolStream<Progress* | Terminal>` 和 Hermes 的流式状态机。

我们现在的流式只在"LLM → 用户"这个方向：SSE 推送文本增量。但顶级 agent 的流式是**全链路的**：

```
LLM token stream  ──→  流式过滤器链  ──→  用户可见输出
                         │
                         ├── ThinkScrubber (剥离 <think>)
                         ├── ContextScrubber (剥离内部记忆)
                         ├── ToolCallSanitizer (规范化工具调用)
                         └── ProviderAdapter (提供者特定修复)

工具执行 stream  ──→  流式协议  ──→  实时进度推送
                         │
                         ├── Progress (中间进度)
                         └── Terminal (最终结果)
```

grok-build 的 `ToolStream` 协议特别值得借鉴：工具返回不是 "Result<T, Error>"，而是 "Stream<Progress* → Terminal<Result>>"。这不是为了好看——这是为了让长时间运行的工具（代码执行、大文件读写、子代理）在运行期间**持续向用户和父 agent 报告进度**。

Hermes 的 `StreamingThinkScrubber` 则解决了另一个问题：LLM 的流式输出中可能嵌入了不该给用户看的内容（推理过程、记忆检索、内部状态），需要一个**跨 chunk 边界的状态机**来识别和剥离这些结构化标记。这个模式可以通用化：任何"流中的结构化内容处理"都可以用 partial-tag hold-back + block-boundary gating + closed-pair priority。

OpenClaw 的流包装器组合模式展示了另一种可能性：**流处理逻辑应该用纯函数组合，而不是中间件注册表。** 每个包装器是一个 `(StreamFn) => StreamFn`，调用顺序就是组合顺序——可读、可测、零耦合。

**行动**: 工具需要支持流式执行协议（Progress stream）。流式输出需要可组合的过滤器链（状态机处理跨 chunk 的结构化标记）。流包装器应该是纯函数组合而非中间件。

---

### 信念 5: Agent 应该"自我进化"——元认知是下一个差异化能力

这是 Hermes 最独特的创新，也是其他 agent 尚未充分探索的方向。

Hermes 的 background review 线程在每轮对话后做一件事：**fork 一个自己的快照，问"我应该从这次对话中学到什么？"**

这个设计有几个极其精妙的约束：
- **Fork 而非原地修改**：反思线程使用独立的 LLM 调用，不影响主循环
- **继承缓存**：反思线程复用主循环的 system prompt 缓存，节省 ~26% 成本
- **受限工具集**：反思线程只能操作 memory 和 skill，不能发消息或执行代码
- **防污染规则**：不记录"工具 X 坏了"（会固化错误认知）、不记录一次性任务（不创建 `fix-pr-1234` 这种技能）、只用类名（描述一类工作而非今天的特定问题）

这实现了一个闭环：**使用 → 反思 → 学习 → 下次更好**。

我们已经有 Skills 系统和 Memory 系统。但它们是人类手动管理的：用户或开发者创建 skill，agent 只能使用。如果 agent 能**创建和更新自己的 skill**——不需要人工干预——这就是从"工具"到"协作者"的质变。

**行动**: 在每轮对话后增加可选的"反思步骤"——低优先级的后台任务，使用 fast model 分析对话、提取可复用的模式、更新记忆和技能。这不是 P0 功能，但它是区分"好用"和"惊喜"的关键。

---

## 二、我们的差距：从自我批评开始

在深入阅读了我们自己的代码后，以下是最需要修复的结构性问题：

### 致命问题（必须立即修复）

1. **MCP 工具执行无超时保护**
   - `mcp/manager.py` 的闭包直接调用 `client.call_tool()`，不经过 `ToolRegistry.execute()` 的 `asyncio.wait_for`
   - 如果 MCP 服务器挂了，agent loop 永久阻塞
   - **修复**: 所有工具调用路径统一加超时 + 断路器

2. **32 个裸 `except Exception` 无错误分类**
   - 持久化失败、压缩失败、内存同步失败都用同一种方式处理：记录日志然后继续
   - 如果某个服务反复失败，agent 会在一个会话中反复重试 10 次而不知道
   - **修复**: 错误分类器 + 每个 try/except 改为具体异常类型

3. **全局单例跨租户状态泄漏**
   - `ToolRegistry`、`MCPManager`、`TaskManager` 都是模块级单例
   - 多租户场景下工具冲突、状态泄漏无法避免
   - **修复**: 改为请求作用域或租户作用域的实例

### 严重问题（应在本季度修复）

4. **缓存哈希计算了但从未发送给 LLM**
   - `ContextCacheOptimizer` 计算了 `stable_cache_hash`，但只用于指标
   - 实际的 Anthropic cache control、OpenAI 自动缓存都没有被触发
   - **修复**: 在 Provider Adapter 中将缓存信号转换为 API 层的缓存控制头

5. **会话锁不跨请求**
   - `SessionResources` 的锁是每个 `TaskContext` 实例创建的
   - 同一 session 的并发请求获得不同的锁，互相不阻塞
   - **修复**: 会话级锁由 `SessionManager` 集中管理

6. **Token 估算用字符数硬除**
   - `estimate_history_tokens` 用 `len / 4` 估算，中英文混用更是按固定比例
   - 系统性偏差导致要么过早压缩浪费窗口，要么溢出静默丢失信息
   - **修复**: 集成 tiktoken 做精确计数，至少对主要模型

### 重要问题（应纳入规划）

7. **子代理无 LLM 调用超时**——deadline 在每次调用后才检查
8. **MCP 无存活监控、无断路器、无自动重连**——服务器崩溃后工具永久不可用
9. **上下文压缩是字符级截断而非语义压缩**——丢失关键信息
10. **工具结果去重逻辑可能让模型看到假结果**——第二个调用被丢弃但模型不知道

---

## 三、设计路线：12 周三阶段改造

我们不会一次性重写整个系统。改造路径应该遵循"修复致命问题 → 增强核心能力 → 引入差异化"的顺序。

### 阶段一：止血（Week 1-3）

**目标**: 消除导致 agent 卡死、静默失败、状态泄漏的根本原因。

```
Week 1: 统一错误分类器
  - 实现 FailoverReason 枚举 + classify_api_error()
  - 为 LLM API 错误、工具错误、MCP 错误建立分类
  - 初始覆盖 8-10 种错误类型（从最常见开始）
  - 替换 agent_loop.py 中最关键的 10 个裸 except

Week 2: MCP 可靠性
  - 所有 MCP 工具调用统一走 timeout + circuit breaker
  - MCPLivenessWatcher 定期健康检查 + 自动重连 + 工具重新注册
  - 全局单例 → 请求/租户作用域（至少 ToolRegistry）

Week 3: 执行保护
  - 所有工具调用路径统一超时
  - 工具结果大小限制在工具执行层（而非后期截断）
  - 会话锁修复为跨请求生效
  - Token 计数切换为 tiktoken
```

### 阶段二：筑基（Week 4-8）

**目标**: 建立正确的架构基础，让后续创新有稳固的土壤。

```
Week 4-5: Actor 模式 AgentLoop
  - AgentActor 替代线性 execute()
  - asyncio.Queue + asyncio.wait 多事件源选择
  - 支持优雅取消（不再依赖 CancelledError）
  - 支持模型热切换

Week 6-7: 三层上下文压缩引擎
  - 步内压缩: 旧工具结果 LLM 摘要化
  - 步间压缩: 历史轮次分批压缩
  - 全量替换: 接近极限时的紧急压缩
  - 尾部保护 + 退化检测 + 草稿前缀

Week 8: PromptContext 序列化 + 缓存信号激活
  - PromptContext dataclass 统一建模所有 prompt 输入
  - 缓存键确定性哈希
  - 缓存信号真正发送到 LLM API
```

### 阶段三：超越（Week 9-12）

**目标**: 引入差异化能力，不是追赶而是领先。

```
Week 9-10: 流式能力升级
  - ToolStream 协议: 工具返回 Progress* → Terminal
  - 流式过滤器链: ThinkScrubber + ContextScrubber + ProviderAdapter
  - 流包装器纯函数组合

Week 11: 结构化输出 + 工具搜索
  - StructuredOutput 合成工具 + JSON Schema 校验 + 修正重试
  - ToolRegistry.search() + tool_search 内置工具

Week 12: 元认知反思
  - BackgroundReflection 后台线程
  - Fork agent + 受限工具集 + 缓存继承
  - 自动创建/更新技能和记忆
  - 反污染规则
```

---

## 四、什么我们不照抄

从四个顶级 agent 中学到的，同样重要的是**什么不该抄**：

### 不抄 grok-build 的

- **`!Send` 类型 + 单线程 LocalSet**：这是 Rust 特有的约束驱动的设计。Python 的 asyncio 天然支持单线程协作式并发，不需要 `!Send` 这种 hack。我们的 Actor 应该是 Pythonic 的，不是 Rust 的翻译。

- **19 种错误类型 + 15 种工具能力标志**：grok-build 的工具系统是为"生产级工具编排器"设计的，需要管理多个后端（xAI API、MCP 服务器、本地终端）。我们不需要这么细粒度，10 种左右的错误类型足够。

- **ACP 协议的完整实现**：grok-build 实现了完整的 Agent Client Protocol 规范和 ReplayBuffer 缓冲。我们目前只需要 SSE 流，不需要 ACP 级别的标准化。

### 不抄 Hermes 的

- **Kanban 看板系统**：这是 Hermes 在没有基础设施的笔记本电脑上用 SQLite 做任务队列的"绝妙荒诞"。我们有 Kubernetes + Redis + Celery，不需要在 SQLite 里实现分布式任务调度。

- **25+ 模型提供者**：Hermes 要为每个提供者的每个古怪行为写适配代码（`llama_cpp_grammar_pattern`？！）。我们的模型注册表已经很好地抽象了 6 个主要提供者，保持这个规模即可。

- **音频管道、TUI、浏览器控制、25 种消息平台适配器**：这些是个人 CLI 助手特有的功能，不是通用平台的关注点。

### 不抄 OpenClaw 的

- **单进程网关 + 20 通道**：这是 OpenClaw 最明显的架构债务。所有通道、代理运行时、工具执行挤在一个 Node.js 进程里。我们用微服务架构已经解决了这个问题——保持网关和助手服务的分离。

- **文件锁实现的认证存储**：OpenClaw 的 `auth-profiles` 用 JSON 文件 + 文件锁管理多凭证。我们有数据库和 Redis——用它们，远比文件锁可靠。

- **Pi Agent Framework 的外部依赖**：OpenClaw 将核心 agent 循环委托给第三方包，这造成了清晰的边界但也产生了大量适配和包装代码。我们的 agent loop 是自研的——保持这个优势，但要修复。

---

## 五、架构愿景：我们想成为什么样的 Agent？

经过这轮研究，我对"好用的、前沿的、稳定的 Agent"有了更清晰的定义：

### 稳定的 Agent

- **失败是常态，恢复是本能**：每种失败都有精确的诊断和恢复路径。当前模型挂了？换 fallback。上下文溢出了？压缩而非截断。MCP 服务器崩了？断路器保护 + 自动重连。
- **状态是可审计的**：每次工具调用有超时、有上限、有日志。每个全局状态有明确的生命周期和作用域。
- **并发是安全的**：会话锁跨请求生效。 Actor 之间通过消息通信而非共享状态。

### 好用的 Agent

- **实时反馈**：工具执行有进度条，不是黑盒等待。子代理有流式中间步骤。
- **不半途而废**：TodoGate 确保有始有终。任务的完成状态全程可见。
- **可以中途介入**：/steer 机制让用户从旁观者变成协作者。不需要打断整个会话就能微调方向。

### 前沿的 Agent

- **自我进化**：从每次对话中学习，自动创建和更新技能。不依赖人工手动管理知识库。
- **生态强大**：Skills + MCP + Plugins 三重扩展机制互相补充。技能是轻量级的 prompt 注入，MCP 是标准化的外部工具协议，插件是深度系统集成。
- **架构清晰**：Actor 模型让并发和状态管理变得简单。可插拔的 ContextEngine 让压缩策略可以按场景定制。流式工具协议让 agent 与外部世界的交互从 RPC 变成持续对话。

---

## 附录：四个项目的贡献权重

| 我们吸收的设计 | 主要来源 | 为什么 |
|---------------|---------|--------|
| Actor 模式 AgentLoop | grok-build | 最成熟的 Rust Actor 实现，但我们需要 Pythonic 版本 |
| 错误分类恢复 | Hermes | 24 种分类是行业最精细的实现 |
| 断路器 | grok-build | 生产验证的滑动窗口实现 |
| 流式工具协议 | grok-build | ToolStream 协议设计最完善 |
| 三层压缩引擎 | grok-build + Hermes | grok-build 提供策略，Hermes 提供防注入细节 |
| 提示缓存分层 | Hermes | stable/context/volatile 三层最清晰 |
| 流式状态机 | Hermes | ThinkScrubber 模式可直接通用化 |
| 流包装器组合 | OpenClaw | 纯函数组合 > 中间件注册表 |
| 认证凭证轮换 | OpenClaw | 冷却追踪 + round-robin 设计最实用 |
| 可插拔 ContextEngine | OpenClaw | 接口设计（bootstrap→assemble→compact→afterTurn）最成熟 |
| 元认知反思 | Hermes | 唯一在生产中实现的自我进化系统 |
| TodoGate | grok-build | 简单有效，直接可用 |
| 结构化输出 | grok-build | 合成工具 + Schema 校验 + 重试最优雅 |
| /steer 机制 | Hermes | 把用户变成协作者的协议创新 |
