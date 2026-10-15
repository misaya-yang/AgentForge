/**
 * TimelineStep — single row in the Activity rail (Claude Design).
 *
 * Ported from chat.jsx `ActivityBody` step rendering. The row is
 * `position: relative` and the circle marker is absolutely positioned
 * onto the rail drawn by the parent ActivityTimeline.
 *
 *  Thought row:
 *    - Bold title (13.5px)
 *    - Pre-wrapped prose body in muted text (12.5px)
 *
 *  Tool row:
 *    - Glyph (search/globe/file/...) + monospace tool name + duration right-aligned
 *    - Primary query/URL/path in a gray code block
 *    - Source chips (pill with leading accent dot)
 */

import { useState } from "react";
import { useTranslation } from "react-i18next";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { T, ui } from "./activityTheme";
import { Icon } from "./activityIcons";

export type TimelineIcon =
  | "thinking"
  | "web"
  | "file_read"
  | "file_write"
  | "code"
  | "image"
  | "document"
  | "other";

export interface TimelineSource {
  domain: string;
  url: string;
  title?: string;
}

export type TimelineStepData =
  | {
      kind: "thinking";
      id: string;
      title: string;
      body: string;
      streaming?: boolean;
      startedAt?: number;
    }
  | {
      kind: "tool";
      id: string;
      icon: TimelineIcon;
      title: string;
      body: string;
      sources?: TimelineSource[];
      durationMs?: number;
      status: "running" | "completed" | "error" | "unknown" | "not_executed";
      /** Original tool name — used for the monospace label (e.g. `web_search`). */
      toolName?: string;
      /** Original query / URL / path — rendered as a code block under the name. */
      queryArg?: string;
    };

// Glyph name (chat.jsx Icon names) for each tool-icon category.
function iconNameForTool(icon: TimelineIcon, toolName?: string): string {
  // Match design: web_search → search, fetch_page → globe, else → tool glyph
  if (icon === "web") {
    if (toolName?.toLowerCase().includes("fetch")) return "globe";
    return "search";
  }
  if (icon === "file_read" || icon === "file_write") return "file";
  if (icon === "code") return "code";
  if (icon === "image") return "image";
  if (icon === "document") return "file";
  return "tool";
}

function formatDuration(ms?: number): string | null {
  if (ms == null || !Number.isFinite(ms) || ms <= 0) return null;
  if (ms < 1000) return `${Math.round(ms)}ms`;
  return `${(ms / 1000).toFixed(1)}s`;
}

// -----------------------------------------------------------------------------
// Source chip — pill with leading accent dot + favicon.
// -----------------------------------------------------------------------------

function SourceChip({ source }: { source: TimelineSource }) {
  const [failed, setFailed] = useState(false);
  const favicon = `https://www.google.com/s2/favicons?domain=${encodeURIComponent(
    source.domain,
  )}&sz=32`;
  return (
    <a
      href={source.url}
      target="_blank"
      rel="noopener noreferrer"
      title={source.title || source.url}
      style={{
        display: "inline-flex",
        alignItems: "center",
        gap: 4,
        padding: "2px 7px",
        borderRadius: 10,
        background: T.panel,
        border: `1px solid ${T.border}`,
        fontSize: 11,
        color: T.textMute,
        maxWidth: 200,
        textDecoration: "none",
      }}
    >
      <span
        style={{
          width: 4,
          height: 4,
          borderRadius: 2,
          background: T.accent,
          flexShrink: 0,
        }}
      />
      {!failed && (
        <img
          src={favicon}
          alt=""
          loading="lazy"
          width={10}
          height={10}
          onError={() => setFailed(true)}
          style={{ width: 10, height: 10, borderRadius: 2, flexShrink: 0 }}
        />
      )}
      <span
        style={{
          overflow: "hidden",
          textOverflow: "ellipsis",
          whiteSpace: "nowrap",
        }}
      >
        {source.domain}
      </span>
    </a>
  );
}

// -----------------------------------------------------------------------------

interface TimelineStepProps {
  step: TimelineStepData;
  /** This step is actively running (spinning dot marker). */
  isRunning?: boolean;
  /** This step has finished successfully (check-mark marker). */
  isCompleted?: boolean;
  /** This step errored (red marker). */
  isError?: boolean;
}

export function TimelineStep({
  step,
  isRunning,
  isCompleted,
  isError,
}: TimelineStepProps) {
  const { t } = useTranslation();

  const toolName = step.kind === "tool" ? step.toolName : undefined;
  const queryArg = step.kind === "tool" ? step.queryArg : undefined;
  const sources = step.kind === "tool" ? step.sources : undefined;
  const duration = step.kind === "tool" ? formatDuration(step.durationMs) : null;
  const glyphName =
    step.kind === "tool"
      ? iconNameForTool(step.icon, toolName)
      : "brain";

  // Marker styling per state. Error uses the global destructive token
  // so the single-accent rule (gold) stays intact and the red signal is
  // reserved for real failure.
  const markerBg = isRunning
    ? T.accentSoft
    : isError
      ? "hsl(var(--destructive) / 0.15)"
      : T.panel;
  const markerBorder = isRunning
    ? T.accent
    : isError
      ? "hsl(var(--destructive))"
      : T.border;

  return (
    <div
      className="act-step-new"
      style={{ position: "relative", marginBottom: 14 }}
    >
      {/* Circle marker on the rail */}
      <div
        style={{
          position: "absolute",
          left: -18,
          top: 3,
          width: 11,
          height: 11,
          borderRadius: 6,
          background: markerBg,
          border: `1.5px solid ${markerBorder}`,
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
        }}
      >
        {isRunning && <span className="act-running-dot" />}
        {!isRunning && isCompleted && (
          <span style={{ color: T.textMute, display: "inline-flex" }}>
            <Icon name="check" size={7} stroke={3} />
          </span>
        )}
      </div>

      {step.kind === "thinking" ? (
        <div>
          <div
            style={{
              fontSize: 13.5,
              fontWeight: 600,
              color: T.text,
              marginBottom: 4,
              fontFamily: ui.sans,
            }}
          >
            {step.title}
          </div>
          {step.body && (
            // The thinking body is markdown — Gemini's thought summaries
            // come back with ``**Bold Heading**`` and bullet lists, and
            // Qwen's reasoning_content is free-form markdown too. Render
            // via ReactMarkdown so headings/emphasis/lists look right
            // instead of showing literal ``**`` and ``##``. Kept scoped
            // to prose elements — no images, no links in thought bodies.
            <div
              className="act-thinking-md"
              style={{
                fontSize: 12.5,
                color: T.textMute,
                lineHeight: 1.55,
                fontFamily: ui.sans,
              }}
            >
              <ReactMarkdown
                remarkPlugins={[remarkGfm]}
                components={{
                  // Tighten paragraph spacing — defaults push too much air.
                  p: ({ children }) => (
                    <p style={{ margin: "4px 0" }}>{children}</p>
                  ),
                  ul: ({ children }) => (
                    <ul style={{ margin: "4px 0", paddingLeft: 18 }}>{children}</ul>
                  ),
                  ol: ({ children }) => (
                    <ol style={{ margin: "4px 0", paddingLeft: 18 }}>{children}</ol>
                  ),
                  li: ({ children }) => (
                    <li style={{ margin: "2px 0" }}>{children}</li>
                  ),
                  h1: ({ children }) => (
                    <div style={{ fontWeight: 600, margin: "6px 0 2px" }}>
                      {children}
                    </div>
                  ),
                  h2: ({ children }) => (
                    <div style={{ fontWeight: 600, margin: "6px 0 2px" }}>
                      {children}
                    </div>
                  ),
                  h3: ({ children }) => (
                    <div style={{ fontWeight: 600, margin: "6px 0 2px" }}>
                      {children}
                    </div>
                  ),
                  code: ({ children }) => (
                    <code
                      style={{
                        fontFamily: ui.mono,
                        fontSize: "0.9em",
                        background: T.bgSoft,
                        padding: "1px 4px",
                        borderRadius: 3,
                      }}
                    >
                      {children}
                    </code>
                  ),
                  strong: ({ children }) => (
                    <strong style={{ color: T.text }}>{children}</strong>
                  ),
                }}
              >
                {step.body}
              </ReactMarkdown>
              {isRunning && <span className="act-caret" />}
            </div>
          )}
        </div>
      ) : (
        <div>
          {/* Tool name row */}
          <div
            style={{
              display: "flex",
              alignItems: "center",
              gap: 6,
              fontSize: 13.5,
              color: T.text,
              marginBottom: 4,
              fontFamily: ui.sans,
            }}
          >
            <span
              style={{
                color: T.textMute,
                display: "inline-flex",
                flexShrink: 0,
              }}
            >
              <Icon name={glyphName} size={12} />
            </span>
            <span
              style={{
                fontFamily: ui.mono,
                fontWeight: 500,
                overflow: "hidden",
                textOverflow: "ellipsis",
                whiteSpace: "nowrap",
                minWidth: 0,
              }}
            >
              {toolName || step.title}
            </span>
            {duration && (
              <span
                style={{
                  fontFamily: ui.mono,
                  fontSize: 11,
                  color: T.textDim,
                  marginLeft: "auto",
                  flexShrink: 0,
                }}
              >
                {duration}
              </span>
            )}
          </div>

          {/* Query / URL / path code block */}
          {queryArg && (
            <div
              style={{
                padding: "6px 8px",
                background: T.bgSoft,
                borderRadius: 6,
                fontSize: 12,
                fontFamily: ui.mono,
                color: T.textMute,
                marginBottom: 6,
                overflow: "hidden",
                textOverflow: "ellipsis",
                whiteSpace: "nowrap",
              }}
              title={queryArg}
            >
              "{queryArg}"
            </div>
          )}

          {/* Fallback body prose if no queryArg */}
          {!queryArg && step.body && (
            <div
              style={{
                fontSize: 12,
                color: T.textMute,
                lineHeight: 1.55,
                marginBottom: 6,
                display: "-webkit-box",
                WebkitLineClamp: 3,
                WebkitBoxOrient: "vertical",
                overflow: "hidden",
              }}
            >
              {step.body}
            </div>
          )}

          {/* Source chips */}
          {sources && sources.length > 0 && (
            <div
              style={{
                display: "flex",
                flexWrap: "wrap",
                gap: 4,
              }}
            >
              {sources.map((src, i) => (
                <SourceChip key={`${src.url}-${i}`} source={src} />
              ))}
            </div>
          )}

          {isError && !step.body && (
            <div
              style={{
                fontSize: 11.5,
                color: "hsl(var(--destructive))",
                marginTop: 4,
              }}
            >
              {t("playground.activity.errorFallback", "Step failed")}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
