export type StreamTerminalOutcome = "succeeded" | "failed" | "cancelled";

export interface StreamTerminalLatch {
  accept: (outcome: StreamTerminalOutcome) => boolean;
  current: () => StreamTerminalOutcome | undefined;
}

export function createStreamTerminalLatch(): StreamTerminalLatch {
  let outcome: StreamTerminalOutcome | undefined;
  return {
    accept(next) {
      if (outcome !== undefined) return false;
      outcome = next;
      return true;
    },
    current() {
      return outcome;
    },
  };
}
