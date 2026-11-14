import { uploadFileWithProgress } from "@/api/files";
import { collabUrl } from "@/api/spaces";
import { getAuthToken } from "@/lib/api";
import { sseFetch } from "@/lib/sse";
import { zh } from "@blocknote/core/locales";
import { withCollaboration } from "@blocknote/core/yjs";
import { useCreateBlockNote } from "@blocknote/react";
import { BlockNoteView } from "@blocknote/shadcn";
import "@blocknote/shadcn/style.css";
import { HocuspocusProvider } from "@hocuspocus/provider";
import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { Sparkles } from "lucide-react";
import { useAppStore } from "@/store/useAppStore";
import { installOverlayFallback } from "../overlay";

type ChatChunk = { choices?: Array<{ delta?: { content?: string } }> };

function seed(blocks: unknown, markdown: string) {
  if (Array.isArray(blocks) && blocks.length > 0) return blocks as never;
  if (!markdown.trim()) return undefined;
  return [{
    id: "seed",
    type: "paragraph",
    props: { textColor: "default", backgroundColor: "default", textAlignment: "left" },
    content: [{ type: "text", text: markdown, styles: {} }],
    children: [],
  }] as never;
}

export function BlockEditor({
  itemId,
  blocks,
  markdown,
  onChange,
}: {
  itemId: string;
  blocks: unknown;
  markdown: string;
  onChange: (blocks: unknown, markdown: string) => void;
}) {
  const { t, i18n } = useTranslation();
  const theme = useAppStore(state => state.resolvedTheme);
  const shell = useRef<HTMLDivElement>(null);
  const change = useRef(onChange);
  change.current = onChange;
  const [ai, setAi] = useState("");
  const [busy, setBusy] = useState(false);
  const provider = useMemo(() => new HocuspocusProvider({
    url: collabUrl(itemId),
    name: itemId,
    token: getAuthToken() ?? undefined,
  }), [itemId]);
  useEffect(() => () => provider.destroy(), [provider]);
  useEffect(() => {
    if (shell.current) installOverlayFallback(shell.current);
  }, [itemId]);
  const editor = useCreateBlockNote(withCollaboration({
    dictionary: i18n.language.toLowerCase().startsWith("zh") ? zh : undefined,
    uploadFile: async (file: File) => {
      const uploaded = await uploadFileWithProgress(file);
      return `/api/v1/files/${encodeURIComponent(uploaded.file_id)}`;
    },
    collaboration: {
      provider: { awareness: provider.awareness ?? undefined },
      fragment: provider.document.getXmlFragment("document-store"),
      user: { name: t("spaces.editor.you"), color: "#958DF1" },
    },
  }), [itemId]);
  useEffect(() => {
    const seeded = seed(blocks, markdown);
    const current = editor.blocksToMarkdownLossy(editor.document).trim();
    if (seeded && !current) editor.replaceBlocks(editor.document, seeded);
    return editor.onChange(() => {
      change.current(editor.document, editor.blocksToMarkdownLossy(editor.document));
    });
  }, [editor, blocks, markdown]);

  async function ask(action: "rewrite" | "continue" | "summarize" | "translate" | "ask") {
    const selected = editor.getSelectedText().trim() || editor.blocksToMarkdownLossy(editor.document);
    const instruction = t(`spaces.editor.ai.${action}`);
    setBusy(true);
    setAi("");
    try {
      for await (const chunk of sseFetch<ChatChunk>("/v1/chat/completions", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          model: "auto",
          stream: true,
          messages: [
            { role: "system", content: instruction },
            { role: "user", content: selected },
          ],
        }),
      })) {
        const delta = chunk.choices?.[0]?.delta?.content;
        if (delta) setAi((current) => current + delta);
      }
    } catch {
      setAi(t("spaces.editor.ai.failed"));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div ref={shell} className="spaces-editor" data-editor-chunk="blocknote">
      <div className="spaces-editor-tools">
        <Button
          id={`space-ai-trigger-${itemId}`}
          type="button"
          variant="secondary"
          size="sm"
          className="spaces-ai-trigger"
          aria-haspopup="menu"
          aria-expanded="false"
          ref={(node) => {
            node?.setAttribute("commandfor", `space-ai-menu-${itemId}`);
            node?.setAttribute("command", "toggle-popover");
          }}
        >
          <Sparkles data-icon="inline-start" aria-hidden="true" />
          {t("spaces.editor.ai.open")}
        </Button>
        <div
          id={`space-ai-menu-${itemId}`}
          popover="auto"
          className="spaces-ai"
          onToggle={(event) => {
            document.getElementById(`space-ai-trigger-${itemId}`)?.setAttribute("aria-expanded", event.newState === "open" ? "true" : "false");
          }}
        >
          <div ref={(node) => node?.setAttribute("focusgroup", "menu nomemory")} role="menu" aria-labelledby={`space-ai-trigger-${itemId}`}>
            {(["rewrite", "continue", "summarize", "translate", "ask"] as const).map((action) => (
              <Button role="menuitem" variant="ghost" size="sm" key={action} type="button" disabled={busy} onClick={() => void ask(action)}>
                {t(`spaces.editor.ai.${action}Label`)}
              </Button>
            ))}
          </div>
          <pre>{ai}</pre>
        </div>
      </div>
      <BlockNoteView editor={editor} theme={theme} className="spaces-blocknote" />
    </div>
  );
}
