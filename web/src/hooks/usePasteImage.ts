import { useCallback } from "react";

/**
 * Generate a filename for a pasted image.
 */
function generatePastedImageFilename(mimeType: string): string {
  const ext = mimeType.split("/")[1] || "png";
  const timestamp = new Date().toISOString().replace(/[:.]/g, "-");
  return `pasted-image-${timestamp}.${ext}`;
}

/**
 * Extract image files from clipboard data.
 */
function extractImagesFromClipboard(items: DataTransferItemList): File[] {
  const imageFiles: File[] = [];

  for (let i = 0; i < items.length; i++) {
    const item = items[i];
    if (item.type.startsWith("image/")) {
      const file = item.getAsFile();
      if (file) {
        const namedFile = new File(
          [file],
          generatePastedImageFilename(item.type),
          { type: item.type }
        );
        imageFiles.push(namedFile);
      }
    }
  }

  return imageFiles;
}

/**
 * Hook to handle pasting images from clipboard.
 *
 * @param onFilesSelected - Callback when image files are extracted from paste
 * @returns handlePaste - Event handler for paste events
 *
 * @example
 * ```tsx
 * const handlePaste = usePasteImage((files) => handleFileSelect(files));
 * return <textarea onPaste={handlePaste} />;
 * ```
 */
export function usePasteImage(
  onFilesSelected: (files: FileList) => void
): (e: React.ClipboardEvent) => void {
  return useCallback(
    (e: React.ClipboardEvent) => {
      const items = e.clipboardData?.items;
      if (!items) return;

      const imageFiles = extractImagesFromClipboard(items);

      if (imageFiles.length > 0) {
        e.preventDefault();
        const dataTransfer = new DataTransfer();
        imageFiles.forEach((f) => dataTransfer.items.add(f));
        onFilesSelected(dataTransfer.files);
      }
    },
    [onFilesSelected]
  );
}
