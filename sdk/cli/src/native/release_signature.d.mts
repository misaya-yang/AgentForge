export const RELEASE_TARGETS: readonly string[];
export function canonical(value: unknown): string;
export function verifyReleaseSignature(receipt: unknown, keys: Record<string,string> | undefined): void;
