import { describe, it, expect } from "vitest";
import { parseGatewayProblem } from "./problem.js";
import { GatewayHttpError } from "./client.js";
describe("typed gateway problems", () => {
  it("keeps structured detail and rejects spoofed HTTP status", () => {
    const p = { type: "urn:agentforge:problem:permission-denied", title: "Forbidden", status: 403, detail: { required_permission: "x" } };
    expect(parseGatewayProblem(403, JSON.stringify(p))?.detail).toEqual(p.detail);
    const error = new GatewayHttpError(403, JSON.stringify(p), 250);
    expect(error.problem?.detail).toEqual(p.detail);
    expect(error.retryAfterMs).toBe(250);
    expect(error.body).toBe(JSON.stringify(p));
    expect(parseGatewayProblem(401, JSON.stringify(p))).toBeUndefined();
    expect(parseGatewayProblem(403, '{"detail":"legacy"}')).toBeUndefined();
  });
});
