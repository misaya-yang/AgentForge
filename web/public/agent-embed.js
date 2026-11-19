const root = document.getElementById("agent-embed-root");

if (root) {
  const publicId = root.dataset.publicId || "";
  const parentOrigin = root.dataset.parentOrigin || "";
  const embedToken = root.dataset.embedToken || "";
  const embedHeaders = {
    "Content-Type": "application/json",
    "X-Agent-Embed-Token": embedToken,
    "X-Agent-Embed-Origin": parentOrigin,
  };
  const protocol = "agent-embed/v1";
  root.removeAttribute("data-embed-token");
  let config = null;
  let sessionId = null;
  let streaming = false;

  const post = (type, payload = {}) => {
    if (window.parent !== window && parentOrigin) {
      window.parent.postMessage({ version: protocol, type, payload }, parentOrigin);
    }
  };

  const safeText = (value) => String(value == null ? "" : value);
  const element = (tag, className, text) => {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined) node.textContent = safeText(text);
    return node;
  };

  const shell = element("main", "ae-shell");
  const header = element("header", "ae-header");
  const brand = element("div", "ae-brand");
  const mark = element("span", "ae-mark", "A");
  const title = element("div", "ae-title");
  const titleName = element("strong", "", "Agent");
  const titleDescription = element("small", "", "Secure embedded chat");
  const close = element("button", "ae-icon-button", "×");
  close.type = "button";
  close.setAttribute("aria-label", "Close chat");
  close.addEventListener("click", () => post("close"));
  title.append(titleName, titleDescription);
  brand.append(mark, title);
  header.append(brand, close);

  const chat = element("section", "ae-chat");
  chat.setAttribute("aria-label", "Agent conversation");
  const error = element("p", "ae-error");
  error.hidden = true;
  error.setAttribute("role", "alert");
  const composer = element("footer", "ae-composer");
  const form = element("form", "ae-form");
  const input = element("textarea");
  input.rows = 1;
  input.placeholder = "Message this agent";
  input.setAttribute("aria-label", "Message this agent");
  const send = element("button", "ae-send", "↑");
  send.type = "submit";
  send.disabled = true;
  send.setAttribute("aria-label", "Send message");
  const status = element("p", "ae-status", "Origin-isolated agent");
  form.append(input, send);
  composer.append(form, status);
  shell.append(header, chat, error, composer);
  root.append(shell);

  const showError = (message) => {
    error.textContent = safeText(message || "The agent is unavailable.");
    error.hidden = false;
    post("error", { code: "AGENT_EMBED_RUNTIME_ERROR" });
  };

  const renderWelcome = () => {
    chat.replaceChildren();
    const welcome = element("div", "ae-welcome");
    const welcomeMark = element("span", "ae-mark", "A");
    const heading = element(
      "h1",
      "",
      config?.identity?.welcome_message || `How can ${config?.name || "this agent"} help?`,
    );
    welcome.append(welcomeMark, heading);
    const prompts = Array.isArray(config?.identity?.suggested_prompts)
      ? config.identity.suggested_prompts.slice(0, 3)
      : [];
    if (prompts.length) {
      const suggestions = element("div", "ae-suggestions");
      prompts.forEach((prompt) => {
        const button = element("button", "", prompt);
        button.type = "button";
        button.addEventListener("click", () => void sendMessage(prompt));
        suggestions.append(button);
      });
      welcome.append(suggestions);
    }
    chat.append(welcome);
  };

  const ensureMessages = () => {
    let messages = chat.querySelector(".ae-messages");
    if (!messages) {
      messages = element("div", "ae-messages");
      messages.setAttribute("aria-live", "polite");
      chat.replaceChildren(messages);
    }
    return messages;
  };

  const appendMessage = (role, content = "") => {
    const node = element("div", `ae-message ${role}`, content);
    ensureMessages().append(node);
    chat.scrollTop = chat.scrollHeight;
    return node;
  };

  const readSse = async (response, onData) => {
    if (!response.ok || !response.body) throw new Error(`Request failed (${response.status})`);
    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    let buffer = "";
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true }).replace(/\r\n/g, "\n");
      const parts = buffer.split("\n\n");
      buffer = parts.pop() || "";
      parts.forEach((part) => {
        const raw = part.split("\n").filter((line) => line.startsWith("data:"))
          .map((line) => line.slice(5).trimStart()).join("\n");
        if (!raw || raw === "[DONE]") return;
        try { onData(JSON.parse(raw)); } catch { /* malformed upstream event is ignored */ }
      });
    }
  };

  const sendMessage = async (raw) => {
    const message = safeText(raw).trim();
    if (!message || streaming) return;
    streaming = true;
    input.value = "";
    send.disabled = true;
    error.hidden = true;
    appendMessage("user", message);
    const answer = appendMessage("assistant", "");
    try {
      if (!sessionId) {
        const sessionResponse = await fetch(`/api/v1/public/agents/${encodeURIComponent(publicId)}/sessions`, {
          method: "POST",
          credentials: "include",
          headers: embedHeaders,
          body: JSON.stringify({ channel: "embed" }),
        });
        if (!sessionResponse.ok) throw new Error(`Session failed (${sessionResponse.status})`);
        sessionId = (await sessionResponse.json()).session_id;
      }
      const response = await fetch(`/api/v1/public/agents/${encodeURIComponent(publicId)}/chat/stream`, {
        method: "POST",
        credentials: "include",
        headers: embedHeaders,
        body: JSON.stringify({ channel: "embed", session_id: sessionId, message, attachments: [] }),
      });
      // Terminal runtime failures arrive as {event_type:"error"|"run_error",
      // data:{message}}. readSse swallows exceptions thrown inside onData, so
      // capture the message here and raise it after the stream completes; the
      // catch below then drops the partial bubble, shows the error banner and
      // posts {type:"error"} to the parent (matching the hosted page).
      let streamError = null;
      await readSse(response, (event) => {
        const type = String(event?.event_type ?? "").toLowerCase();
        if (type === "error" || type === "run_error") {
          streamError = event?.data?.message || "The agent is unavailable.";
          return;
        }
        const delta = typeof event.content === "string"
          ? event.content
          : typeof event.data === "string"
            ? event.data
            : typeof event.data?.content === "string" ? event.data.content : "";
        if (delta) answer.textContent += delta;
      });
      if (streamError) throw new Error(streamError);
      if (!answer.textContent) answer.textContent = "The agent completed without a text response.";
      post("new_message", { role: "assistant" });
    } catch (cause) {
      answer.remove();
      showError(cause instanceof Error ? cause.message : "The response could not be completed.");
    } finally {
      streaming = false;
      send.disabled = !input.value.trim();
    }
  };

  form.addEventListener("submit", (event) => {
    event.preventDefault();
    void sendMessage(input.value);
  });
  input.addEventListener("input", () => { send.disabled = streaming || !input.value.trim(); });
  input.addEventListener("keydown", (event) => {
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      void sendMessage(input.value);
    }
  });

  window.addEventListener("message", (event) => {
    if (event.source !== window.parent || event.origin !== parentOrigin) return;
    const data = event.data;
    if (!data || data.version !== protocol || typeof data.type !== "string") return;
    if (data.type === "focus" || data.type === "open") input.focus();
    if (data.type === "new_message" && typeof data.payload?.message === "string") {
      void sendMessage(data.payload.message);
    }
  });

  new ResizeObserver(() => post("resize", { height: document.documentElement.scrollHeight }))
    .observe(document.documentElement);

  fetch(`/api/v1/public/agents/${encodeURIComponent(publicId)}?channel=embed`, {
    credentials: "include",
    headers: { "X-Agent-Embed-Token": embedToken, "X-Agent-Embed-Origin": parentOrigin },
  })
    .then((response) => {
      if (!response.ok) throw new Error(`Agent unavailable (${response.status})`);
      return response.json();
    })
    .then((value) => {
      config = value;
      titleName.textContent = safeText(value.name || "Agent");
      titleDescription.textContent = safeText(value.description || "Secure embedded chat");
      const accent = /^#[0-9a-f]{6}$/i.test(value.identity?.theme_color || "")
        ? value.identity.theme_color : "#635bff";
      shell.style.setProperty("--ae-accent", accent);
      renderWelcome();
      send.disabled = !input.value.trim();
      post("ready", { protocol });
    })
    .catch((cause) => showError(cause instanceof Error ? cause.message : "Agent unavailable"));
}
