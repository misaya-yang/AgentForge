import type { Document, OffsetPage } from "@/types/knowledge";

export function mergeDocumentPage(updated: Document) {
  return (page: OffsetPage<Document> | undefined): OffsetPage<Document> | undefined =>
    page
      ? {
          ...page,
          items: page.items.map((document) =>
            document.document_id === updated.document_id
              ? { ...document, ...updated }
              : document
          ),
        }
      : page;
}
