// Equivalent of `npx @a2a-js/sdk validate <card-url>`.
// @a2a-js/sdk@1.3.0 ships no `validate` bin (only `a2a-db`), so this script
// parses the card with AgentCard.fromJSON and then requires the 1.0 fields
// that fromJSON leaves empty instead of rejecting.
const url = process.argv[2];
if (!url) {
  console.error("usage: node a2a-card-validate.mjs <agent-card-url>");
  process.exit(2);
}
const { AgentCard } = await import("@a2a-js/sdk");
const response = await fetch(url);
if (!response.ok) {
  console.error(`card request failed: ${response.status}`);
  process.exit(1);
}
const body = await response.json();
const card = AgentCard.fromJSON(body);
const errors = [];
for (const key of ["name", "description", "version"]) {
  if (!card[key]) errors.push(`missing ${key}`);
}
const iface = card.supportedInterfaces?.[0];
if (!iface?.url || !iface.protocolBinding || !iface.protocolVersion) {
  errors.push("supportedInterfaces[0] needs url, protocolBinding, protocolVersion");
} else if (!["JSONRPC", "GRPC", "HTTP+JSON", "URI"].includes(iface.protocolBinding)) {
  errors.push(`unsupported protocolBinding ${iface.protocolBinding}`);
}
if (!card.capabilities) errors.push("missing capabilities");
if (!card.defaultInputModes?.length || !card.defaultOutputModes?.length) {
  errors.push("missing default input or output modes");
}
const skill = card.skills?.[0];
if (!skill?.id || !skill.name || !skill.description || !skill.tags?.length) {
  errors.push("skills[0] needs id, name, description, tags");
}
if (errors.length) {
  console.error(errors.join("\n"));
  process.exit(1);
}
console.log(`agent card ok: ${card.name} ${card.version} ${iface.protocolBinding}`);
