import { render } from "ink";
import meow from "meow";
import React from "react";

import { App } from "./app.js";
import { initConfigDir, updateConfig } from "./config.js";
import type { CLIConfig } from "./types/config.js";

/** Preserve the pre-1.5 remote Gateway client behind an explicit migration path. */
export function runLegacyGatewayCli(argv: string[]): number {
  const cli = meow(
    `
    Usage
      $ ai-gateway gateway [options]

    Options
      --api-key, -k     API key for the hosted AI Gateway
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
        apiKey: { type: "string", shortFlag: "k" },
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
    ...(cli.flags.apiKey && { api_key: cli.flags.apiKey }),
    ...(cli.flags.baseUrl && { base_url: cli.flags.baseUrl }),
    ...(cli.flags.model && { default_model: cli.flags.model }),
    ...(cli.flags.tenantId && { tenant_id: cli.flags.tenantId }),
    ...(cli.flags.kb && { kb_dataset_ids: cli.flags.kb.split(",").map((value) => value.trim()) }),
  };
  if (cli.flags.apiKey || cli.flags.baseUrl || cli.flags.model || cli.flags.tenantId) {
    updateConfig(config);
  }
  if (!config.api_key) {
    console.error(
      "\x1b[31mError: No hosted Gateway API key configured.\x1b[0m\n\n" +
      "Run with: ai-gateway gateway --api-key gw_YOUR_KEY\n" +
      "Or set it in: ~/.ai-gateway/config.json\n",
    );
    return 1;
  }
  render(<App config={config} />);
  return 0;
}
