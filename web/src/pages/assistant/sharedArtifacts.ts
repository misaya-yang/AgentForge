/** Place each artifact from the frozen, authorized share whitelist once. */
export function placeSharedArtifacts<T extends { artifact_id: string }>(
  artifacts: T[], messages: { role: string; metadata?: Record<string, unknown> }[],
): { byMessage: T[][]; remaining: T[] } {
  const whitelist = new Map(artifacts.map((artifact) => [artifact.artifact_id, artifact]));
  const used = new Set<string>();
  const byMessage = messages.map((message) => {
    const ids = message.role === "assistant" && Array.isArray(message.metadata?.artifact_ids)
      ? message.metadata.artifact_ids : [];
    const found: T[] = [];
    for (const id of ids) {
      if (typeof id !== "string" || used.has(id)) continue;
      const artifact = whitelist.get(id);
      if (artifact) { found.push(artifact); used.add(id); }
    }
    return found;
  });
  return { byMessage, remaining: [...whitelist.values()].filter((artifact) => !used.has(artifact.artifact_id)) };
}
