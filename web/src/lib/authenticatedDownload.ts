import { api } from "@/lib/api";

/** Keep file errors actionable without exposing server bodies or concealed existence. */
export function artifactAccessErrorKey(error: unknown, operation: "download" | "preview" = "download"): string {
  const value = error && typeof error === "object"
    ? error as { response?: { status?: unknown }; code?: unknown; name?: unknown }
    : undefined;
  const status = value?.response?.status;
  if (status === 401) return "artifact.errors.signIn";
  if (status === 403) return "artifact.errors.accessDenied";
  // A 404 also conceals another owner's/tenant's file; do not label it expired/deleted.
  if (status === 404) return "artifact.errors.unavailableForAccount";
  if (status === 410) return "artifact.errors.expired";
  if (status === 409) return "artifact.errors.unfinished";
  if (status === 413) return "artifact.errors.tooLarge";
  if (status === 429) return "artifact.errors.rateLimited";
  if (typeof status === "number" && status >= 500 && status <= 599) {
    return "artifact.errors.serviceUnavailable";
  }
  if (value?.name === "ArtifactPreviewBlockedError") return "artifact.errors.popupBlocked";
  if (["ERR_NETWORK", "ECONNABORTED", "ETIMEDOUT"].includes(String(value?.code))) {
    return "artifact.errors.connectionFailed";
  }
  return operation === "preview" ? "artifact.errors.previewFailed" : "artifact.errors.downloadFailed";
}

function protectedApiPath(url: string): boolean {
  return url.startsWith("/api/") && !url.startsWith("//");
}

function permittedDirectUrl(url: string): boolean {
  return /^https?:\/\//i.test(url) || /^(blob:|data:)/i.test(url);
}

async function resolveArtifactUrl(url: string): Promise<{ href: string; temporary: boolean }> {
  if (protectedApiPath(url)) {
    const response = await api.get<Blob>(url, { responseType: "blob" });
    return { href: URL.createObjectURL(response.data), temporary: true };
  }
  if (permittedDirectUrl(url)) return { href: url, temporary: false };
  throw new Error("Unsupported artifact URL");
}

export async function downloadAssistantArtifact(url: string, filename: string): Promise<void> {
  const resolved = await resolveArtifactUrl(url);
  const link = document.createElement("a");
  link.href = resolved.href;
  link.download = filename;
  link.rel = "noopener noreferrer";
  document.body.appendChild(link);
  link.click();
  link.remove();
  if (resolved.temporary) window.setTimeout(() => URL.revokeObjectURL(resolved.href), 60_000);
}

export async function openAssistantArtifact(url: string): Promise<void> {
  if (!protectedApiPath(url)) {
    if (!permittedDirectUrl(url)) throw new Error("Unsupported artifact URL");
    window.open(url, "_blank", "noopener,noreferrer");
    return;
  }
  const previewWindow = window.open("about:blank", "_blank");
  if (!previewWindow) {
    const error = new Error("Preview popup was blocked");
    error.name = "ArtifactPreviewBlockedError";
    throw error;
  }
  try {
    const resolved = await resolveArtifactUrl(url);
    previewWindow.location.replace(resolved.href);
    window.setTimeout(() => URL.revokeObjectURL(resolved.href), 60_000);
  } catch (error) {
    previewWindow?.close();
    throw error;
  }
}
