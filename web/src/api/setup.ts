import { useQuery } from "@tanstack/react-query";
import { api } from "@/lib/api";
import { useAuthStore } from "@/store/useAuthStore";

export interface SetupState {
  configured: boolean;
  missing: string[];
  mode: string;
  default_model: string;
}

async function fetchSetupState(): Promise<SetupState> {
  const { data } = await api.get<SetupState>("/api/v1/setup/state");
  return data;
}

export function useSetupState() {
  const user = useAuthStore((state) => state.user);
  const hasPermission = useAuthStore((state) => state.hasPermission);
  const canViewServices = Boolean(user && hasPermission("console:services:view"));

  return useQuery({
    queryKey: ["setup-state"],
    queryFn: fetchSetupState,
    enabled: canViewServices,
    staleTime: 5 * 60 * 1000, // 5 minutes — setup changes rarely
    refetchOnWindowFocus: true,
    retry: false,
  });
}
