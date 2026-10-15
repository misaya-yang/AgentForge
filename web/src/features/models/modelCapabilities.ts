import type { ModelCapabilityProfile } from "@/api/models";

export function createSafeModelCapabilityProfile(
  supportsTools = true,
  supportsVision = false
): ModelCapabilityProfile {
  return {
    schema_version: 1,
    reasoning: {
      adapter_id: "reasoning/none-v1",
      default_option: "off",
      options: [
        {
          id: "off",
          label: "Off",
          aliases: [],
          canonical_effort: "none",
          settings: {},
        },
      ],
      visibility: "none",
      replay_policy: "discard_after_turn",
    },
    prompt_cache: { adapter_id: "cache/none-v1", config: {} },
    native_search: { adapter_id: "search/none-v1", enabled: false, config: {} },
    tools: {
      function_calling: supportsTools,
      parallel_calls: false,
      strict_schema: false,
    },
    modalities: {
      input: supportsVision ? ["text", "image"] : ["text"],
      output: ["text"],
    },
    streaming: {
      text_deltas: true,
      reasoning_deltas: false,
      tool_call_deltas: supportsTools,
    },
  };
}

export function cloneModelCapabilityProfile(
  profile: ModelCapabilityProfile
): ModelCapabilityProfile {
  return JSON.parse(JSON.stringify(profile)) as ModelCapabilityProfile;
}

/**
 * The backend emits `catalog_capabilities: {}` for models without a builtin
 * catalog entry, and partial objects are possible from direct API writes.
 * Only a structurally complete profile may seed the editor — anything less
 * would crash the editor's unconditional section dereferences.
 */
export function isUsableModelCapabilityProfile(
  profile: Partial<ModelCapabilityProfile> | null | undefined
): profile is ModelCapabilityProfile {
  if (!profile || typeof profile !== "object") return false;
  return Boolean(
    profile.schema_version &&
      profile.reasoning &&
      Array.isArray(profile.reasoning.options) &&
      profile.prompt_cache &&
      profile.native_search &&
      profile.tools &&
      profile.modalities &&
      profile.streaming
  );
}

function stableStringify(value: unknown): string {
  if (Array.isArray(value)) {
    return `[${value.map((item) => stableStringify(item)).join(",")}]`;
  }
  if (value && typeof value === "object") {
    const record = value as Record<string, unknown>;
    return `{${Object.keys(record)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${stableStringify(record[key])}`)
      .join(",")}}`;
  }
  return JSON.stringify(value);
}

/** Order-insensitive deep equality for JSON-safe capability profiles. */
export function modelCapabilityProfilesEqual(
  a: ModelCapabilityProfile | null | undefined,
  b: ModelCapabilityProfile | null | undefined
): boolean {
  if (a === b) return true;
  if (!a || !b) return false;
  return stableStringify(a) === stableStringify(b);
}

const OPTION_ID_PATTERN = /^[a-z][a-z0-9_-]{0,31}$/;

/**
 * Client-side mirror of the server's reasoning-option validation
 * (ai_gateway_core rejects ids/aliases that are empty, malformed,
 * duplicated, or the reserved ``auto`` token — and an alias may not
 * shadow any declared option id). Returns a short failure slug or null.
 */
export function validateCapabilityProfileOptionIds(
  profile: ModelCapabilityProfile | null | undefined
): "invalid" | "duplicate" | null {
  const options = profile?.reasoning?.options ?? [];
  const declaredIds = new Set<string>();
  for (const option of options) {
    const id = String(option?.id ?? "");
    if (id) declaredIds.add(id);
  }
  const seenIds = new Set<string>();
  const seenAliases = new Set<string>();
  for (const option of options) {
    const id = String(option?.id ?? "");
    if (!id || id === "auto" || !OPTION_ID_PATTERN.test(id)) return "invalid";
    if (seenIds.has(id)) return "duplicate";
    seenIds.add(id);
    const label = String(option?.label ?? "").trim();
    if (!label || label.length > 64) return "invalid";
    for (const alias of option?.aliases ?? []) {
      const normalized = String(alias ?? "");
      if (!normalized || normalized === "auto" || !OPTION_ID_PATTERN.test(normalized)) {
        return "invalid";
      }
      if (seenAliases.has(normalized) || declaredIds.has(normalized)) return "duplicate";
      seenAliases.add(normalized);
    }
  }
  return null;
}

export function reasoningOptionsForModel(profile?: ModelCapabilityProfile | null) {
  return profile?.reasoning.options ?? [];
}

export function resolveReasoningOptionId(
  profile: ModelCapabilityProfile | null | undefined,
  requested: string | null | undefined
): string {
  if (!profile || !requested || requested === "auto") return "auto";
  const normalized = requested.toLowerCase();
  const match = profile.reasoning.options.find(
    (option) => option.id === normalized || option.aliases.includes(normalized)
  );
  return match?.id ?? "auto";
}
