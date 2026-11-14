# Claude Agent SDK vertical agent

Uses the platform's OpenAI-compatible **`/v1`** model gateway and local platform **`/mcp`** tools. No provider credential is passed to the example. Read-only space tools are exposed; writes use the platform's separate approval flow.

Mock (one command; requires Node 24+ and npm):

```sh
make test
```

LIVE (one command after configuring the gateway and exporting a **platform** key as `OPENAI_API_KEY`):

```sh
MODEL=qwen3.8-flash OPENAI_BASE_URL=http://127.0.0.1:8080/v1 MCP_URL=http://127.0.0.1:8080/mcp QUERY=platform LIVE=1 make test
```

The gateway's upstream must be configured as `https://dashscope-intl.aliyuncs.com/compatible-mode/v1` with model `qwen3.8-flash`. The gateway reads the Aliyun credential only from `DASHSCOPE_API_KEY` in its environment. Do not use that provider key as `OPENAI_API_KEY`, store keys in files, or point this example directly at DashScope. Change the port to match your gateway.

`MODEL`, `OPENAI_BASE_URL`, `OPENAI_API_KEY`, and `MCP_URL` configure every request. Legacy `BASE_URL` / `API_KEY` aliases remain supported. `QUERY` selects an existing, readable space page; alternatively set `ITEM_ID` or `PROMPT`. LIVE fails if no tool was actually executed or if the final answer does not cite an item ID returned by that tool. Seed a page using your normal platform UI before running. Mock retains the deterministic `cited item-7` assertion. No services are provisioned by this example.

Claude Agent SDK drives a Claude Code subprocess and speaks **Anthropic Messages**; it cannot switch to an OpenAI Chat Completions endpoint by changing its base URL. The default LIVE command above therefore uses the explicitly named OpenAI-compatible tool loop in `../runtime.mjs` with Qwen; it does **not** claim that Qwen ran inside Claude Agent SDK. Mock mode still exercises the official SDK/subprocess protocol. To test the native SDK through the platform's `/v1/messages` translation, run:

```sh
CLAUDE_TRANSPORT=anthropic ANTHROPIC_BASE_URL=http://127.0.0.1:8080 ANTHROPIC_MODEL="$MODEL" LIVE=1 make test
```

This optional path requires an installed Claude Code executable, a gateway Messages translation enabled for the chosen model, and support for the SDK's complete tool protocol. The same tool/citation assertions apply.
