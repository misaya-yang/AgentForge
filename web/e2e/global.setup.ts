import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { chromium, type FullConfig } from "@playwright/test";
import { authenticateConsoleContext, existingCredentials, requireHttpsConsoleOrigin, type ExistingCredentials } from "./support/bff-session.ts";
// Kept only for the independent explicit API/SDK setup contract and its unit tests.
export { prepareE2EAccount, shouldProvisionE2EAccounts } from "./support/api-account-setup.ts";

const setupDir=path.dirname(fileURLToPath(import.meta.url));
const ARTIFACT_DIR=path.resolve(setupDir,"../.playwright");
const USER_FILE=path.join(ARTIFACT_DIR,"e2e-user.json");
async function readPersistedE2ECredentials():Promise<ExistingCredentials|null> {
  try {
    const value:unknown=JSON.parse(await fs.readFile(USER_FILE,"utf8"));
    if(value && typeof value==="object" && "email" in value && "password" in value && typeof value.email==="string" && typeof value.password==="string") return {email:value.email,password:value.password};
  } catch { /* A missing file does not authorize provisioning. */ }
  return null;
}
export default async function globalSetup(config:FullConfig) {
  // Validate the HTTPS/BFF boundary before any credential file or browser action.
  const origin=requireHttpsConsoleOrigin(String(config.projects[0]?.use.baseURL ?? ""));
  if(config.projects.some(project=>requireHttpsConsoleOrigin(String(project.use.baseURL ?? ""))!==origin || project.use.ignoreHTTPSErrors)) throw Error("All Console projects must share the trusted HTTPS BFF origin; TLS bypass is forbidden");
  if(process.env.E2E_PROVISION_ACCOUNTS==="1") throw Error("Console BFF setup is existing-account-only; provisioning belongs to the independent API fixture");
  const statePath=config.projects[0]?.use.storageState;
  if(typeof statePath!=="string" || !statePath) throw Error("Console E2E requires an explicit private storageState path");
  const providedEmail=process.env.E2E_USER_EMAIL,providedPassword=process.env.E2E_USER_PASSWORD;
  if(Boolean(providedEmail)!==Boolean(providedPassword)) throw Error("E2E_USER_EMAIL and E2E_USER_PASSWORD must be provided together");
  const credentials=existingCredentials({providedEmail,providedPassword,persistedCredentials:providedEmail && providedPassword ? null : await readPersistedE2ECredentials()});
  const browser=await chromium.launch();
  try {
    const context=await browser.newContext({baseURL:origin,ignoreHTTPSErrors:false});
    const session=await authenticateConsoleContext(context,origin,credentials);
    const page=await context.newPage();
    await page.goto(`${origin}/login`,{waitUntil:"domcontentloaded"});
    await page.evaluate(()=>{
      localStorage.removeItem("agent-gateway-auth");sessionStorage.removeItem("agent-gateway-auth");
      localStorage.setItem("i18nextLng","zh-CN");
      localStorage.setItem("agent-gateway-storage",JSON.stringify({state:{themeMode:"system",resolvedTheme:"light",darkMode:false},version:3}));
    });
    await page.goto(`${origin}/dashboard`,{waitUntil:"domcontentloaded"});
    await page.waitForURL(/\/dashboard/);
    // The application must actually bootstrap the cookie before evidence is saved.
    await page.locator("aside a[href^='/'], [role='complementary'] a[href^='/']").first().waitFor({state:"visible"});
    const legacy=await page.evaluate(()=>Boolean(localStorage.getItem("agent-gateway-auth") || sessionStorage.getItem("agent-gateway-auth")));
    if(legacy) throw Error("Console unexpectedly persisted JavaScript authentication state");
    await fs.mkdir(path.dirname(statePath),{recursive:true,mode:0o700});
    const state = await context.storageState();
    const temporaryState = `${statePath}.${process.pid}.tmp`;
    await fs.writeFile(temporaryState,JSON.stringify(state),{mode:0o600,flag:"wx"});
    try {
      await fs.rename(temporaryState,statePath);
    } finally {await fs.unlink(temporaryState).catch(()=>{}); }
    await fs.mkdir(ARTIFACT_DIR,{recursive:true,mode:0o700});
    await fs.writeFile(path.join(ARTIFACT_DIR,"stack.json"),JSON.stringify({baseURL:origin,apiURL:process.env.E2E_API_URL,userId:session.user.user_id},null,2),{mode:0o600});
  } finally {await browser.close();}
}
