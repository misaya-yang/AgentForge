#!/usr/bin/env node
// Official OpenAI Node SDK acceptance runner for the Gateway's public /v1 API.
// It is inert unless --execute is supplied; credentials and response bodies are
// never printed. An external SDK install can be selected without repo changes.
import { createRequire } from "node:module";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

let activeStage = "configuration";

function fail(code) {
  const error = new Error("OpenAI SDK smoke failed");
  error.code = code;
  throw error;
}

function loadSdk() {
  const directModule = process.env.OPENAI_SDK_MODULE;
  if (directModule) {
    return import(pathToFileURL(resolve(directModule)).href);
  }
  const moduleRoot = process.env.OPENAI_SDK_NODE_MODULES;
  if (moduleRoot) {
    const require = createRequire(import.meta.url);
    let entry;
    try {
      entry = require.resolve("openai", { paths: [resolve(moduleRoot)] });
    } catch {
      // Also accept a path that points directly at node_modules.
      entry = require.resolve("openai", { paths: [resolve(moduleRoot, "..")] });
    }
    return import(pathToFileURL(entry).href);
  }
  return import("openai");
}

function originFor(baseURL) {
  let url;
  try {
    url = new URL(baseURL);
  } catch {
    fail("invalid_base_url");
  }
  if (url.username || url.password || url.search || url.hash) fail("invalid_base_url");
  const loopback = ["localhost", "127.0.0.1", "::1"].includes(url.hostname);
  if (url.protocol !== "https:" && !(loopback && url.protocol === "http:")) {
    fail("base_url_must_use_https");
  }
  return `${url.protocol}//${url.host}${url.pathname.replace(/\/$/, "")}`;
}

function checks(ok, code) {
  if (!ok) fail(code);
}

async function main() {
  const execute = process.argv.includes("--execute");
  const unknown = process.argv.slice(2).filter((arg) => arg !== "--execute" && arg !== "--help");
  if (unknown.length) fail("unknown_argument");
  if (process.argv.includes("--help")) {
    process.stdout.write([
      "Usage: node scripts/harness/openai_compat_sdk_smoke.mjs [--execute]",
      "Set OPENAI_BASE_URL, OPENAI_API_KEY, OPENAI_CHAT_MODEL, and OPENAI_EMBEDDING_MODEL.",
      "Use OPENAI_SDK_MODULE or OPENAI_SDK_NODE_MODULES (the install parent or node_modules directory) to load an external SDK install.",
      "The script makes no requests unless --execute is supplied.",
    ].join("\n") + "\n");
    return;
  }
  if (!execute) {
    process.stdout.write(JSON.stringify({
      status: "planned",
      sdk: "openai-node",
      checks: ["models.list", "chat.completions.create", "chat stream termination and usage", "embeddings.create"],
    }) + "\n");
    return;
  }

  const baseURL = process.env.OPENAI_BASE_URL;
  const apiKey = process.env.OPENAI_API_KEY;
  const chatModel = process.env.OPENAI_CHAT_MODEL;
  const embeddingModel = process.env.OPENAI_EMBEDDING_MODEL;
  if (!baseURL || !apiKey || !chatModel || !embeddingModel) fail("required_environment_missing");
  const { default: OpenAI } = await loadSdk();
  const client = new OpenAI({ apiKey, baseURL: originFor(baseURL), maxRetries: 0, timeout: 90_000 });
  activeStage = "models_list";
  const models = await client.models.list();
  checks(Array.isArray(models.data), "models_list_shape");
  checks(models.data.some((model) => model.id === chatModel), "chat_model_not_listed");
  checks(models.data.some((model) => model.id === embeddingModel), "embedding_model_not_listed");

  activeStage = "chat_completion";
  const completion = await client.chat.completions.create({
    model: chatModel,
    messages: [{ role: "user", content: "Reply with the word ready." }],
    max_tokens: 32,
  });
  checks(typeof completion.choices?.[0]?.message?.content === "string" && completion.choices[0].message.content.length > 0, "chat_response_shape");

  let streamedText = "";
  let finishReason = null;
  let streamUsage = null;
  activeStage = "chat_stream";
  const stream = await client.chat.completions.create({
    model: chatModel,
    messages: [{ role: "user", content: "Reply with the word ready." }],
    max_tokens: 32,
    stream: true,
    stream_options: { include_usage: true },
  });
  for await (const chunk of stream) {
    const choice = chunk.choices?.[0];
    if (typeof choice?.delta?.content === "string") streamedText += choice.delta.content;
    if (choice?.finish_reason) finishReason = choice.finish_reason;
    if (chunk.usage) streamUsage = chunk.usage;
  }
  checks(streamedText.length > 0, "stream_content_missing");
  checks(finishReason === "stop", "stream_finish_reason_missing");
  checks(streamUsage && Number.isFinite(streamUsage.total_tokens), "stream_usage_missing");

  activeStage = "embeddings";
  const embedding = await client.embeddings.create({
    model: embeddingModel,
    input: "OpenAI-compatible embedding smoke test.",
  });
  checks(Array.isArray(embedding.data?.[0]?.embedding), "embedding_response_shape");
  checks(embedding.data[0].embedding.length > 0, "embedding_vector_empty");

  process.stdout.write(JSON.stringify({
    status: "passed",
    sdk: "openai-node",
    models_listed: models.data.length,
    chat_completion: "passed",
    chat_stream: "passed",
    stream_usage: "passed",
    embedding_dimensions: embedding.data[0].embedding.length,
  }) + "\n");
}

main().catch((error) => {
  const safe = {
    status: "failed",
    stage: activeStage,
    code: typeof error?.code === "string" && /^[A-Za-z0-9_.-]{1,100}$/.test(error.code) ? error.code : "sdk_request_failed",
    http_status: Number.isInteger(error?.status) ? error.status : undefined,
  };
  process.stderr.write(JSON.stringify(safe) + "\n");
  process.exitCode = 1;
});
