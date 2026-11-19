import { useQuery } from "@tanstack/react-query";
import {
  getHealth,
  listProxyServices,
  listServices,
  getProvidersHealth,
} from "@/api/gateway";
import { getArchitectureStatus } from "@/api/architecture";

export function useServices() {
  return useQuery({
    queryKey: ["services"],
    queryFn: listServices,
    staleTime: 30000, // 30秒内不重新请求，避免重复调用
  });
}

export function usePlaygroundServices() {
  return useQuery({
    queryKey: ["playground-services"],
    queryFn: listProxyServices,
    staleTime: 30000,
  });
}

export function useHealth() {
  return useQuery({
    queryKey: ["health"],
    queryFn: getHealth,
    staleTime: 15000, // 15秒内使用缓存，配合30秒自动刷新
    refetchInterval: () =>
      typeof document === "undefined" || !document.hidden ? 30000 : false,
    refetchIntervalInBackground: false,
  });
}

export function useArchitectureStatus(enabled: boolean) {
  return useQuery({
    queryKey: ["architecture-status"],
    queryFn: getArchitectureStatus,
    enabled,
    staleTime: 15000,
    refetchInterval: enabled ? 30000 : false,
    refetchIntervalInBackground: false,
    retry: false,
  });
}

export function useProvidersHealth() {
  return useQuery({
    queryKey: ["providers-health"],
    queryFn: getProvidersHealth,
    staleTime: 30000, // 30秒内使用缓存
    refetchInterval: 60000, // 每分钟刷新
  });
}
