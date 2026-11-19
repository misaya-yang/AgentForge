/** Load public deployment data before requests or console rendering. */
export async function loadRuntimeConfig(
  request: typeof fetch,
  apply: (config: Record<string, string>) => void,
): Promise<void> {
  const response = await request("/runtime-config.json", {
    credentials: "same-origin",
    cache: "no-store",
    redirect: "error",
    signal: AbortSignal.timeout(10_000),
  });
  if (!response.ok) throw new Error("Runtime configuration unavailable");
  const data: unknown = await response.json();
  if (!data || typeof data !== "object" || Array.isArray(data)) {
    throw new Error("Invalid runtime configuration");
  }
  const config: Record<string, string> = {};
  for (const key of ["apiBaseUrl", "apiUrl", "authEmailDomain", "supportEmail", "telemetryEndpoint", "sseDebug", "agentStudioEnabled"]) {
    const value = (data as Record<string, unknown>)[key];
    if (value === undefined) continue;
    if (typeof value !== "string") throw new Error("Invalid runtime configuration");
    config[key] = value;
  }
  apply(config);
}
