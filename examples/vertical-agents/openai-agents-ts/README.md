# openai-agents-ts

`@openai/agents` 0.22.0。只读取 `BASE_URL`、`API_KEY`、`MCP_URL`：模型走 Chat Completions，工具走 MCP Streamable HTTP。

`make test` 默认打本地 mock。`LIVE=1` 时使用已经导出的三个变量访问真实网关。
