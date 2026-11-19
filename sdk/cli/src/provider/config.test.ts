import { lstatSync, mkdtempSync, readFileSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

import {
  PRODUCT_CONFIG_SCHEMA,
  assertProviderCredentials,
  defaultProductConfig,
  ensurePrivateDirectory,
  loadProductConfig,
  renderRuntimeConfig,
  saveProductConfig,
  validateProductConfig,
  type ProductConfig,
} from "./config.js";

function responsesConfig(): ProductConfig {
  return {
    schema_version: PRODUCT_CONFIG_SCHEMA,
    active_provider: "third-party",
    approval_policy: "on-request",
    sandbox_mode: "workspace-write",
    providers: {
      "third-party": {
        name: "Third Party Responses",
        model: "model/name-with-provider-prefix",
        base_url: "https://provider.example/openai/v1",
        wire_api: "responses",
        auth: { type: "bearer", api_key_env: "THIRD_PARTY_API_KEY" },
        http_headers: { "X-Client": "ai-gateway-cli" },
        env_http_headers: { "X-Organization": "THIRD_PARTY_ORG" },
        query_params: { "api-version": "2026-08-01" },
        request_max_retries: 4,
        stream_max_retries: 5,
        stream_idle_timeout_ms: 300_000,
      },
    },
  };
}

describe("independent CLI provider config", () => {
  it("renders the upstream Responses provider contract without secret values", () => {
    const config = validateProductConfig(responsesConfig());
    const provider = config.providers[config.active_provider]!;
    const rendered = renderRuntimeConfig(config, config.active_provider, provider);

    expect(rendered).toContain('model_provider = "third-party"');
    expect(rendered).toContain('base_url = "https://provider.example/openai/v1"');
    expect(rendered).toContain('wire_api = "responses"');
    expect(rendered).toContain('inherit = "core"');
    expect(rendered).toContain("ignore_default_excludes = false");
    expect(rendered).toContain('env_key = "THIRD_PARTY_API_KEY"');
    expect(rendered).not.toContain('web_search = "disabled"');
    expect(rendered).toContain('"X-Organization" = "THIRD_PARTY_ORG"');
    expect(rendered).toContain('"api-version" = "2026-08-01"');
    expect(rendered).toContain('"*KEY*"');
    expect(rendered).not.toContain("provider-secret-value");
  });

  it("maps header authentication to env_http_headers instead of storing a key", () => {
    const config = responsesConfig();
    config.providers["third-party"]!.auth = {
      type: "header",
      header: "api-key",
      api_key_env: "AZURE_OPENAI_KEY",
    };
    const rendered = renderRuntimeConfig(config, "third-party", config.providers["third-party"]!);
    expect(rendered).toContain('"api-key" = "AZURE_OPENAI_KEY"');
    expect(rendered).not.toContain("env_key =");
  });

  it("uses the native approval-policy wire value", () => {
    const config = responsesConfig();
    config.approval_policy = "untrusted";
    const rendered = renderRuntimeConfig(config, "third-party", config.providers["third-party"]!);
    expect(rendered).toContain('approval_policy = "untrusted"');
  });

  it("fails closed on unsafe endpoints and secret-bearing static config", () => {
    const insecure = responsesConfig();
    insecure.providers["third-party"]!.base_url = "http://provider.example/v1";
    expect(() => validateProductConfig(insecure)).toThrow(/must use https/);

    const endpoint = responsesConfig();
    endpoint.providers["third-party"]!.base_url = "https://provider.example/v1/responses";
    expect(() => validateProductConfig(endpoint)).toThrow(/API root/);

    const secretHeader = responsesConfig();
    secretHeader.providers["third-party"]!.http_headers = { Authorization: "Bearer plaintext" };
    expect(() => validateProductConfig(secretHeader)).toThrow(/may contain a credential/);

    const transportHeader = responsesConfig();
    transportHeader.providers["third-party"]!.http_headers = { Host: "override.example" };
    expect(() => validateProductConfig(transportHeader)).toThrow(/cannot override transport header/);

    const unknownStaticHeader = responsesConfig();
    unknownStaticHeader.providers["third-party"]!.http_headers = { "X-Subscription-Key": "plaintext" };
    expect(() => validateProductConfig(unknownStaticHeader)).toThrow(/not a permitted public static header/);

    const cookie = responsesConfig();
    cookie.providers["third-party"]!.http_headers = { Cookie: "session=plaintext" };
    expect(() => validateProductConfig(cookie)).toThrow(/not a permitted public static header/);

    const localhost = responsesConfig();
    localhost.providers["third-party"]!.base_url = "http://localhost:8080/v1";
    localhost.providers["third-party"]!.allow_insecure_localhost = true;
    expect(() => validateProductConfig(localhost)).toThrow(/must use https/);

    const unsafeQuery = responsesConfig();
    unsafeQuery.providers["third-party"]!.query_params = { "api-version": "v1&api-key=plaintext" };
    expect(() => validateProductConfig(unsafeQuery)).toThrow(/URL-unreserved/);

    const reserved = responsesConfig();
    reserved.active_provider = "OpenAI";
    reserved.providers.OpenAI = reserved.providers["third-party"]!;
    delete reserved.providers["third-party"];
    expect(() => validateProductConfig(reserved)).toThrow(/reserved by the native Runtime/);

    const reservedName = responsesConfig();
    reservedName.providers["third-party"]!.name = "OpenAI";
    expect(() => validateProductConfig(reservedName)).toThrow(/name is reserved/);

    const chatRetries = responsesConfig();
    chatRetries.providers["third-party"]!.wire_api = "chat_completions";
    chatRetries.providers["third-party"]!.stream_max_retries = 1;
    expect(() => validateProductConfig(chatRetries)).toThrow(/stream_max_retries is unsupported/);

    const secretField = responsesConfig() as unknown as Record<string, unknown>;
    (secretField.providers as Record<string, Record<string, unknown>>)["third-party"]!.api_key = "plaintext";
    expect(() => validateProductConfig(secretField)).toThrow(/unknown field.*api_key/);
  });

  it("requires every referenced credential environment variable", () => {
    const provider = responsesConfig().providers["third-party"]!;
    expect(() => assertProviderCredentials(provider, {})).toThrow(/THIRD_PARTY_API_KEY.*THIRD_PARTY_ORG/);
    expect(() => assertProviderCredentials(provider, {
      THIRD_PARTY_API_KEY: "secret",
      THIRD_PARTY_ORG: "org",
    })).not.toThrow();
  });

  it("writes private config files and rejects a symlink home", () => {
    const root = mkdtempSync(join(tmpdir(), "ai-gateway-cli-config-"));
    const path = join(root, "home", "providers.json");
    saveProductConfig(defaultProductConfig(), path);
    expect(lstatSync(join(root, "home")).mode & 0o777).toBe(0o700);
    expect(lstatSync(path).mode & 0o777).toBe(0o600);
    expect(loadProductConfig(path)).toEqual(defaultProductConfig());
    expect(readFileSync(path, "utf8")).not.toContain("api_key");

    const target = join(root, "target");
    const link = join(root, "link");
    ensurePrivateDirectory(target);
    symlinkSync(target, link);
    expect(() => ensurePrivateDirectory(link)).toThrow(/not a symlink/);

    const realConfig = join(root, "real-config.json");
    const linkedConfig = join(root, "linked-config.json");
    writeFileSync(realConfig, JSON.stringify(defaultProductConfig()));
    symlinkSync(realConfig, linkedConfig);
    expect(() => loadProductConfig(linkedConfig)).toThrow(/regular non-symlink file/);
  });

  it("renders a local proxy provider with only an ephemeral key reference", () => {
    const config = responsesConfig();
    const provider = config.providers["third-party"]!;
    const rendered = renderRuntimeConfig(config, "third-party", provider, {
      baseUrl: "http://127.0.0.1:43123/v1",
      envKey: "AI_GATEWAY_LOCAL_PROXY_TOKEN",
    });
    expect(rendered).toContain('base_url = "http://127.0.0.1:43123/v1"');
    expect(rendered).toContain('env_key = "AI_GATEWAY_LOCAL_PROXY_TOKEN"');
    expect(rendered).toContain('web_search = "disabled"');
    expect(rendered).not.toContain("THIRD_PARTY_API_KEY");
    expect(rendered).not.toContain("provider.example");
  });
});
