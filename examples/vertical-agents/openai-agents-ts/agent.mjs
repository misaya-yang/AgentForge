import {
  Agent,
  MCPServerStreamableHttp,
  run,
  setOpenAIAPI,
  setTracingDisabled,
} from "@openai/agents";

export async function answer(prompt) {
  setTracingDisabled(true);
  setOpenAIAPI("chat_completions");
  process.env.OPENAI_BASE_URL = process.env.BASE_URL;
  process.env.OPENAI_API_KEY = process.env.API_KEY;
  const mcp = new MCPServerStreamableHttp({
    url: process.env.MCP_URL,
    name: "platform",
  });
  await mcp.connect();
  try {
    const agent = new Agent({
      name: "platform",
      instructions: "Cite the item id returned by the space tool.",
      mcpServers: [mcp],
    });
    const result = await run(agent, prompt);
    return String(result.finalOutput ?? "");
  } finally {
    await mcp.close();
  }
}
