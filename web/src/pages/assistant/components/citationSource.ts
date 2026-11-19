import type { RetrievedChunk } from "../types";

export interface CitationSource {
  datasetId: string;
  documentId: string;
  excerpt: string;
  provenance: "legacy" | "versioned" | "invalid";
  sourceVersion?: number;
  sourceHash?: string;
}

/** Preserve the identity emitted with the hit; never reconstruct it from a URL. */
export function citationSource(datasetId: string, chunk: RetrievedChunk): CitationSource {
  const metadata = chunk.metadata || {};
  const topDocumentId = chunk.document_id;
  const metadataDocumentId = metadata.document_id;
  const selectedDocumentId = topDocumentId ?? metadataDocumentId;
  const documentId = typeof selectedDocumentId === "string" ? selectedDocumentId.trim() : "";
  const base = { datasetId, documentId, excerpt: chunk.content };
  if (!datasetId || !documentId
    || (chunk.dataset_id != null && chunk.dataset_id !== datasetId)
    || (metadata.dataset_id != null && metadata.dataset_id !== datasetId)
    || (topDocumentId != null && metadataDocumentId != null
      && topDocumentId !== metadataDocumentId)) {
    return { ...base, provenance: "invalid" };
  }

  const topVersion = chunk.source_version;
  const topHash = chunk.source_hash;
  const metadataVersion = metadata.source_version;
  const metadataHash = metadata.source_hash;
  const topHasVersion = topVersion != null;
  const topHasHash = topHash != null;
  const metadataHasVersion = metadataVersion != null;
  const metadataHasHash = metadataHash != null;
  if (topHasVersion !== topHasHash || metadataHasVersion !== metadataHasHash) {
    return { ...base, provenance: "invalid" };
  }
  if (!topHasVersion && !metadataHasVersion) {
    return { ...base, provenance: "legacy" };
  }
  const sourceVersion = topHasVersion ? topVersion : metadataVersion;
  const sourceHash = topHasHash ? topHash : metadataHash;
  if ((topHasVersion && metadataHasVersion && topVersion !== metadataVersion)
    || (topHasHash && metadataHasHash
      && String(topHash).toLowerCase() !== String(metadataHash).toLowerCase())
    || typeof sourceVersion !== "number" || !Number.isInteger(sourceVersion) || sourceVersion <= 0
    || typeof sourceHash !== "string" || !/^[0-9a-f]{64}$/i.test(sourceHash)) {
    return { ...base, provenance: "invalid" };
  }
  return { ...base, provenance: "versioned", sourceVersion, sourceHash: sourceHash.toLowerCase() };
}
