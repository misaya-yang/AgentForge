import { useAuthStore } from "@/store/useAuthStore";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";

/** A retired save cannot resume under a later session or a restored snapshot. */
export function knowledgeSaveAuthority(isViewCurrent: () => boolean = () => true) {
  const original = useAuthStore.getState();
  const token = original.token;
  const authority = sdkAuthorityScope(original.user);
  let retired = false;
  function isCurrent() {
    const now = useAuthStore.getState();
    if (!token || !original.user || now.token !== token || sdkAuthorityScope(now.user) !== authority
      || !now.hasPermission("console:eval:run") || !isViewCurrent()) retired = true;
    return !retired;
  }
  const unsubscribe = useAuthStore.subscribe(() => { isCurrent(); });
  return {
    isCurrent,
    assertCurrent() { if (!isCurrent()) throw new Error("Knowledge save no longer belongs to the current view or account"); },
    close() { retired = true; unsubscribe(); },
  };
}
