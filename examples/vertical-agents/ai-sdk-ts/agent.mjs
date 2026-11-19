import { createOpenAI } from "@ai-sdk/openai";
import { generateText, jsonSchema, stepCountIs, tool } from "ai";
import { config, platformTools, resetEvidence } from "../runtime.mjs";

export async function answer(prompt) {
  resetEvidence();
  const c = config();
  const listed = await platformTools();
  const tools = Object.fromEntries(listed.map(spec => [spec.name, tool({
    description: spec.description,
    inputSchema: jsonSchema(spec.inputSchema),
    execute: spec.execute,
  })]));
  const provider = createOpenAI({ baseURL: c.baseURL, apiKey: c.apiKey });
  const generated = await generateText({
    model: provider.chat(c.model), tools, stopWhen: stepCountIs(8),
    system: "Answer using platform space tools. Cite the exact item_id returned by tools in [item_id] brackets.", prompt,
  });
  return generated.text;
}
