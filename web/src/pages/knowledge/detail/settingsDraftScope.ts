/** A draft may be written only after config for the current dataset has loaded. */
export function currentSettingsDraftKey(
  userId: string | undefined,
  datasetId: string | undefined,
  loadedDatasetId: string | null,
  ready: boolean
): string | null {
  if (!ready || !userId || !datasetId || loadedDatasetId !== datasetId) return null;
  return `kb-settings:${userId}:${datasetId}`;
}

export function shouldLoadSettingsConfig(
  datasetId: string | undefined,
  loadedDatasetId: string | null,
  lastAttemptId: string | null,
  active: boolean,
  wasActive: boolean
): boolean {
  return Boolean(active && datasetId && loadedDatasetId !== datasetId
    && (lastAttemptId !== datasetId || !wasActive));
}
