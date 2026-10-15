import type { AgentDetail, AgentVersion } from "../../types/agents";

export type ComparisonValue = { state: "absent" | "unavailable" | "withheld" | "value"; text?: string };
export interface VersionComparisonRow {
  key: string;
  section: string;
  field: string;
  position?: number;
  changed: boolean;
  before: ComparisonValue;
  after: ComparisonValue;
}

const absent = Symbol("absent");
const record = (value: unknown): value is Record<string, unknown> => Boolean(value) && typeof value === "object" && !Array.isArray(value);
function property(value: unknown, key: string): unknown {
  return record(value) && Object.hasOwn(value, key) ? value[key] : absent;
}

// JSON object ordering is immaterial; list order, missing fields and null remain
// distinct in the returned read model. The server may already redact or
// normalize these snapshots. Do not infer original stored identity or use hashes
// as a substitute for comparing the readable values; never add defaults here.
export function sameVersionValue(before: unknown, after: unknown): boolean {
  if (Object.is(before, after)) return true;
  if (Array.isArray(before) && Array.isArray(after)) {
    return before.length === after.length && before.every((value, index) => sameVersionValue(value, after[index]));
  }
  if (!record(before) || !record(after)) return false;
  const keys = Object.keys(before);
  return keys.length === Object.keys(after).length && keys.every(key => Object.hasOwn(after, key) && sameVersionValue(before[key], after[key]));
}

function visible(value: unknown, kind: "string" | "number" | "boolean" | "strings" | "mode" | "color" | "url" | "opaque"): ComparisonValue {
  if (value === absent) return { state: "absent" };
  if (kind === "opaque") return { state: "withheld" };
  if (value === null) return { state: "value", text: "null" };
  if (kind === "strings" && Array.isArray(value) && value.every(item => typeof item === "string")) {
    return { state: "value", text: JSON.stringify(value, null, 2) };
  }
  if (kind === "mode" && typeof value === "string" && ["off", "session", "user"].includes(value)) return { state: "value", text: value };
  if (kind === "color" && typeof value === "string" && /^#[\da-f]{3,8}$/i.test(value)) return { state: "value", text: value };
  if (kind === "url" && typeof value === "string") {
    try {
      const url = new URL(value);
      if (url.protocol === "https:" && !url.username && !url.password && !url.search && !url.hash) return { state: "value", text: value };
    } catch { /* Do not render malformed URLs or opaque credential material. */ }
    return { state: "withheld" };
  }
  if ((kind === "string" && typeof value === "string") || (kind === "boolean" && typeof value === "boolean")
    || (kind === "number" && typeof value === "number" && Number.isFinite(value))) return { state: "value", text: String(value) };
  return { state: "unavailable" };
}

function extra(value: unknown, known: string[]): unknown {
  if (value === absent) return absent;
  if (!record(value)) return value;
  const keys = Object.keys(value).filter(key => !known.includes(key));
  return keys.length ? Object.fromEntries(keys.map(key => [key, value[key]])) : absent;
}

/** The returned rows contain only allowlisted presentation values, never opaque config. */
export function compareAgentVersions(before: AgentVersion, after: AgentVersion): VersionComparisonRow[] {
  const rows: VersionComparisonRow[] = [];
  const add = (section: string, field: string, left: unknown, right: unknown, kind: Parameters<typeof visible>[1], position?: number) => {
    rows.push({ key: `${section}.${position ?? ""}.${field}`, section, field, position,
      changed: !sameVersionValue(left, right), before: visible(left, kind), after: visible(right, kind) });
  };
  const specs = [before.spec, after.spec];
  const fields = (section: string, definitions: Array<[string, Parameters<typeof visible>[1]]>) => {
    const [left, right] = specs.map(spec => property(spec, section));
    if (!record(left) || !record(right)) add(section, "structure", left, right, "opaque");
    for (const [field, kind] of definitions) add(section, field, property(left, field), property(right, field), kind);
    const known = definitions.map(([field]) => field);
    const extras = [extra(left, known), extra(right, known)];
    if (extras.some(value => value !== absent)) add(section, "other", extras[0], extras[1], "opaque");
  };
  add("schema", "schema_version", before.schema_version, after.schema_version, "string");
  add("schema", "spec_schema_version", property(before.spec, "schema_version"), property(after.spec, "schema_version"), "string");
  add("instructions", "instructions", property(before.spec, "instructions"), property(after.spec, "instructions"), "string");
  fields("identity", [["welcome_message", "string"], ["suggested_prompts", "strings"], ["icon_url", "url"], ["theme_color", "color"]]);
  fields("outcome", [["goal", "string"], ["triggers", "strings"], ["inputs", "strings"], ["human_boundaries", "strings"], ["success_criteria", "strings"]]);
  fields("model", [["provider_id", "string"], ["model_id", "string"], ["temperature", "number"], ["max_tokens", "number"], ["thinking_mode", "string"]]);
  fields("memory", [["mode", "mode"]]);
  for (const section of ["capabilities", "knowledge"]) {
    const [left, right] = specs.map(spec => property(spec, section));
    add(section, "count", Array.isArray(left) ? left.length : left, Array.isArray(right) ? right.length : right, "number");
    const definitions: Array<[string, Parameters<typeof visible>[1]]> = section === "capabilities"
      ? [["type", "string"], ["resource_id", "string"], ["resource_version", "string"], ["schema_hash", "string"], ["config", "opaque"]]
      : [["dataset_id", "string"]];
    const size = Math.max(Array.isArray(left) ? left.length : 0, Array.isArray(right) ? right.length : 0);
    for (let index = 0; index < size; index++) {
      const a = Array.isArray(left) && index < left.length ? left[index] : absent;
      const b = Array.isArray(right) && index < right.length ? right[index] : absent;
      for (const [field, kind] of definitions) add(section, field, property(a, field), property(b, field), kind, index + 1);
      if (section === "knowledge") {
        const retrieval = [property(a, "retrieval_config"), property(b, "retrieval_config")];
        if (retrieval.some(value => !record(value))) add(section, "structure", retrieval[0], retrieval[1], "opaque", index + 1);
        const configFields: Array<[string, Parameters<typeof visible>[1]]> = [["mode", "string"], ["top_k", "number"], ["threshold", "number"], ["include_images", "boolean"]];
        for (const [field, kind] of configFields) add(section, field, property(retrieval[0], field), property(retrieval[1], field), kind, index + 1);
        const extras = retrieval.map(value => extra(value, configFields.map(([field]) => field)));
        if (extras.some(value => value !== absent)) add(section, "retrieval_other", extras[0], extras[1], "opaque", index + 1);
      }
      const extras = [a, b].map(value => extra(value, [...definitions.map(([field]) => field), ...(section === "knowledge" ? ["retrieval_config"] : [])]));
      if (extras.some(value => value !== absent)) add(section, "other", extras[0], extras[1], "opaque", index + 1);
    }
  }
  const extras = specs.map(spec => extra(spec, ["schema_version", "identity", "outcome", "instructions", "model", "capabilities", "knowledge", "memory"]));
  if (extras.some(value => value !== absent)) add("other", "other", extras[0], extras[1], "opaque");
  return rows;
}

export function validateVersionRead(agentId: string, agent: AgentDetail, versions: AgentVersion[]): AgentVersion[] {
  const nonempty = (value: unknown): value is string => typeof value === "string" && value.trim().length > 0;
  const revision = (value: unknown) => Number.isSafeInteger(value) && Number(value) >= 1;
  if (!agentId || agent.agent_id !== agentId || !nonempty(agent.tenant_id) || !Array.isArray(versions)) throw new Error("Version observation unavailable");
  const ids = new Set<string>();
  for (const version of versions) {
    if (version.agent_id !== agentId || version.tenant_id !== agent.tenant_id || !nonempty(version.agent_version_id)
      || !nonempty(version.spec_hash) || !nonempty(version.schema_version)
      || !nonempty(version.created_at) || !Number.isFinite(Date.parse(version.created_at))
      || !revision(version.version_number) || !revision(version.source_draft_revision)
      || ids.has(version.agent_version_id) || !record(version.spec)) throw new Error("Version observation unavailable");
    ids.add(version.agent_version_id);
  }
  return [...versions].sort((a, b) => b.version_number - a.version_number);
}
