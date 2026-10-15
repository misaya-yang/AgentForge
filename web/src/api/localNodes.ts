import { api } from "@/lib/api";

// Local Nodes are a Gateway-owned control plane, not a legacy assistant route
// route. Keeping this path under /assistant made every status/pairing request
// 404 after the Runtime cutover.
const LOCAL_NODES_PATH = "/api/v1/local-nodes";

export type LocalNodeDeviceState = "online" | "offline" | "stale" | "revoked";
export type LocalNodeHealthState =
  | "ready"
  | "denied"
  | "needs_action"
  | "unsupported"
  | "unknown";

export interface LocalNodeDeviceSummary {
  device_id: string;
  display_name: string;
  platform: "macos" | "windows" | "linux";
  node_version: string;
  status: LocalNodeDeviceState;
  last_seen_at?: string | null;
}

export interface LocalNodeDeviceStatus {
  device: {
    device_id: string;
    status: LocalNodeDeviceState;
    last_seen_at?: string | null;
    active_action_id?: string | null;
    active_lease_expires_at?: string | null;
    protocol_compatible: boolean;
  };
}

export interface LocalNodeCapabilities {
  device_id: string;
  revision: number;
  capabilities: Array<string | {
    name: string;
    state: LocalNodeHealthState;
    reason_code?: string | null;
  }>;
}

export interface LocalNodeDoctor {
  device_id: string;
  checked_at?: string;
  status?: LocalNodeHealthState;
  permissions: Array<{
    permission: string;
    state: LocalNodeHealthState;
    checked_at: string;
    reason_code?: string | null;
    action_hint?: string | null;
  }>;
}

export interface LocalNodeGrant {
  grant_id: string;
  device_id: string;
  kind: "workspace" | "app" | "domain";
  display_name: string;
  resource_ref?: string | null;
  domain?: string | null;
  capabilities: string[];
  session_id?: string | null;
  status: "active" | "pending" | "expired" | "revoked";
  created_at: string;
  expires_at?: string | null;
}

export interface LocalNodeEvent {
  event_id: string;
  device_id?: string;
  sequence: number;
  event_type?: string;
  /** Gateway Local Node API uses `event` and `created_at` on the wire. */
  event?: string | null;
  occurred_at?: string;
  created_at?: string;
  action_id?: string | null;
  status?:
    | "proposed"
    | "policy_check"
    | "awaiting_approval"
    | "dispatched"
    | "running"
    | "observed"
    | "succeeded"
    | "failed"
    | "cancelled"
    | "interrupted"
    | "unknown"
    | null;
  summary?: string | null;
  payload?: Record<string, unknown> | null;
  result_digest?: string | null;
  artifact_refs?: string[];
  error_code?: string | null;
}

export interface PairingChallenge {
  challenge_id: string;
  user_code: string;
  expires_at: string;
}

function devicePath(deviceId: string, suffix: string): string {
  return `${LOCAL_NODES_PATH}/${encodeURIComponent(deviceId)}${suffix}`;
}

export async function listLocalNodes(): Promise<LocalNodeDeviceSummary[]> {
  const { data } = await api.get<{ devices: LocalNodeDeviceSummary[] }>(
    LOCAL_NODES_PATH,
  );
  return data.devices;
}

export async function getLocalNodeStatus(
  deviceId: string,
): Promise<LocalNodeDeviceStatus> {
  const { data } = await api.get<LocalNodeDeviceStatus>(devicePath(deviceId, "/status"));
  return data;
}

export async function getLocalNodeCapabilities(
  deviceId: string,
): Promise<LocalNodeCapabilities> {
  const { data } = await api.get<LocalNodeCapabilities>(
    devicePath(deviceId, "/capabilities"),
  );
  return data;
}

export async function getLocalNodeDoctor(deviceId: string): Promise<LocalNodeDoctor> {
  const { data } = await api.get<LocalNodeDoctor>(devicePath(deviceId, "/doctor"));
  return data;
}

export async function listLocalNodeGrants(deviceId: string): Promise<LocalNodeGrant[]> {
  const { data } = await api.get<{ grants: LocalNodeGrant[] }>(
    devicePath(deviceId, "/grants"),
  );
  return data.grants;
}

export async function listLocalNodeEvents(deviceId: string): Promise<LocalNodeEvent[]> {
  const { data } = await api.get<{ events: LocalNodeEvent[] }>(
    devicePath(deviceId, "/events"),
    { params: { after_sequence: 0, limit: 100 } },
  );
  return data.events;
}

export async function listLocalNodeEventsAfter(
  deviceId: string,
  afterSequence: number,
): Promise<LocalNodeEvent[]> {
  const { data } = await api.get<{ events: LocalNodeEvent[] }>(
    devicePath(deviceId, "/events"),
    { params: { after_sequence: afterSequence, limit: 100 } },
  );
  return data.events;
}

export async function revokeLocalNodeGrant(
  deviceId: string,
  grantId: string,
): Promise<void> {
  await api.delete(devicePath(deviceId, `/grants/${encodeURIComponent(grantId)}`));
}

export async function createLocalNodePairingChallenge(): Promise<PairingChallenge> {
  const { data } = await api.post<PairingChallenge>(
    `${LOCAL_NODES_PATH}/pairing/challenges`,
    { expires_in_seconds: 180 },
  );
  return data;
}
