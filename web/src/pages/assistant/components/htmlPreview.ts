import { parseFragment, serialize, type DefaultTreeAdapterMap } from "parse5";

/** Remove handlers before browser parsing; DOMPurify owns final sanitization. */
export function preparePreviewHtml(content: string): string {
  const fragment = parseFragment(content, { scriptingEnabled: false });
  function visit(node: DefaultTreeAdapterMap["node"]): void {
    if ("attrs" in node) {
      node.attrs = node.attrs.filter(attribute => !attribute.name.toLowerCase().startsWith("on"));
    }
    if ("content" in node) visit(node.content);
    if ("childNodes" in node) node.childNodes.forEach(visit);
  }
  visit(fragment);
  return serialize(fragment);
}
