import { useMemo, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useAuthenticatedReadScope } from "@/hooks/useAuthenticatedReadScope";
import * as modelsApi from "@/api/models";
import type { LLMModel } from "@/api/models";

type Notice = "saved" | "unknown" | "changed" | "readFailed" | "conflict" | null;
const sameModel = (left: LLMModel, right: LLMModel) => left.tenant_id === right.tenant_id && left.provider_id === right.provider_id && left.model_id === right.model_id;

/** Model status commands and their observed readback share one authenticated view. */
export function useManagedModels() {
  const scope = useAuthenticatedReadScope();
  const client = useQueryClient();
  const operation = useMemo(() => ({ scopeId: scope.id, busy: false, blocked: false }), [scope.id]);
  const [feedback, setFeedback] = useState<{ scope: string; busy: boolean; notice: Notice } | null>(null);
  const publish = (notice: Notice) => {
    if (scope.isCurrent()) setFeedback({ scope: operation.scopeId, busy: operation.busy, notice });
  };
  const query = useQuery({
    queryKey: [...modelsApi.modelQueryKeys.all, "management", scope.id],
    queryFn: async ({ signal }) => {
      if (!scope.isCurrent()) throw new Error("Model management scope changed");
      const rows = await modelsApi.listModels(undefined, true);
      if (!scope.isCurrent() || signal.aborted) throw new Error("Model management scope changed");
      return rows;
    },
    retry: false, gcTime: 0,
  });
  const mutation = useMutation({
    mutationFn: ({ model, enabled }: { model: LLMModel; enabled: boolean }) => {
      if (!scope.isCurrent()) throw new Error("Model management scope changed");
      return modelsApi.toggleModel(model.model_id, enabled, model.provider_id);
    },
    retry: false,
  });
  const ready = Boolean(scope.isCurrent() && query.data && !query.isError && !query.isFetching && !operation.busy && !operation.blocked);
  const toggle = async (model: LLMModel, enabled: boolean) => {
    if (!ready || operation.busy || operation.blocked || !scope.isCurrent()) return;
    operation.busy = true; publish(null);
    let notice: Notice = null;
    try {
      const before = await query.refetch({ throwOnError: true });
      if (!scope.isCurrent()) return;
      const observed = before.data?.find(row => sameModel(row, model));
      if (!observed || observed.is_enabled !== model.is_enabled || observed.updated_at !== model.updated_at) {
        operation.blocked = true; notice = "conflict"; return;
      }
      let accepted = false;
      try { await mutation.mutateAsync({ model, enabled }); accepted = true; } catch { /* A lost response cannot prove the command was not applied. */ }
      if (!scope.isCurrent()) return;
      operation.blocked = true;
      notice = accepted ? "readFailed" : "unknown";
      try {
        const after = await query.refetch({ throwOnError: true });
        if (!scope.isCurrent()) return;
        const current = after.data?.find(row => sameModel(row, model));
        if (accepted && current?.is_enabled === enabled) {
          operation.blocked = false; notice = "saved";
        } else if (accepted) notice = "changed";
      } catch { /* Keep last-observed rows and require explicit review. */ }
      if (scope.isCurrent()) void client.invalidateQueries({
        queryKey: modelsApi.modelQueryKeys.all,
        predicate: entry => entry.queryKey[1] !== "management",
      });
    } catch {
      notice = "readFailed";
    } finally {
      operation.busy = false; publish(notice);
    }
  };
  const review = async () => {
    if (operation.busy || !scope.isCurrent()) return;
    operation.busy = true; publish(feedback?.scope === scope.id ? feedback.notice : null);
    let notice: Notice = "readFailed";
    try {
      await query.refetch({ throwOnError: true });
      if (!scope.isCurrent()) return;
      operation.blocked = false; notice = null;
    } catch { /* An unavailable read cannot authorize another command. */ }
    finally { operation.busy = false; publish(notice); }
  };
  return { query, toggle, review, ready,
    busy: feedback?.scope === scope.id && feedback.busy,
    notice: feedback?.scope === scope.id ? feedback.notice : null };
}
