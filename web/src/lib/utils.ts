import { type ClassValue, clsx } from "clsx";
import { twMerge } from "tailwind-merge";
import { AxiosError } from "axios";
import type { TFunction } from "i18next";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

/**
 * Generate a UUID with fallback for insecure contexts (non-HTTPS).
 * crypto.randomUUID requires a secure context; this function catches
 * the error and falls back to a Math.random-based implementation.
 */
export function generateUUID(): string {
  try {
    if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
      return crypto.randomUUID();
    }
  } catch {
    // crypto.randomUUID exists but throws in insecure context
  }
  // Fallback implementation (RFC 4122 v4 compliant)
  return 'xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx'.replace(/[xy]/g, (c) => {
    const r = Math.random() * 16 | 0;
    const v = c === 'x' ? r : (r & 0x3 | 0x8);
    return v.toString(16);
  });
}

/**
 * Extract user-friendly error message from various error types.
 * Handles axios errors, standard errors, and unknown error types.
 */
export function getErrorMessage(error: unknown): string {
  // Handle axios errors - extract the detail from response
  if (error instanceof AxiosError) {
    const detail = error.response?.data?.detail;
    if (detail) {
      return typeof detail === "string" ? detail : JSON.stringify(detail);
    }
    // Fallback to status text or axios message
    return error.response?.statusText || error.message;
  }

  // Handle standard Error objects
  if (error instanceof Error) {
    return error.message;
  }

  // Handle string errors
  if (typeof error === "string") {
    return error;
  }

  // Fallback for unknown error types
  return "An unexpected error occurred";
}

const STATUS_PATTERN = /(?:failed:\s*|status(?:=|:)\s*|http\s+)(\d{3})/i;

function toNumber(value: unknown): number | undefined {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (typeof value === "string") {
    const parsed = Number(value);
    if (Number.isFinite(parsed)) return parsed;
  }
  return undefined;
}

function includesAny(text: string, keywords: string[]): boolean {
  return keywords.some((keyword) => text.includes(keyword));
}

function extractErrorDetailValue(value: unknown, depth = 0): string {
  if (depth > 4 || value == null) return "";
  if (typeof value === "string") return value.trim();
  if (typeof value !== "object") return "";

  const record = value as Record<string, unknown>;
  for (const key of ["detail", "message", "error"]) {
    const child = record[key];
    const extracted = extractErrorDetailValue(child, depth + 1);
    if (extracted) return extracted;
  }

  if (record.error_type || record.type || record.code) {
    const message = extractErrorDetailValue(record.message, depth + 1);
    if (message) return message;
  }

  return "";
}

function normalizeErrorDetail(raw: string): string {
  const trimmed = raw.trim();
  if (!trimmed) return "";

  try {
    const detail = extractErrorDetailValue(JSON.parse(trimmed));
    if (detail) return detail;
  } catch {
    // Non-JSON text, keep original payload.
  }

  return trimmed;
}

function isInternalModelRuntimeError(text: string): boolean {
  return includesAny(text, [
    "runtime model call failed",
    "failover_status",
    "provider_unavailable",
    "notimplementederror",
    "runtime model override failed",
  ]);
}

function extractStatus(error: unknown, raw: string): number | undefined {
  if (error instanceof AxiosError) {
    return toNumber(error.response?.status);
  }

  if (error && typeof error === "object") {
    const status = toNumber((error as { status?: unknown }).status);
    if (status) return status;
  }

  const match = raw.match(STATUS_PATTERN);
  if (match?.[1]) {
    return toNumber(match[1]);
  }

  return undefined;
}

export function buildHttpFailureError(prefix: string, status: number, responseText: string): Error {
  const detail = normalizeErrorDetail(responseText);
  const message = detail ? `${prefix}: ${status} ${detail}` : `${prefix}: ${status}`;
  return new Error(message);
}

export function getPlaygroundErrorMessage(error: unknown, t: TFunction): string {
  if (error instanceof Error && error.name === "AbortError") {
    return t("playground.errors.cancelled");
  }

  const raw = getErrorMessage(error);
  const status = extractStatus(error, raw);
  const normalized = normalizeErrorDetail(raw);
  const text = `${raw}\n${normalized}`.toLowerCase();

  if (isInternalModelRuntimeError(text)) {
    if (includesAny(text, ["failover_status=exhausted", "status=exhausted"])) {
      return t("playground.errors.modelFallbackExhausted");
    }
    return t("playground.errors.modelProviderUnavailable");
  }

  if (
    includesAny(text, [
      "billing account",
      "insufficient credits",
      "insufficient balance",
      "spending limit",
    ])
  ) {
    return t("playground.errors.modelQuotaExhausted");
  }

  if (status === 401 || includesAny(text, ["unauthorized", "auth", "token expired"])) {
    return t("playground.errors.authExpired");
  }

  if (status === 403 || includesAny(text, ["forbidden", "permission denied", "access denied"])) {
    return t("playground.errors.forbidden");
  }

  if (status === 404 || includesAny(text, ["not found", "service not found"])) {
    return t("playground.errors.notFound");
  }

  if (
    status === 429 ||
    includesAny(text, [
      "rate limit",
      "too many requests",
      "quota",
      "resource_exhausted",
      "insufficient_quota",
      "model overloaded",
    ])
  ) {
    return t("playground.errors.rateLimited");
  }

  if (status === 422 && includesAny(text, ["invalid assistant", "registered graphs"])) {
    return t("playground.errors.invalidAssistant");
  }

  if (
    status === 502 ||
    status === 503 ||
    includesAny(text, [
      "upstream error",
      "all connection attempts failed",
      "connection refused",
      "name or service not known",
      "service unavailable",
    ])
  ) {
    return t("playground.errors.upstreamUnavailable");
  }

  if (
    status === 408 ||
    status === 504 ||
    includesAny(text, ["timeout", "timed out", "deadline exceeded"])
  ) {
    return t("playground.errors.timeout");
  }

  if (includesAny(text, ["networkerror", "network request failed", "failed to fetch"])) {
    return t("playground.errors.network");
  }

  if (status && status >= 500) {
    return t("playground.errors.serverUnavailable", { status });
  }

  if (status && status >= 400) {
    return t("playground.errors.requestFailedWithStatus", { status });
  }

  return t("playground.errors.unknown");
}

export function getSafePlaygroundAssistantContent(content: string, t: TFunction): string {
  if (!content || typeof content !== "string") return content;
  const normalized = normalizeErrorDetail(content);
  const text = `${content}\n${normalized}`.toLowerCase();
  if (!isInternalModelRuntimeError(text)) return content;
  return getPlaygroundErrorMessage(normalized || content, t);
}
