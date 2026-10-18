/** RFC 9457 plus legacy structured detail, bounded and independent of login. */
export interface GatewayProblem {
  type: string;
  title: string;
  status: number;
  detail: unknown;
  instance?: string;
}
export function parseGatewayProblem(status: number, body: string): GatewayProblem | undefined {
  if (body.length > 65536 || status < 400 || status > 599) return;
  try {
    const value: unknown = JSON.parse(body);
    if (!value || typeof value !== "object") return;
    const p = value as Record<string, unknown>;
    if (p.status !== status || typeof p.type !== "string" || p.type.length > 1024
      || typeof p.title !== "string" || p.title.length > 256 || !("detail" in p)) return;
    if (p.type !== "about:blank" && !p.type.startsWith("urn:agentforge:problem:") && !p.type.startsWith("https://")) return;
    return p as unknown as GatewayProblem;
  } catch { return; }
}
