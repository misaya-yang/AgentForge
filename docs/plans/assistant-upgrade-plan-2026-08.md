# AI Assistant 升级计划

> **状态:** superseded — 本文及其旧 successor 均不再是执行入口；当前架构实现只由
> `docs/plans/platform-architecture-convergence-prd-2026-08.md` 管理。本文仅保留 2026-08-13
> 实测数据；「默认关思考当主药」的写法已过时。

- **日期**: 2026-08-13
- **范围**: 当前运行时 `ai-gateway-assistant-service`（本仓库 compose，镜像 `review-current`）
- **方法**: 只读审查 + 生产日志复盘 + DashScope 对照实测
- **约束**: 这是通用代理。**不要**用问候词表、长度阈值、QueryIntent / Scenario 分类器给请求分流。

---

## 1. 结论

「你好」等了 13 秒，不是因为系统「分不清简单和复杂」，而是因为 **每一轮都被强制送进同一条昂贵路径**：Qwen 3.7 Plus 开 thinking、挂满工具、注入记忆，模型先写几百 token 的 Analyze / Identify / Determine，再吐出 17 个字。

分类器会把通用代理做成规则引擎：漏掉「哈喽呀」「在吗」「thanks」、多语言、以及「你好，帮我查一下上周合同」这类问候+任务。正确做法是 **同一套 agent loop**，让模型自己决定要不要工具；harness 只负责把默认第一次模型调用变便宜。

当前默认路径对简单回复不友好，对复杂任务也没有真正的深度增益——它只是让所有请求先付 10 秒思考税。

---

## 2. 实测证据

### 2.1 生产会话（截图同一次）

容器日志 `session=a17b82ce-...`，query=`你好`：

| 指标 | 值 |
| --- | --- |
| 预热 `total prep` | **88ms** |
| 模型轮次 | **1**（没有工具调用） |
| TTFT（首个 **正文** token） | **13310ms** |
| 总时长 | **13486ms** |
| 输出 | **17 chars** |
| 挂上的工具 | `spawn_subagent`, `tool_call`, `tool_describe`, `tool_search`, `update_user_memory`, `skill_create_document`, `context_compact`, `todo_read` |

预热不是瓶颈。13 秒几乎全是模型在 `enable_thinking=true` 下生成 reasoning，正文被挡住。

Activity 里的「2 steps」就是这段思考被当成活动步骤，不是两次工具执行。

### 2.2 对照：同一模型、同一句「你好」（绕过 harness）

直连 DashScope `qwen3.7-plus`，2026-08-13：

| 条件 | 首个思考 token | 首个正文 token | 总时长 | reasoning tokens | 正文 |
| --- | ---: | ---: | ---: | ---: | ---: |
| `enable_thinking=true`（与线上一致） | 2953ms | **11120ms** | 11179ms | 470 | 14 字 |
| `enable_thinking=false` | — | **3070ms** | 3071ms | 0 | 14 字 |
| `enable_thinking=true` + `thinking_budget=256` | 3007ms | **4778ms** | 4804ms | 91 | 16 字 |

线上 13.3s 与「thinking on + 完整 system/tools/memory」吻合：孤立调用已是 11s，再加上提示词和记忆，思考被拉得更长（截图里模型还在读 `User Memory.response_style=professional` 并起草 Option 1 / Option 2）。

关 thinking 后约 3s 回到正文。这是当前国际站 Qwen 的网络+推理下限，不是 harness 预热。`thinking_budget` 有效，但仍然比关 thinking 慢。

### 2.3 代码里是怎么强制思考的

`assistant_service.py` 对所有 Qwen3 **写死** `thinking_level="enabled"`，Gemini 3 写死 `"high"`。前端没有思考档位开关。

```1444:1451:apps/assistant-service/src/assistant_service/core/assistant_service.py
            # Thinking display: enable for thinking-capable models
            thinking_level=(
                "enabled"
                if "qwen3" in (config.model_id or "").lower()
                else "high"
                if "gemini-3" in (config.model_id or "").lower()
                else None
            ),
```

Qwen 3.7 Plus 系列本身就是 **hybrid thinking，提供商默认 thinking on**。`_qwen_thinking_enabled()` 在 `thinking_level is None` 时 **不传** `enable_thinking`，于是回落到提供商默认——仍然开思考。要让简单回复变快，必须 **显式** `enable_thinking=false`，除非用户/会话选择打开。

仓库里已有 Gemini Flash 的同类注释（2 字问候 ~20s TTFT），但只对 Flash 做了 `thinkingLevel=low`，Qwen 路径没有对称处理。

---

## 3. 根因（按影响）

### P0 — 强制思考，且没有预算

- 线上把「能思考」理解成「必须思考」。
- 不支持 `thinking_budget`（DashScope 已提供）。
- TTFT 只统计首个 **content** token。思考已经在流，用户却仍要等完整段独白才看到回复。
- 系统提示虽已收成 `CORE_ASSISTANT_PROMPT`，但思考模式下模型仍会自建 Analyze / Identify / Tone / Draft 流程。提示没有「直接回答，不要为寒暄做流程」。

### P0 — 工具面永远铺满，而不是按需展开

`select_tools()` 声明自己是 token 预算，不是授权。但 `ALWAYS_INCLUDE` 每轮都塞进：

- `spawn_subagent`
- `update_user_memory`
- `tool_search` / `tool_describe` / `tool_call`
- 以及当前可见的 skill / todo / compact

「你好」这种一轮结束的请求仍带 8 个 schema。Discovery 桥是对的；**子代理和记忆写入不该是永远在场的一级工具**。关键词加分表（`_TOOL_KEYWORDS`）和 `should_use_native_search()` 的搜索词表，是另一类硬编码分流，和「通用代理」冲突。

### P1 — 已有分类器，且不该接线

`QueryIntentAnalyzer` 能识别「你好」并给出 `complexity=0.1`，`ScenarioAnalyzer` 也有问候正则。它们在 `AgentLoop.__init__` 里被构造，**热路径从未调用**。

不要把它们接回去。也不要新写「简单/复杂」路由器。死代码应删，缩小攻击面和误导。

### P1 — 记忆与提示会拉长思考

- 每轮加载长期记忆；截图里模型用 `response_style: professional` 论证语气，从「你好」改成「您好」。
- `CORE_ASSISTANT_PROMPT` 仍带评测痕迹：「逐字段比对 schema」「每个决定性 ID」「重算每个指标」。对通用对话过重，思考模式下会诱发清单体。
- 旧的 `AGENT_IDENTITY` / `AGENTIC_WORKFLOW` 大段仍留在文件里（生产 builder 已不再拼接），容易在下次改 prompt 时被重新拼回去。

### P1 — 产品上没有思考控制

Customize 对话框没有 thinking 档位。用户无法选择「直接答 / 深想」。官方 Qwen / Gemini / ChatGPT 都把这做成显式开关，而不是后端猜意图。

### P2 — 工程债（上次审查仍有效，本轮不要混进主线）

2026-08-12 深度审查的 H1–H9（模型分级绕过、沙箱超时泄漏、checkpoint 时序、run 无回收器、记忆全量重索引等）仍然成立。它们影响可靠性和安全，但 **不是这次 13 秒问候的原因**。本计划主线是「同一循环、默认便宜」；H1–H9 单列并行，不绑在思考默认值上。

---

## 4. 设计原则

1. **一条 loop**。所有用户消息走 `StreamingPreparation → model turn → tool → repeat`。不按问候/复杂/领域分叉。
2. **模型选工具，harness 不猜意图**。默认第一次调用要便宜到模型可以直接开口；需要证据时再 `tool_search` / 调工具。
3. **思考是用户或运行档位，不是消息分类结果**。允许：用户开关、execution profile、或「已经发生工具调用之后的后续轮次」。禁止：扫描「你好」「hi」「长度 < 20」。
4. **提供商默认不可信**。Qwen 3.7 Plus 默认 thinking on。我们的默认必须显式关闭，或显式设预算。
5. **能力用渐进披露**。模型始终能搜到已授权工具；不要把 `spawn_subagent` 永久钉在上下文里。
6. **可见信号 ≠ 正文**。思考可以流到 Activity，但不能冒充「已经在回复」。产品指标要拆开。

---

## 5. 升级计划

### 阶段 A — 默认第一次调用变便宜（下一轮先做）

目标：同一句「你好」、同一模型、不写问候词表，端到端从 ~13s 降到接近关 thinking 的 ~3s。

| 项 | 改什么 | 为什么不是分类器 |
| --- | --- | --- |
| A1 | 默认 `enable_thinking=false`（Qwen 必须显式关闭）。Gemini 3 默认 `minimal`/`low`，不要 `high`。 | 全体请求同一默认，不看消息文本 |
| A2 | 把 `thinking_level` 做成请求/会话字段并接到 UI（关 / 开；开时可带 budget）。Customize 旁加一档即可。 | 用户意图，不是规则引擎 |
| A3 | 支持 DashScope `thinking_budget`。即便用户打开思考，也要有上限，避免 470 reasoning tokens 的独白。 | 提供商能力，与文本无关 |
| A4 | 可选：仅当 **本 run 已经执行过工具** 时，后续模型轮自动打开思考。 | 看 loop 状态，不看用户句子 |
| A5 | TTFT 拆成 `time_to_first_visible`（thinking 或 content）和 `time_to_first_answer`。日志、SSE、气泡旁的 13.49s 都用同一套。 | 观测，不分发 |

验收：

- 无开关、无工具的「你好」：`time_to_first_answer` p50 < 4s（受提供商下限约束，以对照表 3s 为基线）。
- 打开思考后仍能在 Activity 看到完整 thought process。
- 单测：Qwen 默认请求体 `enable_thinking is False`；显式 `enabled` 才为 True；`thinking_budget` 原样下发。
- **禁止**出现 `if query.startswith("你好")` 一类分支。

### 阶段 B — 工具面按需展开

目标：简单回复仍走同一 loop，但第一次调用不再被 8 个 schema 和 `spawn_subagent` 干扰。

| 项 | 改什么 |
| --- | --- |
| B1 | `ALWAYS_INCLUDE` 只留 discovery 三件套（`tool_search` / `tool_describe` / `tool_call`）。KB 仅在用户显式绑定且为 tool 模式时进入一级目录。 |
| B2 | `spawn_subagent`、`update_user_memory`、文档 skill、todo、compact 全部改为可发现，而不是每轮直挂。复杂任务由模型 `tool_search` 再调。 |
| B3 | 删掉或停用 `_TOOL_KEYWORDS` 和 `should_use_native_search()` 的关键词分流。Native search 改为：用户打开「联网」时作为模型能力挂上，或作为可发现工具，由模型决定是否搜。 |
| B4 | 删除热路径上的死分类器：`QueryIntentAnalyzer`、`ScenarioAnalyzer`、`DocumentAnalyzer` 的构造与导出。不要在 preparation 里复活它们。 |

验收：

- 「你好」第一次模型调用的 tool schema 只有 discovery 桥（或用户已打开的显式能力，如联网）。
- 「帮我写一份 12 页融资 PPT」仍能经 `tool_search` → `skill_create_document` 完成，不依赖关键词命中。
- 现有 `test_tool_selector.py` 改为断言「未直挂的工具仍可经 discovery 到达」，而不是断言关键词命中。

### 阶段 C — 提示词按通用代理收束

目标：思考打开时也不要写「Analyze the Request」五段论；思考关闭时直接答。

| 项 | 改什么 |
| --- | --- |
| C1 | `CORE_ASSISTANT_PROMPT` 去掉评测清单（强制 JSON 字段、每个 ID、重算每个指标）。这些只属于带 schema 的评测/结构化调用。 |
| C2 | 加一条稳定、非分类的原则：*Match the request's actual work. Reply immediately when no tool is needed. Do not narrate a procedure.* |
| C3 | 记忆注入改成短事实，不要让模型把 `response_style` 论证成思考步骤。寒暄不必先读完整偏好块。 |
| C4 | 把未再使用的 `AGENT_IDENTITY` / `AGENTIC_WORKFLOW` / `ANTI_HALLUCINATION` 大段标成 legacy 或移出默认模块，避免再次拼进生产 prompt。 |

验收：打开思考时，「你好」的 reasoning 应短（或被 budget 截断），且不再出现 Option 1 / Option 2 草稿；正文仍自然、匹配语言。

### 阶段 D — 复杂任务变强（仍是同一 loop）

简单路径变便宜之后，再加深度，不要靠「复杂分类器」触发。

| 项 | 改什么 |
| --- | --- |
| D1 | 用户打开思考，或 A4 在工具之后打开思考，才进入深想。多步任务靠已有 run budget + todo + subagent，不靠新路由。 |
| D2 | 子代理保持审批、深度上限、无父路径 fail-closed。父模型通过 `tool_search` 发现 `spawn_subagent`，而不是每轮看见它。 |
| D3 | 补一套 **行为 eval**，覆盖：寒暄、单轮事实、工具任务、拒绝/审批、长任务恢复。用同一 harness 跑，用结果判断「有没有变好」。不要在运行时用这套标签做分流。 |
| D4 | 并行消化上次审查的可靠性项：H4 checkpoint 时序、H6 run 租约、强制 synthesis 语义、断线续跑。这些决定复杂任务能不能跑完。 |

### 阶段 E — 观测与发布门

| SLI | 定义 | 发布门槛（建议） |
| --- | --- | --- |
| `time_to_first_answer` | 首个正文 token | 默认关思考：p50 < 4s，p95 < 8s（提供商允许时） |
| `time_to_first_visible` | 首个 thinking 或 content | 开思考时 p50 < 4s（用户马上看到 Activity） |
| `thinking_tokens` | reasoning 用量 | 关思考应为 0；开思考且有 budget 时不超过 budget |
| `first_turn_tool_schema_count` | 第一次模型调用挂上的 schema 数 | 默认 ≤ discovery 桥数量 |
| `iterations` | 模型轮次 | 无工具寒暄应为 1 |
| 回归 | 现有 assistant 单测 + 文档/工具黄金集 | 不允许用问候词表换绿 |

---

## 6. 明确不做

- 不把 `QueryIntentAnalyzer` / `ScenarioAnalyzer` 接到 streaming 热路径。
- 不写 `is_simple_task()` / 问候正则 / 「<10 字跳过工具」。
- 不按句子长度开关 thinking。
- 不把「你好」做成特殊 UI 快路径（绕过 AgentLoop）。
- 不在本轮重做 Agent Studio / LangGraph / 换框架。
- 不把 H1–H9 和本主线绑成一个大 PR。先合 A，再合 B/C。

---

## 7. 建议的落地顺序

```text
A1+A3  默认关思考 + thinking_budget 管道
  → A2  UI/API 思考档位
  → A5  TTFT 拆指标
  → B1+B2  缩小 ALWAYS_INCLUDE
  → B3+B4  去掉关键词分流和死分类器
  → C     提示词与记忆瘦身
  → D     复杂任务与可靠性
```

A1 单独就能消化截图里的 13 秒问题。B 防止下一轮模型对寒暄调用 `spawn_subagent`。C 防止打开思考后又变成长独白。

---

## 8. 验证了什么 / 没验证什么

**已验证**

- 本仓库 compose 标签正确（`working_dir=/Users/yang/projects/AI--Platfform`）。
- 网关 `/health` 正常。
- 生产「你好」日志：prep 88ms，1 轮，TTFT 13310ms，总 13486ms。
- 源码：Qwen3 思考写死开启；`QueryIntentAnalyzer` 只构造不调用；`ALWAYS_INCLUDE` 每轮挂子代理；`should_use_native_search` 是关键词表。
- 直连 DashScope 对照：thinking on 11.1s / off 3.1s / budget=256 为 4.8s。

**未验证**

- 未在浏览器里再跑一遍完整 UI（截图与日志已对齐同一次会话）。
- 未测「你好，帮我查合同」在关思考 + discovery-only 下会不会漏工具——这是阶段 B 的必测项。
- 未测 Gemini 3 在本环境的对称路径（当前默认模型是 Qwen，Google key 未配置）。
- 未复跑 2026-08-12 审查里的 H1–H9 是否已有人修。

---

## 9. 一句话

通用代理的分界不该写在 if 里，而该写在默认路径的成本里：同一条 loop，第一次调用默认不思考、少挂工具；用户要深想，或模型已经开始用工具，再付思考的钱。
