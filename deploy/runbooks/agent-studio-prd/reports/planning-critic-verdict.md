# Agent Studio Planning Critic Verdict

**Review scope:** Product requirements, architecture, UX, source packet, Feature Oracle, ten Phase contracts, runtime artifacts and report templates  
**Independent critic:** `/root/agent_studio_prd_critic`  
**Date:** 2026-07-18  
**Final verdict:** `approved`

## Initial Review

The first independent review returned `changes_requested`. It identified seven material gaps that had to be resolved before this planning harness could be considered ready.

| ID | Severity | Initial gap | Durable resolution |
| --- | --- | --- | --- |
| PC-01 | critical | Gateway/Assistant runtime trust authority and forged Agent fields were ambiguous. | `architecture-contract.md`, AS-02 and AS-F003 now make Gateway the sole external resolver; a signed, expiring, replay-protected Envelope carries the canonical Snapshot and binds identity, session, request body, spec, time and nonce. Assistant recalculates hashes, atomically consumes nonce and fails closed on mutation, forgery or replay. |
| PC-02 | critical | MCP credential principal semantics were undefined. | Product/architecture contracts and AS-03 distinguish `service_account` from `user_delegated`, persist grant owner/scope/audience/expiry/revoke state, deny cross-user fallback, and deny anonymous/public use unless an Admin explicitly authorizes a specified read-only service-account tool/channel. Existing Connectors share the contract. |
| PC-03 | high | The planned Embed conflicted with production Nginx/Helm anti-framing headers. | Hosted stays anti-framed; `/embed/agents/:publicId` is a dedicated dynamic document with Publication-derived `frame-ancestors` and no SAMEORIGIN XFO. AS-07 owns `web/nginx.conf`, Helm configuration, contract tests and a required built-image response-header smoke. |
| PC-04 | high | Tenant-owned child tables relied too heavily on repository filtering. | Architecture and AS-01 require explicit `tenant_id`, parent composite unique keys and composite foreign keys for every tenant-owned child; migration tests must prove Tenant A children cannot reference Tenant B parents. |
| PC-05 | high | User Skill frontmatter could select arbitrary executable entrypoints. | V1 tenant uploads are instruction-only, reject user-controlled source/entrypoint schemes, and normalize to server-owned `db://` versions. Platform bundled executable Skills are a separate deployment-controlled artifact class. |
| PC-06 | high | A live Knowledge revision fingerprint was described too close to deterministic replay. | Product/architecture/AS-04 now use normalized Draft/Version Dataset bindings and explicitly define the fingerprint as run provenance and drift evidence, not a historical content snapshot or replay guarantee. |
| PC-07 | high | Phase dependencies were needlessly serial and the implementation Phase also owned the terminal release decision. | AS-03 and AS-04 both depend on AS-02 and may run in parallel; AS-05 waits for both. AS-08 owns operations and the versioned aggregate manifest. AS-09 is a no-feature same-build whole-demand gate; the actor cannot pre-claim the completion gate. |

## Re-review Evidence

- The critic re-opened the actual files rather than relying on the correction summary.
- Strict harness validation reported ten phases, zero errors, zero warnings and quality score 100.
- The re-review found no remaining material product, architecture, security, delivery or harness gap.

## Final Verdict

`approved`

The Runtime Envelope boundary, composite tenant constraints, MCP/Connector credential principals, instruction-only Skill policy, Knowledge provenance boundary, production Embed headers and independent AS-09 terminal gate all have explicit implementation paths and mandatory validation. AS-00 target-branch rebaselining and later external approvals remain intentional execution gates, not planning omissions.
