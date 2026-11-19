import { useMemo } from "react";
import DOMPurify from "dompurify";
import { preparePreviewHtml } from "./htmlPreview";

export default function HtmlDocumentContent({ content }: { content: string }) {
  const html = useMemo(() => DOMPurify.sanitize(preparePreviewHtml(content), { RETURN_TRUSTED_TYPE: true }), [content]);
  return <div dangerouslySetInnerHTML={{ __html: html }} />;
}
