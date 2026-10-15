import axios from "axios";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import {
  createLocalNodePairingChallenge,
  getLocalNodeCapabilities,
  getLocalNodeDoctor,
  getLocalNodeStatus,
  listLocalNodeEvents,
  listLocalNodeEventsAfter,
  listLocalNodeGrants,
  listLocalNodes,
  revokeLocalNodeGrant,
  type LocalNodeDeviceSummary,
  type LocalNodeEvent,
  type LocalNodeGrant,
  type LocalNodeHealthState,
} from "@/api/localNodes";
import { useAppStore } from "@/store/useAppStore";

import type {
  AppGrantCapability,
  DomainGrantCapability,
  LocalActionEvent,
  LocalActionKind,
  LocalNodeConnectionStatus,
  LocalOSControlSurfaceActions,
  LocalOSControlSurfaceModel,
  PermissionHealthItem,
  PermissionKind,
  WorkspaceGrantCapability,
} from "./types";

const EMPTY_GRANTS = { workspaces: [], apps: [], domains: [] };
const LOCAL_CAPABILITIES = ["Local files"];

const PERMISSION_COPY: Record<PermissionKind, { label: string; description: string }> = {
  files: {
    label: "Files",
    description: "Read or write only within explicit Local Node workspace grants.",
  },
  screen_recording: {
    label: "Screen recording",
    description: "Observe the selected screen or application window.",
  },
  accessibility: {
    label: "Accessibility",
    description: "Inspect and control supported desktop application elements.",
  },
  automation: {
    label: "Automation",
    description: "Allow the Local Node to address explicitly granted applications.",
  },
  restricted_process: {
    label: "Restricted process",
    description: "Run structured commands inside an authorized workspace.",
  },
};

const PERMISSION_KINDS = new Set<PermissionKind>(
  Object.keys(PERMISSION_COPY) as PermissionKind[],
);

const EMPTY_MODEL: LocalOSControlSurfaceModel = {
  devices: [],
  grants: EMPTY_GRANTS,
  actions: [],
  artifacts: [],
};

export type LocalOSLoadState = "loading" | "online" | "offline";

export interface LocalOSControlState {
  model: LocalOSControlSurfaceModel;
  actions: LocalOSControlSurfaceActions;
  loadState: LocalOSLoadState;
  onlineDeviceCount: number;
  pairingCode?: string;
  pairingExpiresAt?: string;
  sessionOptInRequested: boolean;
  sessionOptInEffective: boolean;
  canEnableForSession: boolean;
  sessionOptInReason: string;
  refresh: () => Promise<void>;
  pair: () => Promise<void>;
  setSessionOptIn: (enabled: boolean) => boolean;
  disableSessionOptIn: () => void;
  isSessionOptInEffectiveNow: () => boolean;
  getSessionBindingNow: () => LocalOSSessionBinding | undefined;
}

export interface LocalOSSessionBinding {
  deviceId: string;
  grantIds: string[];
}

export interface LocalOSEligibility {
  eligible: boolean;
  reason: string;
  deviceId?: string;
  grantIds: string[];
}

function unavailableModel(reason: string): LocalOSControlSurfaceModel {
  return {
    ...EMPTY_MODEL,
    degradation: {
      reason,
      unavailableCapabilities: LOCAL_CAPABILITIES,
      availableCloudCapabilities: [],
    },
  };
}

function unavailableReason(error: unknown): string {
  if (axios.isAxiosError(error)) {
    if (error.response?.status === 404) {
      return "Local Node support is not installed on this deployment.";
    }
    if (error.response?.status === 503) {
      return "The Local Node control plane is currently unavailable.";
    }
  }
  return "Local Node status could not be loaded. Cloud Assistant features remain unaffected.";
}

function connectionStatus(
  summary: LocalNodeDeviceSummary,
  protocolCompatible?: boolean,
): LocalNodeConnectionStatus {
  if (protocolCompatible === false) return "incompatible";
  return summary.status;
}

function permissionState(state: LocalNodeHealthState): PermissionHealthItem["status"] {
  return state;
}

function permissionItem(permission: {
  permission: string;
  state: LocalNodeHealthState;
  reason_code?: string | null;
  action_hint?: string | null;
}): PermissionHealthItem | null {
  if (!PERMISSION_KINDS.has(permission.permission as PermissionKind)) return null;
  const id = permission.permission as PermissionKind;
  const copy = PERMISSION_COPY[id];
  return {
    id,
    label: copy.label,
    description: copy.description,
    status: permissionState(permission.state),
    detail: permission.action_hint ?? permission.reason_code ?? undefined,
  };
}

function grantModel(grants: LocalNodeGrant[]): LocalOSControlSurfaceModel["grants"] {
  const workspaceCapabilities = new Set<WorkspaceGrantCapability>([
    "list",
    "read",
    "search",
    "hash",
    "watch",
    "write",
    "delete",
  ]);
  const appCapabilities = new Set<AppGrantCapability>(["observe", "control", "submit"]);
  const domainCapabilities = new Set<DomainGrantCapability>(["fetch", "upload"]);

  return {
    workspaces: grants
      .filter((grant) => grant.kind === "workspace")
      .map((grant) => ({
        id: grant.grant_id,
        displayName: grant.display_name,
        displayPath: grant.resource_ref ?? "Opaque device resource",
        capabilities: grant.capabilities.flatMap((capability) => {
          if (!capability.startsWith("file.")) return [];
          const value = capability.slice("file.".length) as WorkspaceGrantCapability;
          return workspaceCapabilities.has(value) ? [value] : [];
        }),
        sessionId: grant.session_id ?? undefined,
        expiresAt: grant.expires_at ?? undefined,
        status: grant.status,
      })),
    apps: grants
      .filter((grant) => grant.kind === "app")
      .map((grant) => ({
        id: grant.grant_id,
        displayName: grant.display_name,
        bundleIdentifier: grant.resource_ref ?? undefined,
        capabilities: grant.capabilities.flatMap((capability) => {
          if (!capability.startsWith("app.")) return [];
          const value = capability.slice("app.".length) as AppGrantCapability;
          return appCapabilities.has(value) ? [value] : [];
        }),
        sessionId: grant.session_id ?? undefined,
        expiresAt: grant.expires_at ?? undefined,
        status: grant.status,
      })),
    domains: grants
      .filter((grant) => grant.kind === "domain" && Boolean(grant.domain ?? grant.resource_ref))
      .map((grant) => ({
        id: grant.grant_id,
        origin: (grant.domain ?? grant.resource_ref) as string,
        capabilities: grant.capabilities.flatMap((capability) => {
          if (!capability.startsWith("network.")) return [];
          const value = capability.slice("network.".length) as DomainGrantCapability;
          return domainCapabilities.has(value) ? [value] : [];
        }),
        sessionId: grant.session_id ?? undefined,
        expiresAt: grant.expires_at ?? undefined,
        status: grant.status,
      })),
  };
}

function activeAt(expiresAt: string | undefined, now: number): boolean {
  if (!expiresAt) return true;
  const value = new Date(expiresAt).getTime();
  return Number.isFinite(value) && value > now;
}

function sessionGrantMatches(
  grantSessionId: string | undefined,
  activeSessionId: string | null | undefined,
): boolean {
  return !grantSessionId || grantSessionId === activeSessionId;
}

export function evaluateLocalOSEligibility(
  model: LocalOSControlSurfaceModel,
  loadState: LocalOSLoadState,
  activeSessionId: string | null | undefined,
  binding?: LocalOSSessionBinding,
  now = Date.now(),
): LocalOSEligibility {
  const device = model.devices.find((item) => item.id === model.selectedDeviceId);
  if (!device || !model.selectedDeviceId) {
    return { eligible: false, reason: "Select a paired Local Node first.", grantIds: [] };
  }
  if (loadState !== "online" || device.connectionStatus !== "online") {
    return {
      eligible: false,
      reason: "The selected Local Node must be online and protocol-compatible.",
      deviceId: device.id,
      grantIds: [],
    };
  }
  if (
    !model.selectedDeviceAuthority?.statusVerified ||
    model.selectedDeviceAuthority.deviceId !== device.id ||
    !model.selectedDeviceAuthority.protocolCompatible
  ) {
    return {
      eligible: false,
      reason: "Verified device status is required before enabling local capabilities.",
      deviceId: device.id,
      grantIds: [],
    };
  }
  if (model.degradation) {
    return {
      eligible: false,
      reason: "Refresh Local Node health before enabling local capabilities.",
      deviceId: device.id,
      grantIds: [],
    };
  }
  if (binding && binding.deviceId !== device.id) {
    return {
      eligible: false,
      reason: "The selected device changed. Enable Local OS again for this device.",
      deviceId: device.id,
      grantIds: [],
    };
  }

  const readyCapabilities = new Set(device.capabilities);
  const candidates = model.grantCapabilityBindings ?? [];
  const bindingGrantIds = binding ? new Set(binding.grantIds) : undefined;
  const grantIds = candidates
    .filter(
      (grant) =>
        grant.status === "active" &&
        activeAt(grant.expiresAt, now) &&
        sessionGrantMatches(grant.sessionId, activeSessionId) &&
        (!bindingGrantIds || bindingGrantIds.has(grant.grantId)) &&
        grant.capabilities.some((capability) => readyCapabilities.has(capability)),
    )
    .map((grant) => grant.grantId);
  if (grantIds.length === 0) {
    return {
      eligible: false,
      reason: binding
        ? "The grants bound to this session are no longer active and healthy."
        : "At least one active grant with a ready capability is required.",
      deviceId: device.id,
      grantIds: [],
    };
  }
  return {
    eligible: true,
    reason: "Local OS is bound to the selected device and active grant set for this session.",
    deviceId: device.id,
    grantIds,
  };
}

function eventKind(eventType: string): LocalActionKind {
  if (eventType.startsWith("file.")) return "file";
  if (eventType.startsWith("process.")) return "process";
  if (eventType.startsWith("browser.")) return "browser";
  if (eventType.startsWith("app.") || eventType.startsWith("desktop.")) return "desktop";
  if (eventType.startsWith("approval.")) return "approval";
  if (eventType.startsWith("takeover.")) return "takeover";
  return "system";
}

function actionEvents(events: LocalNodeEvent[]): LocalActionEvent[] {
  return events.map((event) => ({
    id: event.event_id,
    sequence: event.sequence,
    timestamp: event.occurred_at ?? event.created_at ?? new Date(0).toISOString(),
    kind: eventKind(event.event_type ?? event.event ?? "system"),
    title: event.event_type ?? event.event ?? "Local Node event",
    detail: event.summary ?? (
      event.payload && typeof event.payload.summary === "string"
        ? event.payload.summary
        : undefined
    ),
    status: event.status ?? "observed",
    artifactRefs: event.artifact_refs ?? [],
    errorCode: event.error_code ?? (
      event.payload && typeof event.payload.error_code === "string"
        ? event.payload.error_code
        : undefined
    ),
  }));
}

export function useLocalOSControl(): LocalOSControlState {
  const activeSessionId = useAppStore((state) => state.assistantActiveSessionId);
  const [model, setModel] = useState<LocalOSControlSurfaceModel>(EMPTY_MODEL);
  const [loadState, setLoadState] = useState<LocalOSLoadState>("loading");
  const [pairingCode, setPairingCode] = useState<string>();
  const [pairingExpiresAt, setPairingExpiresAt] = useState<string>();
  const [sessionOptInRequested, setSessionOptInRequested] = useState(false);
  const [sessionBinding, setSessionBinding] = useState<LocalOSSessionBinding>();
  const selectedDeviceRef = useRef<string | undefined>(undefined);
  const requestSequenceRef = useRef(0);
  const modelRef = useRef(model);
  const loadStateRef = useRef(loadState);
  const activeSessionIdRef = useRef(activeSessionId);
  const sessionOptInRequestedRef = useRef(sessionOptInRequested);
  const sessionBindingRef = useRef(sessionBinding);
  const lastEventSequenceRef = useRef(0);
  modelRef.current = model;
  loadStateRef.current = loadState;
  activeSessionIdRef.current = activeSessionId;
  sessionOptInRequestedRef.current = sessionOptInRequested;
  sessionBindingRef.current = sessionBinding;

  const disableSessionOptIn = useCallback(() => {
    sessionOptInRequestedRef.current = false;
    sessionBindingRef.current = undefined;
    setSessionOptInRequested(false);
    setSessionBinding(undefined);
  }, []);

  useEffect(() => {
    // Session consent never carries across a server-selected conversation.
    disableSessionOptIn();
  }, [activeSessionId, disableSessionOptIn]);

  const refreshForDevice = useCallback(async (preferredDeviceId?: string) => {
    const requestSequence = ++requestSequenceRef.current;
    try {
      const summaries = await listLocalNodes();
      if (requestSequence !== requestSequenceRef.current) return;

      if (summaries.length === 0) {
        selectedDeviceRef.current = undefined;
        setModel(unavailableModel("No paired Local Node is available."));
        setLoadState("offline");
        return;
      }

      const selectedDeviceId =
        preferredDeviceId && summaries.some((device) => device.device_id === preferredDeviceId)
          ? preferredDeviceId
          : selectedDeviceRef.current &&
              summaries.some((device) => device.device_id === selectedDeviceRef.current)
            ? selectedDeviceRef.current
            : summaries[0].device_id;
      selectedDeviceRef.current = selectedDeviceId;

      const [statusResult, capabilitiesResult, doctorResult, grantsResult, eventsResult] =
        await Promise.allSettled([
          getLocalNodeStatus(selectedDeviceId),
          getLocalNodeCapabilities(selectedDeviceId),
          getLocalNodeDoctor(selectedDeviceId),
          listLocalNodeGrants(selectedDeviceId),
          listLocalNodeEvents(selectedDeviceId),
        ]);
      if (requestSequence !== requestSequenceRef.current) return;

      const selectedSummary = summaries.find(
        (device) => device.device_id === selectedDeviceId,
      );
      const status = statusResult.status === "fulfilled" ? statusResult.value.device : undefined;
      const capabilities =
          capabilitiesResult.status === "fulfilled"
          ? capabilitiesResult.value.capabilities.map((capability) =>
              typeof capability === "string"
                ? { name: capability, state: "ready" as const }
                : capability,
            )
          : [];
      const permissions =
        doctorResult.status === "fulfilled"
          ? doctorResult.value.permissions
              .map(permissionItem)
              .filter((item): item is PermissionHealthItem => item !== null)
          : [];

      const devices = summaries.map((summary) => ({
        id: summary.device_id,
        label: summary.display_name,
        os: summary.platform,
        nodeVersion: summary.node_version,
        connectionStatus:
          summary.device_id === selectedDeviceId
            ? connectionStatus(summary, status?.protocol_compatible)
            : connectionStatus(summary),
        lastHeartbeatAt:
          summary.device_id === selectedDeviceId
            ? status?.last_seen_at ?? summary.last_seen_at ?? undefined
            : summary.last_seen_at ?? undefined,
          capabilities:
            summary.device_id === selectedDeviceId
              ? capabilities
                .filter((capability) => capability.state === "ready")
                .map((capability) => capability.name)
            : [],
        permissions: summary.device_id === selectedDeviceId ? permissions : [],
      }));
      const selectedOnline =
        selectedSummary?.status === "online" && status?.protocol_compatible === true;
      const partialFailure = [
        statusResult,
        capabilitiesResult,
        doctorResult,
        grantsResult,
        eventsResult,
      ].some((result) => result.status === "rejected");

      setModel({
        devices,
        selectedDeviceId,
        selectedDeviceAuthority: {
          deviceId: selectedDeviceId,
          statusVerified: statusResult.status === "fulfilled",
          protocolCompatible: status?.protocol_compatible === true,
          capabilityRevision:
            capabilitiesResult.status === "fulfilled"
              ? capabilitiesResult.value.revision
              : undefined,
        },
        grants:
          grantsResult.status === "fulfilled"
            ? grantModel(grantsResult.value)
            : EMPTY_GRANTS,
        grantCapabilityBindings:
          grantsResult.status === "fulfilled"
            ? grantsResult.value.map((grant) => ({
                grantId: grant.grant_id,
                capabilities: grant.capabilities,
                sessionId: grant.session_id ?? undefined,
                expiresAt: grant.expires_at ?? undefined,
                status: grant.status,
              }))
            : [],
        actions:
          eventsResult.status === "fulfilled" ? actionEvents(eventsResult.value) : [],
        artifacts: [],
        degradation:
          !selectedOnline || partialFailure
            ? {
                reason: partialFailure
                  ? "Some Local Node status data is unavailable; no missing capability is assumed."
                  : "The selected Local Node is not online and protocol-compatible.",
                unavailableCapabilities: selectedOnline ? [] : LOCAL_CAPABILITIES,
                availableCloudCapabilities: [],
              }
            : undefined,
      });
      lastEventSequenceRef.current =
        eventsResult.status === "fulfilled" && eventsResult.value.length > 0
          ? Math.max(...eventsResult.value.map((event) => event.sequence))
          : 0;
      setLoadState(selectedOnline ? "online" : "offline");
    } catch (error) {
      if (requestSequence !== requestSequenceRef.current) return;
      selectedDeviceRef.current = undefined;
      setModel(unavailableModel(unavailableReason(error)));
      setLoadState("offline");
    }
  }, []);

  const refresh = useCallback(() => refreshForDevice(), [refreshForDevice]);

  const pair = useCallback(async () => {
    const result = await createLocalNodePairingChallenge();
    setPairingCode(result.user_code);
    setPairingExpiresAt(result.expires_at);
  }, []);

  useEffect(() => {
    void refresh();
    return () => {
      requestSequenceRef.current += 1;
    };
  }, [refresh]);

  useEffect(() => {
    if (loadState !== "online" || !model.selectedDeviceId) return;
    const deviceId = model.selectedDeviceId;
    let cancelled = false;
    const poll = async () => {
      try {
        const events = await listLocalNodeEventsAfter(
          deviceId,
          lastEventSequenceRef.current,
        );
        if (cancelled || events.length === 0) return;
        lastEventSequenceRef.current = Math.max(
          lastEventSequenceRef.current,
          ...events.map((event) => event.sequence),
        );
        setModel((current) => ({
          ...current,
          actions: [...current.actions, ...actionEvents(events)]
            .filter(
              (event, index, values) =>
                values.findIndex((candidate) => candidate.id === event.id) === index,
            )
            .sort((left, right) => left.sequence - right.sequence),
        }));
      } catch {
        disableSessionOptIn();
        setLoadState("offline");
      }
    };
    const timer = window.setInterval(() => void poll(), 1_000);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [disableSessionOptIn, loadState, model.selectedDeviceId]);

  const currentEligibility = useMemo(
    () => evaluateLocalOSEligibility(model, loadState, activeSessionId, sessionBinding),
    [activeSessionId, loadState, model, sessionBinding],
  );
  const enableEligibility = useMemo(
    () => evaluateLocalOSEligibility(model, loadState, activeSessionId),
    [activeSessionId, loadState, model],
  );

  useEffect(() => {
    if (sessionOptInRequested && !currentEligibility.eligible) {
      disableSessionOptIn();
    }
  }, [currentEligibility.eligible, disableSessionOptIn, sessionOptInRequested]);

  const setSessionOptIn = useCallback((enabled: boolean): boolean => {
    if (!enabled) {
      disableSessionOptIn();
      return true;
    }
    const eligibility = evaluateLocalOSEligibility(
      modelRef.current,
      loadStateRef.current,
      activeSessionIdRef.current,
    );
    if (!eligibility.eligible || !eligibility.deviceId) return false;
    const binding = {
      deviceId: eligibility.deviceId,
      grantIds: eligibility.grantIds,
    };
    sessionBindingRef.current = binding;
    sessionOptInRequestedRef.current = true;
    setSessionBinding(binding);
    setSessionOptInRequested(true);
    return true;
  }, [disableSessionOptIn]);

  const isSessionOptInEffectiveNow = useCallback((): boolean => {
    if (!sessionOptInRequestedRef.current) return false;
    return evaluateLocalOSEligibility(
      modelRef.current,
      loadStateRef.current,
      activeSessionIdRef.current,
      sessionBindingRef.current,
    ).eligible;
  }, []);

  const getSessionBindingNow = useCallback((): LocalOSSessionBinding | undefined => {
    if (!sessionOptInRequestedRef.current) return undefined;
    const eligibility = evaluateLocalOSEligibility(
      modelRef.current,
      loadStateRef.current,
      activeSessionIdRef.current,
      sessionBindingRef.current,
    );
    if (!eligibility.eligible || !eligibility.deviceId) return undefined;
    return { deviceId: eligibility.deviceId, grantIds: eligibility.grantIds };
  }, []);

  const actions = useMemo<LocalOSControlSurfaceActions>(
    () => ({
      onSelectDevice: (deviceId) => {
        disableSessionOptIn();
        selectedDeviceRef.current = deviceId;
        void refreshForDevice(deviceId);
      },
      onRefreshDevice: (deviceId) => {
        void refreshForDevice(deviceId);
      },
      onRevokeWorkspaceGrant: (grantId) => {
        const deviceId = selectedDeviceRef.current;
        if (!deviceId) return;
        disableSessionOptIn();
        void revokeLocalNodeGrant(deviceId, grantId).then(() =>
          refreshForDevice(deviceId),
        );
      },
    }),
    [disableSessionOptIn, refreshForDevice],
  );

  return {
    model,
    actions,
    loadState,
    onlineDeviceCount: model.devices.filter(
      (device) => device.connectionStatus === "online",
    ).length,
    pairingCode,
    pairingExpiresAt,
    sessionOptInRequested,
    sessionOptInEffective:
      sessionOptInRequested && currentEligibility.eligible,
    canEnableForSession: enableEligibility.eligible,
    sessionOptInReason: sessionOptInRequested
      ? currentEligibility.reason
      : enableEligibility.reason,
    refresh,
    pair,
    setSessionOptIn,
    disableSessionOptIn,
    isSessionOptInEffectiveNow,
    getSessionBindingNow,
  };
}
