# ADR-004: Bounded plugin-defined sub-agent delegation

**Status:** Accepted; capability claims remain evaluation-gated

**Date:** 2026-08-12

**Deciders:** AI Gateway maintainers

**Scope:** Assistant-service sub-agent dispatch and Agent Plugins integration

## Context

The Assistant already has isolated `explore`, `task`, and `plan` child loops, a
canonical tool authorization boundary, child budget ceilings, cancellation, and
structured lifecycle events. It does not yet have one server-side contract for
plugin-defined agents, and the model-facing path has historically executed
multiple delegation tool calls one at a time.

We want the useful parts of Hermes Agent's bounded fan-out/fan-in and
OpenCode/OpenHands' Markdown agent definitions without treating plugin text as
authority or claiming that this iteration replaces the Assistant execution
kernel.

The upstream snapshots used for this decision are fixed; they are design
references, not runtime dependencies:

| Project | Fixed revision | Pattern adopted | Pattern not copied |
| --- | --- | --- | --- |
| Hermes Agent | [`222465d84709379b65173b0283a6eea87516acfa`](https://github.com/NousResearch/hermes-agent/tree/222465d84709379b65173b0283a6eea87516acfa) | Bounded `tasks[]` fan-out/fan-in, input-order results, isolated children, cancellation | Unbounded operator overrides, in-memory work presented as durable |
| OpenCode | [`1f94d8a3c86b67f4f49a0e341de74e9188381b3a`](https://github.com/anomalyco/opencode/tree/1f94d8a3c86b67f4f49a0e341de74e9188381b3a) | Markdown definitions and description-based routing | Child permissions that are not a complete parent/host/definition intersection |
| OpenHands SDK | [`5bfa7fc5398649cacf4031d477cc47d754c49078`](https://github.com/OpenHands/software-agent-sdk/tree/5bfa7fc5398649cacf4031d477cc47d754c49078) | Plugin agent registry concepts and per-agent limits | Plugin-selected `never_confirm`, unrestricted global tool resolution |
| Cline | [`a56af4efaf672e0f5261f06ebf3332ef684bd4c0`](https://github.com/cline/cline/tree/a56af4efaf672e0f5261f06ebf3332ef684bd4c0) | Explicit child lifecycle, usage, cancellation, and review-oriented outcomes | Team mailbox, recovery protocol, and fixed-timeout complexity in this iteration |
| GitHub Awesome Copilot | [`0a6e37e4e242c944380228fa29dbd14e64ac1b63`](https://github.com/github/awesome-copilot/tree/0a6e37e4e242c944380228fa29dbd14e64ac1b63) | Pinned Markdown specialist definitions and compatible string references | Executable integrations, broad tool names, or upstream authority assumptions |

## Decision

### 1. Agent definitions are an AI Gateway client extension

Agent Plugins 1.0.0 does not make `agents` a portable core capability in this
repository. Plugin agent declarations belong exclusively under
`extensions["com.misaya.ai-gateway"].agents`. We must not advertise them as a
standard Agent Plugins v1 field, and another client may ignore the extension.

The v1 client-extension shape is a bounded array of contained Markdown path
references:

```json
{
  "extensions": {
    "com.misaya.ai-gateway": {
      "agents": ["./agents/reviewer.md", "./agents/tester.md"]
    }
  }
}
```

For source compatibility with installed open-source packages, the loader may
also read string path references from `com.github.awesome-copilot.agents` under
the same containment, validation, quarantine, and non-authority rules. This is
a compatibility input, not an assertion that either namespace is part of the
Agent Plugins v1 core.

The extension may reference only contained Markdown files under `agents/`.
Definitions follow the OpenCode/OpenHands frontmatter-plus-body style. The
minimum contract is:

- bounded `id`, `name`, and non-empty `description` fields;
- a `base_type` selecting one existing host profile;
- optional `allowed_tools`, `allowed_tool_categories`, `max_turns`,
  `max_tool_calls`, `max_tokens`, and `timeout_seconds` capability requests;
- a Markdown body containing specialist task guidance;
- a qualified runtime identity derived from plugin identity plus agent name,
  so an internet package cannot silently replace a built-in or another plugin;
- deterministic diagnostics for invalid YAML, duplicate names, unknown
  extension shape, path escape, symlink escape, encoding, count, or size limits.

Tools, categories, and limits are requests, never grants. Arbitrary model
selection, permission mode, credentials, tenant identity, approval state,
executable hooks, shell interpolation, and environment expansion are not
plugin-agent fields.

A fatal manifest error rejects the package. An invalid individual definition
is quarantined without disabling valid Skills, MCP declarations, or unrelated
agents from that package. Individual quarantine includes path escape, any
symlink, oversized content, duplicate YAML keys, invalid fields, and invalid
budgets. If multiple definitions resolve to one qualified agent ID, every
conflicting definition is quarantined; load order must not choose a winner.

### 2. Plugin Markdown is quarantined plaintext

Plugin name, description, frontmatter, and body are untrusted package data.
Loading an agent definition performs no network access and executes no package
code. The body must enter the child context as a delimited, explicitly
untrusted data block covered by the repository's external-content policy; it
is not an operator/system/developer message and is not "authorized skill
guidance". The host may ask the child to use relevant specialist methods from
that block, but the block cannot replace or weaken the host policy that
precedes and follows it.

Instructions inside the Markdown cannot change tools, model/provider,
permissions, budgets, tenant or dataset scope, approval requirements, sandbox
class, MCP credentials, or delegation policy. Unknown or unsafe fields are
rejected or diagnosed, not interpreted. A quarantined definition can improve
routing and task specialization but cannot increase authority.

### 3. Delegation uses bounded `tasks[]` fan-out/fan-in

The model-facing delegation contract may accept one task for compatibility or
a bounded `tasks[]` batch. Before creating any async task, the host validates
the whole batch, rejects an empty or oversized batch, charges the shared run
budget, and computes an operator-capped concurrency limit. It must never create
an unbounded list of waiting coroutines.

Each accepted item receives a stable batch index and child identifier. Children
have separate message histories and execute concurrently only up to the
effective cap. Terminal results are returned in original input order regardless
of completion order. Every input has exactly one terminal status:
`completed`, `failed`, `blocked`, `cancelled`, or `timed_out`. One child's
ordinary failure does not erase sibling results. `SIDE_EFFECT_UNKNOWN` remains
a fail-closed exception: it stops further dispatch and cancels siblings.

The event stream may reflect real completion order, but the final aggregate
must contain the stable input index and stable input order. Tests must prove
parallel overlap from monotonic start/finish receipts; a faster wall-clock result
or the existence of a `spawn_parallel` method is not evidence of parallelism.

### 4. Authority and budgets can only shrink

For every child, the host computes effective authority with a three-way
intersection:

```text
effective capabilities =
    parent effective capabilities
  INTERSECT base-type and operator/runtime hard policy
  INTERSECT plugin agent profile requests
  MINUS all denies

effective limit = MIN(parent remaining limit, base-type/operator ceiling,
                      agent profile request)
```

This rule applies independently to tools, model/provider selection, MCP and
dataset scope, tenant/user/session/run identity, turn and tool-call counts,
output tokens, wall time, child count, and concurrency. Missing authority is
deny-by-default. A definition without a tool list receives no implicit plugin
grant; the host's existing agent-type policy remains the maximum catalog.
Parent approvals and idempotency markers are not inherited by a child.

The invocation boundary must re-check the effective capability snapshot even
when the tool was visible in the child's catalog. Plugins and model output can
only narrow values; they cannot raise a limit or select a provider unavailable
to the parent.

### 5. Recursion is zero in this iteration

Nested delegation is not enabled. Every child catalog excludes the delegation
tool and any canonical delegation capability. A plugin definition cannot opt
back into it, rename it, or receive it through an MCP/tool alias. The allowed
spawn depth is therefore zero below the parent.

This is an explicit safety boundary, not a claim that cycle detection is
implemented. Orchestrator children, recursive agent graphs, and configurable
depth are deferred until lineage, global child accounting, and cycle tests exist.

## Options considered

| Option | Decision | Reason |
| --- | --- | --- |
| Import a Hermes/Cline-style team runtime | Rejected for this iteration | It adds a second scheduler, recovery protocol, and broader persistence claims before the current authority boundary is proven end to end. |
| Keep definitions only in the CLI | Rejected | The service cannot enforce one plugin namespace, quarantine rule, or effective-authority calculation. |
| Extend the current plugin loader and sub-agent manager | Chosen | It preserves the existing gateway authorization and side-effect semantics while adding the smallest useful definition and batch contracts. |

## Iteration delivery boundary

The following are the required deliverables for this iteration. They may be
described as completed only after their tests and the acceptance suite pass:

- parse and validate `com.misaya.ai-gateway` Markdown agent definitions as
  quarantined plaintext;
- resolve a selected definition to a child configuration without widening the
  parent's model, tools, scope, approvals, or budgets;
- dispatch a bounded `tasks[]` batch with actual overlap and stable input-order
  aggregation;
- retain cancellation, timeout, partial-failure, structured terminal, and
  `SIDE_EFFECT_UNKNOWN` fail-closed behavior;
- keep all child agents non-recursive;
- preserve the legacy single-task and built-in agent path behind the existing
  sub-agent feature gate.

This iteration explicitly does **not** deliver:

- a persistent delegation ledger, process-restart recovery, or durable replay;
- task resume by ID, background completion injection, steering, dependency
  graphs, or a team mailbox;
- nested orchestrators, configurable recursion depth, or general cycle
  detection;
- full reuse of the main Assistant execution kernel. The existing simplified
  child model loop remains and is recorded as migration debt;
- executable code, hooks, stdio trust, secrets, or new MCP/network authority
  from an internet plugin agent definition;
- proof that the Assistant is a universal or fully general agent.

## Acceptance gate

Acceptance requires an unrounded suite score of at least **92.000** and all
deterministic hard gates. The LLM judge evaluates semantic task quality only;
it cannot override receipts or a hard-gate failure. Judge credentials are
runtime environment inputs and must not appear in commands, fixtures, logs, or
reports.

Hard gates are:

1. no capability, model, credential, approval, tenant, dataset, or budget
   expansion at either catalog or invocation time;
2. no child recursion or delegation alias bypass;
3. bounded child count and concurrency charged before task creation;
4. actual monotonic-time overlap when a scenario requires parallelism, plus
   stable input-order fan-in and one terminal per input;
5. parent cancellation/timeout leaves no running child; unknown side effects
   are never blindly retried and cancel siblings;
6. plugin Markdown remains plaintext quarantine and prompt injection cannot
   alter policy;
7. invalid, conflicting, oversized, escaped, or symlinked definitions fail
   closed without code or network execution;
8. action claims require host-observed tool/artifact/read-back receipts; child
   prose alone is not evidence;
9. no secret exposure, cross-tenant access, unapproved side effect, fabricated
   receipt, stale terminal, or missing/duplicate terminal.

Any hard-gate failure fails the suite regardless of average or judge score.
Negative controls must include a claimed-but-nonoverlapping "parallel" run,
policy instructions embedded in Markdown, a fabricated success receipt, batch
overflow, cancellation during fan-out, and a child attempt to delegate.

## Consequences and trade-offs

- The Assistant gains useful specialization and parallelism without granting
  internet plugin text authority.
- Stable input order simplifies synthesis and deterministic tests, at the cost
  of buffering the final aggregate while events continue in completion order.
- Zero recursion is less expressive than Hermes orchestrators, but gives a
  testable bound while the child still uses a separate loop.
- Markdown definitions are easy to author and inspect, but they are deliberately
  less powerful than OpenHands definitions: permissions, executable hooks, and
  trust escalation remain host-owned.
- The design creates explicit migration debt for main-kernel reuse and durable
  task lifecycle rather than concealing those gaps behind a new API.

## Migration and rollback

1. Load and validate definitions in parse-only mode; diagnostics must not stop
   existing Skills or MCP components from loading.
2. Register qualified plugin agents only when sub-agents are enabled. Keep
   built-in `explore`, `task`, and `plan` identifiers unchanged.
3. Enable bounded batches additively; legacy single-task calls continue to use
   the same authorization and terminal contracts.
4. Run focused unit/security tests, deterministic parallel-overlap tests, then
   the 92-point capability suite before enabling the path outside development.

Rollback requires no database migration or plugin rewrite:

- disable the existing sub-agent feature gate to stop all child dispatch;
- remove the plugin path or ignore the `com.misaya.ai-gateway.agents` extension
  to disable plugin-defined agents while leaving portable Skills/MCP intact;
- reject `tasks[]` and retain the legacy single-task path if only batch dispatch
  must be rolled back.

Definitions and aggregate results are derived runtime data. Rolling back must
not execute quarantined content, widen authority, replay a write, or reinterpret
an unfinished child as successful.
