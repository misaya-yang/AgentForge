import { randomUUID } from "node:crypto";
import type { ServerResponse } from "node:http";

import { CompatibilityError, namespaceAlias, record } from "./chat_request_adapter.js";
import { readSsePayloads } from "./sse_reader.js";

interface ToolCallState {
  id: string;
  name: string;
  arguments: string;
  outputIndex: number;
  announced: boolean;
}

export async function projectChatStream(
  upstream: Response, output: ServerResponse, model: string, idleTimeoutMs: number, signal: AbortSignal,
  aliases: ReadonlyMap<string, { namespace: string; name: string }> = new Map(),
  allowedNames?: ReadonlySet<string>,
): Promise<void> {
  const disconnected = new AbortController();
  const onClose = () => disconnected.abort(new CompatibilityError("consumer_disconnected"));
  output.once("close", onClose);
  try {
    await projectBoundedChatStream(upstream, output, model, idleTimeoutMs, AbortSignal.any([signal, disconnected.signal]), aliases, allowedNames);
  } finally {
    output.off("close", onClose);
    if (!upstream.body?.locked) await upstream.body?.cancel().catch(() => undefined);
  }
}

function boundedAppend(current: string, delta: string, limit: number, code: string): string {
  if (Buffer.byteLength(current) + Buffer.byteLength(delta) > limit) throw new CompatibilityError(code);
  return current + delta;
}

async function projectBoundedChatStream(
  upstream: Response,
  output: ServerResponse,
  model: string,
  idleTimeoutMs: number,
  signal: AbortSignal,
  aliases: ReadonlyMap<string, { namespace: string; name: string }>,
  allowedNames?: ReadonlySet<string>,
): Promise<void> {
  const responseId = `resp_${randomUUID().replaceAll("-", "")}`;
  const messageId = `msg_${responseId.slice(5)}`;
  const reasoningId = `rs_${responseId.slice(5)}`;
  let sequence = 0;
  let textOutput = "";
  let reasoningOutput = "";
  let messageOutputIndex: number | undefined;
  let reasoningOutputIndex: number | undefined;
  let reasoningClosed = false;
  let nextOutputIndex = 0;
  let usage: Record<string, unknown> | null = null;
  let terminalSeen = false;
  let terminalFailure: string | undefined;
  const tools = new Map<number, ToolCallState>();
  const callIds = new Map<string, number>();
  const toolIdentity = (name: string) => aliases.get(name) ?? { name };
  const emit = async (type: string, payload: Record<string, unknown>) => {
    await writeSse(output, type, { type, sequence_number: sequence++, ...payload }, signal);
  };
  const ensureMessageOpen = async () => {
    if (messageOutputIndex !== undefined) return;
    messageOutputIndex = nextOutputIndex++;
    await emit("response.output_item.added", {
      output_index: messageOutputIndex,
      item: {
        id: messageId,
        type: "message",
        role: "assistant",
        status: "in_progress",
        content: [],
      },
    });
    await emit("response.content_part.added", {
      item_id: messageId,
      output_index: messageOutputIndex,
      content_index: 0,
      part: { type: "output_text", text: "", annotations: [] },
    });
  };
  const closeReasoning = async () => {
    if (reasoningOutputIndex === undefined || reasoningClosed) return;
    const item = {
      id: reasoningId,
      type: "reasoning",
      status: "completed",
      summary: [{ type: "summary_text", text: reasoningOutput }],
    };
    await emit("response.reasoning_summary_text.done", {
      item_id: reasoningId,
      output_index: reasoningOutputIndex,
      summary_index: 0,
      text: reasoningOutput,
    });
    await emit("response.reasoning_summary_part.done", {
      item_id: reasoningId,
      output_index: reasoningOutputIndex,
      summary_index: 0,
      part: { type: "summary_text", text: reasoningOutput },
    });
    await emit("response.output_item.done", { output_index: reasoningOutputIndex, item });
    reasoningClosed = true;
  };
  const responseObject = (status: string, error: Record<string, unknown> | null = null) => ({
    id: responseId,
    object: "response",
    created_at: Math.floor(Date.now() / 1000),
    status,
    error,
    model,
    output: [],
    usage,
  });
  await emit("response.created", { response: responseObject("in_progress") });
  await emit("response.in_progress", { response: responseObject("in_progress") });

  try {
    for await (const payload of readSsePayloads(upstream.body!, idleTimeoutMs, signal)) {
    if (payload === "[DONE]") {
      terminalSeen = true;
      break;
    }
    let event: Record<string, unknown>;
    try {
      event = record(JSON.parse(payload), "provider_stream_event_invalid");
    } catch (error) {
      if (error instanceof CompatibilityError) throw error;
      throw new CompatibilityError("provider_stream_json_invalid");
    }
    // Provider errors may arrive inside an HTTP-200 SSE stream. Never let a
    // later finish reason or [DONE] turn them into a successful response, and
    // never forward provider error details (which may contain credentials).
    if (event.error !== undefined && event.error !== null) {
      throw new CompatibilityError("provider_stream_error");
    }
    if (event.usage && typeof event.usage === "object") usage = normalizeUsage(record(event.usage, "provider_usage_invalid"));
    const choices = Array.isArray(event.choices) ? event.choices : [];
    for (const rawChoice of choices) {
      const choice = record(rawChoice, "provider_choice_invalid");
      if (choice.finish_reason !== undefined && choice.finish_reason !== null) {
        const finishReason = String(choice.finish_reason);
        terminalSeen = true;
        if (!["stop", "tool_calls", "function_call"].includes(finishReason)) {
          terminalFailure = `provider_finish_${finishReason.replace(/[^A-Za-z0-9_.-]/g, "_")}`;
        }
      }
      const delta = choice.delta && typeof choice.delta === "object" ? record(choice.delta, "provider_delta_invalid") : {};
      const rawReasoning = delta.reasoning_content ?? delta.reasoning;
      if (rawReasoning !== undefined && rawReasoning !== null) {
        if (typeof rawReasoning !== "string") {
          throw new CompatibilityError("provider_reasoning_delta_unsupported");
        }
        if (rawReasoning) {
          if (reasoningClosed) throw new CompatibilityError("provider_reasoning_order_unsupported");
          if (reasoningOutputIndex === undefined) {
            reasoningOutputIndex = nextOutputIndex++;
            await emit("response.output_item.added", {
              output_index: reasoningOutputIndex,
              item: {
                id: reasoningId,
                type: "reasoning",
                status: "in_progress",
                summary: [],
              },
            });
            await emit("response.reasoning_summary_part.added", {
              item_id: reasoningId,
              output_index: reasoningOutputIndex,
              summary_index: 0,
              part: { type: "summary_text", text: "" },
            });
          }
          reasoningOutput = boundedAppend(reasoningOutput, rawReasoning, 4 * 1024 * 1024, "provider_reasoning_limit");
          await emit("response.reasoning_summary_text.delta", {
            item_id: reasoningId,
            output_index: reasoningOutputIndex,
            summary_index: 0,
            delta: rawReasoning,
          });
        }
      }
      const content = typeof delta.content === "string" ? delta.content : "";
      if (delta.content !== undefined && delta.content !== null && typeof delta.content !== "string") {
        throw new CompatibilityError("provider_content_delta_unsupported");
      }
      if (content) {
        await closeReasoning();
        await ensureMessageOpen();
        textOutput = boundedAppend(textOutput, content, 4 * 1024 * 1024, "provider_text_limit");
        await emit("response.output_text.delta", {
          item_id: messageId,
          output_index: messageOutputIndex,
          content_index: 0,
          delta: content,
          logprobs: [],
        });
      }
      if (Array.isArray(delta.tool_calls)) {
        for (const rawTool of delta.tool_calls) {
          const tool = record(rawTool, "provider_tool_delta_invalid");
          const index = Number(tool.index ?? 0);
          if (!Number.isInteger(index) || index < 0) throw new CompatibilityError("provider_tool_delta_invalid");
          const fn = tool.function && typeof tool.function === "object" ? record(tool.function, "provider_tool_delta_invalid") : {};
          if (!tools.has(index) && tools.size >= 128) throw new CompatibilityError("provider_tool_count_limit");
          const current = tools.get(index) ?? {
            id: "",
            name: "",
            arguments: "",
            outputIndex: nextOutputIndex++,
            announced: false,
          };
          const wasAnnounced = current.announced;
          if (typeof tool.id === "string" && tool.id) {
            if (callIds.has(tool.id) && callIds.get(tool.id) !== index) throw new CompatibilityError("provider_tool_id_duplicate");
            callIds.set(tool.id, index);
            if (current.id && current.id !== tool.id) {
              throw new CompatibilityError("provider_tool_id_changed");
            }
            current.id = tool.id;
          }
          if (current.announced && fn.name) throw new CompatibilityError("provider_tool_name_changed");
          if (typeof fn.name === "string") current.name = boundedAppend(current.name, fn.name, 256, "provider_tool_name_limit");
          if (typeof fn.arguments === "string") {
            const total = [...tools.values()].reduce((n, item) => n + Buffer.byteLength(item.arguments), 0);
            if (total + Buffer.byteLength(fn.arguments) > 4 * 1024 * 1024) throw new CompatibilityError("provider_tool_arguments_total_limit");
            current.arguments = boundedAppend(current.arguments, fn.arguments, 1024 * 1024, "provider_tool_arguments_limit");
          }
          const ambiguousPrefix = [...(allowedNames ?? []), ...aliases.keys()].some((name) => name !== current.name && name.startsWith(current.name));
          if (!current.announced && !ambiguousPrefix && current.name && current.id && (!allowedNames || allowedNames.has(current.name) || (aliases.has(current.name) && allowedNames.has(namespaceAlias(aliases.get(current.name)!.namespace, aliases.get(current.name)!.name))))) {
            await emit("response.output_item.added", {
              output_index: current.outputIndex,
              item: { id: current.id, type: "function_call", status: "in_progress", call_id: current.id, ...toolIdentity(current.name), arguments: "" },
            });
            current.announced = true;
          }
          if (current.announced && current.arguments && !wasAnnounced) {
            await emit("response.function_call_arguments.delta", {
              item_id: current.id, output_index: current.outputIndex, delta: current.arguments,
            });
          } else if (current.announced && typeof fn.arguments === "string" && fn.arguments) {
            await emit("response.function_call_arguments.delta", {
              item_id: current.id, output_index: current.outputIndex, delta: fn.arguments,
            });
          }
          tools.set(index, current);
        }
      }
    }
  }
  } catch (error) {
    if (signal.aborted || output.destroyed) {
      output.destroy();
      return;
    }
    const code = error instanceof CompatibilityError ? error.code : "provider_stream_failed";
    await emit("response.failed", {
      response: {
        ...responseObject("failed", { code, message: code, type: "server_error" }),
        output: [],
      },
    });
    output.end();
    return;
  }
  if (!terminalSeen) {
    await emit("response.failed", {
      response: {
        ...responseObject("failed", {
          code: "provider_stream_incomplete",
          message: "provider_stream_incomplete",
          type: "server_error",
        }),
        output: [],
      },
    });
    output.end();
    return;
  }
  if (terminalFailure) {
    await emit("response.failed", {
      response: {
        ...responseObject("failed", {
          code: terminalFailure,
          message: terminalFailure,
          type: "server_error",
        }),
        output: [],
      },
    });
    output.end();
    return;
  }
  for (const tool of tools.values()) {
    const identity = aliases.get(tool.name);
    const allowed = !allowedNames || allowedNames.has(tool.name)
      || (identity && allowedNames.has(namespaceAlias(identity.namespace, identity.name)));
    if (!tool.announced && tool.id && tool.name && allowed) {
      await emit("response.output_item.added", { output_index: tool.outputIndex,
        item: { id: tool.id, type: "function_call", status: "in_progress", call_id: tool.id, ...toolIdentity(tool.name), arguments: "" } });
      if (tool.arguments) await emit("response.function_call_arguments.delta", { item_id: tool.id, output_index: tool.outputIndex, delta: tool.arguments });
      tool.announced = true;
    }
  }
  if ([...tools.values()].some((tool) => !tool.id || !tool.name || !tool.announced)) {
    await emit("response.failed", {
      response: {
        ...responseObject("failed", {
          code: "provider_tool_identity_missing",
          message: "provider_tool_identity_missing",
          type: "server_error",
        }),
        output: [],
      },
    });
    output.end();
    return;
  }

  await closeReasoning();
  await ensureMessageOpen();
  await emit("response.output_text.done", {
    item_id: messageId,
    output_index: messageOutputIndex,
    content_index: 0,
    text: textOutput,
    logprobs: [],
  });
  const part = { type: "output_text", text: textOutput, annotations: [] };
  await emit("response.content_part.done", {
    item_id: messageId,
    output_index: messageOutputIndex,
    content_index: 0,
    part,
  });
  const messageItem = {
    id: messageId,
    type: "message",
    role: "assistant",
    status: "completed",
    content: [part],
  };
  await emit("response.output_item.done", {
    output_index: messageOutputIndex,
    item: messageItem,
  });
  const indexedOutput: Array<{ index: number; item: Record<string, unknown> }> = [
    { index: messageOutputIndex!, item: messageItem },
  ];
  if (reasoningOutputIndex !== undefined) {
    indexedOutput.push({
      index: reasoningOutputIndex,
      item: {
        id: reasoningId,
        type: "reasoning",
        status: "completed",
        summary: [{ type: "summary_text", text: reasoningOutput }],
      },
    });
  }
  for (const tool of [...tools.values()].sort((a, b) => a.outputIndex - b.outputIndex)) {
    await emit("response.function_call_arguments.done", {
      item_id: tool.id, output_index: tool.outputIndex, ...toolIdentity(tool.name), arguments: tool.arguments,
    });
    const item = { id: tool.id, type: "function_call", status: "completed", call_id: tool.id, ...toolIdentity(tool.name), arguments: tool.arguments };
    await emit("response.output_item.done", { output_index: tool.outputIndex, item });
    indexedOutput.push({ index: tool.outputIndex, item });
  }
  indexedOutput.sort((a, b) => a.index - b.index);
  const completed = {
    ...responseObject("completed"),
    output: indexedOutput.map(({ item }) => item),
  };
  await emit("response.completed", { response: completed });
  output.end();
}

function normalizeUsage(value: Record<string, unknown>): Record<string, unknown> {
  return {
    input_tokens: Number(value.prompt_tokens ?? value.input_tokens ?? 0),
    output_tokens: Number(value.completion_tokens ?? value.output_tokens ?? 0),
    total_tokens: Number(value.total_tokens ?? 0),
  };
}

async function writeSse(response: ServerResponse, type: string, payload: Record<string, unknown>, signal: AbortSignal): Promise<void> {
  if (signal.aborted || response.destroyed) throw new CompatibilityError("consumer_disconnected");
  if (response.write(`event: ${type}\ndata: ${JSON.stringify(payload)}\n\n`)) return;
  await new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => finish(new CompatibilityError("consumer_backpressure_timeout")), 10_000);
    const drained = () => finish();
    const closed = () => finish(new CompatibilityError("consumer_disconnected"));
    const finish = (error?: Error) => {
      clearTimeout(timer);
      response.off("drain", drained);
      response.off("close", closed);
      signal.removeEventListener("abort", closed);
      if (error) reject(error); else resolve();
    };
    response.once("drain", drained);
    response.once("close", closed);
    signal.addEventListener("abort", closed, { once: true });
    if (signal.aborted || response.destroyed) closed();
  });
}
