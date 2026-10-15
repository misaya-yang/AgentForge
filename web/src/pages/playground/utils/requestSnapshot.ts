/** Diagnostic observations, not replay commands or immutable upstream configuration. */
export interface PlaygroundRequestSnapshot {
  version: 1;
  attempt_id: string;
  captured_at: string;
  protocol: "gateway" | "langgraph";
  service_id: string;
  session_id?: string;
  memory_enabled: boolean;
  thread_id?: string;
  conversation_id?: string;
  stream_mode?: ["messages-tuple", "updates", "custom"];
  durability?: "exit";
  input_text?: string;
}
export interface PlaygroundRequestCapture {
  attempt_id: string;
  protocol: "gateway" | "langgraph";
  service_id: string;
  session_id?: string;
  memory_enabled: boolean;
  thread_id?: string;
  body: unknown;
}
const record = (value: unknown): Record<string, unknown> | undefined =>
  value !== null && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown> : undefined;
// Identity fields are never URLs, headers or arbitrary connector configuration.
const identity = (value: unknown): string | undefined =>
  typeof value === "string" && /^[a-zA-Z0-9_.:-]{1,200}$/.test(value) ? value : undefined;

/** Re-project even persisted data: unknown keys and credential-shaped fields never enter exports. */
export function restoreRequestSnapshot(value: unknown, serviceId: string, sessionId?: string): PlaygroundRequestSnapshot | undefined {
  const data = record(value);
  if (!data || data.version !== 1 || data.service_id !== serviceId || !identity(serviceId) ||
    data.session_id !== sessionId || (sessionId !== undefined && !identity(sessionId)) ||
    !identity(data.attempt_id) || typeof data.memory_enabled !== "boolean" ||
    (data.protocol !== "gateway" && data.protocol !== "langgraph") ||
    typeof data.captured_at !== "string" || !/^\d{4}-\d{2}-\d{2}T[\d:.]+Z$/.test(data.captured_at) ||
    !Number.isFinite(Date.parse(data.captured_at))) return undefined;
  const result: PlaygroundRequestSnapshot = {
    version: 1, attempt_id: data.attempt_id as string, captured_at: data.captured_at,
    protocol: data.protocol, service_id: serviceId, memory_enabled: data.memory_enabled,
    ...(sessionId ? { session_id: sessionId } : {}),
  };
  if (data.protocol === "langgraph") {
    if (data.durability !== "exit" || !Array.isArray(data.stream_mode) ||
      JSON.stringify(data.stream_mode) !== JSON.stringify(["messages-tuple", "updates", "custom"])) return undefined;
    result.stream_mode = ["messages-tuple", "updates", "custom"];
    result.durability = "exit";
    if (identity(data.thread_id)) result.thread_id = data.thread_id as string;
  } else if (identity(data.conversation_id)) result.conversation_id = data.conversation_id as string;
  return result;
}

export function captureRequestSnapshot(capture: PlaygroundRequestCapture): PlaygroundRequestSnapshot | undefined {
  const body = record(capture.body);
  const parameters = record(body?.parameters);
  const data = {
    version: 1, attempt_id: capture.attempt_id, captured_at: new Date().toISOString(),
    protocol: capture.protocol, service_id: capture.service_id, session_id: capture.session_id,
    memory_enabled: capture.memory_enabled, thread_id: capture.thread_id,
    conversation_id: parameters?.conversation_id,
    stream_mode: body?.stream_mode, durability: body?.durability,
  };
  const result = restoreRequestSnapshot(data, capture.service_id, capture.session_id);
  if (!result) return undefined;
  const inputs = capture.protocol === "gateway" ? body?.inputs : record(body?.input)?.messages;
  const first = Array.isArray(inputs) ? record(inputs[0]) : undefined;
  const text = capture.protocol === "gateway" ? first?.data : first?.content;
  if (typeof text === "string") result.input_text = text;
  return result;
}

export function persistedRequestSnapshot(snapshot: PlaygroundRequestSnapshot): PlaygroundRequestSnapshot | undefined {
  // Deliberately omit prompt content; it already belongs to message history.
  return restoreRequestSnapshot(snapshot, snapshot.service_id, snapshot.session_id);
}

export function serializeRequestSnapshot(snapshot: PlaygroundRequestSnapshot): string {
  const safe = persistedRequestSnapshot(snapshot);
  if (!safe) throw new Error("Invalid request snapshot");
  return JSON.stringify({
    ...safe,
    content: typeof snapshot.input_text === "string" ? "includes_request_text_review_before_sharing" : "request_text_omitted",
    ...(typeof snapshot.input_text === "string" ? { input_text: snapshot.input_text } : {}),
    limitations: "Client dispatch observation only; server defaults, model configuration and historical context may change. Not a replay command.",
  }, null, 2);
}

