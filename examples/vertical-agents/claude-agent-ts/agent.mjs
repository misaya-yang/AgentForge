import { query } from "@anthropic-ai/claude-agent-sdk";

export async function answer(prompt) {
  const live = process.env.LIVE === "1";
  const messages = query({
    prompt,
    options: {
      model: "claude-sonnet-5",
      permissionMode: "dontAsk",
      ...(live
        ? {}
        : {
            pathToClaudeCodeExecutable: new URL("./mock-claude.mjs", import.meta.url)
              .pathname,
          }),
      mcpServers: {
        platform: {
          type: "http",
          url: process.env.MCP_URL,
          headers: { Authorization: `Bearer ${process.env.API_KEY}` },
        },
      },
      env: {
        ...process.env,
        ANTHROPIC_BASE_URL: process.env.BASE_URL,
        ANTHROPIC_API_KEY: process.env.API_KEY,
      },
    },
  });
  let text = "";
  for await (const message of messages) {
    if (message.type === "result" && message.subtype === "success") {
      text = message.result;
    }
  }
  return text;
}
