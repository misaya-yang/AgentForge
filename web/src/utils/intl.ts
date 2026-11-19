import { resolveAppLocale, type AppLocale } from "@/i18n";

type DateInput = string | number | Date;

function toDate(value: DateInput): Date {
  return value instanceof Date ? value : new Date(value);
}

export function getAppLocale(input?: string): AppLocale {
  return resolveAppLocale(input);
}

export function formatDateTime(
  value: DateInput,
  locale?: string,
  options?: Intl.DateTimeFormatOptions
): string {
  const date = toDate(value);
  if (Number.isNaN(date.getTime())) return "";
  const targetLocale = getAppLocale(locale);
  return new Intl.DateTimeFormat(targetLocale, {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    ...options,
  }).format(date);
}

export function formatDate(
  value: DateInput,
  locale?: string,
  options?: Intl.DateTimeFormatOptions
): string {
  return formatDateTime(value, locale, {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    ...options,
  });
}

export function formatNumber(
  value: number,
  locale?: string,
  options?: Intl.NumberFormatOptions
): string {
  const targetLocale = getAppLocale(locale);
  return new Intl.NumberFormat(targetLocale, options).format(value);
}

