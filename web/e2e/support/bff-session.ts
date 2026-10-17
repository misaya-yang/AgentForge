import type { BrowserContext } from "@playwright/test";
export const CONSOLE_COOKIE = "__Host-agentforge_session";
export type ExistingCredentials = { email: string; password: string };
export interface BrowserSession {
  user: { user_id: string; tenant_id: string; force_password_change?: boolean; [key: string]: unknown };
  csrf_token: string;
  expires_at: number;
}
/** Fail before browser/account activity; self-signed TLS needs an operator-trusted CA. */
export function requireHttpsConsoleOrigin(value: string | undefined): string {
  let url: URL;
  try { url=new URL(value ?? ""); } catch { throw Error("Console E2E requires an explicit HTTPS E2E_BASE_URL and active BFF configuration"); }
  if (url.protocol!=="https:" || !url.hostname || url.username || url.password || url.search || url.hash || url.pathname!=="/") {
    throw Error("Console E2E requires an HTTPS origin without credentials/path/query and active BFF configuration");
  }
  return url.origin;
}
export function existingCredentials(options:{providedEmail?:string;providedPassword?:string;persistedCredentials:ExistingCredentials|null;provisionAccounts?:boolean}):ExistingCredentials {
  if (options.provisionAccounts) throw Error("Console BFF E2E is existing-account-only; use a separate explicit API fixture for account initialization");
  const {providedEmail,providedPassword,persistedCredentials}=options;
  if(Boolean(providedEmail)!==Boolean(providedPassword)) throw Error("E2E_USER_EMAIL and E2E_USER_PASSWORD must be provided together");
  const selected=providedEmail && providedPassword ? {email:providedEmail,password:providedPassword} : persistedCredentials;
  if(!selected?.email.trim() || !selected.password) throw Error("Existing-account E2E requires configured credentials; account creation is disabled");
  return {email:selected.email.trim(),password:selected.password};
}
const browserHeaders=(origin:string)=>({Origin:origin,"Sec-Fetch-Site":"same-origin"});
function validSession(value:unknown):value is BrowserSession {
  if(!value || typeof value!=="object")return false;
  if ("access_token" in value) return false;
  const s=value as Partial<BrowserSession>;
  return typeof s.csrf_token==="string" && s.csrf_token.length>0 && typeof s.expires_at==="number" && Number.isFinite(s.expires_at)
    && typeof s.user?.user_id==="string" && s.user.user_id.length>0 && typeof s.user.tenant_id==="string" && s.user.tenant_id.length>0;
}
/** Real BFF API over the browser context's cookie jar; never reads its HttpOnly value into JS storage. */
export async function authenticateConsoleContext(context:BrowserContext, baseURL:string, credentials:ExistingCredentials):Promise<BrowserSession> {
  const origin=requireHttpsConsoleOrigin(baseURL);
  if(!credentials.email.trim() || !credentials.password) throw Error("Existing Console credentials required");
  const response=await context.request.post(`${origin}/api/auth/session`,{data:credentials,headers:browserHeaders(origin),maxRedirects:0,failOnStatusCode:false});
  if(response.status()!==200) throw Error(`HTTPS BFF login failed (${response.status()}); verify BFF origins, signing configuration and existing account`);
  const login=await response.json() as {session_type?:unknown;csrf_token?:unknown;access_token?:unknown;force_password_change?:unknown;user?:{user_id?:unknown}};
  if(login.session_type!=="cookie" || typeof login.csrf_token!=="string" || !login.csrf_token || "access_token" in login || typeof login.user?.user_id!=="string") throw Error("BFF login contract missing; bearer fallback is forbidden");
  if(login.force_password_change===true) throw Error("Existing-account E2E cannot change credentials; finish setup outside this run");
  const cookies=await context.cookies(origin);
  const cookie=cookies.find(value=>value.name===CONSOLE_COOKIE);
  const host=new URL(origin).hostname;
  if(!cookie || !cookie.value || !cookie.httpOnly || !cookie.secure || cookie.sameSite!=="Lax" || cookie.path!=="/" || cookie.domain!==host) throw Error("BFF session cookie is missing or lacks required __Host-/HttpOnly/Secure/SameSite=Lax scope");
  const bootstrap=await context.request.get(`${origin}/api/auth/session`,{headers:browserHeaders(origin),maxRedirects:0,failOnStatusCode:false});
  if(bootstrap.status()!==200) throw Error(`BFF cookie bootstrap failed (${bootstrap.status()}); no bearer fallback`);
  const session:unknown=await bootstrap.json();
  if(!validSession(session) || session.user.user_id!==login.user.user_id || session.csrf_token!==login.csrf_token || session.expires_at<=Date.now()/1000) throw Error("BFF bootstrap identity/CSRF/expiry mismatch");
  if(session.user.force_password_change===true) throw Error("Existing-account E2E cannot change credentials; finish setup outside this run");
  return session;
}
