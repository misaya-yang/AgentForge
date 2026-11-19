/**
 * activityTheme — design tokens for the Claude-design Activity system.
 *
 * Tokens now source from the global `--assistant-*` CSS variables defined
 * in `web/src/index.css`, so the Activity panel is guaranteed to share
 * the same palette as the chat canvas in both light and dark modes.
 * Previously this file defined a separate blue-accented palette that
 * clashed with the rest of the assistant page — that drift is gone.
 *
 * Consumers keep using `T.bg` etc. — the identifiers stay stable while
 * the values now follow the page theme automatically via hsl(var(...)).
 */

// Tokens reference shared CSS variables so theme toggles take effect
// without re-rendering React trees.
export const T = {
  bg: "hsl(var(--assistant-canvas-bg))",
  bgSoft: "hsl(var(--assistant-surface-soft))",
  panel: "hsl(var(--assistant-surface-bg))",
  border: "hsl(var(--assistant-border))",
  borderSoft: "hsl(var(--assistant-border-soft))",
  text: "hsl(var(--assistant-text-primary))",
  textMute: "hsl(var(--assistant-text-secondary))",
  textDim: "hsl(var(--assistant-text-tertiary))",
  accent: "hsl(var(--assistant-accent))",
  accentSoft: "hsl(var(--assistant-accent-soft))",
  accentText: "hsl(var(--assistant-accent))",
  rail: "hsl(var(--assistant-surface-bg))",
  userBubble: "hsl(var(--assistant-user-bubble))",
  shadow: "var(--assistant-shadow)",
  shadowLift: "var(--assistant-shadow-lift)",
} as const;

export const ui = {
  sans:
    'Inter, -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif',
  mono: '"JetBrains Mono", ui-monospace, SFMono-Regular, Menlo, Consolas, monospace',
} as const;

// Inject shared keyframes / helper classes once per session. Idempotent.
// Palette variables come from `web/src/index.css`; this function only
// installs the animations and scrollbar/hover utilities the Activity
// components rely on.
export function ensureActivityStyles(): void {
  if (typeof document === "undefined") return;
  if (document.getElementById("assistant-activity-styles")) return;
  const s = document.createElement("style");
  s.id = "assistant-activity-styles";
  s.textContent = `
    .act-scroll::-webkit-scrollbar{width:8px;height:8px}
    .act-scroll::-webkit-scrollbar-thumb{background:hsl(var(--assistant-border));border-radius:4px}
    .act-scroll::-webkit-scrollbar-track{background:transparent}
    .act-btn{transition:background .12s, color .12s, transform .08s}
    .act-btn:active{transform:scale(0.97)}
    @keyframes act-dot{0%,60%,100%{opacity:.3;transform:translateY(0)}30%{opacity:1;transform:translateY(-2px)}}
    .act-dot{display:inline-block;width:4px;height:4px;border-radius:2px;background:currentColor;margin-right:3px;animation:act-dot 1.2s infinite}
    .act-dot:nth-child(2){animation-delay:.15s}
    .act-dot:nth-child(3){animation-delay:.3s}
    @keyframes act-slide-in{from{transform:translateX(100%);opacity:0}to{transform:translateX(0);opacity:1}}
    .act-panel-enter{animation:act-slide-in .28s cubic-bezier(.2,.8,.3,1) both}
    @keyframes act-step-tick{from{opacity:0;transform:translateX(-4px)}to{opacity:1;transform:none}}
    .act-step-new{animation:act-step-tick .35s ease both}
    .act-caret{display:inline-block;width:2px;height:14px;background:currentColor;vertical-align:-2px;margin-left:2px;animation:act-caret 1s infinite}
    @keyframes act-caret{0%,50%{opacity:1}51%,100%{opacity:0}}
    .act-running-dot{width:5px;height:5px;border-radius:3px;background:hsl(var(--assistant-accent));animation:act-dot 1.2s infinite}
    .act-hover:hover{background:hsl(var(--assistant-surface-soft))}
    .act-accent-hover:hover{background:hsl(var(--assistant-accent-soft))}
    .act-pill-link{transition:color .12s}
    .act-pill-link:hover{color:hsl(var(--assistant-text-primary))}
    .act-pill-link:hover > span:not(.act-dot):not(.act-dot-wrap){text-decoration:underline;text-decoration-color:hsl(var(--assistant-text-primary) / .3);text-underline-offset:3px}
  `;
  document.head.appendChild(s);
}
