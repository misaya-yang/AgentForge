/** Every route has a concrete a11y fixture, including parameterized/public paths. */
export const consoleRouteInventory = [
  ["/login", "/login"], ["/403", "/403"], ["/share/:shareId", "/share/demo-share"],
  ["/a/:publicId", "/a/demo-agent"], ["/quiz/:shareCode", "/quiz/demo-quiz"],
  ["/", "/"], ["/workbench", "/workbench"], ["/dashboard", "/dashboard"],
  ["/services", "/services"], ["/knowledge", "/knowledge"], ["/knowledge/create", "/knowledge/create"],
  ["/knowledge/:datasetId", "/knowledge/demo-kb-ai-gateway"], ["/playground", "/playground"],
  ["/assistant", "/assistant"], ["/agents", "/agents"], ["/agents/new", "/agents/new"],
  ["/agents/:agentId/analytics", "/agents/demo-agent/analytics"], ["/agents/:agentId/*", "/agents/demo-agent/editor"],
  ["/eval", "/eval"], ["/tasks", "/tasks"], ["/settings", "/settings"],
  ["/users/roles", "/users/roles"], ["/settings/guardrails", "/settings/guardrails"],
  ["/settings/governance", "/settings/governance"], ["/settings/knowledge-api-keys", "/settings/knowledge-api-keys"],
  ["/settings/plugins", "/settings/plugins"], ["/connections", "/connections"],
  ["/settings/routing", "/settings/routing"], ["/settings/identity", "/settings/identity"],
  ["/settings/mcp-registry", "/settings/mcp-registry"],
  ["/settings/connectors", "/settings/connectors"], ["/users", "/users"],
  ["/users/:userId/edit", "/users/demo-user/edit"], ["*", "/not-a-console-route"],
] as const;
