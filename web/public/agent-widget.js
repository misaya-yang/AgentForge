(() => {
  const protocol = "agent-embed/v1";
  const script = document.currentScript;
  if (!(script instanceof HTMLScriptElement)) return;
  const publicId = script.dataset.agentId || "";
  if (!/^[0-9a-f-]{36}$/i.test(publicId)) return;
  const gatewayOrigin = new URL(script.src, window.location.href).origin;
  const mode = script.dataset.mode === "inline" ? "inline" : "launcher";
  const label = script.dataset.label || "Chat with us";
  const container = script.dataset.container
    ? document.querySelector(script.dataset.container)
    : null;
  const mount = container || document.body;
  const iframe = document.createElement("iframe");
  iframe.src = `${gatewayOrigin}/embed/agents/${encodeURIComponent(publicId)}?mode=${mode}`;
  iframe.title = label;
  iframe.sandbox = "allow-scripts allow-forms allow-same-origin";
  iframe.allow = "clipboard-write";
  iframe.referrerPolicy = "strict-origin-when-cross-origin";
  iframe.style.border = "0";
  iframe.style.background = "transparent";

  let launcher = null;
  let open = mode === "inline";
  const setOpen = (next) => {
    open = next;
    iframe.hidden = !open;
    if (launcher) launcher.setAttribute("aria-expanded", String(open));
    if (open) {
      iframe.contentWindow?.postMessage({ version: protocol, type: "focus", payload: {} }, gatewayOrigin);
    } else {
      launcher?.focus();
    }
  };

  if (mode === "inline") {
    iframe.style.width = "100%";
    iframe.style.minHeight = script.dataset.height || "560px";
    mount.append(iframe);
  } else {
    const panel = document.createElement("div");
    panel.style.position = "fixed";
    panel.style.right = "20px";
    panel.style.bottom = "82px";
    panel.style.zIndex = "2147483000";
    panel.style.width = "min(390px, calc(100vw - 24px))";
    panel.style.height = "min(650px, calc(100dvh - 110px))";
    panel.style.borderRadius = "18px";
    panel.style.overflow = "hidden";
    panel.style.boxShadow = "0 24px 70px rgba(20, 28, 45, .22)";
    iframe.style.width = "100%";
    iframe.style.height = "100%";
    iframe.hidden = true;
    panel.append(iframe);
    launcher = document.createElement("button");
    launcher.type = "button";
    launcher.textContent = label;
    launcher.setAttribute("aria-expanded", "false");
    launcher.style.position = "fixed";
    launcher.style.right = "20px";
    launcher.style.bottom = "20px";
    launcher.style.zIndex = "2147483001";
    launcher.style.minHeight = "48px";
    launcher.style.padding = "0 18px";
    launcher.style.border = "0";
    launcher.style.borderRadius = "999px";
    launcher.style.color = "white";
    launcher.style.background = script.dataset.color || "#635bff";
    launcher.style.font = "600 14px Inter, system-ui, sans-serif";
    launcher.style.cursor = "pointer";
    launcher.style.boxShadow = "0 12px 30px rgba(20, 28, 45, .2)";
    launcher.addEventListener("click", () => setOpen(!open));
    mount.append(panel, launcher);
  }

  window.addEventListener("message", (event) => {
    if (event.origin !== gatewayOrigin || event.source !== iframe.contentWindow) return;
    const data = event.data;
    if (!data || data.version !== protocol || typeof data.type !== "string") return;
    if (data.type === "close" && mode === "launcher") setOpen(false);
    if (data.type === "resize" && mode === "inline") {
      const height = Number(data.payload?.height);
      if (Number.isFinite(height)) iframe.style.height = `${Math.max(320, Math.min(height, 1200))}px`;
    }
  });
})();
