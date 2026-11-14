import { BaseLlm, InMemoryRunner, LlmAgent } from "@google/adk";
import { chatWithTools, config, resetEvidence } from "../runtime.mjs";

class PlatformLlm extends BaseLlm {
  constructor(prompt) { super({ model: config().model }); this.prompt = prompt; }
  async *generateContentAsync() {
    const text = await chatWithTools(this.prompt);
    yield { content: { role: "model", parts: [{ text }] }, turnComplete: true };
  }
}
export async function answer(prompt) {
  resetEvidence();
  const agent = new LlmAgent({ name: "platform", model: new PlatformLlm(prompt), instruction: "Answer from platform tools and cite the item id." });
  const runner = new InMemoryRunner({ agent });
  let text = "";
  for await (const event of runner.runEphemeral({ userId: "user", newMessage: { role: "user", parts: [{ text: prompt }] } })) {
    for (const part of event.content?.parts ?? []) if (part.text) text += part.text;
  }
  return text;
}
