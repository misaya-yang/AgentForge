# ai-sdk-ts

Vercel AI SDK `ai` 7.0.172。`generateText` 走 `BASE_URL` 上的 OpenAI 兼容接口，`readUIMessageStream` 读取 `BASE_URL/api/v1/ai-sdk/chat` 的 UI Message Stream，工具目录来自 `MCP_URL`。

`make test` 默认打本地 mock。`LIVE=1` 时使用已经导出的 `BASE_URL`、`API_KEY`、`MCP_URL`。
