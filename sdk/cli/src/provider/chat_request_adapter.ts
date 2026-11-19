import { createHash } from "node:crypto";

export interface ResponsesRequest {
  model: string;
  input: unknown;
  instructions?: unknown;
  tools?: unknown;
  tool_choice?: unknown;
  parallel_tool_calls?: unknown;
  temperature?: unknown;
  max_output_tokens?: unknown;
  reasoning?: unknown;
  stream?: unknown;
  store?: unknown;
  stream_options?: unknown;
  include?: unknown;
  service_tier?: unknown;
  prompt_cache_key?: unknown;
  text?: unknown;
  client_metadata?: unknown;
  access_programs?: unknown;
}

const SUPPORTED_REQUEST_FIELDS = new Set([
  "model", "input", "instructions", "tools", "tool_choice",
  "parallel_tool_calls", "temperature", "max_output_tokens", "reasoning",
  "stream", "store", "stream_options", "include", "service_tier",
  "prompt_cache_key", "text", "client_metadata", "access_programs",
]);

export function namespaceAlias(namespace: string, name: string): string {
  const digest = createHash("sha256").update(`${namespace}\0${name}`).digest("hex").slice(0, 10);
  const prefix = `ns_${digest}_`;
  return prefix + name.slice(0, 64 - prefix.length);
}

export function namespaceBindings(body: ResponsesRequest): Map<string, { namespace: string; name: string }> {
  const aliases = new Map<string, { namespace: string; name: string }>();
  const direct = new Set<string>();
  const bare = new Map<string, Array<{ namespace: string; name: string }>>();
  for (const raw of Array.isArray(body.tools) ? body.tools : []) {
    const tool = record(raw, "responses_tool_invalid");
    if (tool.type === "function") direct.add(String(tool.name ?? record(tool.function, "responses_tool_invalid").name));
    if (tool.type !== "namespace") continue;
    const namespace = text(tool.name, "responses_tool_invalid");
    for (const rawChild of Array.isArray(tool.tools) ? tool.tools : []) {
      const child = record(rawChild, "responses_tool_invalid");
      const name = text((child.function ?? child).name, "responses_tool_invalid");
      const identity = { namespace, name };
      aliases.set(namespaceAlias(namespace, name), identity);
      bare.set(name, [...(bare.get(name) ?? []), identity]);
    }
  }
  for (const [name, identities] of bare) if (identities.length === 1 && !direct.has(name)) aliases.set(name, identities[0]!);
  return aliases;
}

/** Convert the lossless Responses subset accepted by a Chat-only provider. */
export function responsesToChat(body: ResponsesRequest): Record<string, unknown> {
  validateRequestEnvelope(body);
  if (typeof body.model !== "string" || !body.model.trim()) {
    throw new CompatibilityError("responses_model_required");
  }
  const messages = responsesInputToMessages(body.input, body.instructions);
  const chat: Record<string, unknown> = {
    model: body.model,
    messages,
    stream: true,
    stream_options: { include_usage: true },
  };
  if (body.temperature !== undefined) chat.temperature = finiteNumber(body.temperature, "responses_temperature_invalid");
  if (body.max_output_tokens !== undefined) chat.max_tokens = positiveInteger(body.max_output_tokens, "responses_max_tokens_invalid");
  if (body.reasoning !== undefined && body.reasoning !== null) {
    const reasoning = record(body.reasoning, "responses_reasoning_unsupported");
    if (reasoning.effort !== undefined && reasoning.effort !== null) chat.reasoning_effort = text(reasoning.effort, "responses_reasoning_unsupported");
    if (reasoning.context !== undefined && reasoning.context !== null) throw new CompatibilityError("responses_reasoning_context_unsupported");
  }
  if (body.service_tier !== undefined && body.service_tier !== null) {
    chat.service_tier = text(body.service_tier, "responses_service_tier_invalid");
  }
  if (body.prompt_cache_key !== undefined && body.prompt_cache_key !== null) {
    chat.prompt_cache_key = text(body.prompt_cache_key, "responses_prompt_cache_key_invalid");
  }
  if (body.tools !== undefined) {
    if (!Array.isArray(body.tools)) throw new CompatibilityError("responses_tools_invalid");
    const projectedTools: Array<Record<string, unknown>> = [];
    const projectedNames = new Set<string>();
    const appendFunction = (tool: unknown, namespace?: string) => {
      const value = record(tool, "responses_tool_unsupported");
      const functionValue = value.function && typeof value.function === "object"
        ? record(value.function, "responses_tool_invalid") : value;
      const childName = text(functionValue.name, "responses_tool_invalid");
      const name = namespace ? namespaceAlias(namespace, childName) : childName;
      if (projectedNames.has(name)) throw new CompatibilityError("responses_tool_duplicate");
      projectedNames.add(name);
      projectedTools.push({
        type: "function",
        function: {
          name,
          description: typeof functionValue.description === "string" ? functionValue.description : "",
          parameters: functionValue.parameters ?? { type: "object", properties: {} },
          ...(typeof functionValue.strict === "boolean" ? { strict: functionValue.strict } : {}),
        },
      });
    };
    for (const tool of body.tools) {
      const value = record(tool, "responses_tool_unsupported");
      if (value.type === "function") appendFunction(value);
      else if (value.type === "namespace") {
        if (!Array.isArray(value.tools)) throw new CompatibilityError("responses_tool_unsupported");
        for (const child of value.tools) {
          const childValue = record(child, "responses_tool_unsupported");
          if (childValue.type !== "function") throw new CompatibilityError("responses_tool_unsupported");
          appendFunction(childValue, text(value.name, "responses_tool_invalid"));
        }
      } else throw new CompatibilityError("responses_tool_unsupported");
    }
    const toolChoice = chatToolChoice(body.tool_choice);
    if (typeof toolChoice === "object" && !projectedNames.has(toolChoice.function.name)) {
      throw new CompatibilityError("responses_tool_choice_unavailable");
    }
    if (toolChoice === "none") projectedTools.length = 0;
    if (!projectedTools.length && !["auto", "none"].includes(String(toolChoice))) {
      throw new CompatibilityError("responses_tool_choice_requires_tools");
    }
    if (projectedTools.length) {
      chat.tools = projectedTools;
      chat.tool_choice = toolChoice;
    }
    if (projectedTools.length && body.parallel_tool_calls !== undefined) {
      if (typeof body.parallel_tool_calls !== "boolean") {
        throw new CompatibilityError("responses_parallel_tool_calls_invalid");
      }
      chat.parallel_tool_calls = body.parallel_tool_calls;
    }
  } else if (body.tool_choice !== undefined && !["auto", "none"].includes(String(body.tool_choice))) {
    throw new CompatibilityError("responses_tool_choice_requires_tools");
  }
  return chat;
}

function validateRequestEnvelope(body: ResponsesRequest): void {
  const raw = body as unknown as Record<string, unknown>;
  const unknown = Object.keys(raw).filter((key) => !SUPPORTED_REQUEST_FIELDS.has(key));
  if (unknown.length) throw new CompatibilityError("responses_fields_unsupported");
  if (body.stream !== true) throw new CompatibilityError("streaming_responses_required");
  if (body.store !== undefined && body.store !== false) throw new CompatibilityError("responses_store_unsupported");
  if (body.stream_options !== undefined && body.stream_options !== null) {
    const options = record(body.stream_options, "responses_stream_options_unsupported");
    if (Object.keys(options).length) throw new CompatibilityError("responses_stream_options_unsupported");
  }
  if (body.include !== undefined) {
    if (!Array.isArray(body.include) || body.include.some((value) => value !== "reasoning.encrypted_content")) {
      throw new CompatibilityError("responses_include_unsupported");
    }
  }
  if (body.text !== undefined && body.text !== null) throw new CompatibilityError("responses_text_controls_unsupported");
  if (body.access_programs !== undefined && body.access_programs !== null) {
    throw new CompatibilityError("responses_access_programs_unsupported");
  }
  if (body.client_metadata !== undefined && body.client_metadata !== null) {
    record(body.client_metadata, "responses_client_metadata_invalid");
  }
}

function responsesInputToMessages(input: unknown, instructions: unknown): Array<Record<string, unknown>> {
  const messages: Array<Record<string, unknown>> = [];
  if (instructions !== undefined) messages.push({ role: "system", content: text(instructions, "responses_instructions_invalid") });
  if (typeof input === "string") return [...messages, { role: "user", content: input }];
  if (!Array.isArray(input)) throw new CompatibilityError("responses_input_invalid");
  const pendingCalls = new Map<string, string>();
  const callIds = new Set<string>();
  let assistant: Record<string, any> | undefined;
  const assistantMessage = () => assistant ??= { role: "assistant", content: "" };
  const flushAssistant = () => {
    if (assistant) messages.push(assistant);
    assistant = undefined;
  };
  for (const raw of input) {
    const item = record(raw, "responses_input_item_unsupported");
    const type = item.type ?? "message";
    if (type === "message") {
      const role = item.role;
      if (!["user", "assistant", "system", "developer"].includes(String(role))) {
        throw new CompatibilityError("responses_message_role_unsupported");
      }
      const content = responseContentText(item.content);
      if (role === "assistant") {
        if (pendingCalls.size && !assistant) throw new CompatibilityError("responses_function_output_missing");
        assistantMessage().content += content;
      }
      else {
        if (pendingCalls.size) throw new CompatibilityError("responses_function_output_missing");
        flushAssistant();
        messages.push({ role: role === "developer" ? "system" : role, content });
      }
      continue;
    }
    if (type === "reasoning") {
      if (pendingCalls.size && !assistant) throw new CompatibilityError("responses_function_output_missing");
      // Replay the plain Chat reasoning we projected on the previous request.
      // Opaque Responses state cannot be translated to Chat reasoning text.
      if (item.encrypted_content != null || (item.content != null && (!Array.isArray(item.content) || item.content.length))) {
        throw new CompatibilityError("responses_reasoning_history_unsupported");
      }
      if (!Array.isArray(item.summary)) throw new CompatibilityError("responses_reasoning_history_unsupported");
      const summary = item.summary.map((rawPart: unknown) => {
        const part = record(rawPart, "responses_reasoning_history_unsupported");
        if (part.type !== "summary_text") throw new CompatibilityError("responses_reasoning_history_unsupported");
        return text(part.text, "responses_reasoning_history_unsupported");
      }).join("");
      if (summary) {
        const message = assistantMessage();
        message.reasoning_content = (message.reasoning_content ?? "") + summary;
      }
      continue;
    }
    if (type === "function_call") {
      if (pendingCalls.size && !assistant) throw new CompatibilityError("responses_function_output_missing");
      const id = text(item.call_id ?? item.id, "responses_function_call_invalid");
      const originalName = text(item.name, "responses_function_call_invalid");
      const name = typeof item.namespace === "string" ? namespaceAlias(item.namespace, originalName) : originalName;
      const args = typeof item.arguments === "string" ? item.arguments : JSON.stringify(item.arguments ?? {});
      if (callIds.has(id)) throw new CompatibilityError("responses_function_call_duplicate");
      callIds.add(id);
      pendingCalls.set(id, name);
      const message = assistantMessage();
      (message.tool_calls ??= []).push({ id, type: "function", function: { name, arguments: args } });
      continue;
    }
    if (type === "function_call_output") {
      const id = text(item.call_id, "responses_function_output_invalid");
      const name = pendingCalls.get(id);
      if (!name) throw new CompatibilityError("responses_function_output_unmatched");
      if (item.name !== undefined && item.name !== null) {
        const outputName = text(item.name, "responses_function_output_invalid");
        const identity = typeof item.namespace === "string" ? namespaceAlias(item.namespace, outputName) : outputName;
        if (identity !== name) throw new CompatibilityError("responses_function_output_identity_mismatch");
      }
      flushAssistant();
      messages.push({ role: "tool", tool_call_id: id, name, content: functionOutputText(item.output) });
      pendingCalls.delete(id);
      continue;
    }
    throw new CompatibilityError("responses_input_item_unsupported");
  }
  if (pendingCalls.size) throw new CompatibilityError("responses_function_output_missing");
  flushAssistant();
  if (!messages.length) throw new CompatibilityError("responses_input_empty");
  return messages;
}

function functionOutputText(output: unknown): string {
  if (typeof output === "string") return output;
  if (!Array.isArray(output)) throw new CompatibilityError("responses_function_output_unsupported");
  return output.map((part) => {
    const value = record(part, "responses_function_output_unsupported");
    if (value.type !== "input_text") throw new CompatibilityError("responses_function_output_unsupported");
    return text(value.text, "responses_function_output_unsupported");
  }).join("");
}

function responseContentText(content: unknown): string {
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) throw new CompatibilityError("responses_content_unsupported");
  return content.map((part) => {
    const value = record(part, "responses_content_unsupported");
    if (!["input_text", "output_text", "text"].includes(String(value.type))) {
      throw new CompatibilityError("responses_content_unsupported");
    }
    return text(value.text, "responses_content_unsupported");
  }).join("");
}

function chatToolChoice(choice: unknown): "auto" | "none" | "required" | { type: "function"; function: { name: string } } {
  if (choice === undefined || choice === null) return "auto";
  if (choice === "auto" || choice === "none" || choice === "required") return choice;
  const value = record(choice, "responses_tool_choice_unsupported");
  if (value.type !== "function") throw new CompatibilityError("responses_tool_choice_unsupported");
  const functionValue = value.function && typeof value.function === "object"
    ? record(value.function, "responses_tool_choice_unsupported") : value;
  const name = text(functionValue.name, "responses_tool_choice_unsupported");
  const namespace = functionValue.namespace ?? value.namespace;
  return { type: "function", function: { name: namespace == null ? name : namespaceAlias(text(namespace, "responses_tool_choice_unsupported"), name) } };
}

export function record(value: unknown, code: string): Record<string, any> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new CompatibilityError(code);
  return value as Record<string, any>;
}

function text(value: unknown, code: string): string {
  if (typeof value !== "string" || !value) throw new CompatibilityError(code);
  return value;
}

function finiteNumber(value: unknown, code: string): number {
  const number = Number(value);
  if (!Number.isFinite(number)) throw new CompatibilityError(code);
  return number;
}

function positiveInteger(value: unknown, code: string): number {
  const number = Number(value);
  if (!Number.isInteger(number) || number < 1) throw new CompatibilityError(code);
  return number;
}

export class CompatibilityError extends Error {
  constructor(readonly code: string, readonly status = 400) {
    super(code);
    this.name = "CompatibilityError";
  }
}
