# Phase 06 - Adding a surface or capability is documented and gated

- PHASE_ID: ACU-06
- FEATURE_ID: ACU-F007
- DEPENDS_ON: ACU-04
- UNLOCKS: none

## Outcome

Two documents make the contract usable by someone who did not build it: one for adding a product
form, one for adding a capability. A mechanical gate keeps domain logic out of the shared core
package, so the admission rule survives after the people who agreed to it have moved on.

A documented seam is the proof that a contract exists. `Hermes_agent/gateway/platforms/ADDING_A_PLATFORM.md`
is why that project can carry nine channels; `openclaw`'s stated bar — optional capability ships as
a plugin, and the bar for adding to core is intentionally high — is why it carries forty
extensions across `extensions/`. This repository has four extension mechanisms already (Skill,
Agent Plugin, MCP, Connector) and no written admission rule, which is how quiz and exam domain
logic ended up inside `packages/ai-gateway-core/`.

## Scope

In:
- `docs/harness/adding-a-surface.md`: what a surface may assume, which contract endpoints and
  events it consumes, how authentication and channel policy apply, and a worked minimal example.
- `docs/harness/adding-a-capability.md`: the admission table from
  `docs/harness/platform-architecture.md` §4, plus how to register each of the four mechanisms.
- A `make harness-check` rule that fails when a domain module appears under
  `packages/ai-gateway-core/` — driven by a declared list in `harness.yml`, not hardcoded.
- Add both documents to `harness.yml` `required_docs` so they cannot silently disappear.

Out:
- Actually building a Feishu, ACP, or desktop surface. This phase makes them cheap; it does not
  ship them.
- Moving the existing quiz and exam code. That is its own program, already in flight.
- Refactoring any extension mechanism.

## Done when

- [ ] Both documents exist, are linked from `docs/README.md`, and contain a worked example rather than only prose.
- [ ] Following `adding-a-surface.md` is enough to register a trivial no-op surface without touching kernel code.
- [ ] Following `adding-a-capability.md` is enough to register a trivial capability through each of the four mechanisms, or the document states plainly which are not yet self-service.
- [ ] Adding a domain module under `packages/ai-gateway-core/` makes `make harness-check` fail with a message naming the admission rule.
- [ ] Removing it makes `make harness-check` pass.
- [ ] The allowed and forbidden module lists live in `harness.yml`, not in the checker source.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Clean tree | `make harness-check` | Both documents exist, are linked, and the admission gate is green |
| Gate fires | `uv run --all-packages --extra test pytest -q --no-cov tests/scripts/test_harness_core_admission.py` | Planting a domain module under `packages/ai-gateway-core/` fails with a message naming the rule |
| Self-service | Follow `docs/harness/adding-a-surface.md` to register a no-op surface | Registration touches no kernel code |

## Stop or confirm

- Adding any existing `packages/ai-gateway-core/` module to a permanent exception list; an exception that is never removed repeals the rule quietly.
- Report each proposed exception with its reason and an owner.
