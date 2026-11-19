const DISMISS_STORAGE_PREFIX = "setup-banner-dismissed";

function dismissStorageKey(userId?: string): string {
  return userId ? `${DISMISS_STORAGE_PREFIX}:${userId}` : DISMISS_STORAGE_PREFIX;
}

export function readSetupBannerDismissed(userId?: string): boolean {
  try {
    return localStorage.getItem(dismissStorageKey(userId)) === "1";
  } catch {
    return false;
  }
}

export function writeSetupBannerDismissed(userId?: string): boolean {
  try {
    localStorage.setItem(dismissStorageKey(userId), "1");
    return true;
  } catch {
    return false;
  }
}
