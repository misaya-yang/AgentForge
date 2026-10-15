import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import { loadForFinalLocale, LocaleOperationQueue } from "./localeOperationQueue";

export const APP_LOCALES = ["zh-CN", "en-US"] as const;
export type AppLocale = (typeof APP_LOCALES)[number];
export type DeferredTranslationNamespace = "eval" | "agents" | "knowledge";

const DEFAULT_LOCALE: AppLocale = "en-US";
const APP_LOCALE_SET = new Set<string>(APP_LOCALES);
const activeDeferredNamespaces = new Set<DeferredTranslationNamespace>();
const loadedBundles = new Set<string>();
let initialization: Promise<typeof i18n> | undefined;
const localeOperations = new LocaleOperationQueue();

export function resolveAppLocale(input?: string): AppLocale {
  if (!input) return DEFAULT_LOCALE;
  if (APP_LOCALE_SET.has(input)) return input as AppLocale;

  const normalized = input.replace("_", "-");
  if (APP_LOCALE_SET.has(normalized)) return normalized as AppLocale;

  const lower = normalized.toLowerCase();
  if (lower.startsWith("zh")) return "zh-CN";
  if (lower.startsWith("en")) return "en-US";
  return DEFAULT_LOCALE;
}

async function loadMainLocale(locale: AppLocale): Promise<Record<string, unknown>> {
  const module = locale === "zh-CN"
    ? await import("./locales/zh-CN.json")
    : await import("./locales/en-US.json");
  return module.default;
}

async function loadDeferredLocale(
  locale: AppLocale,
  namespace: DeferredTranslationNamespace,
): Promise<Record<string, unknown>> {
  if (namespace === "knowledge") {
    const module = locale === "zh-CN"
      ? await import("./locales/knowledge-zh-CN.json")
      : await import("./locales/knowledge-en-US.json");
    return module.default;
  }
  if (namespace === "eval") {
    const module = locale === "zh-CN"
      ? await import("./locales/eval-zh-CN.json")
      : await import("./locales/eval-en-US.json");
    return module.default;
  }
  const module = locale === "zh-CN"
    ? await import("./locales/agents-zh-CN.json")
    : await import("./locales/agents-en-US.json");
  return module.default;
}

async function ensureMainLocale(locale: AppLocale): Promise<void> {
  const key = `${locale}:main`;
  if (loadedBundles.has(key)) return;
  i18n.addResourceBundle(locale, "translation", await loadMainLocale(locale), true, true);
  loadedBundles.add(key);
}

async function ensureDeferredLocale(
  locale: AppLocale,
  namespace: DeferredTranslationNamespace,
): Promise<void> {
  const key = `${locale}:${namespace}`;
  if (loadedBundles.has(key)) return;
  const resources = await loadDeferredLocale(locale, namespace);
  i18n.addResourceBundle(locale, "translation", { [namespace]: resources }, true, true);
  loadedBundles.add(key);
}

export function initializeI18n(): Promise<typeof i18n> {
  if (initialization) return initialization;
  initialization = (async () => {
    const savedLng = typeof window !== "undefined"
      ? localStorage.getItem("i18nextLng")
      : null;
    const initialLng = resolveAppLocale(savedLng || undefined);
    const mainResources = await loadMainLocale(initialLng);
    loadedBundles.add(`${initialLng}:main`);
    await i18n
      .use(initReactI18next)
      .init({
        lng: initialLng,
        resources: { [initialLng]: { translation: mainResources } },
        fallbackLng: DEFAULT_LOCALE,
        supportedLngs: [...APP_LOCALES],
        debug: false,
        nonExplicitSupportedLngs: false,
        load: "currentOnly",
        cleanCode: true,
        returnNull: false,
        returnEmptyString: false,
        appendNamespaceToMissingKey: false,
        saveMissing: false,
        // escapeValue is disabled because every t() result is rendered through
        // React (JSX escapes by default); the only dangerouslySetInnerHTML sink
        // sanitizes with DOMPurify first. Keep t() output out of non-JSX sinks.
        interpolation: { escapeValue: false },
      });
    return i18n;
  })();
  // A failed init (e.g. a locale chunk fetch error) must not poison the cache —
  // reset so the next call can retry.
  initialization = initialization.catch((err) => {
    initialization = undefined;
    throw err;
  });
  return initialization;
}

export async function loadTranslationNamespaces(
  ...namespaces: DeferredTranslationNamespace[]
): Promise<void> {
  const requested = [...new Set(namespaces)];
  await localeOperations.run(async () => {
    await initializeI18n();
    const initialLocale = resolveAppLocale(i18n.resolvedLanguage || i18n.language);
    await loadForFinalLocale(
      initialLocale,
      () => resolveAppLocale(i18n.resolvedLanguage || i18n.language),
      async (locale) => {
        await Promise.all(requested.map((namespace) =>
          ensureDeferredLocale(resolveAppLocale(locale), namespace),
        ));
      },
    );
    // Activate only a complete batch. A failed import remains retryable and
    // must not make every future language change retry a broken namespace.
    for (const namespace of requested) activeDeferredNamespaces.add(namespace);
  });
}

export function loadTranslationNamespace(namespace: DeferredTranslationNamespace): Promise<void> {
  return loadTranslationNamespaces(namespace);
}

export async function changeAppLanguage(input: string): Promise<void> {
  await localeOperations.run(async () => {
    await initializeI18n();
    const locale = resolveAppLocale(input);
    await Promise.all([
      ensureMainLocale(locale),
      ...[...activeDeferredNamespaces].map((namespace) =>
        ensureDeferredLocale(locale, namespace),
      ),
    ]);
    // Bundles are pre-loaded before switching, so the language change is
    // synchronous-safe for every active namespace. No second ensure pass needed.
    await i18n.changeLanguage(locale);
  });
}

i18n.on("languageChanged", (lng) => {
  if (typeof window !== "undefined") {
    localStorage.setItem("i18nextLng", lng);
    document.documentElement.lang = lng;
  }
});

export default i18n;

export const languages = [
  { code: "zh-CN", nameKey: "language.zhCN", nativeName: "简体中文", flag: "🇨🇳" },
  { code: "en-US", nameKey: "language.enUS", nativeName: "English", flag: "🇺🇸" },
] as const;

export type LanguageCode = (typeof languages)[number]["code"];
