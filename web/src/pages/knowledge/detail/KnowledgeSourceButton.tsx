import { useState } from "react";
import { useTranslation } from "react-i18next";
import { ExternalLink } from "lucide-react";
import type { RetrieveHit } from "@/types/knowledge";
import { SourceDocumentDialog } from "@/pages/assistant/components/SourceDocumentDialog";
import type { CitationSource } from "@/pages/assistant/components/citationSource";
import { knowledgeCitation } from "./knowledgeCitation";

/** Use the saved hit, never a source URL or a newly resolved document version. */
export function KnowledgeSourceButton({ datasetId, hit }: { datasetId?: string; hit: RetrieveHit }) {
  const { t } = useTranslation();
  const [source, setSource] = useState<CitationSource | null>(null);
  return (
    <>
      <button type="button" onClick={() => setSource(knowledgeCitation(datasetId, hit))}
        className="inline-flex items-center gap-1 text-xs text-primary hover:underline">
        <ExternalLink className="h-3 w-3" />
        {t("assistant.openSource", "Open source")}
      </button>
      <SourceDocumentDialog source={source} onClose={() => setSource(null)} />
    </>
  );
}
