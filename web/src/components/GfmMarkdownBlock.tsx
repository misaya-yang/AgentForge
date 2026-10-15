import type { ReactNode } from "react";
import ReactMarkdown, { defaultUrlTransform, type Options as MarkdownOptions } from "react-markdown";
import remarkGfm from "remark-gfm";

// Mutable PluggableList (react-markdown's remarkPlugins type); `as const` would
// make it readonly and no longer assignable.
const REMARK_PLUGINS: NonNullable<MarkdownOptions["remarkPlugins"]> = [remarkGfm];

function allowDataUrlTransform(url: string): string {
  return url.startsWith("data:image/") ? url : defaultUrlTransform(url);
}

export function GfmMarkdownBlock({
  text,
  components,
}: {
  text: string;
  components: {
    img: (props: { src?: string; alt?: string }) => ReactNode;
    a: (props: { href?: string; children?: ReactNode }) => ReactNode;
  };
}) {
  return (
    <ReactMarkdown remarkPlugins={REMARK_PLUGINS} urlTransform={allowDataUrlTransform} components={components}>
      {text}
    </ReactMarkdown>
  );
}
