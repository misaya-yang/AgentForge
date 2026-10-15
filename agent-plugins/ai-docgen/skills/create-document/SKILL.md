---
name: create-document
description: Create a polished downloadable DOCX, PPTX, XLSX, or PDF when the user asks for a document, presentation, spreadsheet, or report artifact.
compatibility: Requires the AI Gateway capability worker with Office generation enabled.
allowed-tools: generate_document
metadata:
  discovery_delegate_tool: mcp_docgen__generate_document
---

Use the `generate_document` capability whenever the user asks for a downloadable
Word document, presentation, spreadsheet, or PDF.

Before calling the tool:

1. Build the complete factual content as Markdown. Preserve user-provided facts
   and do not invent names, metrics, citations, or business claims.
2. Choose the requested format (`docx`, `pptx`, `xlsx`, or `pdf`) and a locale
   matching the user's language.
3. Pass the complete Markdown through `body_markdown`; do not send only a short
   topic or outline when the user expects a substantive artifact.
4. State the audience and desired outcome concisely in `goal`.

After the call, return the generated artifact link. If `used_llm` is false or
`critic_passed` is false, disclose that quality limitation instead of claiming
the document was fully reviewed.
