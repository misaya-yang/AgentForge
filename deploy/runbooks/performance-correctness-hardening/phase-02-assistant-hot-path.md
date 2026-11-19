# PCH-02 Assistant Hot Path

Scope: H8 memory, trace batching, context/digest reuse, KB SSE payloads, Code Executor and MCP.

Required gates:

- Before/after call counts and elapsed-time benchmark for each changed hot path.
- Assistant golden/eval and runtime suites.
- Resume sequence/durability tests for trace/checkpoint changes.
- Real tool/MCP receipt after current Compose hot update.

No optimization passes on estimated savings alone.
