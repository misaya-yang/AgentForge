import { useTranslation } from "react-i18next";
import {
  Database,
  FileImage,
  FileText,
  Globe,
  HelpCircle,
  Lock,
  MessageSquare,
  Sparkles,
  Users,
} from "lucide-react";

import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import {
  EMBEDDING_MODELS,
  MAX_NAME_LENGTH,
  type KBType,
  type UseCase,
  type VisibilityType,
} from "@/pages/knowledge/create/datasetCreateModel";

const VISIBILITY_OPTIONS: Array<{
  id: VisibilityType;
  nameKey: string;
  descKey: string;
  icon: typeof Lock;
}> = [
  {
    id: "private",
    nameKey: "knowledge.create.visPrivate",
    descKey: "knowledge.create.visPrivateDesc",
    icon: Lock,
  },
  {
    id: "tenant",
    nameKey: "knowledge.create.visTenant",
    descKey: "knowledge.create.visTenantDesc",
    icon: Users,
  },
  {
    id: "public",
    nameKey: "knowledge.create.visPublic",
    descKey: "knowledge.create.visPublicDesc",
    icon: Globe,
  },
];

const KB_TYPE_OPTIONS: Array<{
  id: KBType;
  nameKey: string;
  descKey: string;
  icon: typeof FileText;
  color: string;
}> = [
  {
    id: "document",
    nameKey: "knowledge.create.kbTypeDocument",
    descKey: "knowledge.create.kbTypeDocumentDesc",
    icon: FileText,
    color: "text-blue-500",
  },
  {
    id: "data",
    nameKey: "knowledge.create.kbTypeData",
    descKey: "knowledge.create.kbTypeDataDesc",
    icon: Database,
    color: "text-green-500",
  },
];

const USE_CASE_OPTIONS: Array<{
  id: UseCase;
  nameKey: string;
  descKey: string;
  icon: typeof MessageSquare;
}> = [
  {
    id: "basic_qa",
    nameKey: "knowledge.create.useCaseBasicQA",
    descKey: "knowledge.create.useCaseBasicQADesc",
    icon: MessageSquare,
  },
  {
    id: "rich_text_response",
    nameKey: "knowledge.create.useCaseRichText",
    descKey: "knowledge.create.useCaseRichTextDesc",
    icon: FileImage,
  },
];

interface DatasetCreateBasicStepProps {
  name: string;
  nameError: string | null;
  description: string;
  visibility: VisibilityType;
  kbType: KBType;
  useCase: UseCase;
  embeddingModel: string;
  onNameChange: (value: string) => void;
  onDescriptionChange: (value: string) => void;
  onVisibilityChange: (value: VisibilityType) => void;
  onKbTypeChange: (value: KBType) => void;
  onUseCaseChange: (value: UseCase) => void;
  onEmbeddingModelChange: (value: string) => void;
}

export function DatasetCreateBasicStep({
  name,
  nameError,
  description,
  visibility,
  kbType,
  useCase,
  embeddingModel,
  onNameChange,
  onDescriptionChange,
  onVisibilityChange,
  onKbTypeChange,
  onUseCaseChange,
  onEmbeddingModelChange,
}: DatasetCreateBasicStepProps) {
  const { t } = useTranslation();

  return (
    <div className="space-y-6">
      <div>
        <Label className="text-sm font-medium">
          {t("knowledge.create.nameLabel")} <span className="text-red-500">*</span>
        </Label>
        <Input
          className={`mt-2 ${nameError ? "border-red-500" : ""}`}
          placeholder={t("knowledge.create.namePlaceholder")}
          value={name}
          maxLength={MAX_NAME_LENGTH}
          onChange={(event) => onNameChange(event.target.value)}
        />
        <div className="flex justify-between mt-1">
          {nameError ? (
            <span className="text-xs text-red-500">{nameError}</span>
          ) : (
            <span className="text-xs text-muted-foreground/70">
              {t("knowledge.create.nameLength")}
            </span>
          )}
          <span
            className={`text-xs ${
              name.length > MAX_NAME_LENGTH ? "text-red-500" : "text-muted-foreground/70"
            }`}
          >
            {name.length}/{MAX_NAME_LENGTH}
          </span>
        </div>
      </div>

      <div>
        <Label className="text-sm font-medium">
          {t("knowledge.create.descriptionLabel")}
        </Label>
        <Textarea
          className="mt-2"
          placeholder={t("knowledge.create.descriptionPlaceholder")}
          rows={4}
          value={description}
          onChange={(event) => onDescriptionChange(event.target.value)}
        />
      </div>

      <div>
        <Label className="text-sm font-medium">{t("knowledge.create.embeddingModel")}</Label>
        <Select value={embeddingModel} onValueChange={onEmbeddingModelChange}>
          <SelectTrigger className="mt-2">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {EMBEDDING_MODELS.map((model) => (
              <SelectItem
                key={`${model.provider}:${model.model}`}
                value={`${model.provider}:${model.model}`}
              >
                <div className="flex items-center gap-2">
                  <Sparkles className="h-4 w-4 text-primary" />
                  <span>{t(model.nameKey)}</span>
                  <span className="text-muted-foreground/70 text-xs">
                    ({t("knowledge.create.dimension", { dim: model.dimension })})
                  </span>
                </div>
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      <div>
        <div className="flex items-center gap-2 mb-3">
          <Label className="text-sm font-medium">
            {t("knowledge.create.kbType")} <span className="text-red-500">*</span>
          </Label>
          <TooltipProvider>
            <Tooltip>
              <TooltipTrigger>
                <HelpCircle className="h-4 w-4 text-muted-foreground/70" />
              </TooltipTrigger>
              <TooltipContent>
                <p className="max-w-xs">{t("knowledge.create.kbTypeHint")}</p>
              </TooltipContent>
            </Tooltip>
          </TooltipProvider>
        </div>
        <div
          className="grid grid-cols-1 gap-3 sm:grid-cols-2"
          role="radiogroup"
          aria-label={t("knowledge.create.kbType")}
        >
          {KB_TYPE_OPTIONS.map((option) => {
            const Icon = option.icon;
            return (
              <Card
                key={option.id}
                role="radio"
                aria-checked={kbType === option.id}
                tabIndex={0}
                className={`p-4 cursor-pointer transition-colors ${
                  kbType === option.id
                    ? "border-2 border-primary bg-primary/5"
                    : "border hover:border-primary/30"
                }`}
                onClick={() => onKbTypeChange(option.id)}
                onKeyDown={(event) => {
                  if (event.key === "Enter" || event.key === " ") {
                    event.preventDefault();
                    onKbTypeChange(option.id);
                  }
                }}
              >
                <div className="flex items-center gap-3">
                  <div
                    className={`p-2 rounded-lg bg-muted/50 ${
                      kbType === option.id ? "bg-primary/10" : ""
                    }`}
                  >
                    <Icon
                      className={`h-5 w-5 ${
                        kbType === option.id ? "text-primary" : option.color
                      }`}
                    />
                  </div>
                  <div className="flex-1">
                    <div className="flex items-center justify-between">
                      <span className="text-sm font-medium">{t(option.nameKey)}</span>
                      <div
                        className={`w-4 h-4 rounded-full border-2 flex items-center justify-center ${
                          kbType === option.id
                            ? "border-primary bg-primary/50"
                            : "border-border"
                        }`}
                      >
                        {kbType === option.id && (
                          <div className="w-2 h-2 rounded-full bg-card" />
                        )}
                      </div>
                    </div>
                    <p className="text-xs text-muted-foreground mt-1">{t(option.descKey)}</p>
                  </div>
                </div>
              </Card>
            );
          })}
        </div>
      </div>

      <div>
        <div className="flex items-center gap-2 mb-3">
          <Label className="text-sm font-medium">{t("knowledge.create.useCase")}</Label>
          <TooltipProvider>
            <Tooltip>
              <TooltipTrigger>
                <HelpCircle className="h-4 w-4 text-muted-foreground/70" />
              </TooltipTrigger>
              <TooltipContent>
                <p className="max-w-xs">{t("knowledge.create.useCaseHint")}</p>
              </TooltipContent>
            </Tooltip>
          </TooltipProvider>
        </div>
        <div
          className="grid grid-cols-1 gap-3 sm:grid-cols-2"
          role="radiogroup"
          aria-label={t("knowledge.create.useCase")}
        >
          {USE_CASE_OPTIONS.map((option) => {
            const Icon = option.icon;
            return (
              <Card
                key={option.id}
                role="radio"
                aria-checked={useCase === option.id}
                tabIndex={0}
                className={`p-4 cursor-pointer transition-colors ${
                  useCase === option.id
                    ? "border-2 border-primary bg-primary/5"
                    : "border hover:border-primary/30"
                }`}
                onClick={() => onUseCaseChange(option.id)}
                onKeyDown={(event) => {
                  if (event.key === "Enter" || event.key === " ") {
                    event.preventDefault();
                    onUseCaseChange(option.id);
                  }
                }}
              >
                <div className="flex items-center gap-2">
                  <Icon
                    className={`h-4 w-4 ${
                      useCase === option.id ? "text-primary" : "text-muted-foreground"
                    }`}
                  />
                  <span className="text-sm font-medium">{t(option.nameKey)}</span>
                </div>
                <p className="text-xs text-muted-foreground mt-1">{t(option.descKey)}</p>
              </Card>
            );
          })}
        </div>
      </div>

      <div>
        <div className="flex items-center gap-2 mb-3">
          <Label className="text-sm font-medium">{t("knowledge.create.visibility")}</Label>
          <TooltipProvider>
            <Tooltip>
              <TooltipTrigger>
                <HelpCircle className="h-4 w-4 text-muted-foreground/70" />
              </TooltipTrigger>
              <TooltipContent>
                <p className="max-w-xs">{t("knowledge.create.visibilityHint")}</p>
              </TooltipContent>
            </Tooltip>
          </TooltipProvider>
        </div>
        <div
          className="grid grid-cols-1 gap-3 sm:grid-cols-3"
          role="radiogroup"
          aria-label={t("knowledge.create.visibility")}
        >
          {VISIBILITY_OPTIONS.map((option) => {
            const Icon = option.icon;
            return (
              <Card
                key={option.id}
                role="radio"
                aria-checked={visibility === option.id}
                tabIndex={0}
                className={`p-4 cursor-pointer transition-colors ${
                  visibility === option.id
                    ? "border-2 border-primary bg-primary/5"
                    : "border hover:border-primary/30"
                }`}
                onClick={() => onVisibilityChange(option.id)}
                onKeyDown={(event) => {
                  if (event.key === "Enter" || event.key === " ") {
                    event.preventDefault();
                    onVisibilityChange(option.id);
                  }
                }}
              >
                <div className="flex items-center gap-2">
                  <Icon
                    className={`h-4 w-4 ${
                      visibility === option.id ? "text-primary" : "text-muted-foreground"
                    }`}
                  />
                  <span className="text-sm font-medium">{t(option.nameKey)}</span>
                </div>
                <p className="text-xs text-muted-foreground mt-1">{t(option.descKey)}</p>
              </Card>
            );
          })}
        </div>
      </div>
    </div>
  );
}
