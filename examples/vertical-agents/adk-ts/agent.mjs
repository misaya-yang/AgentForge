import { BaseLlm, InMemoryRunner, LlmAgent, MCPToolset } from "@google/adk";

class PlatformLlm extends BaseLlm {
  constructor() {
    super({ model: "platform-agent" });
  }
  async *generateContentAsync() {
    const response = await fetch(`${process.env.BASE_URL}/chat/completions`, {
      method: "POST",
      headers: {
        authorization: `Bearer ${process.env.API_KEY}`,
        "content-type": "application/json",
      },
      body: JSON.stringify({
        model: this.model,
        messages: [{ role: "user", content: "Read the cited page." }],
      }),
    });
    if (!response.ok) {
      throw new Error(await response.text());
    }
    const body = await response.json();
    yield {
      content: {
        role: "model",
        parts: [{ text: body.choices[0].message.content }],
      },
      turnComplete: true,
    };
  }
}

export async function answer(prompt) {
  const agent = new LlmAgent({
    name: "platform",
    model: new PlatformLlm(),
    instruction: "Cite the item id returned by the space tool.",
    tools: [
      new MCPToolset({
        type: "StreamableHTTPConnectionParams",
        url: process.env.MCP_URL,
      }),
    ],
  });
  const runner = new InMemoryRunner({ agent });
  let text = "";
  for await (const event of runner.runEphemeral({
    userId: "user",
    newMessage: { role: "user", parts: [{ text: prompt }] },
  })) {
    for (const part of event.content?.parts ?? []) {
      if (part.text) text += part.text;
    }
  }
  return text;
}
