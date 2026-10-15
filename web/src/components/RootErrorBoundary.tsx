import { Component, type ReactNode } from "react";

type Props = { children: ReactNode };
type State = { error: Error | null; info: string | null };

/**
 * Root ErrorBoundary — catches any uncaught render / lifecycle error
 * in the React tree and shows a friendly "something broke" panel with
 * a reload button instead of a silent white screen.
 *
 * Pairs with the boot-timeout watchdog in index.html:
 *   - watchdog   → "bundle failed to download"  (network level)
 *   - this class → "bundle loaded but React threw"  (runtime level)
 *
 * Only installed at the app root; route-level pages can still throw
 * their own errors and be caught here.
 */
export class RootErrorBoundary extends Component<Props, State> {
  state: State = { error: null, info: null };

  static getDerivedStateFromError(error: Error): State {
    return { error, info: null };
  }

  componentDidCatch(error: Error, info: { componentStack?: string }) {
    // Console only — no remote telemetry here to avoid a second failure
    // path when the user is already having a bad time.
    console.error("[RootErrorBoundary]", error, info);
    this.setState({ error, info: info.componentStack ?? null });
  }

  handleReload = () => {
    window.location.reload();
  };

  handleReset = () => {
    // Clear persisted state + reload. Matches the `?reset=true` path
    // the emergency reset script in index.html already supports, but
    // triggered by a button so users don't need to know about it.
    try {
      localStorage.removeItem("agent-gateway-storage");
    } catch {
      // localStorage may be disabled; best-effort only
    }
    window.location.href = window.location.pathname;
  };

  render() {
    if (!this.state.error) return this.props.children;

    const isDark = document.documentElement.classList.contains("dark");
    const bg = isDark ? "#0f1013" : "#f7f7f9";
    const fg = isDark ? "#f4f4f5" : "#17181c";
    const sub = isDark ? "#acadb5" : "#62656e";
    const border = isDark ? "#34353e" : "#dcdde2";

    return (
      <div
        style={{
          position: "fixed",
          inset: 0,
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          fontFamily: "system-ui, -apple-system, 'Segoe UI', sans-serif",
          background: bg,
          color: fg,
          padding: 24,
        }}
      >
        <div
          style={{
            maxWidth: 420,
            width: "100%",
            textAlign: "center",
            padding: 24,
            borderRadius: 12,
            border: `1px solid ${border}`,
          }}
        >
          <div style={{ fontWeight: 600, fontSize: 16, marginBottom: 8 }}>
            Something went wrong
          </div>
          <div style={{ fontSize: 13, lineHeight: 1.6, color: sub, marginBottom: 18 }}>
            The app hit an unexpected error while rendering. Your data is safe —
            reloading usually fixes it. If the problem keeps happening, try
            "Reset app state" to clear cached UI state.
          </div>
          <div style={{ display: "flex", gap: 10, justifyContent: "center" }}>
            <button
              onClick={this.handleReload}
              style={{
                padding: "8px 18px",
                borderRadius: 8,
                border: "none",
                background: "#5b5bd6",
                color: "#fff",
                fontSize: 13,
                fontWeight: 500,
                cursor: "pointer",
              }}
            >
              Reload
            </button>
            <button
              onClick={this.handleReset}
              style={{
                padding: "8px 18px",
                borderRadius: 8,
                border: `1px solid ${border}`,
                background: "transparent",
                color: fg,
                fontSize: 13,
                fontWeight: 500,
                cursor: "pointer",
              }}
            >
              Reset app state
            </button>
          </div>
          {import.meta.env.DEV && this.state.error && (
            <pre
              style={{
                textAlign: "left",
                fontSize: 11,
                lineHeight: 1.5,
                color: sub,
                marginTop: 18,
                padding: 12,
                borderRadius: 6,
                background: isDark ? "#202128" : "#f0f1f4",
                overflow: "auto",
                maxHeight: 180,
              }}
            >
              {this.state.error.message}
              {this.state.info && "\n" + this.state.info.slice(0, 600)}
            </pre>
          )}
        </div>
      </div>
    );
  }
}
