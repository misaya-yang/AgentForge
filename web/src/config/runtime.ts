type RuntimeConfig = {
  apiBaseUrl?: string;
  apiUrl?: string;
  authEmailDomain?: string;
  supportEmail?: string;
  telemetryEndpoint?: string;
  sseDebug?: string;
  agentStudioEnabled?: string;
};

declare global {
  interface Window {
    __AI_GATEWAY_RUNTIME_CONFIG__?: RuntimeConfig;
  }
}

function readRuntimeConfig(): RuntimeConfig {
  if (typeof window === "undefined") {
    return {};
  }
  return window.__AI_GATEWAY_RUNTIME_CONFIG__ || {};
}

function firstNonEmpty(...values: Array<string | undefined>): string {
  for (const value of values) {
    if (typeof value === "string" && value.trim()) {
      return value.trim();
    }
  }
  return "";
}

export function getApiBaseUrlConfig(): string {
  const runtime = readRuntimeConfig();
  return firstNonEmpty(
    runtime.apiBaseUrl,
    runtime.apiUrl,
    import.meta.env.VITE_API_BASE_URL,
    import.meta.env.VITE_API_URL
  );
}

export function getAllowedEmailDomain(): string {
  const runtime = readRuntimeConfig();
  return firstNonEmpty(
    runtime.authEmailDomain,
    import.meta.env.VITE_AUTH_EMAIL_DOMAIN,
    "example.com"
  );
}

export function getSupportEmail(): string {
  const runtime = readRuntimeConfig();
  const allowedDomain = getAllowedEmailDomain();
  return firstNonEmpty(
    runtime.supportEmail,
    import.meta.env.VITE_SUPPORT_EMAIL,
    `admin@${allowedDomain}`
  );
}

export function getTelemetryEndpoint(): string {
  const runtime = readRuntimeConfig();
  return firstNonEmpty(
    runtime.telemetryEndpoint,
    import.meta.env.VITE_TELEMETRY_ENDPOINT
  );
}

export function isSseDebugEnabled(): boolean {
  const runtime = readRuntimeConfig();
  const value = firstNonEmpty(runtime.sseDebug, import.meta.env.VITE_SSE_DEBUG);
  return value === "true";
}

export function isAgentStudioEnabled(): boolean {
  const runtime = readRuntimeConfig();
  const value = firstNonEmpty(
    runtime.agentStudioEnabled,
    import.meta.env.VITE_AGENT_STUDIO_ENABLED,
    "true",
  ).toLowerCase();
  return !["0", "false", "no", "off"].includes(value);
}
