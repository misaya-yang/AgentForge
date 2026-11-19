/**
 * RightPanelContext — shared "which right sheet is open?" state.
 *
 * The assistant page has mutually-exclusive right-side sheets:
 *   - "artifacts"  — the generated files panel (ArtifactsPanel)
 *   - "activity"   — the Claude-design Activity drawer (ActivityPanel)
 *   - "subagents"  — delegated-agent lifecycle and receipt workbench
 *   - "local_os"   — paired Local Node status and read-only receipts
 *
 * Never both at once. This context lets deep descendants (e.g. the
 * ActivityPill inside a ChatMessage) request the panel change without
 * prop-drilling; the page owns the actual state.
 *
 * The page component is also the only writer to `setShowArtifacts` in
 * useChatSession, so we mirror artifacts open/close here.
 */

import { createContext, useContext } from "react";

export type RightPanel = "activity" | "artifacts" | "subagents" | "local_os" | null;

export interface RightPanelState {
  rightPanel: RightPanel;
  /** Message ID whose activity is currently displayed (when rightPanel==="activity"). */
  activityMessageId: string | null;
  /** Message ID whose child-agent workbench is displayed. */
  subagentMessageId: string | null;
  openActivity: (messageId: string) => void;
  closeActivity: () => void;
  openSubagents: (messageId: string) => void;
  closeSubagents: () => void;
}

export const RightPanelContext = createContext<RightPanelState | null>(null);

export function useRightPanel(): RightPanelState {
  const ctx = useContext(RightPanelContext);
  if (!ctx) {
    // Defensive fallback: return a no-op instance so the components still
    // render outside the page (e.g. in a storybook or share-page preview).
    return {
      rightPanel: null,
      activityMessageId: null,
      subagentMessageId: null,
      openActivity: () => {},
      closeActivity: () => {},
      openSubagents: () => {},
      closeSubagents: () => {},
    };
  }
  return ctx;
}
