/**
 * ActivityTimeline — the vertical rail + circle markers container.
 *
 * Ported from chat.jsx `ActivityBody`: a continuous 1px rail on the
 * left, with absolute-positioned 11px circle markers at each step.
 * Each row derives its state from its own lifecycle. This keeps tool markers
 * correct when the separately-streaming reasoning row is rendered last.
 *
 * Step rendering delegates to `TimelineStep`.
 */

import { useState } from "react";
import { useTranslation } from "react-i18next";
import type { ChatMessage as ChatMessageType } from "../types";
import { TimelineStep, type TimelineStepData } from "./TimelineStep";
import { T } from "./activityTheme";
import { buildTimeline } from "./buildTimeline";

const ACTIVITY_PAGE_SIZE = 50;

interface ActivityTimelineProps {
  /** Pre-built steps. Prefer this in the drawer (parent already builds). */
  steps?: TimelineStepData[];
  /** Fallback: build from a ChatMessage. Used by share-page / legacy. */
  message?: ChatMessageType;
  /** Whether the last step is actively running. */
  running?: boolean;
  className?: string;
}

export function ActivityTimeline({
  steps: stepsProp,
  message,
  running,
  className,
}: ActivityTimelineProps) {
  const { t } = useTranslation();
  const steps: TimelineStepData[] =
    stepsProp ?? (message ? buildTimeline(message, t).steps : []);

  const latestStart = Math.max(0, steps.length - ACTIVITY_PAGE_SIZE);
  // Keep the first visible identity stable, including while reading the initial
  // latest page. New activity becomes visible only through explicit navigation.
  const [selection, setSelection] = useState(() => ({
    scope: message?.id, startId: steps[latestStart]?.id,
  }));
  const selectedStart = selection.scope === message?.id
    ? steps.findIndex((step) => step.id === selection.startId) : -1;
  if (selection.scope !== message?.id || (steps.length > 0 && selectedStart < 0)) {
    // Message changes or replaced history cannot revive an unrelated old page.
    setSelection({ scope: message?.id, startId: steps[latestStart]?.id });
  }
  const start = selectedStart >= 0 ? selectedStart : latestStart;
  const end = Math.min(start + ACTIVITY_PAGE_SIZE, steps.length);
  const visibleSteps = steps.slice(start, end);
  const selectPage = (offset: number) => {
    const step = steps[offset];
    if (step) setSelection({ scope: message?.id, startId: step.id });
  };

  if (steps.length === 0) {
    return (
      <div className={className}>
        <p style={{ fontSize: 13, color: T.textMute }}>
          {t("playground.activity.empty", {
            defaultValue: "No activity recorded.",
          })}
        </p>
      </div>
    );
  }

  return (
    <div
      className={className}
      style={{ position: "relative", paddingLeft: 18 }}
    >
      {steps.length > ACTIVITY_PAGE_SIZE && (
        <nav aria-label={t("assistant.activity.navigation")} style={{ position: "relative", marginLeft: -18, marginBottom: 16, background: T.bg }}>
          <p style={{ fontSize: 11, color: T.textMute, marginBottom: 8 }}>
            {t("assistant.activity.loadedRange", { start: start + 1, end, count: steps.length })}
          </p>
          <div style={{ display: "flex", flexWrap: "wrap", gap: 8 }}>
            <button type="button" className="act-btn act-hover rounded-md border px-2 py-1 text-xs disabled:cursor-default disabled:opacity-50" disabled={start === 0} onClick={() => selectPage(Math.max(0, start - ACTIVITY_PAGE_SIZE))}>{t("assistant.activity.earlier")}</button>
            <button type="button" className="act-btn act-hover rounded-md border px-2 py-1 text-xs disabled:cursor-default disabled:opacity-50" disabled={end === steps.length} onClick={() => selectPage(end)}>{t("assistant.activity.later")}</button>
            <button type="button" className="act-btn act-hover rounded-md border px-2 py-1 text-xs disabled:cursor-default disabled:opacity-50" disabled={start === latestStart} onClick={() => selectPage(latestStart)}>{t("assistant.activity.latest")}</button>
          </div>
        </nav>
      )}
      <div style={{ position: "relative" }}>
        {/* Continuous vertical rail */}
        <div
          style={{
            position: "absolute",
            left: -13,
            top: 6,
            bottom: 20,
            width: 1,
            background: T.border,
          }}
        />
        {visibleSteps.map((step) => {
          const stepStatus =
            step.kind === "tool"
              ? step.status
              : step.streaming
                ? "running"
                : "completed";
          const isRunning = !!running && stepStatus === "running";
          const isCompleted = stepStatus === "completed";
          const isError = stepStatus === "error";

          return (
            <TimelineStep
              key={step.id}
              step={step}
              isRunning={isRunning}
              isCompleted={isCompleted}
              isError={isError}
            />
          );
        })}
      </div>
    </div>
  );
}
