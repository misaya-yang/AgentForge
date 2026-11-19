function handleEmergencyReset() {
  const params = new URLSearchParams(window.location.search);
  if (params.get("reset") !== "true") return;

  localStorage.removeItem("agent-gateway-storage");
  window.history.replaceState({}, "", window.location.pathname);
}

function applyInitialThemeAndLocale() {
  const storageRaw = localStorage.getItem("agent-gateway-storage");
  const i18nLng = localStorage.getItem("i18nextLng") || "en-US";
  const locale = /^zh/i.test(i18nLng) ? "zh-CN" : "en-US";
  document.documentElement.lang = locale;
  document.documentElement.dir = "ltr";

  let mode = "system";
  let resolved: string;
  let legacyDarkMode: boolean | null = null;
  try {
    const parsed = storageRaw ? JSON.parse(storageRaw) : null;
    const state = parsed && parsed.state ? parsed.state : {};
    mode = state.themeMode || mode;
    legacyDarkMode =
      typeof state.darkMode === "boolean" ? state.darkMode : null;
  } catch {
    mode = "system";
  }

  if (mode === "system") {
    const systemDark =
      window.matchMedia &&
      window.matchMedia("(prefers-color-scheme: dark)").matches;
    resolved = systemDark ? "dark" : "light";
  } else if (mode === "dark" || mode === "light") {
    resolved = mode;
  } else if (legacyDarkMode !== null) {
    resolved = legacyDarkMode ? "dark" : "light";
  } else {
    resolved = "light";
  }

  const isDark = resolved === "dark";
  document.documentElement.classList.toggle("dark", isDark);
  document.documentElement.style.colorScheme = isDark ? "dark" : "light";
  document.documentElement.dataset.theme = isDark ? "dark" : "light";
}

function installBootWatchdog() {
  window.setTimeout(() => {
    const fallback = document.getElementById("app-boot-fallback");
    if (!fallback) return;

    const msg = document.getElementById("app-boot-msg");
    const err = document.getElementById("app-boot-error");
    if (msg) msg.style.display = "none";
    if (err) err.style.display = "block";

    const btn = document.getElementById("app-boot-retry");
    if (btn) {
      btn.addEventListener("click", () => {
        location.reload();
      });
    }
  }, 15000);
}

handleEmergencyReset();
applyInitialThemeAndLocale();
installBootWatchdog();
