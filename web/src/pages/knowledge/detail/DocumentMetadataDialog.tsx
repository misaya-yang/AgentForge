import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import type {
  Document,
  DocumentMetadataField,
  DocumentMetadataRegistry,
} from "@/types/knowledge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";

interface DocumentMetadataDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  documents: Document[];
  registry: DocumentMetadataRegistry;
  saving: boolean;
  onSave: (
    metadataPatch: Record<string, unknown>,
    metadataRemove: string[]
  ) => Promise<void>;
}

function initialFieldValue(document: Document | undefined, field: DocumentMetadataField): string {
  const value = document?.metadata?.[field.name];
  if (value === null || value === undefined) return "";
  return String(value);
}

export function DocumentMetadataDialog({
  open,
  onOpenChange,
  documents,
  registry,
  saving,
  onSave,
}: DocumentMetadataDialogProps) {
  const { t } = useTranslation();
  const [values, setValues] = useState<Record<string, string>>({});
  const [touched, setTouched] = useState<Set<string>>(new Set());
  const [removed, setRemoved] = useState<Set<string>>(new Set());
  const singleDocument = documents.length === 1 ? documents[0] : undefined;

  useEffect(() => {
    if (!open) return;
    setValues(
      Object.fromEntries(
        registry.fields.map((field) => [field.name, initialFieldValue(singleDocument, field)])
      )
    );
    setTouched(new Set(singleDocument ? registry.fields.map((field) => field.name) : []));
    setRemoved(new Set());
  }, [open, registry.fields, singleDocument]);

  async function save() {
    const patch: Record<string, unknown> = {};
    for (const field of registry.fields) {
      if (!touched.has(field.name) || removed.has(field.name)) continue;
      const value = values[field.name] ?? "";
      patch[field.name] = field.type === "number" ? Number(value) : value;
    }
    await onSave(patch, Array.from(removed));
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-xl">
        <DialogHeader>
          <DialogTitle>
            {documents.length === 1
              ? t("knowledge.metadata.editDocument", { title: documents[0]?.title })
              : t("knowledge.metadata.editBatch", { count: documents.length })}
          </DialogTitle>
        </DialogHeader>
        <div className="max-h-[60vh] space-y-4 overflow-auto py-1">
          {registry.fields.map((field) => (
            <div key={field.name} className="space-y-1.5">
              <div className="flex items-center justify-between gap-3">
                <Label htmlFor={`metadata-${field.name}`}>{field.label}</Label>
                <label className="flex items-center gap-2 text-xs text-muted-foreground">
                  <Checkbox
                    checked={removed.has(field.name)}
                    onCheckedChange={(checked) => {
                      setRemoved((previous) => {
                        const next = new Set(previous);
                        if (checked === true) next.add(field.name);
                        else next.delete(field.name);
                        return next;
                      });
                      setTouched((previous) => new Set(previous).add(field.name));
                    }}
                  />
                  {t("knowledge.metadata.clearValue")}
                </label>
              </div>
              <Input
                id={`metadata-${field.name}`}
                data-testid={`metadata-value-${field.name}`}
                value={values[field.name] ?? ""}
                disabled={removed.has(field.name)}
                type={field.type === "number" ? "number" : "text"}
                placeholder={
                  field.type === "datetime"
                    ? "2026-08-29T12:00:00Z"
                    : field.description
                }
                onChange={(event) => {
                  setValues((previous) => ({
                    ...previous,
                    [field.name]: event.target.value,
                  }));
                  setTouched((previous) => new Set(previous).add(field.name));
                }}
              />
            </div>
          ))}
          {registry.fields.length === 0 && (
            <p className="py-8 text-center text-sm text-muted-foreground">
              {t("knowledge.metadata.noFields")}
            </p>
          )}
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            {t("knowledge.detail.cancel")}
          </Button>
          <Button
            data-testid="metadata-save"
            disabled={saving || registry.fields.length === 0 || touched.size === 0}
            onClick={() => void save()}
          >
            {saving ? t("knowledge.detail.saving") : t("knowledge.detail.save")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
