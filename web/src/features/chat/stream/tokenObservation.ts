/** Display-only observations. Never feed text estimates into accounting or quota. */
export type TokenSource = "upstream" | "estimated" | "derived" | "unknown";
export type TokenProvenance = Partial<Record<"input_tokens" | "output_tokens" | "total_tokens", TokenSource>>;

export interface TextTokenEstimate {
  inputTokens: number;
  outputTokens: number;
  totalTokens: number;
}

export interface TokenObservation {
  inputTokens?: number;
  outputTokens?: number;
  totalTokens?: number;
  // "reported" means received from the server, not measured or settled usage.
  tokenUsageSource: "reported" | "unknown";
  textTokenEstimate?: TextTokenEstimate;
  tokenProvenance?: TokenProvenance;
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown> : {};
}

function count(value: unknown): number | undefined {
  // Legacy adapters/history may serialize valid counts as decimal strings.
  const parsed = typeof value === "string" && /^\+?\d+$/.test(value.trim()) ? Number(value.trim()) : value;
  return typeof parsed === "number" && Number.isSafeInteger(parsed) && parsed >= 0 ? parsed : undefined;
}

/** Merge only received fields; an omitted field cannot erase an earlier observation. */
export function observeTokenUsage(previous: TokenObservation, value: unknown): TokenObservation {
  const usage = record(value);
  const next = { ...previous };
  const provenance = { ...previous.tokenProvenance };
  const incomingProvenance = record(usage.token_provenance);
  for (const [field, key, alias] of [
    ["inputTokens", "input_tokens", "prompt_tokens"],
    ["outputTokens", "output_tokens", "completion_tokens"],
    ["totalTokens", "total_tokens", "total_tokens"],
  ] as const) {
    if (key in usage || alias in usage) {
      next[field] = count(usage[key] ?? usage[alias]);
      delete provenance[key];
      const source = incomingProvenance[key];
      if (next[field] !== undefined && (source === "upstream" || source === "estimated" || source === "derived" || source === "unknown")) {
        provenance[key] = source;
      }
    }
  }
  if (Object.keys(provenance).length) next.tokenProvenance = provenance;
  else delete next.tokenProvenance;
  next.tokenUsageSource = [next.inputTokens, next.outputTokens, next.totalTokens].some(value => value !== undefined)
    ? "reported" : "unknown";
  // A partial receipt does not imply that its missing components are zero.
  // Do not synthesize a total or replace a received total with a local sum.
  return next;
}

/** Old history can contain server or browser estimates without any provenance. */
export function restoreTokenObservation(value: unknown): TokenObservation {
  const stats = record(value);
  const observation = observeTokenUsage({ tokenUsageSource: "unknown" }, stats);
  observation.tokenUsageSource = stats.token_usage_source === "reported" ||
    Object.values(observation.tokenProvenance ?? {}).some(source => source !== "unknown")
    ? observation.tokenUsageSource : "unknown";
  const estimate = record(stats.text_token_estimate);
  const inputTokens = count(estimate.input_tokens);
  const outputTokens = count(estimate.output_tokens);
  const totalTokens = count(estimate.total_tokens);
  if (inputTokens !== undefined && outputTokens !== undefined && totalTokens !== undefined &&
      Number.isSafeInteger(inputTokens + outputTokens) && totalTokens === inputTokens + outputTokens) {
    observation.textTokenEstimate = { inputTokens, outputTokens, totalTokens };
  }
  return observation;
}
