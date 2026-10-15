import { useCallback, useEffect, useState } from "react";
import { Plus, Trash2 } from "lucide-react";
import { useTranslation } from "react-i18next";

import {
  getDocumentMetadataRegistry,
  updateDocumentMetadataRegistry,
} from "@/api/knowledge";
import type {
  DocumentMetadataField,
  DocumentMetadataFieldType,
  DocumentMetadataRegistry,
} from "@/types/knowledge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { toast } from "@/hooks/use-toast";

interface MetadataRegistryCardProps {
  datasetId?: string;
  active: boolean;
  permission?: string;
}

const EMPTY_REGISTRY: DocumentMetadataRegistry = {
  version: 1,
  revision: 0,
  fields: [],
};

export function MetadataRegistryCard({
  datasetId,
  active,
  permission,
}: MetadataRegistryCardProps) {
  const { t } = useTranslation();
  const [registry, setRegistry] = useState<DocumentMetadataRegistry>(EMPTY_REGISTRY);
  const [fields, setFields] = useState<DocumentMetadataField[]>([]);
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const canEdit = permission === "owner";

  const load = useCallback(async () => {
    if (!datasetId) return;
    setLoading(true);
    try {
      const next = await getDocumentMetadataRegistry(datasetId);
      setRegistry(next);
      setFields(next.fields);
    } catch (error) {
      toast.error(
        t("knowledge.metadata.loadFailed"),
        error instanceof Error ? error.message : String(error)
      );
    } finally {
      setLoading(false);
    }
  }, [datasetId, t]);

  useEffect(() => {
    if (active) void load();
  }, [active, load]);

  function updateField(index: number, patch: Partial<DocumentMetadataField>) {
    setFields((previous) =>
      previous.map((field, fieldIndex) =>
        fieldIndex === index ? { ...field, ...patch } : field
      )
    );
  }

  async function save() {
    if (!datasetId) return;
    setSaving(true);
    try {
      const next = await updateDocumentMetadataRegistry(
        datasetId,
        registry.revision,
        fields
      );
      setRegistry(next);
      setFields(next.fields);
      toast.success(t("knowledge.metadata.saved"));
    } catch (error) {
      const status = (error as { response?: { status?: number } } | null)?.response?.status;
      if (status === 409) {
        await load();
        toast.warning(
          t("knowledge.metadata.schemaChanged"),
          t("knowledge.metadata.schemaChangedHint")
        );
      } else {
        toast.error(
          t("knowledge.metadata.saveFailed"),
          error instanceof Error ? error.message : String(error)
        );
      }
    } finally {
      setSaving(false);
    }
  }

  return (
    <Card data-testid="metadata-registry-card" className="space-y-4 p-5">
      <div className="flex items-start justify-between gap-4">
        <div>
          <h3 className="font-semibold">{t("knowledge.metadata.registryTitle")}</h3>
          <p className="mt-1 text-sm text-muted-foreground">
            {t("knowledge.metadata.registryHint")}
          </p>
        </div>
        <span className="text-xs text-muted-foreground">
          {t("knowledge.metadata.revision", { revision: registry.revision })}
        </span>
      </div>

      <div className="space-y-2">
        {fields.map((field, index) => (
          <div
            key={`${field.name}-${index}`}
            className="grid gap-2 rounded-lg border border-border p-3 md:grid-cols-[1fr_1fr_180px_auto]"
          >
            <Input
              data-testid={`metadata-field-name-${index}`}
              value={field.name}
              disabled={!canEdit || registry.fields.some((item) => item.name === field.name)}
              placeholder="lower_snake_case"
              maxLength={64}
              onChange={(event) => updateField(index, { name: event.target.value })}
            />
            <Input
              value={field.label}
              disabled={!canEdit}
              maxLength={128}
              placeholder={t("knowledge.metadata.label")}
              onChange={(event) => updateField(index, { label: event.target.value })}
            />
            <Select
              value={field.type}
              disabled={!canEdit || registry.fields.some((item) => item.name === field.name)}
              onValueChange={(value) =>
                updateField(index, { type: value as DocumentMetadataFieldType })
              }
            >
              <SelectTrigger><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value="string">String</SelectItem>
                <SelectItem value="number">Number</SelectItem>
                <SelectItem value="datetime">Datetime</SelectItem>
              </SelectContent>
            </Select>
            {canEdit && (
              <Button
                variant="ghost"
                size="icon"
                aria-label={t("common.delete")}
                onClick={() => setFields((previous) => previous.filter((_, i) => i !== index))}
              >
                <Trash2 className="h-4 w-4" />
              </Button>
            )}
          </div>
        ))}
        {!loading && fields.length === 0 && (
          <p className="py-4 text-center text-sm text-muted-foreground">
            {t("knowledge.metadata.noFields")}
          </p>
        )}
      </div>

      {canEdit && (
        <div className="flex items-center justify-between">
          <Button
            variant="outline"
            disabled={fields.length >= 32 || saving}
            onClick={() =>
              setFields((previous) => [
                ...previous,
                { name: "", label: "", type: "string" },
              ])
            }
          >
            <Plus className="mr-2 h-4 w-4" />
            {t("knowledge.metadata.addField")}
          </Button>
          <Button disabled={saving || loading} onClick={() => void save()}>
            {saving ? t("knowledge.detail.saving") : t("knowledge.detail.save")}
          </Button>
        </div>
      )}
    </Card>
  );
}
