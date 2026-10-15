# Handoff

- Active phase: CHR-05
- Active feature: CHR-F006
- Status: working
- Completed: CHR-03 and CHR-04 code contracts are closed. The candidate now has tenant/revision-bound read-only capability discovery, strict dynamic-tool schemas, approval and dispatch fencing, terminal tool pairing, native interrupt, cancelled Run/lease closure, V2 Web/SDK projection, and V1 run-status compatibility.
- Evidence: controlled fork `c84c011f403a` is source-locked in both OCI artifacts. Focused Python returned 80/80; Assistant runtime 375/375 plus golden gate; Agent Eval 107/107; Runtime write gate 26/26; live PostgreSQL ThreadStore passed. Migration 094 now resolves authoritative `assistant.*` legacy sources from the Gateway-owned import function, rejects source drift and unpaired tool history atomically, blocks resumable Run/approval states, emits paired Agent response Items, and retains only a hash of approval arguments; its PostgreSQL contract returned 13/13 and a real split-schema transaction with caller `search_path=public` produced `message,function_call,function_call_output,message` plus one approval receipt before rollback. Authenticated browser V2 returned `200` for arithmetic (first token 3.74s), produced a 1,568-character Transformer explanation (first token 3.62s), cancelled a long novel, then answered the same-session follow-up. A separate final Qwen UI probe returned the correct 9×9 explanation, retained `think:minimal · 1 步`, reached first token in 4.30s, and produced no new browser warnings/errors. All nine Compose services are healthy; total observed container memory was about 838 MiB and the Rust Runtime used 56 MiB.
- Next action: run the time-bound production canary and rollback rehearsal before any full cutover or Python loop deletion.
- Environment incident: Docker recovered after deleting only the reproducible Rust incremental cache and performing a normal Docker Desktop restart. No prune, volume deletion, Docker reset, or source deletion occurred. Current local Compose ownership is the migration worktree.
- Blockers: none
- Confirmation: none
- Decision: merge the candidate code; keep production canary and legacy-loop deletion gates closed.

Keep this as the latest checkpoint. Use Git history for older handoffs.
