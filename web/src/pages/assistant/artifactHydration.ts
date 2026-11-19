import {
  getArtifactDownloadUrl,
  type ArtifactInfo,
} from "@/api/assistant";
import { resolveArtifactIdsByMessageIndex } from "@/features/chat/sessionArtifactHydration";

import type { ChatMessage, GeneratedArtifact } from "./types";

export function hydrateMessageArtifacts(
  messages: ChatMessage[],
  artifacts: ArtifactInfo[],
): ChatMessage[] {
  if (!artifacts.length) return messages;
  const artifactMap = new Map(artifacts.map((artifact) => [artifact.artifact_id, artifact]));
  const idsByMessageIndex = resolveArtifactIdsByMessageIndex(messages, artifacts);

  return messages.map((message, messageIndex) => {
    if (message.sourceAccessRevoked) return message;
    const ids = idsByMessageIndex.get(messageIndex);
    if (!ids?.length) return message;
    const generated = new Map<string, GeneratedArtifact>(
      (message.generatedArtifacts ?? []).map((artifact) => [artifact.id, artifact]),
    );
    for (const id of ids) {
      const artifact = artifactMap.get(id);
      if (!artifact || artifact.ready === false || artifact.size_bytes <= 0) continue;
      generated.set(id, {
        id: artifact.artifact_id,
        type: (artifact.type || "file") as GeneratedArtifact["type"],
        format: artifact.format || "",
        title: artifact.title || artifact.filename || "Artifact",
        url: getArtifactDownloadUrl(artifact.artifact_id),
        filename: artifact.filename,
        mimeType: artifact.mime_type,
        sizeBytes: artifact.size_bytes,
      });
    }
    return generated.size
      ? {
          ...message,
          // A partial list must not erase durable message bindings for files
          // whose detail is currently unavailable. No metadata is fabricated.
          _artifactIds: [...new Set([...(message._artifactIds ?? []), ...ids])],
          generatedArtifacts: [...generated.values()],
        }
      : message;
  });
}

export function buildLatestRunOutputFilesFromArtifacts(
  messages: ChatMessage[],
  artifacts: ArtifactInfo[],
) {
  if (!artifacts.length || !messages.length) return [];
  const artifactMap = new Map(artifacts.map((artifact) => [artifact.artifact_id, artifact]));

  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index];
    if (message.sourceAccessRevoked) continue;
    if (message.role !== "assistant" || !message._artifactIds?.length) continue;
    const files = message._artifactIds.flatMap((id) => {
      const artifact = artifactMap.get(id);
      return artifact && artifact.ready !== false && artifact.size_bytes > 0
        ? [{
            filename: artifact.filename || artifact.title || "artifact",
            content_base64: "",
            mime_type: artifact.mime_type || null,
            size_bytes: artifact.size_bytes || 0,
            artifact_id: artifact.artifact_id,
            download_url:
              artifact.download_url || getArtifactDownloadUrl(artifact.artifact_id),
          }]
        : [];
    });
    if (files.length) return files;
  }
  return [];
}
