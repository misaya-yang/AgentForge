import type { SessionConfig } from "@/api/sessions";

export const streamStartMetrics = {
  starts: 0,
  awaitedSessionCreate: 0,
};

export function resetStreamStartMetrics(): void {
  streamStartMetrics.starts = 0;
  streamStartMetrics.awaitedSessionCreate = 0;
}

export function mintClientSessionId(
  randomUUID: () => string = () => crypto.randomUUID(),
): string {
  return randomUUID();
}

export function beginNewChatSession(
  activeSessionId: string | undefined,
  mint: () => string = mintClientSessionId,
): { sessionId: string; isNew: boolean } {
  if (activeSessionId) {
    return { sessionId: activeSessionId, isNew: false };
  }
  return { sessionId: mint(), isNew: true };
}

export function persistNewChatSession(
  createSession: (body: {
    session_id: string;
    service_id?: string;
    metadata?: Record<string, unknown>;
    config?: SessionConfig;
  }) => Promise<{ session_id: string }>,
  updateSession: (
    sessionId: string,
    body: { metadata?: Record<string, unknown>; config?: SessionConfig },
  ) => Promise<unknown>,
  input: { sessionId: string; title: string; config?: SessionConfig },
): Promise<{ session_id: string }> {
  const body = {
    session_id: input.sessionId,
    service_id: "__builtin_assistant__",
    metadata: { title: input.title },
    config: input.config,
  };
  return createSession(body).catch(async (error: unknown) => {
    const status = (error as { response?: { status?: unknown } } | null)?.response?.status;
    if (status !== 409) {
      throw error;
    }

    // The stream can win the create race because both paths use the same
    // client-minted id. PATCH is deliberately required here: the backend
    // re-checks ownership, so a collision with another tenant/user is not
    // mistaken for a successfully persisted session.
    await updateSession(input.sessionId, {
      metadata: body.metadata,
      config: body.config,
    });
    return { session_id: input.sessionId };
  });
}

export function acceptPendingRunSession(args: {
  requestedSessionId: string;
  pendingSessionId: string | undefined;
  eventSessionId?: string;
}): string | undefined {
  if (args.pendingSessionId !== args.requestedSessionId) return undefined;
  if (args.eventSessionId && args.eventSessionId !== args.requestedSessionId) {
    return undefined;
  }
  return args.requestedSessionId;
}

export function confirmsAssistantRunAdmission(args: {
  requestedSessionId: string;
  eventSessionId?: string;
  isResume: boolean;
}): boolean {
  return !args.isResume &&
    (!args.eventSessionId || args.eventSessionId === args.requestedSessionId);
}

export function startChatWithoutAwaitingSessionCreate<TStream>(args: {
  sessionId: string;
  isNew: boolean;
  openStream: (sessionId: string) => TStream;
  persistSession?: (sessionId: string) => Promise<unknown>;
}): TStream {
  streamStartMetrics.starts += 1;
  const stream = args.openStream(args.sessionId);
  if (args.isNew && args.persistSession) {
    void args.persistSession(args.sessionId);
  }
  return stream;
}
