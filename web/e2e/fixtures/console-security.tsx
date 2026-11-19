import { useState } from "react";
import { createRoot } from "react-dom/client";
import { DocumentPreview } from "../../src/pages/assistant/components/DocumentPreview";
import { MathMarkdownBlock } from "../../src/components/MathMarkdownBlock";
import { initializeI18n } from "../../src/i18n";
import "../../src/index.css";

export function Fixture() {
  const [changed, setChanged] = useState(false);
  return <main>
    <h1>Console security fixture</h1>
    <button onClick={() => setChanged(true)}>Update preview</button>
    <DocumentPreview title="Sanitized HTML" format="html" defaultExpanded content={
      `<p>${changed ? "Updated safely" : "Preview ready"}</p><img alt="safe pixel" src="data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7" onload="window.__xss=1"><script>window.__xss=2</script><a href="javascript:window.__xss=3">unsafe link</a>`
    } />
    <MathMarkdownBlock text={"# Markdown preview\n\n**Safe text** &copy; &lt;img&gt; and $x^2 + y^2 = z^2$."}
      components={{ img: props => <img {...props} />, a: props => <a {...props} /> }} />
  </main>;
}
await initializeI18n();
createRoot(document.getElementById("root")!).render(<Fixture />);
