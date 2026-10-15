/**
 * useLatexCopy - Hook for copying LaTeX formulas with original source code
 *
 * When users copy rendered math formulas (KaTeX), this hook intercepts the copy event
 * and replaces the clipboard content with the original LaTeX source code.
 *
 * This provides a better UX similar to ChatGPT, Notion, and other modern apps.
 *
 * Usage:
 *   useLatexCopy(); // Call in a component that renders math content
 *
 * Requirements:
 *   - KaTeX must be configured with output: 'htmlAndMathml' to include annotation elements
 */

import { useEffect } from "react";

/**
 * Extract LaTeX source from a KaTeX element
 * KaTeX with htmlAndMathml output includes: <annotation encoding="application/x-tex">...</annotation>
 */
function extractLatexFromKatex(katexElement: Element): string | null {
  // Try to find the annotation element with the original LaTeX
  const annotation = katexElement.querySelector(
    'annotation[encoding="application/x-tex"]'
  );
  if (annotation?.textContent) {
    return annotation.textContent;
  }

  // Fallback: try to get from data attribute (if we add it later)
  const dataLatex = katexElement.getAttribute("data-latex");
  if (dataLatex) {
    return dataLatex;
  }

  return null;
}

/**
 * Check if an element is a display-mode (block) math element
 */
function isDisplayMath(element: Element): boolean {
  return (
    element.classList.contains("katex-display") ||
    element.closest(".katex-display") !== null
  );
}

/**
 * Process selected content and replace KaTeX elements with original LaTeX
 * Returns the processed text with LaTeX formulas preserved
 */
function processSelectionForLatex(selection: Selection): string | null {
  if (!selection.rangeCount) return null;

  const range = selection.getRangeAt(0);
  const container = document.createElement("div");
  container.appendChild(range.cloneContents());

  // Find all KaTeX elements in the selection
  const katexElements = container.querySelectorAll(".katex");

  if (katexElements.length === 0) {
    // No math formulas in selection, let browser handle normally
    return null;
  }

  // Process each KaTeX element
  katexElements.forEach((katexEl) => {
    const latex = extractLatexFromKatex(katexEl);
    if (latex) {
      // Determine delimiter based on display mode
      const isDisplay = isDisplayMath(katexEl);
      const delimiter = isDisplay ? "$$" : "$";
      const latexWithDelimiter = `${delimiter}${latex}${delimiter}`;

      // Replace the KaTeX element with a text node containing the LaTeX
      const textNode = document.createTextNode(latexWithDelimiter);
      katexEl.parentNode?.replaceChild(textNode, katexEl);
    }
  });

  // Also handle .katex-display wrappers (they contain .katex inside)
  const displayWrappers = container.querySelectorAll(".katex-display");
  displayWrappers.forEach((wrapper) => {
    // If the wrapper still contains rendered HTML (not replaced), try to extract
    const innerKatex = wrapper.querySelector(".katex");
    if (innerKatex) {
      const latex = extractLatexFromKatex(innerKatex);
      if (latex) {
        const textNode = document.createTextNode(`$$${latex}$$`);
        wrapper.parentNode?.replaceChild(textNode, wrapper);
      }
    }
  });

  // Return the processed text content
  return container.textContent || container.innerText;
}

/**
 * Hook to enable copying LaTeX source when math formulas are selected
 *
 * @param enabled - Whether the hook is active (default: true)
 */
export function useLatexCopy(enabled: boolean = true): void {
  useEffect(() => {
    if (!enabled) return;

    const handleCopy = (event: ClipboardEvent) => {
      const selection = window.getSelection();
      if (!selection || selection.isCollapsed) return;

      // Check if selection contains any KaTeX elements
      const selectionString = selection.toString();
      if (!selectionString.trim()) return;

      // Check if the common ancestor contains katex elements
      const range = selection.getRangeAt(0);
      const commonAncestor = range.commonAncestorContainer;
      const ancestorElement =
        commonAncestor instanceof Element
          ? commonAncestor
          : commonAncestor.parentElement;

      if (!ancestorElement) return;

      // Quick check: does the selection area contain any katex?
      const hasKatex =
        ancestorElement.querySelector(".katex") !== null ||
        ancestorElement.closest(".katex") !== null;

      if (!hasKatex) return;

      // Process the selection and get text with LaTeX
      const processedText = processSelectionForLatex(selection);

      if (processedText && processedText !== selectionString) {
        // Prevent default copy behavior
        event.preventDefault();

        // Set clipboard data with processed text
        if (event.clipboardData) {
          event.clipboardData.setData("text/plain", processedText);
        }
      }
    };

    document.addEventListener("copy", handleCopy);

    return () => {
      document.removeEventListener("copy", handleCopy);
    };
  }, [enabled]);
}

export default useLatexCopy;
