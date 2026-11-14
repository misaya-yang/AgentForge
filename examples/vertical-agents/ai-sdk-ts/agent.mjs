import { createOpenAI } from "@ai-sdk/openai";
import { generateText, readUIMessageStream } from "ai";

async function uiMessage(prompt) {
  const response = await fetch(`${process.env.BASE_URL}/api/v1/ai-sdk/chat`, {
    method: "POST",
    headers: {
      authorization: `Bearer ${process.env.API_KEY}`,
      "content-type": "application/json",
    },
    body: JSON.stringify({ id: "msg-smoke", prompt }),
  });
  if (!response.ok) {
    throw new Error(await response.text());
  }
  const text = await response.text();
  const chunks = text
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line.startsWith("data:"))
    .map((line) => line.slice(5).trim())
    .filter((line) => line && line !== "[DONE]")
    .map((line) => JSON.parse(line));
  const stream = new ReadableStream({
    start(controller) {
      for (const chunk of chunks) controller.enqueue(chunk);
      controller.close();
    },
  });
  let message;
  for await (const next of readUIMessageStream({ stream })) {
    message = next;
  }
  return message?.parts?.map((part) => part.text ?? "").join("") ?? "";
}

export async function answer(prompt) {
  const listed = await fetch(process.env.MCP_URL, {
    method: "POST",
    headers: {
      authorization: `Bearer ${process.env.API_KEY}`,
      "content-type": "application/json",
      accept: "application/json, text/event-stream",
    },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/list", params: {} }),
  });
  if (!listed.ok) {
    throw new Error(await listed.text());
  }
  const provider = createOpenAI({
    baseURL: process.env.BASE_URL,
    apiKey: process.env.API_KEY,
  });
  const generated = await generateText({
    model: provider("gpt-4.1-mini"),
    prompt,
  });
  const streamed = await uiMessage(prompt);
  return `${generated.text}\n${streamed}`;
}
