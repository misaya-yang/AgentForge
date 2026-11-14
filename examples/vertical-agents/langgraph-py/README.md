# langgraph-py

LangChain 1.x `create_agent`（本环境 PyPI 最新 1.x 是 1.5.0；PRD 附录写的 1.6.5 尚未发布）读取三个环境变量：

- `BASE_URL`：OpenAI 兼容模型根路径，例如 `http://127.0.0.1:8080/v1`
- `API_KEY`：平台 API key
- `MCP_URL`：平台 MCP Streamable HTTP 端点

`make test` 在 mock 模式下拉起本地模型与 MCP，断言回答包含 `cited item-7`。`LIVE=1` 时不启 mock，直接打这三个变量指向的真实网关（由本地运行，不计入本仓库的 SKIPPED）。
