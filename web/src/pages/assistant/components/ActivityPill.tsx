/**
 * ActivityPill — GPT-style inline text link that opens the right-side
 * Activity drawer. Pattern reference: GPT's `Thought for 2m 45s ›`.
 *
 * Zero chrome at rest: no border, no background, no icon halo. Just
 * text at text-secondary level + chevron, underlined on hover. Running
 * state prefixes three animated dots in accent tone so the user knows
 * thinking is live. Subtitle ("· N steps · XXs") appended in mono.
 */

import { T, ui, ensureActivityStyles } from "./activityTheme";
import { Icon } from "./activityIcons";

interface ActivityPillProps {
  steps: number;
  durationLabel: string;
  running: boolean;
  onOpen: () => void;
  variant?: "pill" | "chip";
  label: string; // localized "Thinking" | "Activity"
}

export function ActivityPill({
  steps,
  durationLabel,
  running,
  onOpen,
  variant = "pill",
  label,
}: ActivityPillProps) {
  ensureActivityStyles();

  const subtitle =
    durationLabel && steps > 0
      ? `· ${steps} ${steps === 1 ? "step" : "steps"} · ${durationLabel}`
      : steps > 0
        ? `· ${steps} ${steps === 1 ? "step" : "steps"}`
        : durationLabel
          ? `· ${durationLabel}`
          : "";

  const isChip = variant === "chip";
  const fontSize = isChip ? 13 : 14;
  const subtitleSize = isChip ? 12 : 12.5;

  return (
    <button
      type="button"
      onClick={onOpen}
      title={label}
      aria-label={label}
      className="act-pill-link"
      style={{
        display: "inline-flex",
        alignItems: "center",
        gap: 6,
        padding: "2px 0",
        border: "none",
        background: "transparent",
        color: T.textMute,
        fontFamily: ui.sans,
        fontSize,
        cursor: "pointer",
        lineHeight: 1.3,
      }}
    >
      {running && (
        <span
          className="act-dot-wrap"
          style={{
            display: "inline-flex",
            color: T.accent,
            width: 14,
            justifyContent: "center",
          }}
        >
          <span className="act-dot" />
          <span className="act-dot" />
          <span className="act-dot" />
        </span>
      )}
      <span
        style={{
          fontWeight: 500,
          maxWidth: "min(60vw, 36rem)",
          overflow: "hidden",
          textOverflow: "ellipsis",
          whiteSpace: "nowrap",
        }}
      >
        {label}
      </span>
      {subtitle && (
        <span
          style={{
            color: T.textDim,
            fontFamily: ui.mono,
            fontSize: subtitleSize,
          }}
        >
          {subtitle}
        </span>
      )}
      <Icon name="chevRight" size={13} />
    </button>
  );
}
