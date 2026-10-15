/**
 * Chat module — streaming and non-streaming chat via the gateway.
 * Ported from Python SDK: sdk/python/ai_assistant/chat.py
 */

import type { StreamEvent } from "../types/events.js";
import type { GatewayClient } from "./client.js";

export interface ChatOptions {
  sessionId?: string;
  modelId?: string;
  temperature?: number;
  maxTokens?: number;
  systemPrompt?: string;
  kbDatasetIds?: string[];
  webSearchEnabled?: boolean;
  executionProfile?: "safe" | "balanced" | "power";
  osAgentEnabled?: boolean;
}

interface ChatRequestBody {
  message: string;
  stream: boolean;
  session_id?: string;
  model_id?: string;
  temperature: number;
  max_tokens?: number;
  system_prompt?: string;
  kb_dataset_ids?: string[];
  web_search_enabled?: boolean;
  execution_profile?: string;
  os_agent_enabled?: boolean;
}

function buildBody(message: string, stream: boolean, opts: ChatOptions): ChatRequestBody {
  const body: ChatRequestBody = {
    message,
    stream,
    temperature: opts.temperature ?? 0.7,
  };

  // Omit model_id when unset — the server applies its deployment default.
  if (opts.modelId) body.model_id = opts.modelId;
  if (opts.sessionId) body.session_id = opts.sessionId;
  if (opts.maxTokens) body.max_tokens = opts.maxTokens;
  if (opts.systemPrompt) body.system_prompt = opts.systemPrompt;
  if (opts.kbDatasetIds?.length) body.kb_dataset_ids = opts.kbDatasetIds;
  if (opts.webSearchEnabled) body.web_search_enabled = true;
  if (opts.executionProfile) body.execution_profile = opts.executionProfile;
  if (opts.osAgentEnabled) body.os_agent_enabled = true;

  return body;
}

export class ChatModule {
  private client: GatewayClient;

  constructor(client: GatewayClient) {
    this.client = client;
  }

  /**
   * Non-streaming chat — returns the full response.
   */
  async send(message: string, opts: ChatOptions = {}): Promise<any> {
    return this.client.request(
      "POST",
      "/api/v1/assistant/chat",
      buildBody(message, false, opts),
    );
  }

  /**
   * Streaming chat — yields SSE events.
   */
  async *stream(
    message: string,
    opts: ChatOptions = {},
  ): AsyncGenerator<StreamEvent> {
    yield* this.client.streamSSE(
      "/api/v1/assistant/chat/stream",
      buildBody(message, true, opts),
    );
  }
}
