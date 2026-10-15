export const MAX_ASSISTANT_FILES = 5;

export function planAssistantFileSelection<T>(incoming: T[], existingCount: number) {
  const remaining = Math.max(0, MAX_ASSISTANT_FILES - existingCount);
  return {
    accepted: incoming.slice(0, remaining),
    exceedsLimit: incoming.length > remaining,
  };
}
