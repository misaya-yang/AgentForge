import { createHash } from "node:crypto";
import { EventEmitter } from "node:events";
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { describe, expect, it } from "vitest";

import { LOCAL_PROXY_TOKEN_ENV } from "../provider/chat_responses_proxy.js";
import {
  PRODUCT_CONFIG_SCHEMA,
  productConfigPath,
  runtimeHome,
  saveProductConfig,
  type ProductConfig,
} from "../provider/config.js";
import { resolveNativeBinary, runIndependentCli } from "./launcher.js";

const TEST_UPSTREAM_SHA = "1".repeat(40);
const TEST_OVERLAY_SHA256 = "2".repeat(64);

function home(): string {
  return mkdtempSync(join(tmpdir(), "ai-gateway-cli-launcher-"));
}

function config(wire: "responses" | "chat_completions" = "responses"): ProductConfig {
  return {
    schema_version: PRODUCT_CONFIG_SCHEMA,
    active_provider: "provider-a",
    approval_policy: "on-request",
    sandbox_mode: "workspace-write",
    providers: {
      "provider-a": {
        name: "Provider A",
        model: "model-a",
        base_url: "https://provider.example/v1",
        wire_api: wire,
        auth: { type: "bearer", api_key_env: "PROVIDER_A_KEY" },
      },
    },
  };
}

function capture() {
  let value = "";
  return {
    stream: { write(chunk: string | Uint8Array) { value += String(chunk); return true; } } as any,
    value: () => value,
  };
}

describe("independent native Runtime launcher", () => {
  it("initializes and validates a private product config", async () => {
    const root = home();
    const stdout = capture();
    const stderr = capture();
    expect(await runIndependentCli(["config", "init"], { AI_GATEWAY_CLI_HOME: root }, {
      stdout: stdout.stream,
      stderr: stderr.stream,
    })).toBe(0);
    expect(readFileSync(productConfigPath(root), "utf8")).toContain(PRODUCT_CONFIG_SCHEMA);
    expect(await runIndependentCli(["config", "validate"], { AI_GATEWAY_CLI_HOME: root }, {
      stdout: stdout.stream,
      stderr: stderr.stream,
    })).toBe(0);
    expect(stdout.value()).toContain("Valid ai-gateway-cli/providers/v1 config");
    expect(stderr.value()).toBe("");
  });

  it("launches the composed Rust CLI with an isolated CODEX_HOME and provider config", async () => {
    const root = home();
    saveProductConfig(config(), productConfigPath(root));
    const script = join(root, "capture.mjs");
    const receipt = join(root, "receipt.json");
    writeFileSync(script, `
      import fs from "node:fs";
      fs.writeFileSync(process.argv[2], JSON.stringify({
        codexHome: process.env.CODEX_HOME,
        providerKey: process.env.PROVIDER_A_KEY,
        unrelatedToken: process.env.UNRELATED_SERVICE_TOKEN,
        config: fs.readFileSync(process.env.CODEX_HOME + "/config.toml", "utf8")
      }));
    `);
    const exit = await runIndependentCli([script, receipt], {
      AI_GATEWAY_CLI_HOME: root,
      AI_GATEWAY_AGENT_RUNTIME_BIN: process.execPath,
      AI_GATEWAY_UNSAFE_DEV_RUNTIME: "1",
      PROVIDER_A_KEY: "synthetic-secret",
      UNRELATED_SERVICE_TOKEN: "must-not-reach-runtime",
    });
    expect(exit).toBe(0);
    const data = JSON.parse(readFileSync(receipt, "utf8"));
    expect(data.codexHome).toBe(runtimeHome(root));
    expect(data.providerKey).toBe("synthetic-secret");
    expect(data.unrelatedToken).toBeUndefined();
    expect(data.config).toContain('base_url = "https://provider.example/v1"');
    expect(data.config).toContain('env_key = "PROVIDER_A_KEY"');
    expect(data.config).not.toContain("synthetic-secret");
  });

  it("keeps a Chat provider secret out of the Rust child and uses an ephemeral proxy credential", async () => {
    const root = home();
    saveProductConfig(config("chat_completions"), productConfigPath(root));
    const script = join(root, "capture.mjs");
    const receipt = join(root, "receipt.json");
    writeFileSync(script, `
      import fs from "node:fs";
      fs.writeFileSync(process.argv[2], JSON.stringify({
        providerKey: process.env.PROVIDER_A_KEY,
        proxyToken: process.env.${LOCAL_PROXY_TOKEN_ENV},
        unrelatedToken: process.env.UNRELATED_SERVICE_TOKEN,
        config: fs.readFileSync(process.env.CODEX_HOME + "/config.toml", "utf8")
      }));
    `);
    let closed = false;
    const exit = await runIndependentCli([script, receipt], {
      AI_GATEWAY_CLI_HOME: root,
      AI_GATEWAY_AGENT_RUNTIME_BIN: process.execPath,
      AI_GATEWAY_UNSAFE_DEV_RUNTIME: "1",
      PROVIDER_A_KEY: "synthetic-secret",
      UNRELATED_SERVICE_TOKEN: "must-not-reach-runtime",
    }, {
      startProxy: async () => ({
        baseUrl: "http://127.0.0.1:45678/v1",
        token: "ephemeral-local-token",
        close: async () => { closed = true; },
      }),
    });
    expect(exit).toBe(0);
    expect(closed).toBe(true);
    const data = JSON.parse(readFileSync(receipt, "utf8"));
    expect(data.providerKey).toBeUndefined();
    expect(data.proxyToken).toBe("ephemeral-local-token");
    expect(data.unrelatedToken).toBeUndefined();
    expect(data.config).toContain('base_url = "http://127.0.0.1:45678/v1"');
    expect(data.config).toContain(`env_key = "${LOCAL_PROXY_TOKEN_ENV}"`);
    expect(data.config).not.toContain("PROVIDER_A_KEY");
    expect(data.config).not.toContain("synthetic-secret");
  });

  it("selects a provider without forwarding the launcher-only flag", async () => {
    const root = home();
    const product = config();
    product.providers["provider-b"] = { ...product.providers["provider-a"]!, name: "B", model: "model-b" };
    saveProductConfig(product, productConfigPath(root));
    const script = join(root, "capture.mjs");
    const receipt = join(root, "receipt.json");
    writeFileSync(script, `
      import fs from "node:fs";
      fs.writeFileSync(process.argv[2], JSON.stringify({ argv: process.argv.slice(3), config: fs.readFileSync(process.env.CODEX_HOME + "/config.toml", "utf8") }));
    `);
    const exit = await runIndependentCli([
      script, receipt, "exec", "--cli-provider", "provider-b", "hello",
    ], {
      AI_GATEWAY_CLI_HOME: root,
      AI_GATEWAY_AGENT_RUNTIME_BIN: process.execPath,
      AI_GATEWAY_UNSAFE_DEV_RUNTIME: "1",
      PROVIDER_A_KEY: "synthetic-secret",
    });
    expect(exit).toBe(0);
    const data = JSON.parse(readFileSync(receipt, "utf8"));
    expect(data.argv).toEqual(["exec", "hello"]);
    expect(data.config).toContain('model = "model-b"');
  });

  it("reports invalid launcher-only provider selection without rejecting", async () => {
    const root = home();
    saveProductConfig(config(), productConfigPath(root));
    const stderr = capture();
    expect(await runIndependentCli(["exec", "--cli-provider"], {
      AI_GATEWAY_CLI_HOME: root,
      PROVIDER_A_KEY: "synthetic-secret",
    }, { stderr: stderr.stream })).toBe(2);
    expect(stderr.value()).toContain("--cli-provider requires a provider id");
  });

  it("fails before spawning Linux auto-review when bubblewrap is unavailable", async () => {
    const root = home();
    saveProductConfig(config(), productConfigPath(root));
    const stderr = capture();
    const exit = await runIndependentCli(["exec", "--approve-for-me", "write a file"], {
      AI_GATEWAY_CLI_HOME: root,
      PROVIDER_A_KEY: "synthetic-secret",
      PATH: "/usr/bin",
    }, {
      platform: "linux",
      hasExecutable: () => false,
      stderr: stderr.stream,
    });
    expect(exit).toBe(1);
    expect(stderr.value()).toContain("requires bubblewrap");
  });

  it("forwards launcher SIGINT to the native child", async () => {
    const root = home();
    saveProductConfig(config(), productConfigPath(root));
    const signals = new EventEmitter();
    let forwarded: NodeJS.Signals | undefined;
    const spawnProcess = (() => {
      const child = new EventEmitter() as any;
      child.exitCode = null;
      child.signalCode = null;
      child.kill = (signal: NodeJS.Signals) => {
        forwarded = signal;
        child.signalCode = signal;
        queueMicrotask(() => child.emit("exit", null, signal));
        return true;
      };
      queueMicrotask(() => signals.emit("SIGINT"));
      return child;
    }) as typeof import("node:child_process").spawn;

    const exit = await runIndependentCli(["exec", "hello"], {
      AI_GATEWAY_CLI_HOME: root,
      AI_GATEWAY_AGENT_RUNTIME_BIN: process.execPath,
      AI_GATEWAY_UNSAFE_DEV_RUNTIME: "1",
      PROVIDER_A_KEY: "synthetic-secret",
    }, {
      spawnProcess,
      signalSource: signals as any,
    });

    expect(forwarded).toBe("SIGINT");
    expect(exit).toBe(130);
    expect(signals.listenerCount("SIGINT")).toBe(0);
    expect(signals.listenerCount("SIGTERM")).toBe(0);
  });

  it.each([
    ["SIGHUP", null, 129], ["SIGINT", null, 130], ["SIGQUIT", null, 131], ["SIGABRT", null, 134],
    ["SIGKILL", null, 137], ["SIGPIPE", null, 141], ["SIGALRM", null, 142], ["SIGTERM", null, 143],
    ["SIGUNKNOWN", null, 129], [null, 0, 0], [null, 7, 7], [null, null, 1],
  ] as const)("preserves the Linux child %s exit status", async (signal, code, expected) => {
    const root = home();
    saveProductConfig(config(), productConfigPath(root));
    const signals = new EventEmitter();
    const stdout = capture();
    const stderr = capture();
    let launches = 0;
    const spawnProcess = (() => {
      launches++;
      const child = new EventEmitter() as any;
      queueMicrotask(() => child.emit("exit", code, signal));
      return child;
    }) as typeof import("node:child_process").spawn;
    const exit = await runIndependentCli(["exec", "--json", "hello"], {
      AI_GATEWAY_CLI_HOME: root,
      AI_GATEWAY_AGENT_RUNTIME_BIN: process.execPath,
      AI_GATEWAY_UNSAFE_DEV_RUNTIME: "1",
      PROVIDER_A_KEY: "synthetic-secret",
    }, { spawnProcess, signalSource: signals as any, stdout: stdout.stream, stderr: stderr.stream });
    expect(exit).toBe(expected);
    expect(launches).toBe(1);
    expect(stdout.value()).toBe("");
    expect(stderr.value()).toBe("");
    expect(signals.listenerCount("SIGINT")).toBe(0);
    expect(signals.listenerCount("SIGTERM")).toBe(0);
  });

  it("reports an actual fixture child killed by SIGKILL as 137 without replay", async () => {
    const root = home();
    saveProductConfig(config(), productConfigPath(root));
    const script = join(root, "killed.mjs");
    const receipt = join(root, "started.txt");
    writeFileSync(script, `
      import fs from "node:fs";
      fs.appendFileSync(process.argv[2], "started\\n");
      process.kill(process.pid, "SIGKILL");
    `);
    const signals = new EventEmitter();
    const stdout = capture();
    const stderr = capture();
    expect(await runIndependentCli([script, receipt], {
      AI_GATEWAY_CLI_HOME: root,
      AI_GATEWAY_AGENT_RUNTIME_BIN: process.execPath,
      AI_GATEWAY_UNSAFE_DEV_RUNTIME: "1",
      PROVIDER_A_KEY: "synthetic-secret",
    }, { signalSource: signals as any, stdout: stdout.stream, stderr: stderr.stream })).toBe(137);
    expect(readFileSync(receipt, "utf8")).toBe("started\n");
    expect(stdout.value()).toBe("");
    expect(stderr.value()).toBe("");
    expect(signals.listenerCount("SIGINT")).toBe(0);
    expect(signals.listenerCount("SIGTERM")).toBe(0);
  });

  it("rejects unknown and duplicate provider-add flags", async () => {
    const root = home();
    saveProductConfig({ ...config(), active_provider: "", providers: {} }, productConfigPath(root));
    const stderr = capture();
    expect(await runIndependentCli([
      "provider", "add", "provider-a", "--base-url", "https://provider.example/v1",
      "--model", "model-a", "--wire-api", "responses", "--unknown", "value",
    ], { AI_GATEWAY_CLI_HOME: root }, { stderr: stderr.stream })).toBe(2);
    expect(stderr.value()).toContain("unknown provider flag");

    const duplicate = capture();
    expect(await runIndependentCli([
      "provider", "add", "provider-a", "--base-url", "https://provider.example/v1",
      "--model", "model-a", "--model", "model-b", "--wire-api", "responses",
    ], { AI_GATEWAY_CLI_HOME: root }, { stderr: duplicate.stream })).toBe(2);
    expect(duplicate.value()).toContain("duplicate provider flag");
  });

  it("verifies a packaged native binary and rejects receipt tampering", () => {
    expect(() => resolveNativeBinary({ AI_GATEWAY_AGENT_RUNTIME_BIN: process.execPath })).toThrow(/development-only/);
    const root = home();
    const dist = join(root, "dist");
    const vendor = join(root, "vendor", "linux-x64");
    mkdirSync(dist, { recursive: true });
    mkdirSync(vendor, { recursive: true });
    const modulePath = join(dist, "cli.js");
    const binaryPath = join(vendor, "codex");
    const receiptPath = join(vendor, "artifact.json");
    writeFileSync(modulePath, "");
    writeFileSync(join(dist, "native-source.json"), JSON.stringify({
      schema_version: "ai-gateway-cli/native-source/v1",
      upstream_sha: TEST_UPSTREAM_SHA,
      overlay_sha256: TEST_OVERLAY_SHA256,
    }));
    writeFileSync(binaryPath, "synthetic-native-binary");
    writeFileSync(join(root, "vendor", "source.json"), JSON.stringify({
      schema_version: "ai-gateway-cli/native-source/v1",
      upstream_sha: TEST_UPSTREAM_SHA,
      overlay_sha256: TEST_OVERLAY_SHA256,
    }));
    const receipt = {
      schema_version: "ai-gateway-cli/native-artifact/v1",
      upstream_sha: TEST_UPSTREAM_SHA,
      overlay_sha256: TEST_OVERLAY_SHA256,
      target_system: "linux",
      target_arch: "x64",
      binary: "codex",
      sha256: createHash("sha256").update(readFileSync(binaryPath)).digest("hex"),
    };
    writeFileSync(receiptPath, JSON.stringify(receipt));
    const options = { platform: "linux" as const, arch: "x64", moduleUrl: pathToFileURL(modulePath).href };
    expect(resolveNativeBinary({}, options)).toBe(binaryPath);

    writeFileSync(join(root, "vendor", "source.json"), JSON.stringify({
      schema_version: "ai-gateway-cli/native-source/v1",
      upstream_sha: "3".repeat(40),
      overlay_sha256: TEST_OVERLAY_SHA256,
    }));
    expect(() => resolveNativeBinary({}, options)).toThrow(/Packaged native source upstream SHA mismatch/);

    writeFileSync(join(root, "vendor", "source.json"), JSON.stringify({
      schema_version: "ai-gateway-cli/native-source/v1",
      upstream_sha: TEST_UPSTREAM_SHA,
      overlay_sha256: TEST_OVERLAY_SHA256,
    }));
    writeFileSync(receiptPath, JSON.stringify({ ...receipt, upstream_sha: "3".repeat(40) }));
    expect(() => resolveNativeBinary({}, options)).toThrow(/upstream SHA mismatch/);
  });
});
