import { useQuery } from "@tanstack/react-query";
import { getTask, getTaskResult, listTasks } from "@/api/gateway";
import { useAuthStore } from "@/store/useAuthStore";
import { TASK_PAGE_SIZE } from "@/pages/tasks/taskNavigation";

export function useTasks(status?: import("@/api/contracts").RequestQuery<"GET /api/v1/tasks">["status"], offset = 0, scope?: { id: string; isCurrent: () => boolean }) {
  const userId = useAuthStore((state) => state.token ? state.user?.user_id : undefined);
  return useQuery({
    queryKey: ["tasks", userId, { status, offset }, ...(scope ? [scope.id] : [])],
    queryFn: async ({ signal }) => {
      if (scope && !scope.isCurrent()) throw new Error("Task observation retired");
      const result = await listTasks({ status, limit: TASK_PAGE_SIZE + 1, offset }, signal);
      if (scope && !scope.isCurrent()) throw new Error("Task observation retired");
      return result;
    },
    enabled: !!userId && (!scope || scope.isCurrent()),
    refetchInterval: (query) => {
      const tasks = query.state.data ?? [];
      return tasks.some((task) => ["pending", "processing"].includes(task.status))
        ? 3_000
        : 15_000;
    },
  });
}

export function useTask(taskId?: string, scope?: { id: string; isCurrent: () => boolean }) {
  const userId = useAuthStore((state) => state.token ? state.user?.user_id : undefined);
  return useQuery({
    queryKey: ["task", userId, taskId, ...(scope ? [scope.id] : [])],
    queryFn: async ({ signal }) => {
      if (scope && !scope.isCurrent()) throw new Error("Task observation retired");
      const result = await getTask(taskId!, signal);
      if (scope && !scope.isCurrent()) throw new Error("Task observation retired");
      return result;
    },
    enabled: !!userId && !!taskId && (!scope || scope.isCurrent()),
    // A terminal or unavailable receipt needs no background reads. Keep explicit
    // refetch available after errors instead of polling an invalid ID forever.
    refetchInterval: (query) => query.state.status !== "error"
      && ["pending", "processing"].includes(query.state.data?.status ?? "")
      ? 5_000
      : false,
  });
}

export function useTaskResult(taskId?: string, scope?: { id: string; isCurrent: () => boolean }) {
  const userId = useAuthStore((state) => state.token ? state.user?.user_id : undefined);
  return useQuery({
    queryKey: ["task-result", userId, taskId, ...(scope ? [scope.id] : [])],
    queryFn: async ({ signal }) => {
      if (scope && !scope.isCurrent()) throw new Error("Task observation retired");
      const result = await getTaskResult(taskId!, signal);
      if (scope && !scope.isCurrent()) throw new Error("Task observation retired");
      return result;
    },
    enabled: !!userId && !!taskId && (!scope || scope.isCurrent()),
  });
}
