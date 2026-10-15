export function futurePublicExpiry(value: string, now = Date.now()): string | null {
  if (!value) return null;
  const time = new Date(value).getTime();
  return Number.isFinite(time) && time > now ? new Date(time).toISOString() : null;
}
