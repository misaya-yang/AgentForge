export const UPLOAD_RECEIPT_PREFIX = "kb-upload-receipts:";
const MAX_AGE_MS = 24 * 60 * 60 * 1000;
const MAX_STORAGE_BYTES = 256 * 1024;
const MAX_ATTEMPTS = 10;
const STATES = new Set(["not_submitted", "unknown", "accepted", "rejected", "created"]);
let storageRetired = false;

export interface UploadRecoveryFile {
  position: number;
  name: string;
  sizeBytes: number;
  state: "not_submitted" | "unknown" | "accepted" | "rejected" | "created";
  documentId?: string;
}
export interface UploadRecoveryReceipt {
  id: string;
  createdAt: number;
  files: UploadRecoveryFile[];
}

/** Called by the global auth lifecycle, even when Knowledge has never mounted. */
export function clearUploadRecoverySession(): void {
  try {
    const keys = Array.from({ length: sessionStorage.length }, (_, index) => sessionStorage.key(index));
    keys.filter((key): key is string => Boolean(key?.startsWith(UPLOAD_RECEIPT_PREFIX)))
      .forEach((key) => sessionStorage.removeItem(key));
  } catch {
    // If old metadata cannot be removed, do not revive it in this document.
    storageRetired = true;
  }
}

export function uploadRecoveryKey(authority: string, tenantId?: string, datasetId?: string): string | null {
  return authority && tenantId && datasetId
    ? `${UPLOAD_RECEIPT_PREFIX}${JSON.stringify([authority, tenantId, datasetId])}` : null;
}

/** Remove expired metadata across datasets whenever recovery storage is accessed. */
function pruneExpiredUploadRecovery(now: number): void {
  const keys = Array.from({ length: sessionStorage.length }, (_, index) => sessionStorage.key(index));
  for (const key of keys) {
    if (!key?.startsWith(UPLOAD_RECEIPT_PREFIX)) continue;
    const raw = sessionStorage.getItem(key);
    if (!raw || raw.length > MAX_STORAGE_BYTES) continue;
    let records: unknown;
    try { records = JSON.parse(raw); } catch { continue; }
    if (!Array.isArray(records)) continue;
    const retained = records.filter((record) => !(record && Number.isFinite(record.createdAt)
      && record.createdAt <= now - MAX_AGE_MS));
    if (retained.length === records.length) continue;
    if (retained.length) sessionStorage.setItem(key, JSON.stringify(retained));
    else sessionStorage.removeItem(key);
  }
}

export function readUploadRecovery(key: string, now = Date.now()): UploadRecoveryReceipt[] {
  if (storageRetired) throw new Error("Upload recovery storage unavailable");
  pruneExpiredUploadRecovery(now);
  const raw = sessionStorage.getItem(key);
  if (!raw) return [];
  if (new TextEncoder().encode(raw).byteLength > MAX_STORAGE_BYTES) throw new Error("Upload recovery storage limit");
  const value: unknown = JSON.parse(raw);
  if (!Array.isArray(value) || value.length > MAX_ATTEMPTS) throw new Error("Invalid upload recovery receipt");
  const ids = new Set<string>();
  for (const receipt of value) {
    if (!receipt || typeof receipt.id !== "string" || !receipt.id || receipt.id.length > 128 || ids.has(receipt.id)
      || !Number.isFinite(receipt.createdAt) || receipt.createdAt > now
      || !Array.isArray(receipt.files) || receipt.files.length < 1 || receipt.files.length > 50) {
      throw new Error("Invalid upload recovery receipt");
    }
    ids.add(receipt.id);
    for (const [index, file] of receipt.files.entries()) {
      if (!file || file.position !== index + 1 || typeof file.name !== "string" || file.name.length > 512
        || !Number.isSafeInteger(file.sizeBytes) || file.sizeBytes < 0 || !STATES.has(file.state)
        || (file.documentId !== undefined && (typeof file.documentId !== "string" || !file.documentId || file.documentId.length > 255))
        || (file.state === "created" && !file.documentId)
        || (file.documentId && !["created", "accepted"].includes(file.state))) {
        throw new Error("Invalid upload recovery file");
      }
    }
  }
  return (value as UploadRecoveryReceipt[]).filter((receipt) => now - receipt.createdAt < MAX_AGE_MS)
    .map((receipt) => ({ id: receipt.id, createdAt: receipt.createdAt,
      files: receipt.files.map(({ position, name, sizeBytes, state, documentId }) =>
        ({ position, name, sizeBytes, state, ...(documentId ? { documentId } : {}) })),
    }));
}

export function writeUploadRecovery(key: string, receipts: UploadRecoveryReceipt[]): void {
  if (storageRetired) throw new Error("Upload recovery storage unavailable");
  if (!receipts.length) { sessionStorage.removeItem(key); return; }
  pruneExpiredUploadRecovery(Date.now());
  const raw = JSON.stringify(receipts);
  let bytes = new TextEncoder().encode(key + raw).byteLength;
  for (let index = 0; index < sessionStorage.length; index += 1) {
    const otherKey = sessionStorage.key(index);
    if (otherKey && otherKey !== key && otherKey.startsWith(UPLOAD_RECEIPT_PREFIX)) {
      bytes += new TextEncoder().encode(otherKey + (sessionStorage.getItem(otherKey) ?? "")).byteLength;
    }
  }
  if (receipts.length > MAX_ATTEMPTS || bytes > MAX_STORAGE_BYTES) throw new Error("Upload recovery storage limit");
  sessionStorage.setItem(key, raw);
}

export function beginUploadRecovery(key: string, files: File[]): UploadRecoveryReceipt {
  if (!files.length || files.length > 50 || files.some((file) => file.name.length > 512)) throw new Error("Upload recovery file limit");
  const receipts = readUploadRecovery(key);
  const receipt: UploadRecoveryReceipt = { id: crypto.randomUUID(), createdAt: Date.now(),
    files: files.map((file, index) => ({ position: index + 1, name: file.name, sizeBytes: file.size, state: "not_submitted" })),
  };
  writeUploadRecovery(key, [...receipts, receipt]);
  return receipt;
}

/** Exact local attempt CAS. A dismissed or expired receipt is never recreated. */
export function updateUploadRecovery(key: string, receipt: UploadRecoveryReceipt): boolean {
  const receipts = readUploadRecovery(key);
  const index = receipts.findIndex((item) => item.id === receipt.id);
  if (index < 0) return false;
  receipts[index] = receipt;
  writeUploadRecovery(key, receipts);
  return true;
}
