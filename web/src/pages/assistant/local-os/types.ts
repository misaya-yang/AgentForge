/**
 * View-only contracts for the Assistant Local OS control surface.
 *
 * These types deliberately do not grant authority. The server and Local Node
 * remain the source of truth for capabilities, approvals, and action results.
 */

export type DeviceOperatingSystem = "macos" | "windows" | "linux" | "unknown";

export type LocalNodeConnectionStatus =
  | "online"
  | "offline"
  | "stale"
  | "pairing"
  | "revoked"
  | "incompatible";

export type PermissionHealthStatus =
  | "ready"
  | "denied"
  | "needs_action"
  | "unsupported"
  | "unknown";

export type PermissionKind =
  | "files"
  | "screen_recording"
  | "accessibility"
  | "automation"
  | "restricted_process";

export interface PermissionHealthItem {
  id: PermissionKind;
  label: string;
  description: string;
  status: PermissionHealthStatus;
  detail?: string;
  actionLabel?: string;
}

export interface LocalOSDevice {
  id: string;
  label: string;
  os: DeviceOperatingSystem;
  osVersion?: string;
  nodeVersion?: string;
  connectionStatus: LocalNodeConnectionStatus;
  lastHeartbeatAt?: string;
  capabilities: string[];
  permissions: PermissionHealthItem[];
}

export interface LocalOSSelectedDeviceAuthority {
  deviceId: string;
  protocolCompatible: boolean;
  statusVerified: boolean;
  capabilityRevision?: number;
}

export type WorkspaceGrantCapability =
  | "list"
  | "read"
  | "search"
  | "hash"
  | "watch"
  | "write"
  | "delete";

export interface WorkspaceGrant {
  id: string;
  displayName: string;
  /** A user-safe display path. It may already be redacted by the adapter. */
  displayPath: string;
  capabilities: WorkspaceGrantCapability[];
  sessionId?: string;
  expiresAt?: string;
  status: "active" | "pending" | "expired" | "revoked";
}

export type AppGrantCapability = "observe" | "control" | "submit";

export interface AppGrant {
  id: string;
  displayName: string;
  bundleIdentifier?: string;
  capabilities: AppGrantCapability[];
  sessionId?: string;
  expiresAt?: string;
  status: "active" | "pending" | "expired" | "unavailable" | "revoked";
}

export type DomainGrantCapability = "navigate" | "read" | "fetch" | "upload" | "submit";

export interface DomainGrant {
  id: string;
  origin: string;
  capabilities: DomainGrantCapability[];
  sessionId?: string;
  expiresAt?: string;
  status: "active" | "pending" | "expired" | "revoked";
}

export interface LocalOSGrants {
  workspaces: WorkspaceGrant[];
  apps: AppGrant[];
  domains: DomainGrant[];
}

export interface LocalOSGrantCapabilityBinding {
  grantId: string;
  capabilities: string[];
  sessionId?: string;
  expiresAt?: string;
  status: "active" | "pending" | "expired" | "revoked";
}

export type ComputerUseSessionStatus =
  | "idle"
  | "observing"
  | "awaiting_approval"
  | "controlling"
  | "paused"
  | "stopping"
  | "stopped"
  | "unavailable";

export type ComputerUseDriver =
  | "connector"
  | "dom"
  | "accessibility"
  | "computer_vision"
  | "unknown";

export interface LocalOSObservation {
  id: string;
  capturedAt: string;
  sequence: number;
  screenshotUrl?: string;
  screenshotAlt?: string;
  deviceLabel: string;
  appName?: string;
  windowTitle?: string;
  origin?: string;
  driver: ComputerUseDriver;
  sessionStatus: ComputerUseSessionStatus;
  maskingApplied: boolean;
  detail?: string;
}

export type LocalActionStatus =
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
  | "unknown";

export type LocalActionKind =
  | "file"
  | "process"
  | "browser"
  | "desktop"
  | "approval"
  | "takeover"
  | "system";

export interface LocalActionEvent {
  id: string;
  sequence: number;
  timestamp: string;
  kind: LocalActionKind;
  title: string;
  detail?: string;
  target?: string;
  driver?: ComputerUseDriver;
  status: LocalActionStatus;
  observationRef?: string;
  artifactRefs?: string[];
  errorCode?: string;
}

export type ApprovalRisk = "medium" | "high" | "critical";

export type ApprovalScope = "once" | "session" | "workspace" | "narrow_rule";

export interface ApprovalTargetSnapshot {
  label: string;
  digest: string;
  observedAt: string;
}

export interface DataDisclosure {
  summary: string;
  destination: string;
  bytes?: number;
}

export interface ExactApprovalRequest {
  id: string;
  runId: string;
  deviceId: string;
  deviceLabel: string;
  actionLabel: string;
  targetLabel: string;
  normalizedArguments: string;
  argumentsDigest: string;
  policySnapshotDigest: string;
  targetSnapshot: ApprovalTargetSnapshot;
  risk: ApprovalRisk;
  riskReasons: string[];
  reversible: boolean;
  rollbackDescription?: string;
  allowedScopes: ApprovalScope[];
  requestedScope: ApprovalScope;
  disclosures: DataDisclosure[];
  expiresAt: string;
}

export type TrustedConfirmationStatus =
  | "not_requested"
  | "waiting_for_device"
  | "shown_on_device"
  | "confirmed"
  | "rejected"
  | "expired"
  | "invalidated";

export interface TrustedConfirmationState {
  status: TrustedConfirmationStatus;
  deviceLabel: string;
  detail?: string;
  confirmedAt?: string;
}

export type LocalArtifactKind =
  | "diff"
  | "file"
  | "screenshot"
  | "process_output"
  | "rollback";

export interface LocalArtifactReceipt {
  id: string;
  kind: LocalArtifactKind;
  title: string;
  createdAt: string;
  status: "available" | "redacted" | "expired" | "unavailable";
  detail?: string;
  beforeDigest?: string;
  afterDigest?: string;
  diffText?: string;
  previewUrl?: string;
  rollbackRef?: string;
  rollbackStatus?: "available" | "running" | "succeeded" | "failed" | "not_supported";
}

export interface OfflineDegradation {
  reason: string;
  since?: string;
  unavailableCapabilities: string[];
  availableCloudCapabilities: string[];
}

export interface LocalOSControlSurfaceModel {
  devices: LocalOSDevice[];
  selectedDeviceId?: string;
  selectedDeviceAuthority?: LocalOSSelectedDeviceAuthority;
  grants: LocalOSGrants;
  /** Canonical server capability names retained for eligibility checks only. */
  grantCapabilityBindings?: LocalOSGrantCapabilityBinding[];
  observation?: LocalOSObservation;
  actions: LocalActionEvent[];
  approval?: ExactApprovalRequest;
  trustedConfirmation?: TrustedConfirmationState;
  artifacts: LocalArtifactReceipt[];
  degradation?: OfflineDegradation;
}

export interface LocalOSControlSurfaceActions {
  onSelectDevice: (deviceId: string) => void;
  onPairDevice?: () => void;
  onRevokeDevice?: (deviceId: string) => void;
  onRefreshDevice?: (deviceId: string) => void;
  onOpenPermissionSettings?: (deviceId: string, permission: PermissionKind) => void;
  onAddWorkspaceGrant?: () => void;
  onAddAppGrant?: () => void;
  onAddDomainGrant?: () => void;
  onRevokeWorkspaceGrant?: (grantId: string) => void;
  onRevokeAppGrant?: (grantId: string) => void;
  onRevokeDomainGrant?: (grantId: string) => void;
  onRequestTakeover?: () => void;
  onStopComputerUse?: () => void;
  onRequestTrustedConfirmation?: (approvalId: string) => void;
  onRejectApproval?: (approvalId: string) => void;
  onChangeApprovalScope?: (approvalId: string, scope: ApprovalScope) => void;
  onOpenArtifact?: (artifactId: string) => void;
  onRollbackArtifact?: (artifactId: string, rollbackRef: string) => void;
  onVerifyUnknownAction?: (actionId: string) => void;
}
