export interface HydratableMessage {
  id: string;
  role: string;
  createdAt?: string;
  _artifactIds?: string[];
}

export interface PersistedArtifactRef {
  artifact_id: string;
  message_id?: string;
  created_at?: string;
  source?: string;
}

/**
 * Resolve session artifacts to assistant messages.
 *
 * Runtime V2 office artifacts are durable before the final assistant message
 * is projected and therefore may not have a message_id/artifact_ids binding.
 * Explicit bindings win; unbound generated artifacts go to the first assistant
 * message at/after their creation time, or the latest assistant as a fallback.
 */
export function resolveArtifactIdsByMessageIndex(
  messages: HydratableMessage[],
  artifacts: PersistedArtifactRef[],
): Map<number, string[]> {
  const artifactIds = new Set(artifacts.map((artifact) => artifact.artifact_id));
  const resolved = new Map<number, string[]>();
  const claimed = new Set<string>();
  const assistantIndexes: number[] = [];

  messages.forEach((message, index) => {
    if (message.role === "assistant") assistantIndexes.push(index);
    const explicit = (message._artifactIds ?? []).filter((id) => artifactIds.has(id));
    if (explicit.length > 0) {
      resolved.set(index, [...new Set(explicit)]);
      explicit.forEach((id) => claimed.add(id));
    }
  });
  if (assistantIndexes.length === 0) return resolved;

  for (const artifact of artifacts) {
    if (claimed.has(artifact.artifact_id) || artifact.source === "user") continue;
    let target = assistantIndexes.find(
      (index) => artifact.message_id && messages[index]?.id === artifact.message_id,
    );
    const artifactCreatedAt = Date.parse(artifact.created_at ?? "");
    if (target === undefined && Number.isFinite(artifactCreatedAt)) {
      target = assistantIndexes.find((index) => {
        const messageCreatedAt = Date.parse(messages[index]?.createdAt ?? "");
        return Number.isFinite(messageCreatedAt) && messageCreatedAt >= artifactCreatedAt;
      });
    }
    target ??= assistantIndexes[assistantIndexes.length - 1];
    if (target === undefined) continue;
    resolved.set(target, [...(resolved.get(target) ?? []), artifact.artifact_id]);
    claimed.add(artifact.artifact_id);
  }
  return resolved;
}
