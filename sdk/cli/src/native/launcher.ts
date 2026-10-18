import { accessSync, constants, existsSync, lstatSync, statSync, type Stats } from "node:fs";
import { createRequire } from "node:module";
import { spawn } from "node:child_process";
import { constants as osConstants } from "node:os";
import { fileURLToPath } from "node:url";
import { delimiter, dirname, join, resolve } from "node:path";

import {
  LOCAL_PROXY_TOKEN_ENV,
  startChatCompatibilityProxy,
  type ChatCompatibilityProxy,
} from "../provider/chat_responses_proxy.js";
import {
  PRODUCT_CONFIG_SCHEMA,
  assertProviderCredentials,
  defaultProductConfig,
  ensurePrivateDirectory,
  loadProductConfig,
  productConfigPath,
  productHome,
  runtimeHome,
  saveProductConfig,
  validateProductConfig,
  writeRuntimeConfig,
  type ProductConfig,
  type ProviderProfile,
} from "../provider/config.js";
import { runAcpStdio } from "../acp/adapter.js";
import { RELEASE_TARGETS } from "./release_signature.mjs";
import { assertNativeSourceMatch, readNativeSourceIdentity, verifyNativeArtifact } from "./identity.js";

const NATIVE_CHILD_ENV_ALLOWLIST = [
  "PATH", "HOME", "USER", "LOGNAME", "SHELL", "TMPDIR", "TMP", "TEMP",
  "SystemRoot", "SYSTEMROOT", "USERPROFILE", "APPDATA", "LOCALAPPDATA", "PATHEXT",
  "LANG", "LC_ALL", "LC_CTYPE", "TERM", "COLORTERM", "NO_COLOR", "FORCE_COLOR",
  "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY", "SSL_CERT_FILE", "SSL_CERT_DIR",
  "SSH_AUTH_SOCK", "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_DATA_HOME",
] as const;
const PROVIDER_ADD_FLAGS = new Set([
  "name", "model", "base-url", "wire-api", "api-key-env", "auth-header",
  "allow-insecure-localhost",
]);

interface LauncherDependencies {
  spawnProcess?: typeof spawn;
  startProxy?: typeof startChatCompatibilityProxy;
  stdout?: Pick<NodeJS.WriteStream, "write">;
  stderr?: Pick<NodeJS.WriteStream, "write">;
  platform?: NodeJS.Platform;
  arch?: string;
  moduleUrl?: string;
  hasExecutable?: (name: string, env: NodeJS.ProcessEnv) => boolean;
  signalSource?: Pick<NodeJS.Process, "on" | "off">;
}

export async function runIndependentCli(
  argv: string[],
  env: NodeJS.ProcessEnv = process.env,
  dependencies: LauncherDependencies = {},
): Promise<number> {
  const stdout = dependencies.stdout ?? process.stdout;
  const stderr = dependencies.stderr ?? process.stderr;
  if (argv.length===1 && ["--help","-h"].includes(argv[0]!)) {
    stdout.write("Usage: ai-gateway [native agent arguments]\n  config init|path|validate\n  provider add|list|use\n  doctor [--json]\n  acp [--cli-provider ID]  ACP v1 over local stdio\n  gateway auth login|status|logout --base-url <origin>\n  gateway [--api-key-file FILE]  Hosted mode; AI_GATEWAY_API_KEY for CI\nProvider setup and the independent Runtime stay local. Gateway tokens use the OS vault.\n");return 0;
  }
  const home = productHome(env);
  const configPath = productConfigPath(home);
  if (argv[0] === "doctor") return handleDoctorCommand(argv.slice(1), env, dependencies, stdout, stderr);
  if (argv[0] === "config") return handleConfigCommand(argv.slice(1), home, configPath, stdout, stderr);
  if (argv[0] === "provider") return handleProviderCommand(argv.slice(1), configPath, stdout, stderr);

  let config: ProductConfig;
  try {
    config = loadProductConfig(configPath);
  } catch (error) {
    stderr.write(`${message(error)}\nRun \`ai-gateway config init\`, edit ${configPath}, then select a provider.\n`);
    return 2;
  }
  let providerId: string;
  let forwardedArgs: string[];
  let provider: ProviderProfile;
  try {
    ({ providerId, forwardedArgs } = consumeProviderOverride(argv, config));
    const selected = config.providers[providerId];
    if (!selected) throw new Error(`Provider does not exist: ${providerId}`);
    provider = selected;
    assertProviderCredentials(provider, env);
  } catch (error) {
    stderr.write(`${message(error)}\n`);
    return 2;
  }

  const runtimeDir = runtimeHome(home);
  let proxy: ChatCompatibilityProxy | undefined;
  const childEnv = nativeChildEnvironment(env, runtimeDir);
  try {
    const platform = dependencies.platform ?? process.platform;
    if (
      platform === "linux" &&
      forwardedArgs.includes("--approve-for-me") &&
      !(dependencies.hasExecutable ?? executableOnPath)("bwrap", childEnv)
    ) {
      throw new Error(
        "--approve-for-me on Linux requires bubblewrap (bwrap); install it or use an explicit sandbox/approval mode.",
      );
    }
    if (provider.wire_api === "chat_completions") {
      proxy = await (dependencies.startProxy ?? startChatCompatibilityProxy)(provider, env);
      childEnv[LOCAL_PROXY_TOKEN_ENV] = proxy.token;
      writeRuntimeConfig(config, providerId, provider, {
        baseUrl: proxy.baseUrl,
        envKey: LOCAL_PROXY_TOKEN_ENV,
      }, runtimeDir);
    } else {
      for (const name of providerEnvironmentNames(provider)) childEnv[name] = env[name];
      writeRuntimeConfig(config, providerId, provider, undefined, runtimeDir);
    }
    const binary = resolveNativeBinary(env, {
      platform: dependencies.platform,
      arch: dependencies.arch,
      moduleUrl: dependencies.moduleUrl,
    });
    if (forwardedArgs[0]==="acp") {
      if (forwardedArgs.length!==1) throw new Error("Usage: ai-gateway acp [--cli-provider ID]");
      return await runAcpStdio(binary,childEnv);
    }
    return await spawnAndWait(
      dependencies.spawnProcess ?? spawn,
      binary,
      forwardedArgs,
      childEnv,
      dependencies.signalSource ?? process,
    );
  } catch (error) {
    stderr.write(`Independent Agent CLI failed: ${message(error)}\n`);
    return 1;
  } finally {
    await proxy?.close().catch(() => undefined);
  }
}

function handleConfigCommand(
  argv: string[],
  home: string,
  path: string,
  stdout: Pick<NodeJS.WriteStream, "write">,
  stderr: Pick<NodeJS.WriteStream, "write">,
): number {
  const command = argv[0] ?? "path";
  if (command === "path") {
    stdout.write(`${path}\n`);
    return 0;
  }
  if (command === "init") {
    if (existsSync(path)) {
      stderr.write(`Config already exists: ${path}\n`);
      return 2;
    }
    ensurePrivateDirectory(home);
    saveProductConfig(defaultProductConfig(), path);
    stdout.write(`Created ${path}\nAdd a provider, then run: ai-gateway provider use <id>\n`);
    return 0;
  }
  if (command === "validate") {
    try {
      const config = loadProductConfig(path);
      const providerCount = Object.keys(config.providers).length;
      stdout.write(`Valid ${PRODUCT_CONFIG_SCHEMA} config (${providerCount} provider${providerCount === 1 ? "" : "s"})\n`);
      return 0;
    } catch (error) {
      stderr.write(`${message(error)}\n`);
      return 2;
    }
  }
  stderr.write("Usage: ai-gateway config init|path|validate\n");
  return 2;
}

function handleProviderCommand(
  argv: string[],
  path: string,
  stdout: Pick<NodeJS.WriteStream, "write">,
  stderr: Pick<NodeJS.WriteStream, "write">,
): number {
  let config: ProductConfig;
  try {
    config = loadProductConfig(path);
  } catch (error) {
    stderr.write(`${message(error)}\n`);
    return 2;
  }
  const command = argv[0] ?? "list";
  if (command === "list") {
    for (const [id, provider] of Object.entries(config.providers)) {
      stdout.write(`${id === config.active_provider ? "*" : " "} ${id}\t${provider.wire_api}\t${provider.model}\t${provider.base_url}\n`);
    }
    if (!Object.keys(config.providers).length) stdout.write("No providers configured. Edit the providers config shown by `ai-gateway config path`.\n");
    return 0;
  }
  if (command === "use") {
    const id = argv[1];
    if (!id || !config.providers[id]) {
      stderr.write(`Unknown provider: ${id ?? ""}\n`);
      return 2;
    }
    config.active_provider = id;
    saveProductConfig(config, path);
    stdout.write(`Active provider: ${id}\n`);
    return 0;
  }
  if (command === "add") {
    try {
      const { id, profile } = parseProviderAdd(argv.slice(1));
      config.providers[id] = profile;
      if (!config.active_provider) config.active_provider = id;
      saveProductConfig(validateProductConfig(config), path);
      stdout.write(`Configured provider ${id}; secret value remains in ${profile.auth.type === "none" ? "no environment variable" : profile.auth.api_key_env}.\n`);
      return 0;
    } catch (error) {
      stderr.write(`${message(error)}\n${providerAddUsage()}`);
      return 2;
    }
  }
  stderr.write(`Usage: ai-gateway provider list|use <id>|add ...\n${providerAddUsage()}`);
  return 2;
}

function parseProviderAdd(argv: string[]): { id: string; profile: ProviderProfile } {
  const id = argv[0];
  if (!id) throw new Error("provider id is required");
  const flags = new Map<string, string>();
  for (let index = 1; index < argv.length; index += 2) {
    const name = argv[index];
    const value = argv[index + 1];
    if (!name?.startsWith("--") || value === undefined) throw new Error(`invalid provider flag: ${name ?? ""}`);
    const key = name.slice(2);
    if (!PROVIDER_ADD_FLAGS.has(key)) throw new Error(`unknown provider flag: ${name}`);
    if (flags.has(key)) throw new Error(`duplicate provider flag: ${name}`);
    flags.set(key, value);
  }
  const wire = flags.get("wire-api") === "chat-completions" ? "chat_completions" : flags.get("wire-api");
  const authHeader = flags.get("auth-header");
  const apiKeyEnv = flags.get("api-key-env");
  const raw = {
    schema_version: PRODUCT_CONFIG_SCHEMA,
    active_provider: id,
    providers: {
      [id]: {
        name: flags.get("name") ?? id,
        model: flags.get("model"),
        base_url: flags.get("base-url"),
        wire_api: wire,
        auth: apiKeyEnv
          ? authHeader
            ? { type: "header", header: authHeader, api_key_env: apiKeyEnv }
            : { type: "bearer", api_key_env: apiKeyEnv }
          : { type: "none" },
        allow_insecure_localhost: flags.get("allow-insecure-localhost") === "true",
      },
    },
  };
  const validated = validateProductConfig(raw);
  return { id, profile: validated.providers[id]! };
}

function providerAddUsage(): string {
  return "  ai-gateway provider add <id> --base-url <https://.../v1> --model <id> --wire-api responses|chat-completions [--api-key-env NAME] [--auth-header NAME]\n";
}

function consumeProviderOverride(argv: string[], config: ProductConfig): { providerId: string; forwardedArgs: string[] } {
  const index = argv.indexOf("--cli-provider");
  if (index < 0) return { providerId: config.active_provider, forwardedArgs: argv };
  const providerId = argv[index + 1];
  if (!providerId) throw new Error("--cli-provider requires a provider id");
  return {
    providerId,
    forwardedArgs: [...argv.slice(0, index), ...argv.slice(index + 2)],
  };
}

function providerEnvironmentNames(provider: ProviderProfile): Set<string> {
  return new Set([
    ...(provider.auth.type === "none" ? [] : [provider.auth.api_key_env]),
    ...Object.values(provider.env_http_headers ?? {}),
  ]);
}

function executableOnPath(name: string, env: NodeJS.ProcessEnv): boolean {
  return (env.PATH ?? "")
    .split(delimiter)
    .filter(Boolean)
    .some((directory) => existsSync(join(directory, name)));
}

function nativeChildEnvironment(source: NodeJS.ProcessEnv, codexHome: string): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = { CODEX_HOME: codexHome };
  for (const name of NATIVE_CHILD_ENV_ALLOWLIST) {
    if (source[name] !== undefined) env[name] = source[name];
  }
  return env;
}

export function resolveNativeBinary(
  env: NodeJS.ProcessEnv,
  options: { platform?: NodeJS.Platform; arch?: string; moduleUrl?: string } = {},
): string {
  const platform = options.platform ?? process.platform;
  const arch = options.arch ?? process.arch;
  const explicit = env.AI_GATEWAY_AGENT_RUNTIME_BIN?.trim();
  if (explicit) {
    const path = resolve(explicit);
    if (!existsSync(path)) throw new Error(`AI_GATEWAY_AGENT_RUNTIME_BIN does not exist: ${path}`);
    if (env.AI_GATEWAY_UNSAFE_DEV_RUNTIME !== "1") {
      throw new Error("Explicit native Runtime is development-only and requires AI_GATEWAY_UNSAFE_DEV_RUNTIME=1");
    }
    const receipt = env.AI_GATEWAY_AGENT_RUNTIME_RECEIPT?.trim();
    if (receipt) verifyNativeArtifact(path, resolve(receipt), platform, arch);
    return path;
  }
  const extension = platform === "win32" ? ".exe" : "";
  const currentModule = options.moduleUrl ?? import.meta.url;
  const packageRoot = resolve(dirname(fileURLToPath(currentModule)), "..");
  const directory=packagedDirectory(packageRoot,currentModule,platform,arch);
  if (directory && existsSync(join(directory,`codex${extension}`))) {
    const packaged=join(directory,`codex${extension}`);
    const expectedSource=readNativeSourceIdentity(join(packageRoot,"dist","native-source.json"));
    const source=readNativeSourceIdentity(existsSync(join(directory,"source.json")) ? join(directory,"source.json") : join(directory,"..","source.json"));
    assertNativeSourceMatch(expectedSource,source);
    verifyNativeArtifact(packaged,join(directory,"artifact.json"),platform,arch,expectedSource);
    return packaged;
  }
  if (env.AI_GATEWAY_USE_SYSTEM_CODEX === "1" && env.AI_GATEWAY_UNSAFE_DEV_RUNTIME === "1") {
    return `codex${extension}`;
  }
  throw new Error(
    `No packaged Agent Runtime binary for ${platform}-${arch}. ` +
    "Set AI_GATEWAY_AGENT_RUNTIME_BIN to the composed-source codex binary or explicitly opt into AI_GATEWAY_USE_SYSTEM_CODEX=1.",
  );
}

function packagedDirectory(root:string,moduleUrl:string,platform:NodeJS.Platform,arch:string):string|undefined {
  if (!RELEASE_TARGETS.includes(`${platform}-${arch}`)) return undefined;
  const embedded=join(root,"vendor",`${platform}-${arch}`);
  if (existsSync(embedded)) return embedded;
  try { return dirname(createRequire(moduleUrl).resolve(`@agentforge/cli-${platform}-${arch}/package.json`)); }
  catch { return undefined; }
}
/** A release-signed portable platformctl owns all OS vault operations. */
export function resolveClientAuthHelper(moduleUrl=import.meta.url,platform:NodeJS.Platform=process.platform,arch=process.arch):string {
  const root=resolve(dirname(fileURLToPath(moduleUrl)),"..");
  const directory=packagedDirectory(root,moduleUrl,platform,arch);
  if (!directory) throw new Error("Signed client credential helper is not installed");
  const source=readNativeSourceIdentity(join(root,"dist","native-source.json"));
  const name=platform==="win32"?"platformctl.exe":"platformctl";
  const binary=join(directory,name);
  verifyNativeArtifact(binary,join(directory,"platformctl-artifact.json"),platform,arch,source,name);
  return binary;
}

function spawnAndWait(
  spawnProcess: typeof spawn,
  binary: string,
  argv: string[],
  env: NodeJS.ProcessEnv,
  signalSource: Pick<NodeJS.Process, "on" | "off">,
): Promise<number> {
  return new Promise((resolvePromise, reject) => {
    const child = spawnProcess(binary, argv, {
      stdio: "inherit",
      env,
      windowsHide: false,
    });
    const forwardSignal = (signal: NodeJS.Signals) => {
      if (child.exitCode === null && child.signalCode === null) child.kill(signal);
    };
    const onSigint = () => forwardSignal("SIGINT");
    const onSigterm = () => forwardSignal("SIGTERM");
    const cleanup = () => {
      signalSource.off("SIGINT", onSigint);
      signalSource.off("SIGTERM", onSigterm);
    };
    signalSource.on("SIGINT", onSigint);
    signalSource.on("SIGTERM", onSigterm);
    child.once("error", (error) => {
      cleanup();
      reject(error);
    });
    child.once("exit", (code, signal) => {
      cleanup();
      if (typeof code === "number") resolvePromise(code);
      else resolvePromise(signal ? 128 + signalNumber(signal) : 1);
    });
  });
}

function signalNumber(signal: NodeJS.Signals): number {
  return osConstants.signals[signal] ?? 1;
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}


interface DoctorCheck {
  id: string;
  status: "pass" | "warn" | "fail" | "not_checked";
  code: string;
  guidance: string;
}

function handleDoctorCommand(
  argv: string[],
  env: NodeJS.ProcessEnv,
  dependencies: LauncherDependencies,
  stdout: Pick<NodeJS.WriteStream, "write">,
  stderr: Pick<NodeJS.WriteStream, "write">,
): number {
  const usage = "Usage: ai-gateway doctor [--json] [--cli-provider <id>]\n";
  if (argv.length === 1 && (argv[0] === "--help" || argv[0] === "-h")) {
    stdout.write(usage);
    return 0;
  }
  let json = false;
  let selectedId: string | undefined;
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === "--json" && !json) json = true;
    else if (argv[i] === "--cli-provider" && selectedId === undefined &&
             argv[i + 1] !== undefined && /^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$/.test(argv[i + 1]!)) {
      selectedId = argv[++i];
    } else {
      stderr.write(usage);
      return 2;
    }
  }
  const checks: DoctorCheck[] = [];
  const home = productHome(env);
  const path = productConfigPath(home);
  const platform = dependencies.platform ?? process.platform;
  const arch = dependencies.arch ?? process.arch;
  let config: ProductConfig | undefined;
  try {
    config = loadProductConfig(path);
    checks.push({ id: "config", status: "pass", code: "local_config_valid", guidance: "CLI-local configuration is valid." });
  } catch {
    checks.push({ id: "config", status: "fail", code: existsSync(path) ? "local_config_invalid" : "local_config_missing",
      guidance: "Use ai-gateway config path to locate the private file; initialize missing configuration with ai-gateway config init." });
  }
  const provider = config ? config.providers[selectedId ?? config.active_provider] : undefined;
  if (config && provider) {
    checks.push({ id: "provider", status: "pass", code: provider.wire_api === "responses" ? "responses_configured" : "chat_compatibility_configured",
      guidance: "Configured wire format is supported locally; no provider request or protocol handshake was performed." });
    try {
      assertProviderCredentials(provider, env);
      checks.push({ id: "credentials", status: "pass", code: "required_environment_present", guidance: "Required credential/header variables are present; validity was not tested." });
    } catch {
      checks.push({ id: "credentials", status: "fail", code: "required_environment_missing", guidance: "Set the selected profile's required environment variables before normal launch; never include secrets in diagnostic reports." });
    }
  } else {
    checks.push({ id: "provider", status: config ? "fail" : "not_checked", code: config ? "provider_not_selected_or_missing" : "config_required",
      guidance: "Configure a local provider and select it with ai-gateway provider use <id>, or pass --cli-provider <id>." });
    checks.push({ id: "credentials", status: "not_checked", code: "provider_required", guidance: "Credential presence can be checked after a valid provider is selected." });
  }
  const supportedTarget=RELEASE_TARGETS.includes(`${platform}-${arch}`);
  checks.push({ id: "release_target", status: supportedTarget ? "pass" : "fail",
    code: supportedTarget ? "release_target_declared" : "unsupported_release_target",
    guidance: "The target is in the five-target source release matrix; installed artifact identity and signature are checked separately." });
  try {
    const binary = resolveNativeBinary(env, dependencies);
    const development = Boolean(env.AI_GATEWAY_AGENT_RUNTIME_BIN?.trim()) || binary === "codex" || binary === "codex.exe";
    checks.push({ id: "native_artifact", status: development ? "warn" : "pass", code: development ? "development_identity_unverified" : "packaged_identity_verified",
      guidance: development ? "An explicit unsafe development selection is enabled; this is not verified release identity. No binary was executed."
        : "Packaged source identity, target and binary receipt/hash match. No binary or runtime protocol handshake was executed." });
    checks.push(observeDoctorExecutable(binary, env));
  } catch (error) {
    // Map only known validator categories; never emit error text, paths or JSON fragments.
    const detail = error instanceof Error ? error.message : "";
    const code = /^(No packaged Agent Runtime|AI_GATEWAY_AGENT_RUNTIME_BIN does not exist)/.test(detail) ? "native_artifact_missing"
      : /^Native Agent Runtime target mismatch/.test(detail) ? "native_target_mismatch"
      : /^(Native Agent Runtime|Packaged native source) (upstream SHA|overlay SHA) mismatch/.test(detail) ? "native_source_mismatch"
      : /^Native Agent Runtime binary SHA-256 mismatch/.test(detail) ? "native_binary_hash_mismatch"
      : /^(Cannot read native Agent Runtime receipt|Native Agent Runtime receipt)/.test(detail) ? "native_receipt_invalid"
      : /^Explicit native Runtime is development-only/.test(detail) ? "development_override_denied"
      : "native_identity_invalid";
    checks.push({ id: "native_artifact", status: "fail", code, guidance: "Install a matching trusted package with its source identity and receipt, or explicitly configure a development override. Doctor never repairs artifacts." });
    checks.push({ id: "native_executable", status: "not_checked", code: "artifact_required", guidance: "Resolve the artifact selection before executable availability can be observed." });
  }
  checks.push(observeDoctorDirectory("product_directory", home));
  checks.push(observeDoctorDirectory("runtime_directory", runtimeHome(home)));
  const bwrap = platform === "linux" && (dependencies.hasExecutable ?? executableOnPath)("bwrap", env);
  checks.push({ id: "optional_bwrap", status: platform !== "linux" ? "not_checked" : bwrap ? "pass" : "warn",
    code: platform !== "linux" ? "not_applicable" : bwrap ? "path_entry_present" : "optional_tool_missing",
    guidance: "Linux --approve-for-me requires bwrap. PATH presence is only a lookup; no tool was executed, and absence does not block unrelated setup." });
  const exitCode = checks.some((check) => check.status === "fail") ? 1 : 0;
  const report = { schema_version: "ai-gateway-cli/doctor/v1", scope: "local-offline", result: exitCode === 0 ? "local_prerequisites_observed" : "blocked",
    provider_selection: selectedId === undefined ? "active_local_profile" : "explicit_local_override",
    network_checked: false, runtime_executed: false, checks };
  if (json) stdout.write(`${JSON.stringify(report)}\n`);
  else {
    stdout.write("AI Gateway CLI doctor (local, offline, read-only)\n");
    for (const check of checks) stdout.write(`${check.status.toUpperCase()} ${check.id}: ${check.code} — ${check.guidance}\n`);
    stdout.write("No provider connectivity or runtime protocol handshake was tested.\n");
  }
  return exitCode;
}

function observeDoctorDirectory(id: string, requested: string): DoctorCheck {
  let path = resolve(requested);
  let parentOnly = false;
  try {
    let stat: Stats;
    while (true) {
      try {
        stat = lstatSync(path);
        break;
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
        const parent = dirname(path);
        if (parent === path) throw error;
        path = parent;
        parentOnly = true;
      }
    }
    if (stat.isSymbolicLink() || !stat.isDirectory()) {
      return { id, status: "fail", code: "directory_type_unsafe", guidance: "Use a real local directory; doctor does not follow a selected symlink or change permissions." };
    }
    accessSync(path, constants.R_OK | constants.W_OK | constants.X_OK);
    const broadPermissions = (stat.mode & 0o077) !== 0;
    return { id, status: parentOnly || broadPermissions ? "warn" : "pass",
      code: parentOnly ? "missing_parent_access_observed" : broadPermissions ? "directory_permissions_broad" : "directory_access_observed",
      guidance: "Existing directory/nearest parent access was observed without writing. Review permissions locally; this is not a guarantee that a later launch can create files." };
  } catch {
    return { id, status: "fail", code: "directory_access_unavailable", guidance: "Check local directory access and permissions; no write probe or permission change was attempted." };
  }
}


function observeDoctorExecutable(binary: string, env: NodeJS.ProcessEnv): DoctorCheck {
  const systemSelection = binary === "codex" || binary === "codex.exe";
  const paths = systemSelection ? (env.PATH ?? "").split(delimiter).filter(Boolean).map((path) => join(path, binary)) : [binary];
  let code = "native_executable_missing";
  for (const path of paths) {
    try {
      if (!statSync(path).isFile()) {
        code = "native_executable_invalid_type";
        continue;
      }
      code = "native_executable_access_denied";
      accessSync(path, constants.X_OK);
      return { id: "native_executable", status: "pass", code: "executable_access_observed", guidance: "A regular executable file is currently accessible. No runtime process or protocol handshake was started." };
    } catch {
      // No filesystem exception or private path is included in the report.
    }
  }
  return { id: "native_executable", status: "fail", code, guidance: "Provide a regular accessible executable for the selected native runtime; system selection also requires it on PATH. Doctor does not change files or permissions." };
}
