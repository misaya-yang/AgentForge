import { render } from "ink";
import meow from "meow";
import React from "react";

import { hostedConfig } from "./remote/device_auth.js";
import { App } from "./app.js";
import { initConfigDir, updateConfig } from "./config.js";
import type { CLIConfig } from "./types/config.js";

/** Preserve the pre-1.5 remote Gateway client behind an explicit migration path. */
export async function runLegacyGatewayCli(argv: string[]): Promise<number> {
  if (argv.some(a=>a==="--api-key" || a==="-k" || a.startsWith("--api-key="))) {console.error("API keys are not accepted in arguments. Use AI_GATEWAY_API_KEY, --api-key-file, or gateway auth login.");return 2;}
  const cli = meow(
    `
    Usage
      $ ai-gateway gateway [options]

    Options
      --api-key-file    Private API key file for CI (or AI_GATEWAY_API_KEY)
      gateway auth login --base-url <origin> stores a scoped session in the OS vault
      --base-url, -u    Gateway base URL (default: http://localhost:8080)
      --model, -m       Default model (server default when unset)
      --tenant-id, -t   Tenant identifier
      --kb              Knowledge base dataset IDs (comma-separated)

    This compatibility client uses ~/.ai-gateway/config.json. The independent
    Agent CLI uses ~/.ai-gateway-cli/providers.json and does not read it.
  `,
    {
      importMeta: import.meta,
      argv,
      flags: {
        apiKeyFile: { type: "string" },
        baseUrl: { type: "string", shortFlag: "u" },
        model: { type: "string", shortFlag: "m" },
        tenantId: { type: "string", shortFlag: "t" },
        kb: { type: "string" },
      },
    },
  );
  const savedConfig = initConfigDir();
  const config: CLIConfig = {
    ...savedConfig,

    ...(cli.flags.baseUrl && { base_url: cli.flags.baseUrl }),
    ...(cli.flags.model && { default_model: cli.flags.model }),
    ...(cli.flags.tenantId && { tenant_id: cli.flags.tenantId }),
    ...(cli.flags.kb && { kb_dataset_ids: cli.flags.kb.split(",").map((value) => value.trim()) }),
  };
  if (cli.flags.baseUrl || cli.flags.model || cli.flags.tenantId) {
    updateConfig(config);
  }
  let authenticated:CLIConfig;
  try { authenticated=await hostedConfig(config,{},cli.flags.apiKeyFile); }
  catch { console.error("Gateway login required: ai-gateway gateway auth login --base-url <origin>. CI may use AI_GATEWAY_API_KEY or --api-key-file.");return 1; }
  render(<App config={authenticated} />);
  return 0;
}
