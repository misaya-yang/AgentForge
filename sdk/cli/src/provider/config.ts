import {
  chmodSync,
  existsSync,
  lstatSync,
  mkdirSync,
  readFileSync,
  renameSync,
  writeFileSync,
} from "node:fs";
import { homedir } from "node:os";
import { dirname, join } from "node:path";

export const PRODUCT_CONFIG_SCHEMA = "ai-gateway-cli/providers/v1" as const;

export type ProviderWireApi = "responses" | "chat_completions";
export type ProviderAuth =
  | { type: "bearer"; api_key_env: string }
  | { type: "header"; api_key_env: string; header: string }
  | { type: "none" };

export interface ProviderProfile {
  name: string;
  model: string;
  base_url: string;
  wire_api: ProviderWireApi;
  auth: ProviderAuth;
  http_headers?: Record<string, string>;
  env_http_headers?: Record<string, string>;
  query_params?: Record<string, string>;
  request_max_retries?: number;
  stream_max_retries?: number;
  stream_idle_timeout_ms?: number;
  allow_insecure_localhost?: boolean;
}

export interface ProductConfig {
  schema_version: typeof PRODUCT_CONFIG_SCHEMA;
  active_provider: string;
  providers: Record<string, ProviderProfile>;
  approval_policy?: "on-request" | "untrusted" | "never";
  sandbox_mode?: "read-only" | "workspace-write" | "danger-full-access";
}

export interface RuntimeProviderOverride {
  baseUrl: string;
  envKey: string;
}

const PROVIDER_ID = /^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$/;
const ENV_NAME = /^[A-Za-z_][A-Za-z0-9_]*$/;
const HEADER_NAME = /^[!#$%&'*+.^_`|~0-9A-Za-z-]+$/;
const SECRET_HEADER = /authorization|api[-_]?key|token|secret|credential/i;
const SECRET_QUERY = /api[-_]?key|token|secret|credential|signature/i;
const FORBIDDEN_HEADER = /^(accept|connection|content-length|content-type|host|proxy-authenticate|proxy-authorization|te|trailer|transfer-encoding|upgrade)$/i;
const SAFE_STATIC_HEADER = /^(version|x-client|x-client-version|x-sdk-version)$/i;
const QUERY_COMPONENT = /^[A-Za-z0-9._~-]+$/;
const RESERVED_PROVIDER_IDS = new Set([
  "openai",
  "amazon-bedrock",
  "amazon-bedrock-runtime",
  "ollama",
  "lmstudio",
]);
const ALLOWED_ROOT_KEYS = new Set([
  "schema_version",
  "active_provider",
  "providers",
  "approval_policy",
  "sandbox_mode",
]);
const ALLOWED_PROVIDER_KEYS = new Set([
  "name",
  "model",
  "base_url",
  "wire_api",
  "auth",
  "http_headers",
  "env_http_headers",
  "query_params",
  "request_max_retries",
  "stream_max_retries",
  "stream_idle_timeout_ms",
  "allow_insecure_localhost",
]);

export function productHome(env: NodeJS.ProcessEnv = process.env): string {
  const configured = env.AI_GATEWAY_CLI_HOME?.trim();
  return configured || join(homedir(), ".ai-gateway-cli");
}

export function productConfigPath(home = productHome()): string {
  return join(home, "providers.json");
}

export function runtimeHome(home = productHome()): string {
  return join(home, "runtime");
}

export function defaultProductConfig(): ProductConfig {
  return {
    schema_version: PRODUCT_CONFIG_SCHEMA,
    active_provider: "",
    providers: {},
    approval_policy: "on-request",
    sandbox_mode: "workspace-write",
  };
}

export function ensurePrivateDirectory(path: string): void {
  if (existsSync(path)) {
    const stat = lstatSync(path);
    if (stat.isSymbolicLink() || !stat.isDirectory()) {
      throw new Error(`CLI home must be a real directory, not a symlink: ${path}`);
    }
    chmodSync(path, 0o700);
    return;
  }
  mkdirSync(path, { recursive: true, mode: 0o700 });
}

export function loadProductConfig(path = productConfigPath()): ProductConfig {
  let raw: unknown;
  try {
    const stat = lstatSync(path);
    if (stat.isSymbolicLink() || !stat.isFile()) throw new Error("config must be a regular non-symlink file");
    raw = JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    throw new Error(`Cannot read CLI provider config ${path}: ${message(error)}`);
  }
  return validateProductConfig(raw);
}

export function saveProductConfig(config: ProductConfig, path = productConfigPath()): void {
  const validated = validateProductConfig(config);
  ensurePrivateDirectory(dirname(path));
  const temporary = `${path}.tmp-${process.pid}`;
  writeFileSync(temporary, `${JSON.stringify(validated, null, 2)}\n`, {
    encoding: "utf8",
    mode: 0o600,
    flag: "wx",
  });
  chmodSync(temporary, 0o600);
  renameSync(temporary, path);
  chmodSync(path, 0o600);
}

export function validateProductConfig(raw: unknown): ProductConfig {
  const root = object(raw, "config");
  rejectUnknown(root, ALLOWED_ROOT_KEYS, "config");
  if (root.schema_version !== PRODUCT_CONFIG_SCHEMA) {
    throw new Error(`config.schema_version must be ${PRODUCT_CONFIG_SCHEMA}`);
  }
  const activeProvider = string(root.active_provider, "config.active_provider", true);
  const rawProviders = object(root.providers, "config.providers");
  const providers: Record<string, ProviderProfile> = {};
  for (const [id, value] of Object.entries(rawProviders)) {
    if (!PROVIDER_ID.test(id)) throw new Error(`Invalid provider id: ${id}`);
    if (RESERVED_PROVIDER_IDS.has(id.toLowerCase())) {
      throw new Error(`Provider id is reserved by the native Runtime: ${id}`);
    }
    providers[id] = validateProvider(value, `config.providers.${id}`);
  }
  if (activeProvider && !providers[activeProvider]) {
    throw new Error(`Active provider does not exist: ${activeProvider}`);
  }
  const approval = root.approval_policy ?? "on-request";
  if (!new Set(["on-request", "untrusted", "never"]).has(String(approval))) {
    throw new Error("config.approval_policy is invalid");
  }
  const sandbox = root.sandbox_mode ?? "workspace-write";
  if (!new Set(["read-only", "workspace-write", "danger-full-access"]).has(String(sandbox))) {
    throw new Error("config.sandbox_mode is invalid");
  }
  return {
    schema_version: PRODUCT_CONFIG_SCHEMA,
    active_provider: activeProvider,
    providers,
    approval_policy: approval as ProductConfig["approval_policy"],
    sandbox_mode: sandbox as ProductConfig["sandbox_mode"],
  };
}

function validateProvider(raw: unknown, path: string): ProviderProfile {
  const provider = object(raw, path);
  rejectUnknown(provider, ALLOWED_PROVIDER_KEYS, path);
  const wire = string(provider.wire_api, `${path}.wire_api`) as ProviderWireApi;
  if (wire !== "responses" && wire !== "chat_completions") {
    throw new Error(`${path}.wire_api must be responses or chat_completions`);
  }
  const allowInsecure = provider.allow_insecure_localhost === true;
  const baseUrl = validateBaseUrl(string(provider.base_url, `${path}.base_url`), wire, allowInsecure);
  const authRaw = object(provider.auth, `${path}.auth`);
  rejectUnknown(authRaw, new Set(["type", "api_key_env", "header"]), `${path}.auth`);
  const authType = string(authRaw.type, `${path}.auth.type`);
  let auth: ProviderAuth;
  if (authType === "none") {
    auth = { type: "none" };
  } else {
    const apiKeyEnv = validateEnvName(string(authRaw.api_key_env, `${path}.auth.api_key_env`));
    if (authType === "bearer") auth = { type: "bearer", api_key_env: apiKeyEnv };
    else if (authType === "header") {
      const header = validateHeaderName(string(authRaw.header, `${path}.auth.header`));
      auth = { type: "header", api_key_env: apiKeyEnv, header };
    } else throw new Error(`${path}.auth.type must be bearer, header, or none`);
  }
  const requestRetries = optionalInteger(provider.request_max_retries, `${path}.request_max_retries`, 0, 20);
  const streamRetries = optionalInteger(provider.stream_max_retries, `${path}.stream_max_retries`, 0, 20);
  if (wire === "chat_completions" && streamRetries !== undefined && streamRetries !== 0) {
    throw new Error(`${path}.stream_max_retries is unsupported for chat_completions; only pre-stream request retries are safe`);
  }
  const name = string(provider.name, `${path}.name`);
  if (name.toLowerCase() === "openai") throw new Error(`${path}.name is reserved by the native Runtime`);
  return {
    name,
    model: string(provider.model, `${path}.model`),
    base_url: baseUrl,
    wire_api: wire,
    auth,
    http_headers: validateMap(provider.http_headers, `${path}.http_headers`, "static-header"),
    env_http_headers: validateMap(provider.env_http_headers, `${path}.env_http_headers`, "env-header"),
    query_params: validateMap(provider.query_params, `${path}.query_params`, "query"),
    request_max_retries: requestRetries,
    stream_max_retries: streamRetries,
    stream_idle_timeout_ms: optionalInteger(provider.stream_idle_timeout_ms, `${path}.stream_idle_timeout_ms`, 1_000, 900_000),
    allow_insecure_localhost: allowInsecure || undefined,
  };
}

function validateBaseUrl(value: string, wire: ProviderWireApi, allowInsecure: boolean): string {
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    throw new Error("provider base_url must be an absolute URL");
  }
  if (url.username || url.password || url.hash || url.search) {
    throw new Error("provider base_url cannot contain credentials, query parameters, or fragments");
  }
  const loopback = url.hostname === "127.0.0.1" || url.hostname === "[::1]";
  if (url.protocol !== "https:" && !(url.protocol === "http:" && loopback && allowInsecure)) {
    throw new Error("provider base_url must use https; loopback http requires allow_insecure_localhost=true");
  }
  if (wire === "responses" && /\/responses\/?$/i.test(url.pathname)) {
    throw new Error("Responses provider base_url must be the API root, not the /responses endpoint");
  }
  return value.replace(/\/$/, "");
}

function validateMap(
  raw: unknown,
  path: string,
  kind: "static-header" | "env-header" | "query",
): Record<string, string> | undefined {
  if (raw === undefined) return undefined;
  const values = object(raw, path);
  const result: Record<string, string> = {};
  for (const [key, rawValue] of Object.entries(values)) {
    const value = string(rawValue, `${path}.${key}`);
    if (kind !== "query") validateHeaderName(key);
    if (kind === "static-header" && SECRET_HEADER.test(key)) {
      throw new Error(`${path}.${key} may contain a credential; use auth or env_http_headers`);
    }
    if (kind === "static-header" && !SAFE_STATIC_HEADER.test(key)) {
      throw new Error(`${path}.${key} is not a permitted public static header; use env_http_headers`);
    }
    if (kind === "env-header") validateEnvName(value);
    if (kind === "query" && SECRET_QUERY.test(key)) {
      throw new Error(`${path}.${key} may contain a credential and is not allowed`);
    }
    if (kind === "query" && (!QUERY_COMPONENT.test(key) || !QUERY_COMPONENT.test(value))) {
      throw new Error(`${path}.${key} must use URL-unreserved query key/value characters`);
    }
    if (kind === "static-header" && /[\r\n]/.test(value)) {
      throw new Error(`${path}.${key} contains invalid header characters`);
    }
    result[key] = value;
  }
  return Object.keys(result).length ? result : undefined;
}

export function activeProvider(config: ProductConfig): [string, ProviderProfile] {
  if (!config.active_provider) throw new Error("No active provider. Run `ai-gateway provider use <id>`. ");
  const provider = config.providers[config.active_provider];
  if (!provider) throw new Error(`Active provider does not exist: ${config.active_provider}`);
  return [config.active_provider, provider];
}

export function assertProviderCredentials(provider: ProviderProfile, env: NodeJS.ProcessEnv): void {
  const required = new Set<string>();
  if (provider.auth.type !== "none") required.add(provider.auth.api_key_env);
  for (const name of Object.values(provider.env_http_headers ?? {})) required.add(name);
  const missing = [...required].filter((name) => !env[name]?.trim());
  if (missing.length) throw new Error(`Missing provider credential environment variable(s): ${missing.join(", ")}`);
}

export function writeRuntimeConfig(
  config: ProductConfig,
  providerId: string,
  provider: ProviderProfile,
  override?: RuntimeProviderOverride,
  home = runtimeHome(),
): string {
  ensurePrivateDirectory(home);
  const path = join(home, "config.toml");
  const temporary = `${path}.tmp-${process.pid}`;
  writeFileSync(temporary, renderRuntimeConfig(config, providerId, provider, override), {
    encoding: "utf8",
    mode: 0o600,
    flag: "wx",
  });
  chmodSync(temporary, 0o600);
  renameSync(temporary, path);
  chmodSync(path, 0o600);
  return path;
}

export function renderRuntimeConfig(
  config: ProductConfig,
  providerId: string,
  provider: ProviderProfile,
  override?: RuntimeProviderOverride,
): string {
  const lines = [
    `model = ${toml(provider.model)}`,
    `model_provider = ${toml(providerId)}`,
    `approval_policy = ${toml(config.approval_policy ?? "on-request")}`,
    `sandbox_mode = ${toml(config.sandbox_mode ?? "workspace-write")}`,
    ...(override ? ['web_search = "disabled"'] : []),
    "",
    "[shell_environment_policy]",
    'inherit = "core"',
    "ignore_default_excludes = false",
    `exclude = ${tomlArray(secretEnvironmentPatterns(provider, override))}`,
    "",
    `[model_providers.${tomlKey(providerId)}]`,
    `name = ${toml(`AI Gateway CLI / ${provider.name}`)}`,
    `base_url = ${toml(override?.baseUrl ?? provider.base_url)}`,
    'wire_api = "responses"',
  ];
  if (override) {
    lines.push(`env_key = ${toml(override.envKey)}`);
    lines.push("request_max_retries = 0", "stream_max_retries = 0");
  } else {
    if (provider.auth.type === "bearer") lines.push(`env_key = ${toml(provider.auth.api_key_env)}`);
    if (provider.request_max_retries !== undefined) lines.push(`request_max_retries = ${provider.request_max_retries}`);
    if (provider.stream_max_retries !== undefined) lines.push(`stream_max_retries = ${provider.stream_max_retries}`);
    if (provider.stream_idle_timeout_ms !== undefined) lines.push(`stream_idle_timeout_ms = ${provider.stream_idle_timeout_ms}`);
    appendTable(lines, providerId, "query_params", provider.query_params);
    appendTable(lines, providerId, "http_headers", provider.http_headers);
    const envHeaders = { ...provider.env_http_headers };
    if (provider.auth.type === "header") envHeaders[provider.auth.header] = provider.auth.api_key_env;
    appendTable(lines, providerId, "env_http_headers", envHeaders);
  }
  return `${lines.join("\n")}\n`;
}

function appendTable(
  lines: string[],
  providerId: string,
  name: string,
  values?: Record<string, string>,
): void {
  if (!values || !Object.keys(values).length) return;
  lines.push("", `[model_providers.${tomlKey(providerId)}.${name}]`);
  for (const key of Object.keys(values).sort()) lines.push(`${tomlKey(key)} = ${toml(values[key]!)}`);
}

function secretEnvironmentPatterns(provider: ProviderProfile, override?: RuntimeProviderOverride): string[] {
  const exact = new Set<string>([
    "*KEY*",
    "*TOKEN*",
    "*SECRET*",
    "*PASSWORD*",
    "*CREDENTIAL*",
    ...(!override && provider.auth.type !== "none" ? [provider.auth.api_key_env] : []),
    ...(!override ? Object.values(provider.env_http_headers ?? {}) : []),
    ...(override ? [override.envKey] : []),
  ]);
  return [...exact].sort();
}

function toml(value: string): string {
  return JSON.stringify(value);
}

function tomlKey(value: string): string {
  return JSON.stringify(value);
}

function tomlArray(values: string[]): string {
  return `[${values.map(toml).join(", ")}]`;
}

function object(value: unknown, path: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(`${path} must be an object`);
  return value as Record<string, unknown>;
}

function string(value: unknown, path: string, allowEmpty = false): string {
  if (typeof value !== "string" || (!allowEmpty && !value.trim())) throw new Error(`${path} must be a non-empty string`);
  return value;
}

function rejectUnknown(value: Record<string, unknown>, allowed: Set<string>, path: string): void {
  const unknown = Object.keys(value).filter((key) => !allowed.has(key));
  if (unknown.length) throw new Error(`${path} has unknown field(s): ${unknown.join(", ")}`);
}

function validateEnvName(value: string): string {
  if (!ENV_NAME.test(value)) throw new Error(`Invalid environment variable name: ${value}`);
  return value;
}

function validateHeaderName(value: string): string {
  if (!HEADER_NAME.test(value)) throw new Error(`Invalid HTTP header name: ${value}`);
  if (FORBIDDEN_HEADER.test(value)) throw new Error(`Provider cannot override transport header: ${value}`);
  return value;
}

function optionalInteger(value: unknown, path: string, min: number, max: number): number | undefined {
  if (value === undefined) return undefined;
  if (!Number.isInteger(value) || Number(value) < min || Number(value) > max) {
    throw new Error(`${path} must be an integer between ${min} and ${max}`);
  }
  return Number(value);
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
