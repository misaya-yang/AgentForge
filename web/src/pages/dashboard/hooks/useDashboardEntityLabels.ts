import { useMemo } from "react";
import { useQuery } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { listServices } from "@/api/gateway";
import { listUsers } from "@/api/users";
import { listApiKeys } from "@/api/apiKeys";
import { getUsageBreakdown } from "@/api/usage";
import { useDashboardContext } from "../DashboardContext";
import { useAuthStore } from "@/store/useAuthStore";

interface SelectOption {
  label: string;
  value: string;
}

function toSafeString(value: unknown): string {
  return String(value || "").trim();
}

function normalizeLabel(value: string): string {
  return value.replace(/\s+/g, " ").trim();
}

function formatApiKeyFallback(userId: string): string {
  const suffix = userId.replace(/^apikey:/, "").slice(0, 8);
  return `API Key · ${suffix}`;
}

export function useDashboardEntityLabels() {
  const { t } = useTranslation();
  const { dateRange } = useDashboardContext();
  const roles = useAuthStore((state) => state.user?.roles || []);
  const canReadUserDirectory = roles.includes("admin") || roles.includes("operator");

  const servicesQuery = useQuery({
    queryKey: ["dashboard-services"],
    queryFn: listServices,
    staleTime: 60_000,
  });

  const serviceBreakdownQuery = useQuery({
    queryKey: ["dashboard-service-breakdown-options", dateRange],
    queryFn: () =>
      getUsageBreakdown({
        dimension: "service",
        start_date: dateRange[0],
        end_date: dateRange[1],
        limit: 100,
      }),
    staleTime: 60_000,
  });

  const userBreakdownQuery = useQuery({
    queryKey: ["dashboard-user-breakdown-options", dateRange],
    queryFn: () =>
      getUsageBreakdown({
        dimension: "user",
        start_date: dateRange[0],
        end_date: dateRange[1],
        limit: 100,
      }),
    staleTime: 60_000,
  });

  const usersQuery = useQuery({
    queryKey: ["dashboard-user-directory"],
    queryFn: () => listUsers({ page: 1, page_size: 100 }),
    staleTime: 120_000,
    enabled: canReadUserDirectory,
  });

  const apiKeysQuery = useQuery({
    queryKey: ["dashboard-api-keys"],
    queryFn: () => listApiKeys(true),
    staleTime: 120_000,
  });

  const serviceNameMap = useMemo(() => {
    const map = new Map<string, string>();
    for (const svc of servicesQuery.data || []) {
      const id = toSafeString(svc.service_id);
      if (!id) continue;
      const display = normalizeLabel(
        toSafeString(svc.display_name) || toSafeString(svc.name) || id
      );
      map.set(id, display);
    }

    for (const item of serviceBreakdownQuery.data?.items || []) {
      const id = toSafeString(item.service);
      if (!id) continue;
      const label = normalizeLabel(toSafeString(item.label) || id);
      if (!map.has(id)) {
        map.set(id, label);
      }
    }

    return map;
  }, [serviceBreakdownQuery.data?.items, servicesQuery.data]);

  const userNameMap = useMemo(() => {
    const map = new Map<string, string>();
    for (const user of usersQuery.data?.users || []) {
      const id = toSafeString(user.user_id);
      if (!id) continue;
      const display = normalizeLabel(
        toSafeString(user.display_name) ||
          toSafeString(user.username) ||
          toSafeString(user.email) ||
          id
      );
      map.set(id, display);
    }

    for (const key of apiKeysQuery.data || []) {
      const name = normalizeLabel(toSafeString(key.name));
      const derivedUserId = toSafeString(key.derived_user_id);
      if (derivedUserId) {
        map.set(derivedUserId, name ? `API Key · ${name}` : formatApiKeyFallback(derivedUserId));
      }
      const keyUserId = toSafeString(key.user_id);
      if (keyUserId.startsWith("apikey:")) {
        map.set(keyUserId, name ? `API Key · ${name}` : formatApiKeyFallback(keyUserId));
      }
    }

    for (const item of userBreakdownQuery.data?.items || []) {
      const id = toSafeString(item.user);
      if (!id) continue;
      const label = normalizeLabel(toSafeString(item.label));
      if (label) {
        map.set(id, label);
      }
    }

    return map;
  }, [apiKeysQuery.data, userBreakdownQuery.data?.items, usersQuery.data?.users]);

  const resolveServiceLabel = (serviceId: string): string => {
    const id = toSafeString(serviceId);
    if (!id) return t("common.unknown");
    return serviceNameMap.get(id) || id;
  };

  const resolveUserLabel = (userId: string): string => {
    const id = toSafeString(userId);
    if (!id) return t("common.unknown");
    if (userNameMap.has(id)) return userNameMap.get(id)!;
    if (id.startsWith("anon:")) {
      return t("dashboard.filters.anonymousUser", { id: id.slice(-6) });
    }
    if (id.startsWith("apikey:")) {
      return formatApiKeyFallback(id);
    }
    return id;
  };

  const serviceOptions: SelectOption[] = useMemo(
    () => [
      { label: t("dashboard.filters.allServices"), value: "all" },
      ...Array.from(serviceNameMap.entries())
        .sort((a, b) => a[1].localeCompare(b[1]))
        .map(([value, label]) => ({ value, label })),
    ],
    [serviceNameMap, t]
  );

  const userIds = new Set<string>();
  for (const item of userBreakdownQuery.data?.items || []) {
    const id = toSafeString(item.user);
    if (id) userIds.add(id);
  }
  for (const id of userNameMap.keys()) {
    if (id) userIds.add(id);
  }

  const userOptions: SelectOption[] = [
    { label: t("dashboard.filters.allUsers"), value: "all" },
    ...Array.from(userIds)
      .sort((a, b) => resolveUserLabel(a).localeCompare(resolveUserLabel(b)))
      .map((id) => ({
        value: id,
        label: resolveUserLabel(id),
      })),
  ];

  return {
    serviceOptions,
    userOptions,
    resolveServiceLabel,
    resolveUserLabel,
    isLoading:
      servicesQuery.isLoading ||
      serviceBreakdownQuery.isLoading ||
      userBreakdownQuery.isLoading,
  };
}
