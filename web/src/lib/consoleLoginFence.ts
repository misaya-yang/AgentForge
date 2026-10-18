import { sdkAuthorityScope } from "./sdkAuthorityScope.ts";
interface Identity { token: string | null; csrfToken: string | null; isAuthenticated: boolean; user: Parameters<typeof sdkAuthorityScope>[0] }
/** Fence one interactive login independently of store loading flags. Own failed
 * anonymous logins may clear auth; that must still display the 401 error. */
export function consoleLoginFence(read: () => Identity, generation: () => number, subscribe: (listener: () => void) => () => void, mounted: () => boolean) {
  const initial = read(), serial = generation(), authority = sdkAuthorityScope(initial.user); let retired = false;
  const unchanged = () => { const state = read(); return state.token === initial.token && state.csrfToken === initial.csrfToken && state.isAuthenticated === initial.isAuthenticated && sdkAuthorityScope(state.user) === authority; };
  const dispose = subscribe(() => { if (!unchanged()) retired = true; });
  const current = () => mounted() && !retired && unchanged() && generation() === serial;
  return { current, dispose, ownsFailure(status?: number) {
    return current() || (status === 401 && mounted() && !retired && unchanged() && !initial.isAuthenticated && generation() === serial + 1);
  } };
}
