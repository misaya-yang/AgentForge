# ATE-02 Assistant Trace Capture Plan

Status: planned

## Scope

Implement ATE-F003 only: AI Assistant trace capture for non-streaming chat and streaming AgentLoop flows. No Eval frontend, LangGraph proxy, RAG trace family, migration, deployment, or production data work.

## Todo

1. Add `apps/assistant-service/src/assistant_service/core/trace_writer.py` with a bounded, best-effort background writer.
2. Wire `AssistantService` to create or accept the trace writer and capture non-stream chat root, spans, ordered events, terminal status, and redacted errors.
3. Wire `AgentLoop` to capture streaming lifecycle/events without awaiting trace persistence; keep `ExecutionGateway.finish_run` before trace finish submission.
4. Add focused tests for trace root/span/event SQL, redaction/bounding, duplicate terminal behavior, persistence failure tolerance, non-stream final-response latency, streaming first-event latency, and run-status latency.
5. Run the ATE-02 validation commands, then update the harness runtime files and independent critic artifact.

## Latency Guard

Trace writer methods must be synchronous submit calls only: they may create bounded background tasks but must not await database writes. Tests will use a blocked database fake to prove first stream event, final non-stream response, and run status completion do not wait for trace persistence.

## Minimal Change

Reuse existing tenant/user/session/run context, `AgentLoop` lifecycle events, existing redaction posture, and the ATE-01 table contract. Store redacted bounded previews only.
