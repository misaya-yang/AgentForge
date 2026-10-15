/**
 * Page-level dialogs for the dataset detail shell.
 *
 * These dialogs are triggered from more than one place (header dropdown,
 * Documents tab toolbar, Sources tab), so their open state stays in the page
 * shell while each dialog owns its own form state here.
 *
 * Extracted verbatim from DatasetDetail.tsx (C2 split); no behavior change.
 */

import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";

import {
  createDocumentFromText,
  createDocumentFromUrl,
  deleteDataset,
  updateDataset,
} from "@/api/knowledge";
import type { Dataset } from "@/types/knowledge";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { Label } from "@/components/ui/label";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from "@/components/ui/dialog";
import { toast } from "@/hooks/use-toast";

interface EditDatasetDialogProps {
  datasetId?: string;
  dataset?: Dataset;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

export function EditDatasetDialog({ datasetId, dataset, open, onOpenChange }: EditDatasetDialogProps) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [settingsName, setSettingsName] = useState("");
  const [settingsDesc, setSettingsDesc] = useState("");
  const [settingsSaving, setSettingsSaving] = useState(false);

  useEffect(() => {
    if (dataset) {
      setSettingsName(dataset.name || "");
      setSettingsDesc(dataset.description || "");
    }
  }, [dataset]);

  async function handleSaveSettings() {
    if (!datasetId) return;
    setSettingsSaving(true);
    try {
      await updateDataset(datasetId, {
        name: settingsName.trim(),
        description: settingsDesc.trim(),
      });
      await qc.invalidateQueries({ queryKey: ["kb-dataset", datasetId] });
      await qc.invalidateQueries({ queryKey: ["kb-datasets"] });
      onOpenChange(false);
    } finally {
      setSettingsSaving(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>{t("knowledge.detail.editKB")}</DialogTitle>
        </DialogHeader>
        <div className="space-y-4">
          <div>
            <Label>{t("knowledge.detail.nameLabel")}</Label>
            <Input
              value={settingsName}
              onChange={(e) => setSettingsName(e.target.value)}
              className="mt-1"
            />
          </div>
          <div>
            <Label>{t("knowledge.detail.descLabel")}</Label>
            <Textarea
              value={settingsDesc}
              onChange={(e) => setSettingsDesc(e.target.value)}
              rows={3}
              className="mt-1"
            />
          </div>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            {t("knowledge.detail.cancel")}
          </Button>
          <Button onClick={handleSaveSettings} disabled={settingsSaving} className="bg-primary hover:bg-primary/90">
            {settingsSaving ? t("knowledge.detail.saving") : t("knowledge.detail.save")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

interface DeleteDatasetDialogProps {
  datasetId?: string;
  datasetName?: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

export function DeleteDatasetDialog({ datasetId, datasetName, open, onOpenChange }: DeleteDatasetDialogProps) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const nav = useNavigate();
  const [deleting, setDeleting] = useState(false);
  const [deletePassword, setDeletePassword] = useState("");

  // The header dropdown used to clear the password right before opening.
  useEffect(() => {
    if (open) setDeletePassword("");
  }, [open]);

  const closeDeleteDialog = () => {
    onOpenChange(false);
    setDeletePassword("");
  };

  async function handleDeleteDataset() {
    if (!datasetId || deleting) return;
    if (!deletePassword) {
      toast.warning(t("knowledge.detail.deletePasswordRequired"));
      return;
    }

    setDeleting(true);
    try {
      await deleteDataset(datasetId, { password: deletePassword });
      await qc.invalidateQueries({ queryKey: ["kb-datasets"] });
      nav("/knowledge");
    } catch (e) {
      toast.error(
        t("knowledge.datasets.deleteFailed"),
        e instanceof Error ? e.message : String(e),
      );
    } finally {
      setDeleting(false);
    }
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(nextOpen) => {
        if (nextOpen) {
          onOpenChange(true);
          return;
        }
        if (deleting) {
          return;
        }
        closeDeleteDialog();
      }}
    >
      <DialogContent className="max-w-sm">
        <DialogHeader>
          <DialogTitle className="text-red-600">{t("knowledge.detail.deleteConfirmTitle")}</DialogTitle>
        </DialogHeader>
        <div className="space-y-3">
          <p className="text-sm text-muted-foreground">
            {t("knowledge.detail.deleteConfirmText", { name: datasetName })}
          </p>
          <div className="space-y-1.5">
            <Label htmlFor="delete-kb-password">{t("knowledge.detail.deletePasswordLabel")}</Label>
            <Input
              id="delete-kb-password"
              type="password"
              autoComplete="off"
              name="delete-confirm-pwd"
              value={deletePassword}
              onChange={(e) => setDeletePassword(e.target.value)}
              placeholder={t("knowledge.detail.deletePasswordPlaceholder")}
              disabled={deleting}
              onKeyDown={(e) => {
                if (e.key === "Enter" && deletePassword && !deleting) {
                  void handleDeleteDataset();
                }
              }}
            />
          </div>
        </div>
        <DialogFooter>
          <Button
            variant="outline"
            onClick={() => {
              if (deleting) {
                return;
              }
              closeDeleteDialog();
            }}
            disabled={deleting}
          >
            {t("knowledge.detail.cancel")}
          </Button>
          <Button
            variant="destructive"
            onClick={handleDeleteDataset}
            disabled={deleting || !deletePassword}
          >
            {deleting ? t("knowledge.detail.deleting") : t("knowledge.detail.confirmDelete")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

interface CreateTextDocumentDialogProps {
  datasetId?: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

export function CreateTextDocumentDialog({ datasetId, open, onOpenChange }: CreateTextDocumentDialogProps) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [textTitle, setTextTitle] = useState("");
  const [textContent, setTextContent] = useState("");
  const [textSaving, setTextSaving] = useState(false);

  async function handleCreateText() {
    if (!datasetId || !textTitle.trim() || !textContent.trim()) return;
    setTextSaving(true);
    try {
      await createDocumentFromText(datasetId, {
        title: textTitle.trim(),
        content: textContent.trim(),
      });
      await qc.invalidateQueries({ queryKey: ["kb-documents", datasetId] });
      onOpenChange(false);
      setTextTitle("");
      setTextContent("");
    } finally {
      setTextSaving(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>{t("knowledge.detail.createTextDoc")}</DialogTitle>
        </DialogHeader>
        <div className="space-y-4">
          <div>
            <Label>{t("knowledge.detail.titleLabel")}</Label>
            <Input
              value={textTitle}
              onChange={(e) => setTextTitle(e.target.value)}
              placeholder={t("knowledge.detail.titlePlaceholder")}
              className="mt-1"
            />
          </div>
          <div>
            <Label>{t("knowledge.detail.contentLabel")}</Label>
            <Textarea
              value={textContent}
              onChange={(e) => setTextContent(e.target.value)}
              placeholder={t("knowledge.detail.contentPlaceholder")}
              rows={12}
              className="mt-1"
            />
          </div>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            {t("knowledge.detail.cancel")}
          </Button>
          <Button
            onClick={handleCreateText}
            disabled={textSaving || !textTitle.trim() || !textContent.trim()}
            className="bg-primary hover:bg-primary/90"
          >
            {textSaving ? t("knowledge.detail.creating") : t("knowledge.detail.create")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

interface CreateUrlDocumentDialogProps {
  datasetId?: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

export function CreateUrlDocumentDialog({ datasetId, open, onOpenChange }: CreateUrlDocumentDialogProps) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [urlInput, setUrlInput] = useState("");
  const [urlTitle, setUrlTitle] = useState("");
  const [urlSaving, setUrlSaving] = useState(false);

  async function handleCreateFromUrl() {
    if (!datasetId || !urlInput.trim()) return;
    setUrlSaving(true);
    try {
      await createDocumentFromUrl(datasetId, {
        url: urlInput.trim(),
        title: urlTitle.trim() || undefined,
      });
      await qc.invalidateQueries({ queryKey: ["kb-documents", datasetId] });
      onOpenChange(false);
      setUrlInput("");
      setUrlTitle("");
    } catch (e) {
      toast.error(t("knowledge.detail.urlFetchFailed"), e instanceof Error ? e.message : String(e));
    } finally {
      setUrlSaving(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>{t("knowledge.detail.addUrlDoc")}</DialogTitle>
        </DialogHeader>
        <div className="space-y-4">
          <div>
            <Label>{t("knowledge.detail.urlLabel")} <span className="text-red-500">*</span></Label>
            <Input
              value={urlInput}
              onChange={(e) => setUrlInput(e.target.value)}
              placeholder="https://example.com/document.html"
              className="mt-1"
            />
          </div>
          <div>
            <Label>{t("knowledge.detail.urlTitleOptional")}</Label>
            <Input
              value={urlTitle}
              onChange={(e) => setUrlTitle(e.target.value)}
              placeholder={t("knowledge.detail.urlTitlePlaceholder")}
              className="mt-1"
            />
          </div>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            {t("knowledge.detail.cancel")}
          </Button>
          <Button
            onClick={handleCreateFromUrl}
            disabled={urlSaving || !urlInput.trim()}
            className="bg-primary hover:bg-primary/90"
          >
            {urlSaving ? t("knowledge.detail.fetching") : t("knowledge.detail.add")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
