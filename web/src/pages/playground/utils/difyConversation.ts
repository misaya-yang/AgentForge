import { getSession, updateSession } from "@/api/sessions";

/** No process-wide or browser-storage cache: identity comes from the session API. */
export async function prepareDifyConversation(
  serviceId: string,
  sessionId: string | undefined,
  isCurrent: () => boolean,
) {
  const assertCurrent = () => {
    if (!isCurrent()) throw new DOMException("Request changed", "AbortError");
  };
  assertCurrent();
  let conversationId: string | undefined;
  if (sessionId) {
    const session = await getSession(sessionId);
    assertCurrent();
    if (session.service_id !== serviceId) throw new Error("Dify session service mismatch");
    const saved = session.metadata?.dify_conversation;
    if (saved && typeof saved === "object") {
      const binding = saved as Record<string, unknown>;
      if (binding.service_id === serviceId && typeof binding.conversation_id === "string" && binding.conversation_id.trim()) {
        conversationId = binding.conversation_id;
      }
    }
  }
  return {
    parameters: conversationId ? { conversation_id: conversationId } : undefined,
    async accept(metadata: Record<string, unknown> | undefined) {
      assertCurrent();
      const next = metadata?.dify_conversation_id;
      if (!sessionId || typeof next !== "string" || !next.trim() || next === conversationId) return;
      // Persist as soon as identity arrives, including streams later interrupted.
      // Failure is visible and must never trigger a second upstream invocation.
      await updateSession(sessionId, {
        metadata: { dify_conversation: { service_id: serviceId, conversation_id: next } },
      });
      assertCurrent();
      conversationId = next;
    },
  };
}
