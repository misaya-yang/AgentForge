# claude-agent-ts

`@anthropic-ai/claude-agent-sdk` 0.3.323 的 `query()`。模型请求发到 `BASE_URL/v1/messages`，MCP 配置为 `MCP_URL`。

本机没有 Claude Code 可执行文件。mock 模式把 `pathToClaudeCodeExecutable` 指到 `mock-claude.mjs`，它只做一件事：用 `API_KEY` 调 `/v1/messages`，再把文本交回 SDK。`LIVE=1` 时不替换可执行文件，由本机的 Claude Code 走真实网关。
