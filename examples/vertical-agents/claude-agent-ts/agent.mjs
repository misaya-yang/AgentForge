import { chatWithTools, config, recordTool, resetEvidence } from "../runtime.mjs";
import { query } from "@anthropic-ai/claude-agent-sdk";

export async function answer(prompt) {
  const live = process.env.LIVE === "1";
  const c = config();
  resetEvidence();
  if (live && process.env.CLAUDE_TRANSPORT !== "anthropic") return chatWithTools(prompt);
  const messages = query({
    prompt,
    options: {
      model: process.env.ANTHROPIC_MODEL || c.model,
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
          url: c.mcpURL,
          headers: { Authorization: `Bearer ${c.apiKey}` },
        },
      },
      env: {
        ...process.env,
        ANTHROPIC_BASE_URL: process.env.ANTHROPIC_BASE_URL || new URL(c.baseURL).origin,
        ANTHROPIC_API_KEY: c.apiKey,
      },
    },
  });
  let text = "";
  for await (const message of messages) {
    if (message.type === "user") {
      for (const part of message.message?.content || []) if (part.type === "tool_result") recordTool("claude_mcp", { content: [{ text: typeof part.content === "string" ? part.content : JSON.stringify(part.content) }], isError: part.is_error });
    }
    if (message.type === "result" && message.subtype === "success") {
      text = message.result;
    }
  }
  return text;
}
