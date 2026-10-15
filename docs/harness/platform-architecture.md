# Platform Architecture

> The product law this platform is built on. `architecture.md` governs how the *code* is
> arranged; this file governs how the *product* is arranged — what may be added where, and why
> adding a product form or a capability must never require changing the kernel.
>
> Read this before proposing a new surface, a new extension mechanism, or a new field on
> `AgentSpec`.

**Schema:** `harness/platform-architecture/v2`

---

## 1. The five product layers

```
┌──────────────────────────────────────────────────────────────┐
│ SURFACES — pluggable clients                                 │
│ console · embed widget · SDKs · desktop · mobile ·           │
│ enterprise channels (Feishu/DingTalk/Slack/Teams) · ACP/IDE  │
└───────────────────────────┬──────────────────────────────────┘
                            │
┌───────────────────────────┴──────────────────────────────────┐
│ CONTRACT — the only thing surfaces and the kernel share      │
│ AgentSpec (definition) · event protocol · runtime API · SDK  │
└───────────────────────────┬──────────────────────────────────┘
                            │
┌───────────────────────────┴──────────────────────────────────┐
│ KERNEL — one agent loop                                      │
│ AI Assistant  = the default AgentSpec instance               │
│ Studio Agent  = a fork of it with changed fields             │
│ Subagent      = the same type with mode = subagent           │
└───────────────────────────┬──────────────────────────────────┘
                            │
┌───────────────────────────┴──────────────────────────────────┐
│ EXTENSIONS — pluggable capability                            │
│ Skill · Agent Plugin · MCP · Connector · Knowledge Base      │
└───────────────────────────┬──────────────────────────────────┘
                            │
┌───────────────────────────┴──────────────────────────────────┐
│ GATEWAY — model routing · identity · quota · billing ·       │
│ agent-framework adapters (LangGraph today, others later)     │
└──────────────────────────────────────────────────────────────┘
```

## 2. The five laws

**L1. The assistant is not special.**
The first-party AI Assistant is the *default* `AgentSpec` instance, not a privileged code path.
Anything the assistant can do must be expressible in `AgentSpec`. A capability the assistant has
and `AgentSpec` cannot declare is a platform gap, not an assistant feature — fix the schema, do
not add a back door. Vertical customization in Agent Studio means forking the default spec and
changing fields, so a builder's starting point is the best assistant we ship.

**L2. Surfaces multiply only through the contract.**
A new product form is a new client of the contract layer. If adding one requires touching the
kernel, the contract is incomplete — fix the contract. Our own console is a client like any
other; it must consume the same public runtime contract as an embedded widget, because being our
own first consumer is the only reliable way to keep the contract honest.

**L3. The core stays lean.**
Optional capability ships as an extension. The bar for adding anything to the kernel is
intentionally high. Domain logic in a shared package is the failure mode this law exists to
prevent — see §4 for the admission table.

**L4. Capabilities are discovered; policy is declared.**
The agent finds tools at runtime through discovery. What an agent is *allowed* to do is declared
up front. `permissions` therefore expresses the boundary, and static `capabilities` bindings
express only what must be *frozen* — see §3.

**L5. The gateway owns control policy, not execution semantics.**
Identity, tenant policy, model access, quota, billing, provider adaptation, and immutable launch /
snapshot signing live below the kernel. The Gateway may resolve what an Agent is allowed to run; it
must not own model/tool iteration, compaction, cancellation state, approval state transitions, or
event ordering. A provider- or framework-shaped concept leaking into `AgentSpec` remains a layering
defect.

## 3. Permissions and capabilities coexist — on purpose

Two mechanisms, two jobs. This is a deliberate decision, not a transitional state.

| | `permissions` (Ruleset) | `capabilities` (versioned bindings) |
| --- | --- | --- |
| Answers | "What is this agent allowed to touch?" | "Which exact resource version must not move under it?" |
| Shape | Allow/deny rules over tool and resource patterns | `resource_id` + `resource_version` + `schema_hash` |
| Platform adds a new tool | Agent inherits it if the rules allow | Agent does not see it until rebound |
| Reproducibility | Behaviour may improve over time | Byte-stable, auditable |
| Use for | The default boundary of every agent | Pinning a KB dataset version, a skill version, a certified workflow |

The rule of thumb: **declare `permissions` always; add a `capabilities` pin only where drift is
unacceptable.** An agent that pins everything is frozen and will fall behind the platform; an
agent that pins nothing cannot promise a regulated customer a reproducible answer. Both failures
are real, which is why both mechanisms exist.

This choice also settles the upgrade contract: an agent forked from the default spec inherits
platform improvements through `permissions`, and holds still exactly where it pinned.

## 4. What goes in the kernel and what ships as an extension

| If it is… | It belongs in |
| --- | --- |
| Needed unconditionally by every agent (loop, context, approval, memory lifecycle, tracing) | **Kernel** |
| Domain knowledge or an artifact type (quiz generation, document generation, diagrams) | **Agent Plugin** |
| A way of working expressed in prompt (how to write a weekly report, how to run a review) | **Skill** |
| Data and actions in a third-party system | **Connector** |
| A tool someone else already implemented | **MCP** |
| Corpus | **Knowledge Base** |
| A new way for a human or client program to reach an agent | **Surface** (see §5) |

Business logic must never land in `packages/ai-gateway-core/`. That package is imported by every
Python service; a domain concept there forces the gateway and the knowledge service to carry it too.

## 5. Adding a surface

Every surface speaks the contract layer and nothing else. A surface may not import kernel code,
may not add a bespoke endpoint, and may not require a kernel change.

Optional external reference implementations are summarized in the in-repository
[`four-OSS comparison`](../research/ai-assistant-harness-4oss-comparison-research-2026-08.md).
Their local checkouts are not an execution prerequisite and must not be treated as system-of-record
input:

| Project | What to look at | Why |
| --- | --- | --- |
| `opencode` | `packages/schema/src/agent.ts` (38 lines), `packages/{protocol,sdk}` | Agent as data; `mode: subagent \| primary \| all`; policy over capability lists |
| `openclaw` | `VISION.md`, `extensions/` (40 channels), `apps/{android,ios,macos}` | What a lean core plus a high admission bar buys you |
| `Hermes_agent` | `gateway/platforms/ADDING_A_PLATFORM.md`, `acp_adapter/`, `acp_registry/agent.json` | A documented seam is the proof a contract exists; ACP publishes one agent to every editor |
| `grok-build` | `crates/` | Proportional responses; discovery tools always present |

For this product's positioning, enterprise channels (Feishu, DingTalk, WeCom, Teams) rank far
above consumer messaging, and [ACP](https://agentclientprotocol.com) is the cheapest surface we
can add — one adapter reaches every ACP-compatible editor.

## 6. How to change this file

This document states intent that should outlive any one program. Change it when a law changes,
not when an implementation does. The prior `agent-contract-unification` scaffold is superseded by
the completed Rust single-kernel cutover. The next queued conformance work is
[`platform-architecture-convergence-prd-2026-08.md`](../plans/platform-architecture-convergence-prd-2026-08.md);
it becomes executable only after RAG acceptance and after exactly one authoritative program is
created or adopted. This file remains authoritative for product law, not execution status.
