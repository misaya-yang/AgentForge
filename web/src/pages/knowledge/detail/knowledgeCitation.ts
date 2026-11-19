import type { RetrieveHit } from "../../../types/knowledge.ts";
import { citationSource, type CitationSource } from "../../assistant/components/citationSource.ts";

/** Knowledge API identity is authoritative; absent versions must not become current-document links. */
export function knowledgeCitation(datasetId: string | undefined, hit: RetrieveHit): CitationSource {
  const source = citationSource(datasetId ?? "", {
    ...hit, content: hit.text,
    source_version: hit.source_version ?? undefined,
    source_hash: hit.source_hash ?? undefined,
    metadata: hit.metadata,
  });
  return typeof hit.document_id === "string" && hit.document_id.trim().length > 0
    && hit.source_version != null && hit.source_hash != null && source.provenance === "versioned"
    ? source : { ...source, provenance: "invalid" };
}
