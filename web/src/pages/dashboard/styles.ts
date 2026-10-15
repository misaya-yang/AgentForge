// Dashboard design tokens for the AI Gateway operations console.
// The palette intentionally stays quiet and operational: carbon neutrals,
// indigo product chrome, coral support, and distinct semantic states.

export const SPACING = {
  xs: 4, sm: 8, md: 12, lg: 16, xl: 20, '2xl': 24, '3xl': 32, '4xl': 40, '5xl': 56, '6xl': 72,
} as const;

export const RADIUS = { sm: 5, md: 8, lg: 10 } as const;

export const TYPOGRAPHY = {
  pageTitle:    { fontSize: 17, fontWeight: 600, letterSpacing: '0', lineHeight: 1.2 },
  sectionTitle: { fontSize: 13.5, fontWeight: 600, letterSpacing: '0' },
  eyebrow:      { fontSize: 11, fontWeight: 500, letterSpacing: '0' },
  cardLabel:    { fontSize: 12.5, fontWeight: 500 },
  kpiValue:     { fontSize: 26, fontWeight: 600, fontFeatureSettings: '"tnum"', letterSpacing: '0' },
  kpiUnit:      { fontSize: 13, fontWeight: 500 },
  body:         { fontSize: 13, fontWeight: 400 },
  caption:      { fontSize: 11.5, fontWeight: 500 },
  mono:         { fontFamily: '"IBM Plex Mono", "JetBrains Mono", ui-monospace, SFMono-Regular, Consolas, monospace', fontFeatureSettings: '"tnum"' },
} as const;

export const FONT_FAMILY = {
  sans: '"Geist Sans", "Geist", "Noto Sans SC", "PingFang SC", "Microsoft YaHei", system-ui, sans-serif',
  mono: '"IBM Plex Mono", "JetBrains Mono", ui-monospace, SFMono-Regular, Consolas, monospace',
} as const;

export const TRANSITION = {
  fast:   'color 140ms cubic-bezier(0.16, 1, 0.3, 1), background-color 140ms cubic-bezier(0.16, 1, 0.3, 1), border-color 140ms cubic-bezier(0.16, 1, 0.3, 1), box-shadow 140ms cubic-bezier(0.16, 1, 0.3, 1), opacity 140ms cubic-bezier(0.16, 1, 0.3, 1), transform 140ms cubic-bezier(0.16, 1, 0.3, 1)',
  normal: 'color 200ms cubic-bezier(0.16, 1, 0.3, 1), background-color 200ms cubic-bezier(0.16, 1, 0.3, 1), border-color 200ms cubic-bezier(0.16, 1, 0.3, 1), box-shadow 200ms cubic-bezier(0.16, 1, 0.3, 1), opacity 200ms cubic-bezier(0.16, 1, 0.3, 1), transform 200ms cubic-bezier(0.16, 1, 0.3, 1)',
  slow:   'color 320ms cubic-bezier(0.16, 1, 0.3, 1), background-color 320ms cubic-bezier(0.16, 1, 0.3, 1), border-color 320ms cubic-bezier(0.16, 1, 0.3, 1), box-shadow 320ms cubic-bezier(0.16, 1, 0.3, 1), opacity 320ms cubic-bezier(0.16, 1, 0.3, 1), transform 320ms cubic-bezier(0.16, 1, 0.3, 1)',
} as const;

export const REDUCED_MOTION_CSS = `@media(prefers-reduced-motion:reduce){*,*::before,*::after{animation-duration:0.01ms!important;transition-duration:0.01ms!important}}`;

export const ELEVATION = (dark: boolean) => ({
  none: 'none',
  sm: dark ? 'none' : '0 1px 2px rgba(16,18,24,0.04)',
  md: dark ? 'none' : '0 4px 16px rgba(16,18,24,0.05)',
  lg: dark ? 'none' : '0 8px 32px rgba(16,18,24,0.08)',
});

export const LAYOUT = {
  CARD_GAP: 16,
  CARD_PADDING: 16,
  CARD_RADIUS: 8,
  PAGE_PADDING: 0,
  KPI_HEIGHT: 'auto' as unknown as number,
  PANEL_MIN_HEIGHT: 320,
  DASHBOARD_MIN_CONTENT_WIDTH: 1180,
  GRID_GAP: 16,
  SECTION_GAP: 16,
  KPI_COLUMNS: 5,
  PROVIDER_COLUMNS: 5,
  FIVE_COL_MIN_ITEM_WIDTH: 190,
  BREAKPOINTS: { sm: 640, md: 768, lg: 1024, xl: 1280 },
} as const;

export const getColors = (darkMode: boolean) => ({
  // Canvas
  pageBg:    darkMode ? '#0f1013' : '#f7f7f9',
  cardBg:    darkMode ? '#17181d' : '#ffffff',
  cardHover: darkMode ? '#202128' : '#f0f1f4',
  innerBg:   darkMode ? '#121317' : '#f0f1f4',

  // Borders
  border:       darkMode ? '#34353e' : '#dcdde2',
  borderSoft:   darkMode ? '#292a32' : '#e7e8ec',
  divider:      darkMode ? '#24252c' : '#ececf0',
  borderHover:  darkMode ? '#474954' : '#c3c4ca',
  borderStrong: darkMode ? '#5c5f6c' : '#9598a2',

  // Ink
  textPrimary:   darkMode ? '#f4f4f5' : '#17181c',
  textSecondary: darkMode ? '#acadb5' : '#62656e',
  textMuted:     darkMode ? '#898c96' : '#6d707a',
  textFaint:     darkMode ? '#7d808a' : '#6e717b',

  // Brand and operations accents
  accent:       darkMode ? '#a5a5ff' : '#5b5bd6',
  accentBright: darkMode ? '#c3c3ff' : '#6f6fe0',
  accentDeep:   darkMode ? '#8585ea' : '#4848b8',
  accentSoft:   darkMode ? 'rgba(165,165,255,0.14)' : '#ececff',
  operator:     darkMode ? '#a5a5ff' : '#5b5bd6',
  operatorSoft: darkMode ? 'rgba(165,165,255,0.12)' : '#ececff',
  navy:         darkMode ? '#c3c3ff' : '#4848b8',

  // Semantic
  success:      darkMode ? '#69c294' : '#2f8f68',
  successSoft:  darkMode ? 'rgba(105,194,148,0.14)' : '#e6f3ed',
  error:        darkMode ? '#f07c78' : '#d64545',
  errorSoft:    darkMode ? 'rgba(240,124,120,0.14)' : '#fff0ef',
  warning:      darkMode ? '#d9aa62' : '#a86d26',
  warningSoft:  darkMode ? 'rgba(217,170,98,0.14)' : '#f7eddb',
  danger:       darkMode ? '#f07c78' : '#d64545',

  // Back-compat aliases
  gold:   darkMode ? '#d9aa62' : '#a86d26',
  purple: darkMode ? '#e89a83' : '#d0795f',
  info:   darkMode ? '#a5a5ff' : '#5b5bd6',

  // Tinted fills
  successBg: darkMode ? 'rgba(105,194,148,0.15)' : '#e6f3ed',
  warningBg: darkMode ? 'rgba(217,170,98,0.15)' : '#f7eddb',
  errorBg:   darkMode ? 'rgba(240,124,120,0.15)' : '#fff0ef',
  accentBg:  darkMode ? 'rgba(165,165,255,0.15)' : '#ececff',
  purpleBg:  darkMode ? 'rgba(232,154,131,0.14)' : '#faeee9',
  infoBg:    darkMode ? 'rgba(165,165,255,0.14)' : '#ececff',

  shadowSm: darkMode ? 'none' : '0 1px 2px rgba(16,18,24,0.04)',
  shadowMd: darkMode ? 'none' : '0 4px 16px rgba(16,18,24,0.05)',
  shadowLg: darkMode ? 'none' : '0 8px 32px rgba(16,18,24,0.08)',

  // Legacy gradient keys
  accentGradient:  darkMode ? 'linear-gradient(135deg,#a5a5ff,#8585ea)' : 'linear-gradient(135deg,#5b5bd6,#4848b8)',
  successGradient: darkMode ? 'linear-gradient(135deg,#69c294,#4f9f76)' : 'linear-gradient(135deg,#2f8f68,#237251)',
  warningGradient: darkMode ? 'linear-gradient(135deg,#d9aa62,#ba8745)' : 'linear-gradient(135deg,#a86d26,#84521b)',
  errorGradient:   darkMode ? 'linear-gradient(135deg,#f07c78,#d65f5b)' : 'linear-gradient(135deg,#d64545,#b83535)',
  purpleGradient:  darkMode ? 'linear-gradient(135deg,#e89a83,#ca745e)' : 'linear-gradient(135deg,#d0795f,#ac5c47)',
});

// ── KPI per-metric color map (dashboard.jsx KPIS) ─────────────────────
export const getKpiAccents = (darkMode: boolean) => {
  const c = getColors(darkMode);
  return {
    requests: { fg: c.accent, bg: c.accentBg },
    cost:     { fg: c.success, bg: c.successBg },
    latency:  { fg: c.info,   bg: c.infoBg },
    success:  { fg: c.success, bg: c.successBg },
    tokens:   { fg: c.purple, bg: c.purpleBg },
  };
};

// ── Chart series palette (exact from dashboard.jsx) ───────────────────
export const getChartPalette = (darkMode: boolean) => {
  const c = getColors(darkMode);
  return darkMode ? [
    '#a5a5ff', '#e89a83', '#69c294', '#f07c78', '#d9aa62', c.textFaint,
  ] : [
    '#5b5bd6',
    '#d0795f',
    '#2f8f68',
    '#d64545',
    '#a86d26',
    '#9598a2',
  ];
};

export const commonStyles = { transition: TRANSITION.normal, cardHeaderHeight: 44 };

export const gridStyles = {
  fiveColumn: { display: 'grid' as const, gridTemplateColumns: 'repeat(5, 1fr)', gap: LAYOUT.GRID_GAP },
  fiveColumnResponsive: { display: 'grid' as const, gridTemplateColumns: `repeat(auto-fit, minmax(${LAYOUT.FIVE_COL_MIN_ITEM_WIDTH}px, 1fr))`, gap: LAYOUT.GRID_GAP },
};

export const getCardStyles = (darkMode: boolean) => {
  const c = getColors(darkMode);
  return { background: c.cardBg, border: `1px solid ${c.borderSoft}`, borderRadius: LAYOUT.CARD_RADIUS, padding: LAYOUT.CARD_PADDING };
};

export const getSectionStyles = (darkMode: boolean) => {
  const c = getColors(darkMode);
  return { marginBottom: LAYOUT.SECTION_GAP, background: c.cardBg, border: `1px solid ${c.borderSoft}`, borderRadius: LAYOUT.CARD_RADIUS, padding: LAYOUT.CARD_PADDING };
};
