import { useEffect, useState } from 'react';
import { ConfigProvider, App as AntApp, theme, type ConfigProviderProps } from 'antd';
import { useTranslation } from 'react-i18next';
import { AppRouter } from "@/router";
import { useAppStore } from "@/store/useAppStore";
import { useThemeSync } from "@/hooks/useThemeSync";
import { useAuthSessionGuard } from "@/hooks/useAuthSessionGuard";
import { resolveAppLocale } from "@/i18n";
import { lightTheme, darkTheme } from "@/theme/themeConfig";
import { Toaster } from "@/components/ui/toaster";

export default function App() {
  const resolvedTheme = useThemeSync();
  useAuthSessionGuard();
  const darkMode = useAppStore((s) => s.darkMode);
  const { i18n } = useTranslation();
  const currentTheme = darkMode ? darkTheme : lightTheme;

  // Keep document language in sync with app locale
  useEffect(() => {
    const locale = resolveAppLocale(i18n.resolvedLanguage || i18n.language);
    document.documentElement.lang = locale;
    document.documentElement.dir = "ltr";
  }, [i18n.language, i18n.resolvedLanguage]);

  const locale = resolveAppLocale(i18n.resolvedLanguage || i18n.language);
  const [antdLocale, setAntdLocale] = useState<ConfigProviderProps["locale"]>();
  useEffect(() => {
    let active = true;
    const loader = locale === "en-US"
      ? import("antd/locale/en_US")
      : import("antd/locale/zh_CN");
    void loader.then((module) => {
      if (active) setAntdLocale(module.default);
    });
    return () => {
      active = false;
    };
  }, [locale]);

  return (
    <ConfigProvider
      locale={antdLocale}
      theme={{
        ...currentTheme,
        algorithm: resolvedTheme === "dark" ? theme.darkAlgorithm : theme.defaultAlgorithm,
      }}
    >
      <AntApp>
        <AppRouter />
        <Toaster />
      </AntApp>
    </ConfigProvider>
  );
}
