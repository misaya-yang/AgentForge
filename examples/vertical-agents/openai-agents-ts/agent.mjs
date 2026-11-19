import OpenAI from "openai";
import { config, recordTool, resetEvidence } from "../runtime.mjs";
import {
  Agent,
  MCPServerStreamableHttp,
  run,
  setOpenAIAPI,
  setTracingDisabled,
  setDefaultOpenAIClient,
} from "@openai/agents";

export async function answer(prompt) {
  setTracingDisabled(true);
  setOpenAIAPI("chat_completions");
  const c = config();
  resetEvidence();
  setDefaultOpenAIClient(new OpenAI({ baseURL: c.baseURL, apiKey: c.apiKey }));
  const mcp = new MCPServerStreamableHttp({
    url: c.mcpURL,
    requestInit: { headers: { Authorization: `Bearer ${c.apiKey}` } },
    name: "platform",
  });
  const callTool = mcp.callTool.bind(mcp);
  mcp.callTool = async (...args) => recordTool(args[0], await callTool(...args));
  await mcp.connect();
  try {
    const agent = new Agent({
      name: "platform",
      model: c.model,
      modelSettings: { parallelToolCalls: false },
      instructions: "Cite the item id returned by the space tool.",
      mcpServers: [mcp],
    });
    const result = await run(agent, prompt);
    return String(result.finalOutput ?? "");
  } finally {
    await mcp.close();
  }
}
