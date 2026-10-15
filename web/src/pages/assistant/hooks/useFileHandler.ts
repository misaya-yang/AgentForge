import { useState, useCallback, useRef, useLayoutEffect } from "react";
import {
  uploadFileWithProgress,
  isFileTypeSupported,
  type FileUploadResponse,
} from "@/api/files";
import { usePasteImage } from "@/hooks/usePasteImage";
import { generateUUID } from "@/lib/utils";
import { useAuthStore } from "@/store/useAuthStore";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";
import { planAssistantFileSelection } from "../fileSelection";

export interface UploadedFile {
  id: string;
  file: File;
  status: "pending" | "uploading" | "success" | "error";
  selected: boolean;
  progress?: number;
  errorCode?: "unsupported" | "too_large" | "upload_failed";
  response?: FileUploadResponse;
}

const MAX_ASSISTANT_ATTACHMENT_BYTES = 32 * 1024 * 1024;

export function useFileHandler(sessionKey: string) {
  const [filesBySession, setFilesBySession] = useState<Record<string, UploadedFile[]>>({});
  const filesBySessionRef = useRef<Record<string, UploadedFile[]>>({});
  const activeUploads = useRef(new Map<string, AbortController>());
  const mounted = useRef(true);
  const authorityEpoch = useRef(0);
  const epoch = authorityEpoch.current;
  const isCurrent = useCallback(() => {
    const auth = useAuthStore.getState();
    return mounted.current && epoch === authorityEpoch.current && auth.isAuthenticated && Boolean(auth.token && auth.user);
  }, [epoch]);
  useLayoutEffect(() => {
    mounted.current = true;
    const uploads = activeUploads.current;
    const unsubscribe = useAuthStore.subscribe((state, previous) => {
      if (state.token !== previous.token || state.isAuthenticated !== previous.isAuthenticated || sdkAuthorityScope(state.user) !== sdkAuthorityScope(previous.user)) {
        authorityEpoch.current += 1;
        uploads.forEach((controller) => controller.abort());
        uploads.clear();
        filesBySessionRef.current = {};
        setFilesBySession({});
        setNoticesBySession({});
      }
    });
    return () => {
      unsubscribe();
      mounted.current = false;
      uploads.forEach((controller) => controller.abort());
      uploads.clear();
    };
  }, []);
  const [noticesBySession, setNoticesBySession] = useState<Record<string, string>>({});
  const fileInputRef = useRef<HTMLInputElement>(null);
  const files = filesBySession[sessionKey] ?? [];
  const isUploading = files.some((entry) => entry.status === "pending" || entry.status === "uploading");
  const selectionNotice = noticesBySession[sessionKey] ?? "";

  const updateSessionFiles = useCallback((key: string, update: (entries: UploadedFile[]) => UploadedFile[]) => {
    const next = update(filesBySessionRef.current[key] ?? []);
    filesBySessionRef.current = { ...filesBySessionRef.current, [key]: next };
    setFilesBySession(filesBySessionRef.current);
  }, []);

  const uploadFiles = useCallback(async (entries: UploadedFile[]) => {
    // Resolve by stable ID on every step: draft binding can move a queued file.
    const findKey = (id: string) => Object.keys(filesBySessionRef.current).find((key) =>
      filesBySessionRef.current[key].some((item) => item.id === id),
    );
    for (const entry of entries) {
      if (!isCurrent()) return;
      const key = findKey(entry.id);
      if (key === undefined || activeUploads.current.has(entry.id)) continue;
      const controller = new AbortController();
      activeUploads.current.set(entry.id, controller);
      const update = (change: (item: UploadedFile) => UploadedFile) => {
        if (!isCurrent() || controller.signal.aborted || activeUploads.current.get(entry.id) !== controller) return;
        const currentKey = findKey(entry.id);
        if (currentKey !== undefined) updateSessionFiles(currentKey, (current) =>
          current.map((item) => item.id === entry.id ? change(item) : item),
        );
      };
      update((item) => ({ ...item, status: "uploading", progress: 0, errorCode: undefined }));
      try {
        const response = await uploadFileWithProgress(entry.file, (event) => {
          update((item) => ({ ...item, progress: event.percent }));
        }, true, controller.signal);
        update((item) => ({ ...item, status: "success", selected: true, response, progress: 100 }));
      } catch (error) {
        const status = (error as { response?: { status?: number } })?.response?.status;
        update((item) => ({ ...item, status: "error", selected: false,
          errorCode: status === 413 ? "too_large" : "upload_failed" }));
      } finally {
        if (activeUploads.current.get(entry.id) === controller) activeUploads.current.delete(entry.id);
      }
    }
  }, [updateSessionFiles, isCurrent]);

  const handleFileSelect = useCallback((selectedFiles: FileList | null) => {
    if (!isCurrent() || !selectedFiles?.length) return;
    const incoming = Array.from(selectedFiles);
    const plan = planAssistantFileSelection(incoming, filesBySessionRef.current[sessionKey]?.length ?? 0);
    const accepted = plan.accepted.map((file): UploadedFile => {
      const supported = isFileTypeSupported(file);
      const validSize = file.size > 0 && file.size <= MAX_ASSISTANT_ATTACHMENT_BYTES;
      return {
        id: generateUUID(),
        file,
        status: supported && validSize ? "pending" : "error",
        selected: false,
        errorCode: !supported ? "unsupported" : !validSize ? "too_large" : undefined,
      };
    });
    setNoticesBySession((current) => ({
      ...current,
      [sessionKey]: plan.exceedsLimit ? "limit" : "",
    }));
    updateSessionFiles(sessionKey, (current) => [...current, ...accepted]);
    void uploadFiles(accepted.filter((entry) => entry.status === "pending"));
  }, [sessionKey, updateSessionFiles, uploadFiles, isCurrent]);

  const handlePaste = usePasteImage(handleFileSelect);

  const removeFile = useCallback((index: number) => {
    if (!isCurrent()) return;
    const entry = filesBySessionRef.current[sessionKey]?.[index];
    if (entry) activeUploads.current.get(entry.id)?.abort();
    updateSessionFiles(sessionKey, (current) => current.filter((_, itemIndex) => itemIndex !== index));
  }, [sessionKey, updateSessionFiles, isCurrent]);

  const retryFile = useCallback((index: number) => {
    if (!isCurrent()) return;
    const entry = filesBySessionRef.current[sessionKey]?.[index];
    if (!entry || entry.status !== "error" || entry.errorCode === "unsupported" || entry.errorCode === "too_large") return;
    void uploadFiles([entry]);
  }, [sessionKey, uploadFiles, isCurrent]);

  const toggleFileSelection = useCallback((index: number) => {
    if (!isCurrent()) return;
    updateSessionFiles(sessionKey, (current) => current.map((entry, itemIndex) =>
      itemIndex === index && entry.status === "success"
        ? { ...entry, selected: !entry.selected }
        : entry,
    ));
  }, [sessionKey, updateSessionFiles, isCurrent]);

  const consumeSelectedFiles = useCallback(() => {
    if (!isCurrent()) return;
    updateSessionFiles(sessionKey, (current) => current.filter((entry) =>
      !(entry.status === "success" && entry.selected),
    ));
  }, [sessionKey, updateSessionFiles, isCurrent]);

  const clearFiles = useCallback(() => {
    if (!isCurrent()) return;
    filesBySessionRef.current[sessionKey]?.forEach((entry) => activeUploads.current.get(entry.id)?.abort());
    updateSessionFiles(sessionKey, () => []);
  }, [sessionKey, updateSessionFiles, isCurrent]);

  const rekeyFiles = useCallback((fromKey: string, toKey: string) => {
    if (!isCurrent()) return;
    if (fromKey === toKey) return;
    const incoming = filesBySessionRef.current[fromKey] ?? [];
    if (!incoming.length) return;
    const existing = filesBySessionRef.current[toKey] ?? [];
    const ids = new Set(existing.map((entry) => entry.id));
    filesBySessionRef.current = {
      ...filesBySessionRef.current,
      [fromKey]: [],
      [toKey]: [...existing, ...incoming.filter((entry) => !ids.has(entry.id))],
    };
    setFilesBySession(filesBySessionRef.current);
    setNoticesBySession((current) => ({ ...current, [toKey]: current[fromKey] ?? "", [fromKey]: "" }));
  }, [isCurrent]);

  return {
    files,
    isUploading,
    selectionNotice,
    fileInputRef,
    handleFileSelect,
    handlePaste,
    removeFile,
    retryFile,
    toggleFileSelection,
    consumeSelectedFiles,
    clearFiles,
    rekeyFiles,
  };
}
