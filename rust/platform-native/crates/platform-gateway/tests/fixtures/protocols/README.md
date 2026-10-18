# Pinned protocol fixtures

`mcp-2026-07-28.json` is the official revision schema. `a2a-v1.0.0.proto`
is the official `a2aproject/A2A` tag `v1.0.0` specification/a2a.proto
(blob `184a555b4b45f281004d534850eb582b6e1a7dcd`, SHA256
`4b74c0baa923ae0acb55474e548f1d6e5d3f83b80d757b65f8bf3e99a3c2257f`).
The official source was obtained by the primary agent; no generator runs at build.

`a2a-1.0-profile.json` exercises the supported HTTP+JSON text profile and actual
managed-turn state mapping. Unsupported media, task-ID continuation and legacy
v0.3 wire forms fail explicitly. Streaming/push are advertised false. These
fixtures are unit evidence; external peer and MCP reference-client conformance
remain separate integration acceptance.

`a2a-task-authority-v1.json` is a test-only Node Ed25519 compact-JWS fixture
(raw body, exact claims, public key, fixed clock and deterministic test seed).
Native tests compare the actual egress request builder byte-for-byte; the
external adapter verifies the same fixture with its real contract/owner store.
The fixed seed is public test material and must never be a deployment key.
