import { Plus, RotateCcw, Trash2 } from "lucide-react";
import { useTranslation } from "react-i18next";
import type {
  CanonicalReasoningEffort,
  ModelCapabilityAdapter,
  ModelCapabilityProfile,
  ModelReasoningOption,
} from "@/api/models";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

const EFFORTS: CanonicalReasoningEffort[] = [
  "none",
  "minimal",
  "low",
  "medium",
  "high",
  "xhigh",
  "adaptive",
  "max",
  "ultra",
];

const OPTION_ID_PATTERN = /^[a-z][a-z0-9_-]{0,31}$/;

/** Mirrors the server-side rejection of empty/malformed/reserved/duplicated ids. */
function optionIdProblem(
  option: ModelReasoningOption,
  options: ModelReasoningOption[],
  index: number
): boolean {
  const id = option.id;
  if (!id || id === "auto" || !OPTION_ID_PATTERN.test(id)) return true;
  return options.some(
    (other, otherIndex) =>
      otherIndex !== index && (other.id === id || (other.aliases ?? []).includes(id))
  );
}

interface ModelCapabilityEditorProps {
  profile: ModelCapabilityProfile;
  adapters: ModelCapabilityAdapter[];
  onChange: (profile: ModelCapabilityProfile) => void;
  onReset?: () => void;
}

function settingsForAdapter(
  adapterId: string,
  option: ModelReasoningOption
): Record<string, unknown> {
  const enabled = option.canonical_effort !== "none";
  if (adapterId === "reasoning/dashscope-thinking-v1") {
    return enabled ? { enabled: true, budget_tokens: 128 } : { enabled: false };
  }
  if (adapterId === "reasoning/deepseek-thinking-effort-v1") {
    return enabled ? { enabled: true, effort: "high" } : { enabled: false };
  }
  if (adapterId === "reasoning/anthropic-adaptive-v1") {
    return enabled
      ? { enabled: true, effort: option.canonical_effort === "max" ? "max" : "low" }
      : { enabled: false };
  }
  if (adapterId === "reasoning/anthropic-budget-v1") {
    return enabled
      ? { enabled: true, budget_tokens: Number(option.settings.budget_tokens ?? 1024) }
      : { enabled: false };
  }
  if (adapterId === "reasoning/openai-responses-effort-v1") {
    return { effort: option.canonical_effort ?? "low" };
  }
  if (adapterId === "reasoning/gemini-thinking-v1") {
    return { level: "low", include_thoughts: true };
  }
  if (adapterId === "reasoning/xai-effort-v1") {
    return { effort: option.canonical_effort === "none" ? "none" : "low" };
  }
  if (adapterId === "reasoning/openai-compatible-binary-v1") {
    return { enabled };
  }
  return {};
}

export function ModelCapabilityEditor({
  profile,
  adapters,
  onChange,
  onReset,
}: ModelCapabilityEditorProps) {
  const { t } = useTranslation();
  const update = (mutate: (draft: ModelCapabilityProfile) => void) => {
    const draft = JSON.parse(JSON.stringify(profile)) as ModelCapabilityProfile;
    mutate(draft);
    onChange(draft);
  };
  const reasoningAdapters = adapters.filter((item) => item.kind === "reasoning");
  const cacheAdapters = adapters.filter((item) => item.kind === "prompt_cache");
  const searchAdapters = adapters.filter((item) => item.kind === "native_search");

  const updateOption = (index: number, patch: Partial<ModelReasoningOption>) =>
    update((draft) => {
      draft.reasoning.options[index] = {
        ...draft.reasoning.options[index],
        ...patch,
      };
    });

  return (
    <section className="grid gap-4 rounded-lg border border-border/60 p-4">
      <div className="flex items-center justify-between gap-3">
        <div>
          <h3 className="text-sm font-semibold">{t("services.modelCapabilities.title")}</h3>
          <p className="text-xs text-muted-foreground">
            {t("services.modelCapabilities.description")}
          </p>
        </div>
        {onReset && (
          <Button type="button" variant="outline" size="sm" onClick={onReset}>
            <RotateCcw className="mr-1.5 h-3.5 w-3.5" />
            {t("services.modelCapabilities.providerDefaults")}
          </Button>
        )}
      </div>

      <div className="grid gap-3 sm:grid-cols-2">
        <div className="grid gap-2">
          <Label>{t("services.modelCapabilities.reasoningAdapter")}</Label>
          <Select
            value={profile.reasoning.adapter_id}
            onValueChange={(adapterId) =>
              update((draft) => {
                draft.reasoning.adapter_id = adapterId;
                draft.reasoning.options = draft.reasoning.options.map((option) => ({
                  ...option,
                  settings: settingsForAdapter(adapterId, option),
                }));
              })
            }
          >
            <SelectTrigger><SelectValue /></SelectTrigger>
            <SelectContent>
              {reasoningAdapters.map((adapter) => (
                <SelectItem key={adapter.id} value={adapter.id}>{adapter.label}</SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        <div className="grid gap-2">
          <Label>{t("services.modelCapabilities.defaultOption")}</Label>
          <Select
            value={profile.reasoning.default_option}
            onValueChange={(value) => update((draft) => { draft.reasoning.default_option = value; })}
          >
            <SelectTrigger><SelectValue /></SelectTrigger>
            <SelectContent>
              {profile.reasoning.options.map((option) => (
                <SelectItem key={option.id} value={option.id}>{option.label}</SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
      </div>

      <div className="grid gap-3">
        {profile.reasoning.options.map((option, index) => {
          const idProblem = optionIdProblem(option, profile.reasoning.options, index);
          return (
          <div key={`${option.id}-${index}`} className="grid gap-2 rounded-md bg-muted/25 p-3">
            <div className="grid gap-2 sm:grid-cols-3">
              <Input
                aria-label={t("services.modelCapabilities.optionId")}
                aria-invalid={idProblem}
                className={idProblem ? "border-destructive" : undefined}
                value={option.id}
                onChange={(event) => updateOption(index, { id: event.target.value.toLowerCase() })}
                placeholder="low"
              />
              <Input
                aria-label={t("services.modelCapabilities.optionLabel")}
                value={option.label}
                onChange={(event) => updateOption(index, { label: event.target.value })}
                placeholder="Low"
              />
              <Select
                value={option.canonical_effort ?? "low"}
                onValueChange={(value) => {
                  const canonical = value as CanonicalReasoningEffort;
                  updateOption(index, {
                    canonical_effort: canonical,
                    settings: settingsForAdapter(profile.reasoning.adapter_id, {
                      ...option,
                      canonical_effort: canonical,
                    }),
                  });
                }}
              >
                <SelectTrigger><SelectValue /></SelectTrigger>
                <SelectContent>
                  {EFFORTS.map((effort) => <SelectItem key={effort} value={effort}>{effort}</SelectItem>)}
                </SelectContent>
              </Select>
            </div>
            <div className="grid gap-2 sm:grid-cols-3">
              {"enabled" in option.settings && (
                <label className="flex items-center gap-2 text-xs">
                  <Switch
                    checked={Boolean(option.settings.enabled)}
                    onCheckedChange={(enabled) => updateOption(index, { settings: { ...option.settings, enabled } })}
                  />
                  {t("services.modelCapabilities.thinkingEnabled")}
                </label>
              )}
              {"effort" in option.settings && (
                <Input
                  aria-label={t("services.modelCapabilities.providerEffort")}
                  value={String(option.settings.effort ?? "")}
                  onChange={(event) => updateOption(index, { settings: { ...option.settings, effort: event.target.value } })}
                  placeholder={t("services.modelCapabilities.providerEffort")}
                />
              )}
              {"budget_tokens" in option.settings && (
                <Input
                  aria-label={t("services.modelCapabilities.thinkingBudget")}
                  type="number"
                  min={1}
                  value={Number(option.settings.budget_tokens ?? 1)}
                  onChange={(event) => updateOption(index, { settings: { ...option.settings, budget_tokens: Number(event.target.value) } })}
                />
              )}
              <Button
                type="button"
                variant="ghost"
                size="sm"
                disabled={profile.reasoning.options.length === 1}
                onClick={() => update((draft) => {
                  draft.reasoning.options.splice(index, 1);
                  if (!draft.reasoning.options.some((item) => item.id === draft.reasoning.default_option)) {
                    draft.reasoning.default_option = draft.reasoning.options[0].id;
                  }
                })}
              >
                <Trash2 className="mr-1.5 h-3.5 w-3.5" /> {t("common.remove")}
              </Button>
            </div>
          </div>
          );
        })}
        <Button
          type="button"
          variant="outline"
          size="sm"
          disabled={profile.reasoning.options.length >= 16}
          onClick={() => update((draft) => {
            // Collision-free id: scan existing ids AND aliases (the server
            // rejects an id that shadows either) instead of deriving from the
            // current length, which reused ids after a removal.
            const taken = new Set<string>();
            for (const item of draft.reasoning.options) {
              taken.add(item.id);
              for (const alias of item.aliases ?? []) taken.add(alias);
            }
            let suffix = draft.reasoning.options.length + 1;
            while (taken.has(`option_${suffix}`)) suffix += 1;
            const id = `option_${suffix}`;
            const option: ModelReasoningOption = {
              id,
              label: `Option ${suffix}`,
              aliases: [],
              canonical_effort: "low",
              settings: {},
            };
            option.settings = settingsForAdapter(draft.reasoning.adapter_id, option);
            draft.reasoning.options.push(option);
          })}
        >
          <Plus className="mr-1.5 h-3.5 w-3.5" /> {t("services.modelCapabilities.addOption")}
        </Button>
      </div>

      <div className="grid gap-3 sm:grid-cols-2">
        <div className="grid gap-2">
          <Label>{t("services.modelCapabilities.promptCacheAdapter")}</Label>
          <Select value={profile.prompt_cache.adapter_id} onValueChange={(value) => update((draft) => { draft.prompt_cache = { adapter_id: value, config: {} }; })}>
            <SelectTrigger><SelectValue /></SelectTrigger>
            <SelectContent>{cacheAdapters.map((adapter) => <SelectItem key={adapter.id} value={adapter.id}>{adapter.label}</SelectItem>)}</SelectContent>
          </Select>
        </div>
        <div className="grid gap-2">
          <Label>{t("services.modelCapabilities.nativeSearchAdapter")}</Label>
          <Select value={profile.native_search.adapter_id} onValueChange={(value) => update((draft) => { draft.native_search = { adapter_id: value, enabled: value !== "search/none-v1", config: value === "search/anthropic-server-tool-v1" ? { max_uses: 5 } : {} }; })}>
            <SelectTrigger><SelectValue /></SelectTrigger>
            <SelectContent>{searchAdapters.map((adapter) => <SelectItem key={adapter.id} value={adapter.id}>{adapter.label}</SelectItem>)}</SelectContent>
          </Select>
        </div>
      </div>
    </section>
  );
}
