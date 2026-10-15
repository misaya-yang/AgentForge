import { useCallback, useRef, useState } from "react";

import { DatasetUploadDialog } from "@/pages/knowledge/detail/DatasetUploadDialog";
import type { Dataset } from "@/types/knowledge";

interface UseDatasetUploadDialogOptions {
  datasetId?: string;
  dataset?: Dataset;
}

export function useDatasetUploadDialog({
  datasetId,
  dataset,
}: UseDatasetUploadDialogOptions) {
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const [open, setOpen] = useState(false);
  const [pendingFiles, setPendingFiles] = useState<File[]>([]);
  const [uploading, setUploading] = useState(false);

  const openFilePicker = useCallback(() => {
    fileInputRef.current?.click();
  }, []);

  const handleFilesSelected = useCallback(
    (files?: FileList | null) => {
      if (!files || files.length === 0) return;
      const newFiles = Array.from(files);
      setPendingFiles((current) => (open ? [...current, ...newFiles] : newFiles));
      setOpen(true);
      if (fileInputRef.current) fileInputRef.current.value = "";
    },
    [open]
  );

  return {
    fileInputRef,
    uploading,
    openFilePicker,
    handleFilesSelected,
    dialog: (
      <DatasetUploadDialog
        datasetId={datasetId}
        dataset={dataset}
        open={open}
        pendingFiles={pendingFiles}
        uploading={uploading}
        onOpenChange={setOpen}
        onPendingFilesChange={setPendingFiles}
        onFilesSelected={handleFilesSelected}
        onUploadingChange={setUploading}
        onOpenFilePicker={openFilePicker}
      />
    ),
  };
}
