import { AlertCircle } from "lucide-react";
import { useTranslation } from "react-i18next";

import {
  listFailedSources,
  type PendingFile,
  type PendingUrl,
} from "./datasetCreateModel";

interface SourceUploadFailureAlertProps {
  error: string | null;
  datasetCreated: boolean;
  outcomeUnknown: boolean;
  files: PendingFile[];
  urls: PendingUrl[];
}

export function SourceUploadFailureAlert({
  error,
  datasetCreated,
  outcomeUnknown,
  files,
  urls,
}: SourceUploadFailureAlertProps) {
  const { t } = useTranslation();
  if (!error) return null;

  const failedSources = listFailedSources(files, urls, t("knowledge.create.uploadFailed"));
  const title = outcomeUnknown
    ? t(datasetCreated ? "knowledge.create.partialUploadUnknownTitle" : "knowledge.create.createOutcomeUnknownTitle")
    : t(datasetCreated ? "knowledge.create.partialUploadTitle" : "knowledge.create.createFailed");

  return (
    <div
      className="mb-6 p-4 bg-red-500/10 dark:bg-red-500/15 border border-red-500/20 rounded-lg flex items-start gap-3"
      role="alert"
    >
      <AlertCircle className="h-5 w-5 text-red-500 shrink-0 mt-0.5" />
      <div className="min-w-0">
        <p className="text-sm font-medium text-red-800 dark:text-red-300">{title}</p>
        <p className="text-sm text-red-600 dark:text-red-400 mt-1">{error}</p>
        {failedSources.length > 0 && (
          <ul className="mt-3 space-y-1 text-sm text-red-700 dark:text-red-300">
            {failedSources.map((source) => (
              <li key={source.key} className="break-words">
                <span className="font-medium">{source.name}</span>: {source.error}
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}
