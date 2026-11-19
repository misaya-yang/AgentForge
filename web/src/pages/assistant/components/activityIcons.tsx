/**
 * activityIcons — the subset of stroke-path icons from chat.jsx that
 * the Activity system uses. Kept as a hand-rolled SVG component so the
 * visual output matches the Claude Design prototype exactly (lucide's
 * paths are close but not identical).
 */

const PATHS: Record<string, string> = {
  sparkle:
    "M12 3l1.5 4.5L18 9l-4.5 1.5L12 15l-1.5-4.5L6 9l4.5-1.5zM19 15l.7 2 2 .7-2 .7-.7 2-.7-2-2-.7 2-.7zM5 15l.7 2 2 .7-2 .7-.7 2-.7-2-2-.7 2-.7z",
  brain:
    "M9 3a3 3 0 0 0-3 3v0a3 3 0 0 0-3 3v0a3 3 0 0 0 1.5 2.6A3 3 0 0 0 3 14v0a3 3 0 0 0 3 3v0a3 3 0 0 0 3 3v0a3 3 0 0 0 3-3V6a3 3 0 0 0-3-3zM15 3a3 3 0 0 1 3 3v0a3 3 0 0 1 3 3v0a3 3 0 0 1-1.5 2.6A3 3 0 0 1 21 14v0a3 3 0 0 1-3 3v0a3 3 0 0 1-3 3v0a3 3 0 0 1-3-3V6a3 3 0 0 1 3-3z",
  search: "M11 19a8 8 0 1 0 0-16 8 8 0 0 0 0 16zM21 21l-4.3-4.3",
  globe:
    "M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18zM3 12h18M12 3a13 13 0 0 1 0 18 13 13 0 0 1 0-18z",
  tool: "M14.7 6.3a4 4 0 0 1 5 5l-11 11-5-5 11-11zM12 8l5 5",
  file: "M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8zM14 2v6h6",
  check: "M20 6L9 17l-5-5",
  x: "M18 6L6 18M6 6l12 12",
  chevRight: "M9 18l6-6-6-6",
  image:
    "M4 4h16v16H4zM4 16l4-4 4 4 4-4 4 4M15 8a1.5 1.5 0 1 0 0 3 1.5 1.5 0 0 0 0-3z",
  code: "M8 6l-5 6 5 6M16 6l5 6-5 6M14 4l-4 16",
  edit: "M12 20h9M16.5 3.5a2 2 0 0 1 3 3L7 19l-4 1 1-4z",
};

interface IconProps {
  name: keyof typeof PATHS | string;
  size?: number;
  stroke?: number;
  className?: string;
}

export function Icon({ name, size = 16, stroke = 1.6, className }: IconProps) {
  const d = PATHS[name] ?? PATHS.tool;
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={stroke}
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className}
      aria-hidden="true"
    >
      <path d={d} />
    </svg>
  );
}
