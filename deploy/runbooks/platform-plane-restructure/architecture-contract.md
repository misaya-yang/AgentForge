# PPR 架构契约

> 本程序执行期间不可漂移的不变量。与 `docs/harness/architecture.md` 和
> `docs/harness/platform-architecture.md` 冲突时，以仓库级文档为准并停止当前 phase。

## 1. 五个逻辑应用 plane

| Plane | 职责 | SLO 类别 | 默认语言取向 |
| --- | --- | --- | --- |
| **Edge** | 公共入口、鉴权、准入、限流、路由、配额判定 | 不阻塞；本地附加延迟按 PPR-00 门禁 | Python/Lua，只有证据支持才改变 |
| **Control** | Agent/Thread 生命周期、能力目录、策略、Studio、计费 | 不在 token 路径；业务可快速迭代 | Python |
| **Data** | Agent kernel、模型流投影、能力执行 | 每 token 与每流资源有界 | Rust kernel；model plane 条件采用 |
| **Index** | 摄入、切块、嵌入编排、检索 | 批处理不得挤压交互检索 | Python + 条件性原生内核 |
| **Governance** | Eval、审计、trace 消费、质量门 | 异步；不得同步阻塞 Edge/Data | Python |

PostgreSQL、Redis、Qdrant 和对象存储是**共享基础设施底座**，不是应用 plane。逻辑 plane 是所有权与 SLO 边界，不自动意味着一个新容器。部署拓扑由 ADR-008 在以下方案中选择：

- **T0：** 保留现有 gateway 部署单元，优先多副本/worker 和异步隔离。
- **T1：** 只把 Governance 拆成独立部署单元。
- **T2：** 拆 Edge 与 Governance，Control 留在 gateway-control。
- **T3：** 在 T0/T1/T2 上条件性增加 Rust model-plane 部署单元。

## 2. 硬规则

**H1 — Data token 路径不得同步调用 Control 或 Governance。**
授权、目录和计费所需的不可变输入在 turn 开始时随已签名 snapshot/lease 固定。当前 capability-worker 直连 knowledge-service，知识侧本地验证 HMAC proof；不得重新引入 gateway 回调。

**H2 — 跨 plane 只走版本化、认证且可回放的契约。**
优先复用 `agent-event/v2`、Capability Contract V2 和现有内部 HTTP 契约。如果物理拆分需要新的内部 handoff，ADR 必须先定义 schema 版本、服务身份、防重放、超时、重试、幂等、背压、错误映射和 SSE owner；禁止临时 header 或未版本化 RPC。公共契约仍然零变更。

**H3 — Governance 不得在用户请求路径同步执行。**
Eval、审计和 trace 消费必须异步、可限流且有独立资源预算。是否需要独立进程由 PPR-00 noisy-neighbor 证据和 ADR-008 决定；“逻辑独立”不得被偷换成“必须新增服务”。

**H4 — 业务逻辑不得进入 `packages/ai-gateway-core/`，也不得以 Rust 形式重演该耦合。**

**H5 — 仓库依赖方向不变。**
`web -> src -> apps/* -> packages/ai-gateway-core`；`apps/*` 不得 import `src/`，app 之间不得互相 import，core 是叶子。由 `make test-isolation` 强制。

**H6 — 公共契约零变更。**
OpenAPI、`agent-event/v2`、Capability Contract V2 和四端 SDK 行为不得漂移。任何公共契约变化必须退出本程序，单独决策。

**H7 — Agent Runtime 保持隔离运行时家目录。**
不得继承宿主指令、插件、MCP、凭据或文件系统状态。

**H8 — 供应链身份单一权威。**
镜像身份权威是 `deploy/agent-runtime-source/lock.json`；其余位置必须由工具派生，不得手改。

**H9 — 一次真实 turn 最多一个供应商模型请求。**
Rust model-plane shadow 必须 tee 同一份已接收 provider frames，或对已保存、已脱敏的 frames 离线回放；不得为影子对比额外调用供应商、重复计费或改变随机性。

**H10 — 采用与完成是两个不同状态。**
允许负向结论的 phase 可以以 `measured_not_adopted` 完成，但证据必须明确原实现仍是 owner；不得把实验代码留在默认路径。

## 3. 不得触碰

| 项 | 原因 |
| --- | --- |
| `deploy/agent-runtime-source/` 锁与 SBOM 的证明语义 | 只能通过权威工具更新 |
| `rust/agent-runtime-overlay/` 上游派生关系 | overlay 是受控叠加，不得就地分叉 |
| `assistant_runtime_*` 审计链的 `ON DELETE RESTRICT` | 删除必须墓碑化 |
| `agent-plugins/`、`skills/`、`sdk/` 产品资产 | 不是本程序的脚手架 |
| 能力审批策略 | 安全边界 |
| 默认模型、产品性能门槛、真实数据 | 需要用户或产品 owner 明确批准 |

## 4. 每阶段回归基线

| 门禁 | 命令或要求 |
| --- | --- |
| 依赖方向与隔离 | `make test-isolation` |
| Harness 契约 | `make harness-check` |
| 供应链 | `make agent-runtime-source-contract` |
| 退役依赖未回流 | `make runtime-dependency-gate` |
| 四端 SSE 信封 | `make sdk-sse-contract` |
| Python | `uv run --all-packages --extra test pytest -q --no-cov tests packages/*/tests` |
| 真机产品路径 | 按 `docs/harness/runtime-and-secrets.md` 执行 live Playwright，基线不少于 141 passed、0 failed，skip 不得超过具名 allowlist |

只运行命令不证明产品行为；采用新部署单元、模型路径或数据迁移时必须补真实运行 transcript、HTTP/事件 receipt、资源曲线或数据库前后指纹。

## 5. Review 与证据

- PPR-00：性能/统计方法评审。
- PPR-01：供应链和基础镜像安全评审。
- PPR-02～05：架构评审；涉及鉴权、配额、模型调用或跨服务身份时加安全评审。
- PPR-06：claim/lease、检索行为和任何原生边界评审。
- PPR-07：数据库迁移评审和用户批准。
- PPR-08：产品/Eval 评审；更改默认模型或门槛需用户批准。
- PPR-09：发布、安全和回滚证据评审。

实现者不能批准自己的 phase。每阶段证据必须包含：变更前基线、预先固定的门槛、原始 receipt 指针、结果、取消项、回滚方式和独立 review 结论。
