# adk-ts

`@google/adk` 2.5.0 的 `LlmAgent` 与 `MCPToolset`。模型请求发到 `BASE_URL/chat/completions`，工具发现走 `MCP_URL`。

`make test` 默认打本地 mock。`LIVE=1` 时使用已经导出的 `BASE_URL`、`API_KEY`、`MCP_URL`。
