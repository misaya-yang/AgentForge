export const NEW_ASSISTANT_DRAFT = "new";
const STORAGE_PREFIX = "assistant.sessionDrafts.v1";

export function assistantDraftKey(sessionId: string | null | undefined): string {
  return sessionId || NEW_ASSISTANT_DRAFT;
}

export function transitionAssistantDraft(
  drafts: Record<string, string>,
  from: string,
  text: string,
  to: string,
): { drafts: Record<string, string>; input: string } {
  const next = { ...drafts, [from]: text };
  return { drafts: next, input: next[to] ?? "" };
}

export function bindNewAssistantDraft(
  drafts: Record<string, string>,
  text: string,
  sessionId: string,
): Record<string, string> {
  return { ...drafts, [assistantDraftKey(sessionId)]: text, [NEW_ASSISTANT_DRAFT]: "" };
}

export function clearAdmittedAssistantDrafts(
  drafts: Record<string, string>,
  sourceSessionId: string | null | undefined,
  acceptedSessionId: string,
): Record<string, string> {
  return {
    ...drafts,
    [assistantDraftKey(acceptedSessionId)]: "",
    ...(!sourceSessionId ? { [NEW_ASSISTANT_DRAFT]: "" } : {}),
  };
}

export function readAssistantDrafts(userId: string | undefined): Record<string, string> {
  if (!userId) return {};
  try {
    const raw = window.localStorage.getItem(`${STORAGE_PREFIX}:${userId}`);
    const parsed: unknown = raw ? JSON.parse(raw) : {};
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    return Object.fromEntries(
      Object.entries(parsed).filter(([key, value]) => key && typeof value === "string"),
    );
  } catch {
    return {};
  }
}

export function writeAssistantDrafts(userId: string | undefined, drafts: Record<string, string>): void {
  if (!userId) return;
  try {
    window.localStorage.setItem(`${STORAGE_PREFIX}:${userId}`, JSON.stringify(drafts));
  } catch {
    // Browser storage is best-effort; in-memory drafts still survive switching.
  }
}
