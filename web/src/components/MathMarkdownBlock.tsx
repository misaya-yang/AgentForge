import type { ReactNode } from "react";
import ReactMarkdown, { defaultUrlTransform, type Options as MarkdownOptions } from "react-markdown";
import rehypeKatex from "rehype-katex";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";
import "katex/dist/katex.min.css";

// Mutable PluggableList (react-markdown's plugin prop types); `as const` would
// make them readonly and no longer assignable.
const REMARK_PLUGINS: NonNullable<MarkdownOptions["remarkPlugins"]> = [
  remarkGfm,
  [remarkMath, { singleDollarTextMath: true }],
];
const REHYPE_PLUGINS: NonNullable<MarkdownOptions["rehypePlugins"]> = [
  [rehypeKatex, { throwOnError: false, strict: false, output: "htmlAndMathml" }],
];

function allowDataUrlTransform(url: string): string {
  return url.startsWith("data:image/") ? url : defaultUrlTransform(url);
}

export function MathMarkdownBlock({
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
    <ReactMarkdown
      remarkPlugins={REMARK_PLUGINS}
      rehypePlugins={REHYPE_PLUGINS}
      urlTransform={allowDataUrlTransform}
      components={components}
    >
      {text}
    </ReactMarkdown>
  );
}
