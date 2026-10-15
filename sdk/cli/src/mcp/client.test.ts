import { describe, expect, it } from "vitest";

import { buildMCPEnvironment } from "./client.js";

describe("legacy local MCP environment isolation", () => {
  it("inherits only code-owned ambient keys plus explicit server values", () => {
    const env = buildMCPEnvironment(
      { MCP_EXPLICIT_TOKEN: "operator-scoped", EMPTY: "" },
      {
        PATH: "/usr/bin",
        HOME: "/tmp/home",
        PROVIDER_API_KEY: "must-not-leak",
        AI_PLATFORM_INTERNAL_TOKEN: "must-not-leak",
      },
    );
    expect(env).toEqual({
      PATH: "/usr/bin",
      HOME: "/tmp/home",
      MCP_EXPLICIT_TOKEN: "operator-scoped",
    });
  });
});
