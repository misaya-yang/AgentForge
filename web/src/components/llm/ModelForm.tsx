/**
 * Model Form Component
 *
 * Modal form for creating/editing LLM models.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useForm } from "react-hook-form";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
import { ChevronDown } from "lucide-react";
import type {
  LLMModel,
  ModelCapabilityAdapter,
  ModelCapabilityProfile,
  ModelCreate,
  ModelUpdate,
  ModelAccessLevel,
} from "@/api/models";
import type { Provider, ProviderTemplate } from "@/api/providers";
import { ModelCapabilityEditor } from "./ModelCapabilityEditor";
import {
  cloneModelCapabilityProfile,
  createSafeModelCapabilityProfile,
  isUsableModelCapabilityProfile,
  modelCapabilityProfilesEqual,
  validateCapabilityProfileOptionIds,
} from "@/features/models/modelCapabilities";

interface ModelFormProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  model?: LLMModel | null;
  providers: Provider[];
  providerTemplates?: ProviderTemplate[];
  providersLoading?: boolean;
  providerTemplatesLoading?: boolean;
  capabilityAdapters?: ModelCapabilityAdapter[];
  onSubmit: (data: ModelCreate | ModelUpdate) => Promise<void>;
  /**
   * Edit mode only: clear tenant overrides server-side (back to provider
   * defaults). When absent, "Provider defaults" only resets the local draft.
   */
  onResetCapabilities?: () => Promise<void>;
  loading?: boolean;
}

interface FormData {
  model_id: string;
  provider_id: string;
  catalog_model_id: string;
  display_name: string;
  context_window: number;
  max_output_tokens: number;
  supports_vision: boolean;
  supports_tools: boolean;
  input_price_per_1k: number;
  output_price_per_1k: number;
  access_level: ModelAccessLevel;
  is_enabled: boolean;
  sort_order: number;
}

interface CatalogProviderOption {
  value: string;
  label: string;
  description?: string;
  provider?: Provider;
  template?: ProviderTemplate;
  registered: boolean;
  hasApiKey: boolean;
}

const CUSTOM_MODEL_VALUE = "__custom_model__";

function providerHasRuntimeCredentials(provider: Provider): boolean {
  return Boolean(provider.has_api_key || provider.allow_environment_credentials);
}

function normalizeUrl(value?: string | null) {
  return (value || "").trim().replace(/\/+$/, "").toLowerCase();
}

function matchProviderTemplate(
  provider: Provider,
  templates: ProviderTemplate[]
): ProviderTemplate | undefined {
  const providerId = provider.provider_id.toLowerCase();
  const baseUrl = normalizeUrl(provider.base_url);

  return (
    templates.find((template) => template.default_provider_id === providerId) ||
    templates.find(
      (template) =>
        normalizeUrl(template.default_base_url) !== "" &&
        normalizeUrl(template.default_base_url) === baseUrl
    )
  );
}

function applyCatalogModel(
  catalogModel: ProviderTemplate["default_models"][number],
  setValue: ReturnType<typeof useForm<FormData>>["setValue"]
) {
  setValue("model_id", catalogModel.model_id);
  setValue("catalog_model_id", catalogModel.model_id);
  setValue("display_name", catalogModel.display_name);
  setValue("context_window", catalogModel.context_window);
  setValue("max_output_tokens", catalogModel.max_output_tokens);
  setValue("supports_vision", catalogModel.supports_vision);
  setValue("supports_tools", catalogModel.supports_tools);
  setValue("input_price_per_1k", catalogModel.input_price_per_1k);
  setValue("output_price_per_1k", catalogModel.output_price_per_1k);
  setValue("access_level", catalogModel.access_level as ModelAccessLevel);
  setValue("sort_order", catalogModel.sort_order);
}

function createCatalogModelDefaults(
  catalogModel?: ProviderTemplate["default_models"][number]
) {
  return {
    model_id: catalogModel?.model_id || "",
    catalog_model_id: catalogModel?.model_id || CUSTOM_MODEL_VALUE,
    display_name: catalogModel?.display_name || "",
    context_window: catalogModel?.context_window ?? 128000,
    max_output_tokens: catalogModel?.max_output_tokens ?? 4096,
    supports_vision: catalogModel?.supports_vision ?? false,
    supports_tools: catalogModel?.supports_tools ?? true,
    input_price_per_1k: catalogModel?.input_price_per_1k ?? 0,
    output_price_per_1k: catalogModel?.output_price_per_1k ?? 0,
    access_level: (catalogModel?.access_level as ModelAccessLevel | undefined) ?? "public",
    is_enabled: true,
    sort_order: catalogModel?.sort_order ?? 0,
  };
}

export function ModelForm({
  open,
  onOpenChange,
  model,
  providers,
  providerTemplates = [],
  providersLoading = false,
  providerTemplatesLoading = false,
  capabilityAdapters = [],
  onSubmit,
  onResetCapabilities,
  loading,
}: ModelFormProps) {
  const { t } = useTranslation();
  const isEdit = !!model;
  const [advancedCatalogOverride, setAdvancedCatalogOverride] = useState(false);
  const [showAdvancedSettings, setShowAdvancedSettings] = useState(false);
  const [capabilityProfile, setCapabilityProfile] = useState<ModelCapabilityProfile>(
    () => createSafeModelCapabilityProfile()
  );
  // The profile the form was seeded with. Compared against the live profile
  // at submit time so an untouched form does NOT persist a maximal override
  // layer that would shadow every future catalog update.
  const baselineCapabilityProfileRef = useRef<ModelCapabilityProfile>(
    createSafeModelCapabilityProfile()
  );
  const [capabilityError, setCapabilityError] = useState<string | null>(null);
  const seedCapabilityProfile = useCallback((profile: ModelCapabilityProfile) => {
    setCapabilityProfile(profile);
    baselineCapabilityProfileRef.current = cloneModelCapabilityProfile(profile);
    setCapabilityError(null);
  }, []);
  const initializedFormKeyRef = useRef<string | null>(null);
  // Create mode: whether the typed model_id currently matches a catalog
  // entry. When it transitions matched → unmatched the profile re-seeds to
  // the safe default once (mirrors what the server computes for a custom
  // model without overrides); it must NOT re-seed on every keystroke.
  const catalogMatchedModelIdRef = useRef<string | null>(null);

  const ACCESS_LEVELS: { value: ModelAccessLevel; label: string }[] = [
    { value: "public", label: t("llm.model.accessLevels.public") },
    { value: "premium", label: t("llm.model.accessLevels.premium") },
    { value: "admin", label: t("llm.model.accessLevels.admin") },
  ];

  const {
    register,
    handleSubmit,
    reset,
    watch,
    setValue,
    formState: { errors },
  } = useForm<FormData>({
    defaultValues: {
      model_id: "",
      provider_id: "",
      catalog_model_id: CUSTOM_MODEL_VALUE,
      display_name: "",
      context_window: 128000,
      max_output_tokens: 4096,
      supports_vision: false,
      supports_tools: true,
      input_price_per_1k: 0,
      output_price_per_1k: 0,
      access_level: "public",
      is_enabled: true,
      sort_order: 0,
    },
    mode: "onBlur", // Validate on blur for better UX
  });

  const providerId = watch("provider_id");
  const modelId = watch("model_id");
  const catalogModelId = watch("catalog_model_id");
  const accessLevel = watch("access_level");
  const providerOptions = useMemo<CatalogProviderOption[]>(() => {
    const registeredOptions = providers.map((provider) => {
      const template = matchProviderTemplate(provider, providerTemplates);
      return {
        value: provider.provider_id,
        label: provider.display_name,
        description: provider.base_url || undefined,
        provider,
        template,
        registered: true,
        hasApiKey: providerHasRuntimeCredentials(provider),
      };
    });
    const coveredTemplateIds = new Set(
      registeredOptions
        .map((option) => option.template?.template_id)
        .filter(Boolean) as string[]
    );
    const registeredProviderIds = new Set(
      registeredOptions.map((option) => option.value)
    );
    const templateOptions = providerTemplates
      .filter((template) => {
        if (!template.default_provider_id) return false;
        if (template.advanced) return false;
        if (coveredTemplateIds.has(template.template_id)) return false;
        if (registeredProviderIds.has(template.default_provider_id)) return false;
        return template.default_models.length > 0;
      })
      .map((template) => ({
        value: template.default_provider_id,
        label: template.display_name,
        description: template.default_base_url,
        template,
        registered: false,
        hasApiKey: false,
      }));

    return [...registeredOptions, ...templateOptions].sort((a, b) => {
      const aCatalog = a.template?.default_models.length ? 1 : 0;
      const bCatalog = b.template?.default_models.length ? 1 : 0;
      const aConfigured = a.registered && a.hasApiKey ? 1 : 0;
      const bConfigured = b.registered && b.hasApiKey ? 1 : 0;
      if (aConfigured !== bConfigured) return bConfigured - aConfigured;
      if (aCatalog !== bCatalog) return bCatalog - aCatalog;
      if (a.registered !== b.registered) return a.registered ? -1 : 1;
      return a.label.localeCompare(b.label);
    });
  }, [providers, providerTemplates]);
  const selectableProviderOptions = useMemo(() => {
    const runtimeReadyOptions = providerOptions.filter(
      (option) =>
        option.registered &&
        option.hasApiKey &&
        option.provider?.is_enabled !== false
    );
    if (runtimeReadyOptions.length > 0) {
      return runtimeReadyOptions;
    }

    const registeredOptions = providerOptions.filter((option) => option.registered);
    return registeredOptions.length > 0 ? registeredOptions : providerOptions;
  }, [providerOptions]);
  const selectedProviderOption = useMemo(
    () => providerOptions.find((option) => option.value === providerId),
    [providerId, providerOptions]
  );
  const catalogModels = useMemo(
    () => selectedProviderOption?.template?.default_models ?? [],
    [selectedProviderOption]
  );
  const hasCatalogModels = !isEdit && catalogModels.length > 0;
  const knownModelProviders = useMemo(() => {
    const normalized = (modelId || "").trim().toLowerCase();
    if (!normalized) return [];
    return providerTemplates
      .filter((template) =>
        template.default_models.some((catalogModel) => {
          return catalogModel.model_id.toLowerCase() === normalized;
        })
      )
      .map((template) => template.default_provider_id);
  }, [modelId, providerTemplates]);
  const selectedProviderCatalogMatchesModel = useMemo(() => {
    const normalized = (modelId || "").trim().toLowerCase();
    if (!normalized) return false;

    return Boolean(
      selectedProviderOption?.template?.default_models.some(
        (catalogModel) => catalogModel.model_id.toLowerCase() === normalized
      )
    );
  }, [modelId, selectedProviderOption]);
  const catalogMismatch =
    !isEdit &&
    !advancedCatalogOverride &&
    knownModelProviders.length > 0 &&
    !knownModelProviders.includes(providerId) &&
    !selectedProviderCatalogMatchesModel;

  useEffect(() => {
    if (!open) {
      initializedFormKeyRef.current = null;
      catalogMatchedModelIdRef.current = null;
      return;
    }

    const formKey = model
      ? `edit:${model.model_id}:${model.updated_at}`
      : "create";
    if (initializedFormKeyRef.current === formKey) return;
    if (!model && (providersLoading || providerTemplatesLoading)) return;

    initializedFormKeyRef.current = formKey;

    if (model) {
      setAdvancedCatalogOverride(false);
      setShowAdvancedSettings(true);
      reset({
        model_id: model.model_id,
        provider_id: model.provider_id,
        catalog_model_id: CUSTOM_MODEL_VALUE,
        display_name: model.display_name,
        context_window: model.context_window,
        max_output_tokens: model.max_output_tokens,
        supports_vision: model.supports_vision,
        supports_tools: model.supports_tools,
        input_price_per_1k: model.input_price_per_1k,
        output_price_per_1k: model.output_price_per_1k,
        access_level: model.access_level,
        is_enabled: model.is_enabled,
        sort_order: model.sort_order,
      });
      seedCapabilityProfile(
        isUsableModelCapabilityProfile(model.effective_capabilities)
          ? cloneModelCapabilityProfile(model.effective_capabilities)
          : createSafeModelCapabilityProfile(model.supports_tools, model.supports_vision)
      );
    } else {
      catalogMatchedModelIdRef.current = null;
      const firstProviderOption = selectableProviderOptions[0];
      const firstCatalogModel = firstProviderOption?.template?.default_models[0];
      setAdvancedCatalogOverride(false);
      setShowAdvancedSettings(!firstCatalogModel);
      reset({
        ...createCatalogModelDefaults(firstCatalogModel),
        provider_id: firstProviderOption?.value || "",
      });
      seedCapabilityProfile(
        firstCatalogModel && isUsableModelCapabilityProfile(firstCatalogModel.catalog_capabilities)
          ? cloneModelCapabilityProfile(firstCatalogModel.catalog_capabilities)
          : createSafeModelCapabilityProfile(
              firstCatalogModel?.supports_tools ?? true,
              firstCatalogModel?.supports_vision ?? false
            )
      );
    }
  }, [
    model,
    open,
    providerTemplatesLoading,
    providersLoading,
    reset,
    seedCapabilityProfile,
    selectableProviderOptions,
  ]);

  useEffect(() => {
    if (isEdit || !modelId || !providerId) return;

    const template = selectedProviderOption?.template;
    const catalogModel = template?.default_models.find(
      (item) => item.model_id.toLowerCase() === modelId.trim().toLowerCase()
    );
    if (!catalogModel) {
      if (catalogMatchedModelIdRef.current !== null) {
        catalogMatchedModelIdRef.current = null;
        seedCapabilityProfile(
          createSafeModelCapabilityProfile(watch("supports_tools"), watch("supports_vision"))
        );
      }
      return;
    }

    catalogMatchedModelIdRef.current = catalogModel.model_id;
    applyCatalogModel(catalogModel, setValue);
    seedCapabilityProfile(
      isUsableModelCapabilityProfile(catalogModel.catalog_capabilities)
        ? cloneModelCapabilityProfile(catalogModel.catalog_capabilities)
        : createSafeModelCapabilityProfile(
            catalogModel.supports_tools,
            catalogModel.supports_vision
          )
    );
  }, [isEdit, modelId, providerId, selectedProviderOption, setValue, seedCapabilityProfile, watch]);

  useEffect(() => {
    if (isEdit || !providerId || catalogModelId === CUSTOM_MODEL_VALUE) return;
    const catalogModel = catalogModels.find(
      (item) => item.model_id === catalogModelId
    );
    if (!catalogModel) return;

    applyCatalogModel(catalogModel, setValue);
  }, [catalogModelId, catalogModels, isEdit, providerId, setValue]);

  const onFormSubmit = async (data: FormData) => {
    const optionIdIssue = validateCapabilityProfileOptionIds(capabilityProfile);
    if (optionIdIssue) {
      setCapabilityError(
        t(
          optionIdIssue === "duplicate"
            ? "llm.model.validation.capabilityOptionIdDuplicate"
            : "llm.model.validation.capabilityOptionIdInvalid"
        )
      );
      return;
    }
    setCapabilityError(null);

    // An untouched profile must NOT be persisted: sending the whole seeded
    // profile as ``capability_overrides`` would create a maximal override
    // layer that shadows every future provider-catalog update for this model.
    const profileUnchanged = modelCapabilityProfilesEqual(
      capabilityProfile,
      baselineCapabilityProfileRef.current
    );
    const capabilityOverrides = profileUnchanged ? undefined : capabilityProfile;

    const submitData: ModelCreate | ModelUpdate = isEdit
      ? {
          // ``model_id`` may be a rename in edit mode; the server treats
          // unchanged values as a no-op and new values as a PK UPDATE.
          model_id: data.model_id,
          display_name: data.display_name,
          context_window: data.context_window,
          max_output_tokens: data.max_output_tokens,
          supports_vision: data.supports_vision,
          supports_tools: data.supports_tools,
          input_price_per_1k: data.input_price_per_1k,
          output_price_per_1k: data.output_price_per_1k,
          access_level: data.access_level,
          is_enabled: data.is_enabled,
          sort_order: data.sort_order,
          capability_overrides: capabilityOverrides,
          expected_capability_revision: model?.capability_revision,
        }
      : {
          model_id: data.model_id,
          provider_id: data.provider_id,
          display_name: data.display_name,
          context_window: data.context_window,
          max_output_tokens: data.max_output_tokens,
          supports_vision: data.supports_vision,
          supports_tools: data.supports_tools,
          input_price_per_1k: data.input_price_per_1k,
          output_price_per_1k: data.output_price_per_1k,
          access_level: data.access_level,
          is_enabled: data.is_enabled,
          sort_order: data.sort_order,
          capability_overrides: capabilityOverrides,
        };

    await onSubmit(submitData);
  };

  const handleProviderChange = (value: string) => {
    setValue("provider_id", value);
    const option = selectableProviderOptions.find((item) => item.value === value);
    const firstCatalogModel = option?.template?.default_models[0];
    if (firstCatalogModel) {
      applyCatalogModel(firstCatalogModel, setValue);
      seedCapabilityProfile(
        isUsableModelCapabilityProfile(firstCatalogModel.catalog_capabilities)
          ? cloneModelCapabilityProfile(firstCatalogModel.catalog_capabilities)
          : createSafeModelCapabilityProfile(
              firstCatalogModel.supports_tools,
              firstCatalogModel.supports_vision
            )
      );
      setShowAdvancedSettings(false);
    } else {
      setValue("catalog_model_id", CUSTOM_MODEL_VALUE);
      seedCapabilityProfile(
        createSafeModelCapabilityProfile(watch("supports_tools"), watch("supports_vision"))
      );
      setShowAdvancedSettings(true);
    }
  };

  const handleCatalogModelChange = (value: string) => {
    setValue("catalog_model_id", value);
    if (value === CUSTOM_MODEL_VALUE) {
      seedCapabilityProfile(
        createSafeModelCapabilityProfile(watch("supports_tools"), watch("supports_vision"))
      );
      setShowAdvancedSettings(true);
      return;
    }

    const catalogModel = catalogModels.find((item) => item.model_id === value);
    if (catalogModel) {
      applyCatalogModel(catalogModel, setValue);
      seedCapabilityProfile(
        isUsableModelCapabilityProfile(catalogModel.catalog_capabilities)
          ? cloneModelCapabilityProfile(catalogModel.catalog_capabilities)
          : createSafeModelCapabilityProfile(
              catalogModel.supports_tools,
              catalogModel.supports_vision
            )
      );
      setShowAdvancedSettings(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-[760px] max-h-[90vh] overflow-y-auto">
        <form onSubmit={handleSubmit(onFormSubmit)}>
          <DialogHeader>
            <DialogTitle>{isEdit ? t("llm.model.editTitle") : t("llm.model.addTitle")}</DialogTitle>
            <DialogDescription>
              {isEdit ? t("llm.model.editDescription") : t("llm.model.addDescription")}
            </DialogDescription>
          </DialogHeader>

          <div className="grid gap-5 py-4">
            <section className="grid gap-4">
              {!isEdit && (
                <div className="grid gap-2">
                  <Label htmlFor="provider_id">{t("llm.model.provider")}</Label>
                  <Select
                    value={providerId}
                    onValueChange={handleProviderChange}
                  >
                    <SelectTrigger>
                      <SelectValue placeholder={t("llm.model.providerPlaceholder")} />
                    </SelectTrigger>
                    <SelectContent>
                      {selectableProviderOptions.map((option) => (
                        <SelectItem key={option.value} value={option.value}>
                          {option.label}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                  {selectedProviderOption && !selectedProviderOption.registered && (
                    <p className="text-xs text-amber-600 dark:text-amber-300">
                      {t(
                        "llm.model.templateProviderHint",
                        "Template entry. Save the provider credentials before using it in runtime."
                      )}
                    </p>
                  )}
                </div>
              )}

              {hasCatalogModels && (
                <div className="grid gap-2">
                  <Label htmlFor="catalog_model_id">
                    {t("llm.model.catalogModel", "Model")}
                  </Label>
                  <Select
                    value={catalogModelId}
                    onValueChange={handleCatalogModelChange}
                  >
                    <SelectTrigger>
                      <SelectValue
                        placeholder={t(
                          "llm.model.catalogModelPlaceholder",
                          "Select model"
                        )}
                      />
                    </SelectTrigger>
                    <SelectContent>
                      {catalogModels.map((catalogModel) => (
                        <SelectItem
                          key={catalogModel.model_id}
                          value={catalogModel.model_id}
                        >
                          {catalogModel.display_name}
                        </SelectItem>
                      ))}
                      <SelectItem value={CUSTOM_MODEL_VALUE}>
                        {t("llm.model.customModel", "Custom model ID")}
                      </SelectItem>
                    </SelectContent>
                  </Select>
                </div>
              )}
            </section>

            <Collapsible
              open={showAdvancedSettings}
              onOpenChange={setShowAdvancedSettings}
              className="rounded-lg border border-border/70"
            >
              <CollapsibleTrigger className="flex w-full items-center justify-between px-4 py-3 text-left text-sm font-medium text-foreground">
                <span>{t("llm.model.advancedSettings", "Advanced settings")}</span>
                <ChevronDown
                  className={`h-4 w-4 text-muted-foreground transition-transform ${
                    showAdvancedSettings ? "rotate-180" : ""
                  }`}
                />
              </CollapsibleTrigger>
              <CollapsibleContent
                aria-hidden={!showAdvancedSettings}
                className={showAdvancedSettings ? undefined : "hidden"}
              >
                <div className="grid gap-6 border-t border-border/70 p-4">
                  <section className="grid gap-4">
                    <h3 className="text-sm font-semibold text-foreground">
                      {t("llm.model.sections.identity", "Identity")}
                    </h3>

                    {/* Model ID is the provider API identifier. It stays in
                        advanced settings for catalog-backed models, but is
                        still editable for custom models and rename flows. */}
                    <div className="grid gap-2">
                      <Label htmlFor="model_id">{t("llm.model.modelId")}</Label>
                      <Input
                        id="model_id"
                        placeholder={t("llm.model.modelIdPlaceholder")}
                        {...register("model_id", {
                          required: t("llm.model.modelIdRequired"),
                          onChange: () => {
                            if (!isEdit) {
                              setValue("catalog_model_id", CUSTOM_MODEL_VALUE);
                              setShowAdvancedSettings(true);
                            }
                          },
                        })}
                      />
                      {errors.model_id && (
                        <p className="text-sm text-destructive">{errors.model_id.message}</p>
                      )}
                      <p className="text-xs text-muted-foreground">
                        {isEdit
                          ? t(
                              "llm.model.modelIdEditHint",
                              "This is the identifier sent to the provider's API. Renaming it updates the primary key; prior usage records keep their original id for audit.",
                            )
                          : t("llm.model.modelIdHint")}
                      </p>
                    </div>

                    {catalogMismatch && (
                      <div className="rounded-lg border border-amber-200 bg-amber-50 p-3 text-sm text-amber-800 dark:border-amber-900 dark:bg-amber-950/30 dark:text-amber-200">
                        {t(
                          "llm.model.catalogMismatch",
                          "This model belongs to a known provider catalog. Switch to the matching provider or enable advanced override."
                        )}{" "}
                        <button
                          type="button"
                          className="font-semibold underline"
                          onClick={() => setAdvancedCatalogOverride(true)}
                        >
                          {t("llm.model.enableAdvancedOverride", "Enable advanced override")}
                        </button>
                      </div>
                    )}

                    <div className="grid gap-2">
                      <Label htmlFor="display_name">{t("llm.model.displayName")}</Label>
                      <Input
                        id="display_name"
                        placeholder={t("llm.model.displayNamePlaceholder")}
                        {...register("display_name", { required: t("llm.model.displayNameRequired") })}
                      />
                      {errors.display_name && (
                        <p className="text-sm text-destructive">{errors.display_name.message}</p>
                      )}
                    </div>
                  </section>

                  {/* ``supports_native_search`` and ``native_search_config``
                      are intentionally not exposed here. They are derived
                      from the provider/model pair in model_registry.py. */}
                  <section className="grid gap-4">
                    <h3 className="text-sm font-semibold text-foreground">
                      {t("llm.model.sections.capabilities", "Capabilities")}
                    </h3>

                    <div className="grid grid-cols-2 gap-4">
                      <div className="grid gap-2">
                        <Label htmlFor="context_window">
                          {t("llm.model.contextWindow")} <span className="text-destructive">*</span>
                        </Label>
                        <Input
                          id="context_window"
                          type="number"
                          {...register("context_window", {
                            valueAsNumber: true,
                            required: t("llm.model.validation.contextWindowRequired"),
                            min: { value: 1, message: t("llm.model.validation.contextWindowMin") },
                            max: { value: 10000000, message: t("llm.model.validation.contextWindowMax") },
                          })}
                        />
                        {errors.context_window && (
                          <p className="text-sm text-destructive">{errors.context_window.message}</p>
                        )}
                        <p className="text-xs text-muted-foreground">{t("llm.model.tokenCount")}</p>
                      </div>
                      <div className="grid gap-2">
                        <Label htmlFor="max_output_tokens">{t("llm.model.maxOutputTokens")}</Label>
                        <Input
                          id="max_output_tokens"
                          type="number"
                          {...register("max_output_tokens", {
                            valueAsNumber: true,
                            min: { value: 1, message: t("llm.model.validation.maxOutputTokensMin") },
                          })}
                        />
                        {errors.max_output_tokens && (
                          <p className="text-sm text-destructive">{errors.max_output_tokens.message}</p>
                        )}
                        <p className="text-xs text-muted-foreground">{t("llm.model.tokenCount")}</p>
                      </div>
                    </div>

                    <div className="flex items-center justify-between">
                      <div>
                        <Label htmlFor="supports_vision">{t("llm.model.supportsVision")}</Label>
                        <p className="text-xs text-muted-foreground">
                          {t("llm.model.supportsVisionHint")}
                        </p>
                      </div>
                      <Switch
                        id="supports_vision"
                        checked={watch("supports_vision")}
                        onCheckedChange={(checked) => setValue("supports_vision", checked)}
                      />
                    </div>

                    <div className="flex items-center justify-between">
                      <div>
                        <Label htmlFor="supports_tools">{t("llm.model.supportsTools")}</Label>
                        <p className="text-xs text-muted-foreground">
                          {t("llm.model.supportsToolsHint")}
                        </p>
                      </div>
                      <Switch
                        id="supports_tools"
                        checked={watch("supports_tools")}
                        onCheckedChange={(checked) => setValue("supports_tools", checked)}
                      />
                    </div>
                  </section>

                  <ModelCapabilityEditor
                    profile={capabilityProfile}
                    adapters={capabilityAdapters}
                    onChange={setCapabilityProfile}
                    onReset={async () => {
                      if (isEdit && onResetCapabilities) {
                        await onResetCapabilities();
                        return;
                      }
                      seedCapabilityProfile(
                        model && isUsableModelCapabilityProfile(model.catalog_capabilities)
                          ? cloneModelCapabilityProfile(model.catalog_capabilities)
                          : createSafeModelCapabilityProfile(
                              watch("supports_tools"),
                              watch("supports_vision")
                            )
                      );
                    }}
                  />
                  {capabilityError && (
                    <p className="text-sm text-destructive">{capabilityError}</p>
                  )}

                  <section className="grid gap-4">
                    <h3 className="text-sm font-semibold text-foreground">
                      {t("llm.model.sections.pricing", "Pricing")}
                    </h3>
                    <div className="grid grid-cols-2 gap-4">
                      <div className="grid gap-2">
                        <Label htmlFor="input_price_per_1k">{t("llm.model.inputPrice")}</Label>
                        <Input
                          id="input_price_per_1k"
                          type="number"
                          step="0.000001"
                          {...register("input_price_per_1k", {
                            valueAsNumber: true,
                            min: { value: 0, message: t("llm.model.validation.priceMin") },
                          })}
                        />
                        {errors.input_price_per_1k && (
                          <p className="text-sm text-destructive">{errors.input_price_per_1k.message}</p>
                        )}
                        <p className="text-xs text-muted-foreground">{t("llm.model.priceUnit")}</p>
                      </div>
                      <div className="grid gap-2">
                        <Label htmlFor="output_price_per_1k">{t("llm.model.outputPrice")}</Label>
                        <Input
                          id="output_price_per_1k"
                          type="number"
                          step="0.000001"
                          {...register("output_price_per_1k", {
                            valueAsNumber: true,
                            min: { value: 0, message: t("llm.model.validation.priceMin") },
                          })}
                        />
                        {errors.output_price_per_1k && (
                          <p className="text-sm text-destructive">{errors.output_price_per_1k.message}</p>
                        )}
                        <p className="text-xs text-muted-foreground">{t("llm.model.priceUnit")}</p>
                      </div>
                    </div>
                  </section>

                  <section className="grid gap-4">
                    <h3 className="text-sm font-semibold text-foreground">
                      {t("llm.model.sections.accessDisplay", "Access & Display")}
                    </h3>

                    <div className="grid gap-2">
                      <Label htmlFor="access_level">{t("llm.model.accessLevel")}</Label>
                      <Select
                        value={accessLevel}
                        onValueChange={(value) => setValue("access_level", value as ModelAccessLevel)}
                      >
                        <SelectTrigger>
                          <SelectValue placeholder={t("llm.model.accessLevelPlaceholder")} />
                        </SelectTrigger>
                        <SelectContent>
                          {ACCESS_LEVELS.map((level) => (
                            <SelectItem key={level.value} value={level.value}>
                              {level.label}
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                    </div>

                    <div className="grid gap-2">
                      <Label htmlFor="sort_order">{t("llm.model.sortOrder")}</Label>
                      <Input
                        id="sort_order"
                        type="number"
                        {...register("sort_order", { valueAsNumber: true })}
                      />
                      <p className="text-xs text-muted-foreground">{t("llm.model.sortOrderHint")}</p>
                    </div>

                    <div className="flex items-center justify-between">
                      <div>
                        <Label htmlFor="is_enabled">{t("llm.model.enableModel")}</Label>
                        <p className="text-xs text-muted-foreground">
                          {t("llm.model.enableModelHint")}
                        </p>
                      </div>
                      <Switch
                        id="is_enabled"
                        checked={watch("is_enabled")}
                        onCheckedChange={(checked) => setValue("is_enabled", checked)}
                      />
                    </div>
                  </section>
                </div>
              </CollapsibleContent>
            </Collapsible>
          </div>

          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
              {t("common.cancel")}
            </Button>
            <Button type="submit" disabled={loading || catalogMismatch}>
              {loading ? t("common.loading") : t("common.save")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
