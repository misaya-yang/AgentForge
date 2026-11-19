/** Late Worker facts refine cancellation without reopening the original run. */
export function cancelledReceiptFlags(
  messages: Array<{ metadata?: Record<string, unknown> | null }>,
  runId: string,
): { outcomeUncertain: boolean; sourceAccessRevoked: boolean } {
  const matching = messages.filter((m) => m.metadata?.runtime_run_id === runId);
  return {
    outcomeUncertain: matching.some((m) => {
      const summary = m.metadata?.process_summary;
      return Boolean(summary && typeof summary === "object" &&
        (summary as Record<string, unknown>).outcome_uncertain === true);
    }),
    sourceAccessRevoked: matching.some((m) => m.metadata?.source_access_revoked === true),
  };
}
