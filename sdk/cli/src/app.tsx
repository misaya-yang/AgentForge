/**
 * Ink main application — v2.0
 * Full SSE event rendering, Markdown output, token display, stream cancellation.
 */

import React, { useState, useCallback, useRef } from "react";
import { Box, Text, useApp, useInput } from "ink";
import TextInput from "ink-text-input";
import type { CLIConfig } from "./types/config.js";
import { EventType, type StreamEvent } from "./types/events.js";
import { GatewayClient } from "./remote/client.js";
import { GatewaySessionClient } from "./remote/session_client.js";
import { ChatModule } from "./remote/chat.js";
import { parseSlashCommand } from "./agent/slash-commands.js";
import { saveMemory, listMemories } from "./memory.js";
import { MCPManager } from "./mcp/manager.js";
import {
  listSkills,
  uploadSkill,
  createAndUploadSkill,
  toggleSkill,
  deleteSkill,
  testSkill,
} from "./skills.js";
import { loadProjectMemory } from "./project-memory.js";
import { listAgentsSummary, getAgent } from "./agents.js";
import { writeFileSync } from "node:fs";
import { CLI_VERSION } from "./version.js";

// ─── Types ───────────────────────────────────────────────────────────

interface Message {
  role: "user" | "assistant" | "system";
  content: string;
  tools?: ToolDisplay[];
  agents?: AgentDisplay[];
  meta?: MessageMeta;
}

interface MessageMeta {
  tokens?: { input: number; output: number };
  durationMs?: number;
  thinkingText?: string;
  ragContext?: string[];
  webResults?: string[];
  artifacts?: ArtifactInfo[];
}

interface ArtifactInfo {
  type: string;
  title: string;
  format?: string;
  url?: string;
}

interface ToolDisplay {
  name: string;
  status: "running" | "done" | "error" | "denied";
  result?: string;
  durationMs?: number;
}

interface AgentDisplay {
  id: string;
  type: "explore" | "task" | "plan" | string;
  description: string;
  status: "running" | "done" | "failed";
  tools: Array<{ name: string; status: string; summary?: string }>;
  streamingText: string;
  resultSummary: string;
  durationMs?: number;
}

interface ConfirmState {
  summary: string;
  toolName: string;
  resolve: (approved: boolean) => void;
}

// ─── Main App ────────────────────────────────────────────────────────

export function App({ config }: { config: CLIConfig }) {
  const { exit } = useApp();
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);

  // Streaming state
  const [streamText, setStreamText] = useState("");
  const [streamTools, setStreamTools] = useState<ToolDisplay[]>([]);
  const [streamAgents, setStreamAgents] = useState<AgentDisplay[]>([]);
  const [statusMsg, setStatusMsg] = useState("");
  const [thinkingText, setThinkingText] = useState("");
  const [tokenInfo, setTokenInfo] = useState("");
  const [ragDocs, setRagDocs] = useState<string[]>([]);
  const [webResults, setWebResults] = useState<string[]>([]);
  const [artifacts, setArtifacts] = useState<ArtifactInfo[]>([]);

  // Confirmation prompt
  const [confirm, setConfirm] = useState<ConfirmState | null>(null);

  // Session state
  const [sessionId, setSessionId] = useState<string | undefined>();
  const [model, setModel] = useState(config.default_model);
  const [kbIds, setKbIds] = useState<string[]>(config.kb_dataset_ids ?? []);

  // Stream cancellation
  const cancelledRef = useRef(false);

  // Refs for final commit
  const streamTextRef = useRef("");
  const completedRef = useRef(false);
  const streamToolsRef = useRef<ToolDisplay[]>([]);
  const streamAgentsRef = useRef<AgentDisplay[]>([]);
  const metaRef = useRef<MessageMeta>({});

  // Command history
  const [history] = useState<string[]>([]);
  const [historyIdx, setHistoryIdx] = useState(-1);

  const client = React.useMemo(() => new GatewayClient(config), [config]);
  const sessionClient = React.useMemo(
    () => new GatewaySessionClient(config.base_url, process.env.AI_GATEWAY_SESSION_TOKEN, config.timeout), [config],
  );
  const chat = React.useMemo(() => new ChatModule(client), [client]);
  const mcpManager = React.useMemo(() => new MCPManager(), []);

  // Load project memory once
  const projectMemory = React.useMemo(() => loadProjectMemory(), []);

  // ─── Confirmation handler ──────────────────────────────────────

  const handleConfirm = useCallback(
    (summary: string, toolName: string): Promise<boolean> => {
      return new Promise((resolve) => {
        setConfirm({ summary, toolName, resolve });
      });
    },
    [],
  );

  // ─── SSE event handler (Phase A: ALL events) ──────────────────

  const handleEvent = useCallback((event: StreamEvent) => {
    switch (event.eventType) {
      case EventType.SESSION_CREATED:
        setSessionId(event.data.session_id);
        break;

      // ── Text streaming ──
      case EventType.TEXT_DELTA: {
        const text = event.data.content ?? event.data.text ?? "";
        streamTextRef.current += text;
        setStreamText(streamTextRef.current);
        break;
      }

      // ── Thinking/reasoning ──
      case EventType.THINKING_DELTA: {
        const text = event.data.content ?? event.data.text ?? "";
        setThinkingText((t) => t + text);
        break;
      }
      case EventType.THINKING_END:
        // Thinking complete — keep display until response finishes
        break;

      // ── Status ──
      case EventType.STATUS:
        setStatusMsg(event.data.message ?? event.data.status ?? "");
        break;

      // ── RAG / Knowledge Base ──
      case EventType.CONTEXT_RETRIEVED: {
        const docs = event.data.documents ?? event.data.chunks ?? [];
        const summaries = docs.map((d: any) =>
          `${d.title ?? d.source ?? "doc"}: ${(d.content ?? d.text ?? "").slice(0, 100)}...`
        );
        setRagDocs(summaries);
        break;
      }
      case EventType.RAG_EVALUATION:
        setStatusMsg(`RAG: ${event.data.relevant_count ?? 0} relevant / ${event.data.total_count ?? 0} retrieved`);
        break;

      // ── Web search ──
      case EventType.WEB_SEARCH_RESULTS: {
        const results = event.data.results ?? [];
        setWebResults(results.map((r: any) => `${r.title ?? ""}: ${r.url ?? ""}`).slice(0, 5));
        break;
      }
      case EventType.SEARCH_STARTED:
        setStatusMsg(`Searching: ${event.data.query ?? ""}...`);
        break;
      case EventType.SEARCH_COMPLETED:
        setStatusMsg("");
        break;

      // ── Tool lifecycle ──
      case EventType.TOOL_CALL_START:
        setStreamTools((tools) => {
          const updated = [...tools, {
            name: event.data.tool_name ?? event.data.name ?? "unknown",
            status: "running" as const,
          }];
          streamToolsRef.current = updated;
          return updated;
        });
        break;

      case EventType.TOOL_CALL_RESULT:
      case EventType.TOOL_RESULT:
        setStreamTools((tools) => {
          const updated = [...tools];
          const last = updated.findLast((t) => t.status === "running");
          if (last) {
            last.status = "done";
            last.result = truncate(typeof event.data.result === "string" ? event.data.result : JSON.stringify(event.data.result ?? ""), 200);
            last.durationMs = event.data.duration_ms;
          }
          streamToolsRef.current = updated;
          return updated;
        });
        break;

      case EventType.TOOL_ERROR:
        setStreamTools((tools) => {
          const updated = [...tools];
          const last = updated.findLast((t) => t.status === "running");
          if (last) { last.status = "error"; last.result = event.data.error ?? "Tool error"; }
          streamToolsRef.current = updated;
          return updated;
        });
        break;

      // ── Artifacts ──
      case EventType.ARTIFACT_CREATED:
        setArtifacts((a) => [...a, {
          type: event.data.type ?? "file",
          title: event.data.title ?? event.data.filename ?? "artifact",
          format: event.data.format,
          url: event.data.url ?? event.data.download_url,
        }]);
        break;

      case EventType.IMAGE_GENERATION_START:
        setStatusMsg("Generating image...");
        break;
      case EventType.IMAGE_GENERATION_RESULT:
        setArtifacts((a) => [...a, {
          type: "image",
          title: event.data.title ?? "Generated Image",
          format: event.data.format ?? "png",
          url: event.data.url ?? event.data.image_url,
        }]);
        setStatusMsg("");
        break;

      case EventType.DOCUMENT_GENERATION_START:
        setStatusMsg("Generating document...");
        break;
      case EventType.DOCUMENT_GENERATION_RESULT:
        setArtifacts((a) => [...a, {
          type: "document",
          title: event.data.title ?? "Document",
          format: event.data.format ?? "docx",
          url: event.data.url ?? event.data.download_url,
        }]);
        setStatusMsg("");
        break;

      // ── Code execution ──
      case EventType.CODE_EXECUTION_START:
        setStatusMsg("Executing code...");
        break;
      case EventType.CODE_EXECUTION_OUTPUT:
      case EventType.CODE_EXECUTION_RESULT:
        setStatusMsg("");
        break;

      // ── Usage / tokens ──
      case EventType.USAGE:
        setTokenInfo(`${event.data.input_tokens ?? 0} → ${event.data.output_tokens ?? 0} tokens`);
        metaRef.current.tokens = { input: event.data.input_tokens ?? 0, output: event.data.output_tokens ?? 0 };
        break;

      // ── Run lifecycle ──
      case EventType.RUN_STARTED:
        setStatusMsg("Processing...");
        break;
      case EventType.RUN_FINISHED:
        metaRef.current.durationMs = event.data.metadata?.duration_ms ?? event.data.duration_ms;
        completedRef.current = true;
        break;
      case EventType.DONE:
        completedRef.current = true;
        break;

      // ── Sub-Agents ──
      case EventType.SUBAGENT_STARTED:
        setStreamAgents((agents) => {
          const updated = [...agents, {
            id: event.data.agent_id ?? "",
            type: event.data.agent_type ?? "explore",
            description: event.data.description ?? "",
            status: "running" as const,
            tools: [], streamingText: "", resultSummary: "",
          }];
          streamAgentsRef.current = updated;
          return updated;
        });
        break;
      case EventType.SUBAGENT_TOOL_START:
        setStreamAgents((agents) => {
          const updated = agents.map((a) =>
            a.id === event.data.agent_id
              ? { ...a, tools: [...a.tools, { name: event.data.tool_name ?? "", status: "running", summary: undefined }] }
              : a
          );
          streamAgentsRef.current = updated;
          return updated;
        });
        break;
      case EventType.SUBAGENT_TOOL_RESULT:
        setStreamAgents((agents) => {
          const updated = agents.map((a) => {
            if (a.id !== event.data.agent_id) return a;
            const tools = [...a.tools];
            const last = tools.findLast((t) => t.status === "running");
            if (last) { last.status = event.data.success ? "done" : "error"; last.summary = event.data.summary ?? ""; }
            return { ...a, tools };
          });
          streamAgentsRef.current = updated;
          return updated;
        });
        break;
      case EventType.SUBAGENT_TEXT_DELTA:
        setStreamAgents((agents) => {
          const updated = agents.map((a) =>
            a.id === event.data.agent_id ? { ...a, streamingText: a.streamingText + (event.data.text ?? "") } : a
          );
          streamAgentsRef.current = updated;
          return updated;
        });
        break;
      case EventType.SUBAGENT_FINISHED:
        setStreamAgents((agents) => {
          const updated = agents.map((a) =>
            a.id === event.data.agent_id
              ? { ...a, status: (event.data.status ?? "done") as any, resultSummary: event.data.result_summary ?? "", durationMs: event.data.duration_ms }
              : a
          );
          streamAgentsRef.current = updated;
          return updated;
        });
        break;

      // ── Server-reported terminal failure/cancellation ──
      case EventType.ERROR:
      case EventType.RUN_ERROR:
      case EventType.CANCELLED: {
        const cancelled = event.eventType === EventType.CANCELLED
          || (event.eventType === EventType.RUN_ERROR && event.data.status === "cancelled");
        const reason = typeof event.data.reason === "string" ? event.data.reason
          : typeof event.data.terminal_reason === "string" ? event.data.terminal_reason : "";
        const error = typeof event.data.error === "string" ? event.data.error
          : typeof event.data.error?.message === "string" ? event.data.error.message
          : typeof event.data.message === "string" ? event.data.message : "Unknown";
        const detail = cancelled ? (reason ? `Cancelled: ${reason}` : "Cancelled.")
          : `Error: ${error}`;
        // Commit refs before React processes queued UI updates or the stream closes.
        streamTextRef.current += `\n${detail}`;
        setStreamText(streamTextRef.current);
        break;
      }
    }
  }, []);

  // ─── Reset streaming state ────────────────────────────────────

  function resetStreamState() {
    setStreamText(""); setStreamTools([]); setStreamAgents([]);
    setStatusMsg(""); setThinkingText(""); setTokenInfo("");
    setRagDocs([]); setWebResults([]); setArtifacts([]);
    streamTextRef.current = ""; streamToolsRef.current = []; streamAgentsRef.current = [];
    metaRef.current = {};
    completedRef.current = false;
    cancelledRef.current = false;
  }

  // ─── Send message ─────────────────────────────────────────────

  const handleSubmit = useCallback(
    async (text: string) => {
      const trimmed = text.trim();
      if (!trimmed || busy) return;
      setInput("");

      // Save to history
      history.push(trimmed);
      setHistoryIdx(-1);

      // Helper to add system message
      function sysMsg(content: string) {
        setMessages((m) => [...m, { role: "system" as const, content }]);
      }

      // Slash commands
      if (trimmed.startsWith("/")) {
        const result = parseSlashCommand(trimmed, {
          model, kbDatasetIds: kbIds, sessionId, baseUrl: config.base_url,
          messageCount: messages.length, mcpServers: mcpManager.servers,
        });

        if (result.handled) {
          if (result.action === "exit") { exit(); return; }
          if (result.action === "clear") { setMessages([]); return; }
          if (result.action === "new") {
            setMessages([{ role: "system", content: "New conversation started." }]);
            setSessionId(undefined); setStreamText(""); return;
          }
          if (result.action === "compact") {
            const userMsgs = messages.filter((m) => m.role !== "system");
            if (userMsgs.length <= 2) { sysMsg("Conversation too short to compact."); return; }
            // P2.1: LLM-powered summarization via gateway
            setBusy(true); setStatusMsg("Compacting conversation with AI...");
            try {
              const historyForSummary = userMsgs.slice(-30).map((m) => ({
                role: m.role, content: m.content.slice(0, 500),
              }));
              let summaryText = "";
              for await (const evt of client.streamSSE("/api/v1/assistant/chat/stream", {
                message: "Please summarize the entire conversation above in under 500 words. " +
                  "Preserve all important context: file paths, code decisions, key facts, and action items. " +
                  "Output ONLY the summary, no preamble.",
                session_id: sessionId || "compact-temp",
                model_id: model,
                history: historyForSummary,
              })) {
                if (evt.eventType === "text_delta") {
                  summaryText += typeof evt.data === "string" ? evt.data : (evt.data as any)?.text || "";
                }
              }
              if (!summaryText.trim()) {
                // Fallback to naive truncation if LLM fails
                summaryText = userMsgs.map((m) => `${m.role}: ${m.content.slice(0, 200)}`).join("\n").slice(0, 2000);
              }
              setMessages([{ role: "system", content: `[Compacted ${userMsgs.length} messages → AI summary]\n\n${summaryText}` }]);
            } catch {
              // Fallback
              const fallback = userMsgs.map((m) => `${m.role}: ${m.content.slice(0, 200)}`).join("\n").slice(0, 2000);
              setMessages([{ role: "system", content: `[Compacted ${userMsgs.length} messages]\n\n${fallback}` }]);
            } finally { setBusy(false); setStatusMsg(""); }
            return;
          }
          if (result.action === "memory") {
            if (result.memoryText === "__list__") sysMsg(listMemories());
            else if (result.memoryText) { saveMemory(result.memoryText); sysMsg(`Memory saved: "${result.memoryText}"`); }
            return;
          }

          // /kb search
          if (result.stateUpdates?._kbSearch) {
            const query = result.stateUpdates._kbSearch;
            if (kbIds.length === 0) { sysMsg("No KBs bound. Use /kb <id> first."); return; }
            setBusy(true); setStatusMsg(`Searching KB: "${query}"...`);
            try {
              const data = await client.request("POST", "/api/v1/knowledge/search", {
                query, dataset_ids: kbIds, top_k: 5,
              });
              const results = data.results ?? data.chunks ?? [];
              if (results.length === 0) { sysMsg(`No results for: "${query}"`); }
              else {
                const list = results.map((r: any, i: number) =>
                  `  ${i + 1}. [${(r.score ?? 0).toFixed(2)}] ${(r.content ?? r.text ?? "").slice(0, 150)}...`
                ).join("\n");
                sysMsg(`KB Search Results (${results.length}):\n${list}`);
              }
            } catch (err: any) { sysMsg(`KB search error: ${err.message}`); }
            setBusy(false); setStatusMsg(""); return;
          }

          // /kb status
          if (result.stateUpdates?._kbStatus) {
            setBusy(true); setStatusMsg("Fetching KB status...");
            try {
              const data = await client.request("GET", "/api/v1/knowledge/datasets");
              const datasets = Array.isArray(data) ? data : data.datasets ?? [];
              const lines = datasets.map((d: any) =>
                `  ${d.dataset_id}: ${d.name}\n    Documents: ${d.total_documents ?? 0} | Vectors: ${d.total_vectors ?? "?"} | Status: ${d.status ?? "active"}`
              ).join("\n");
              sysMsg(datasets.length ? `Knowledge Base Status:\n${lines}` : "No knowledge bases found.");
            } catch (err: any) { sysMsg(`Error: ${err.message}`); }
            setBusy(false); setStatusMsg(""); return;
          }

          // Async commands
          if (["history", "resume", "mcp_connect", "mcp_disconnect", "mcp_list"].includes(result.action ?? "")
            || result.action?.startsWith("skill_")
            || result.stateUpdates?._fetchKBList) {
            setBusy(true);
            try {
              await handleAsyncCommand(result, client, sessionClient, mcpManager, sysMsg, setSessionId, setMessages, setStatusMsg);
            } catch (err: any) {
              sysMsg(`Error: ${err.message}`);
            }
            setBusy(false); setStatusMsg(""); return;
          }

          // /export
          if (result.stateUpdates?._export) {
            const fmt = result.stateUpdates._export;
            const ts = new Date().toISOString().slice(0, 19).replace(/[T:]/g, "-");
            const filename = `chat-${ts}.${fmt === "json" ? "json" : "md"}`;
            if (fmt === "json") {
              writeFileSync(filename, JSON.stringify(messages, null, 2), "utf-8");
            } else {
              const md = messages.map((m) => {
                const prefix = m.role === "user" ? "**You**" : m.role === "assistant" ? "**AI**" : "*System*";
                return `${prefix}: ${m.content}`;
              }).join("\n\n---\n\n");
              writeFileSync(filename, md, "utf-8");
            }
            sysMsg(`Exported to: ${filename}`);
            return;
          }

          // /image, /artifact, /usage, /whoami, /models — backend commands
          if (result.stateUpdates?._imagePrompt || result.stateUpdates?._artifactList ||
              result.stateUpdates?._artifactDownload || result.stateUpdates?._usage ||
              result.stateUpdates?._whoami || result.stateUpdates?._modelsList) {
            setBusy(true);
            try {
              if (result.stateUpdates._imagePrompt) {
                setStatusMsg("Generating image...");
                const res = await client.request("POST", "/api/v1/assistant/generate-image", {
                  prompt: result.stateUpdates._imagePrompt, model_id: model,
                });
                sysMsg(`Image generated: ${res.url ?? res.image_url ?? "check artifacts"}`);
              }
              if (result.stateUpdates._artifactList) {
                if (!sessionId) { sysMsg("No active session."); } else {
                  setStatusMsg("Fetching artifacts...");
                  const data = await client.request("GET", `/api/v1/assistant/sessions/${sessionId}/artifacts`);
                  const arts = Array.isArray(data) ? data : data.artifacts ?? [];
                  sysMsg(arts.length
                    ? `Artifacts (${arts.length}):\n` + arts.map((a: any) => `  ${a.id?.slice(0, 8) ?? "?"}: ${a.title ?? a.filename ?? "?"} (${a.type ?? "file"})`).join("\n")
                    : "No artifacts in this session.");
                }
              }
              if (result.stateUpdates._artifactDownload) {
                setStatusMsg("Downloading...");
                const id = result.stateUpdates._artifactDownload;
                sysMsg(`Download: ${config.base_url}/api/v1/assistant/artifacts/${id}/download`);
              }
              if (result.stateUpdates._usage) {
                setStatusMsg("Fetching usage...");
                const data = await client.request("GET", "/api/v1/usage/summary");
                sysMsg(`API Usage:\n${JSON.stringify(data, null, 2).slice(0, 500)}`);
              }
              if (result.stateUpdates._whoami) {
                const data = await client.request("GET", "/api/v1/auth/me");
                sysMsg(`User: ${data.username ?? data.email ?? "unknown"}\nRole: ${data.role ?? "user"}\nTenant: ${data.tenant_id ?? config.tenant_id ?? "default"}`);
              }
              if (result.stateUpdates._modelsList) {
                setStatusMsg("Fetching models...");
                const data = await client.request("GET", "/api/v1/admin/models");
                const models = Array.isArray(data) ? data : data.models ?? [];
                sysMsg(models.length
                  ? `Available Models (${models.length}):\n` + models.map((m: any) => `  ${m.enabled ? "ON" : "OFF"} ${m.model_id}: ${m.display_name ?? m.model_id} (${m.provider ?? "?"})`).join("\n")
                  : "No models configured.");
              }
            } catch (err: any) { sysMsg(`Error: ${err.message}`); }
            setBusy(false); setStatusMsg(""); return;
          }

          // /agent list
          if (result.stateUpdates?._agentList) {
            sysMsg(`Available Agents:\n${listAgentsSummary()}`);
            return;
          }

          // /agent run <name> <prompt>
          if (result.stateUpdates?._agentRun) {
            const agentName = result.stateUpdates._agentRun;
            const agentPrompt = result.stateUpdates._agentPrompt || "";
            const agent = getAgent(agentName);
            if (!agent) { sysMsg(`Agent not found: ${agentName}\nUse /agent list to see available agents.`); return; }
            // Send as a regular message with agent context injected
            const fullPrompt = `[Agent: ${agent.name}]\n${agent.instructions}\n\nUser task: ${agentPrompt}`;
            // Trigger as a normal chat with the agent's system prompt
            setMessages((m) => [...m, { role: "system", content: `Running agent: ${agent.name} — ${agent.description}` }]);
            setBusy(true); resetStreamState(); setStatusMsg(`Agent: ${agent.name}...`);
            try {
              for await (const event of chat.stream(agentPrompt, {
                sessionId, modelId: agent.model ?? model,
                systemPrompt: agent.instructions + (projectMemory ? "\n\n" + projectMemory : ""),
                webSearchEnabled: true,
              })) {
                if (cancelledRef.current) break;
                handleEvent(event);
              }
            } catch (err: any) {
              streamTextRef.current += `\nError: ${err.message}`;
              setStreamText(streamTextRef.current);
            }
            const finalText = streamTextRef.current;
            if (finalText) setMessages((m) => [...m, { role: "assistant", content: finalText, meta: { ...metaRef.current } }]);
            setBusy(false); resetStreamState();
            return;
          }

          if (result.stateUpdates) {
            if ("model" in result.stateUpdates) setModel(result.stateUpdates.model);
            if ("kbDatasetIds" in result.stateUpdates) setKbIds(result.stateUpdates.kbDatasetIds);
            if ("sessionId" in result.stateUpdates) setSessionId(result.stateUpdates.sessionId);
          }
          if (result.output) sysMsg(result.output);
          return;
        }
      }

      // ── Regular message ──
      setMessages((m) => [...m, { role: "user", content: trimmed }]);
      setBusy(true);
      resetStreamState();
      setStatusMsg("Thinking...");

      try {
        for await (const event of chat.stream(trimmed, {
          sessionId, modelId: model,
          kbDatasetIds: kbIds.length > 0 ? kbIds : undefined,
          webSearchEnabled: true,
          systemPrompt: projectMemory || undefined,
        })) {
          if (cancelledRef.current) break;
          handleEvent(event);
        }
      } catch (err: any) {
        if (!cancelledRef.current) {
          const errMsg = `Error: ${err.message}`;
          streamTextRef.current += (streamTextRef.current ? "\n" : "") + errMsg;
          setStreamText(streamTextRef.current);
        }
      }

      // Commit assistant message
      const finalText = streamTextRef.current;
      const finalTools = streamToolsRef.current;
      const finalAgents = streamAgentsRef.current;
      const meta = { ...metaRef.current };

      if (finalText || finalTools.length || finalAgents.length) {
        setMessages((m) => [...m, {
          role: "assistant", content: finalText,
          tools: finalTools.length > 0 ? finalTools : undefined,
          agents: finalAgents.length > 0 ? finalAgents : undefined,
          meta: Object.keys(meta).length > 0 ? meta : undefined,
        }]);
      } else if (completedRef.current) {
        setMessages((m) => [...m, { role: "system", content: "Completed without text output." }]);
      } else if (!cancelledRef.current) {
        setMessages((m) => [...m, { role: "system", content: "No response received. Check API key and base URL." }]);
      }

      setBusy(false);
      resetStreamState();
    },
    [busy, model, kbIds, sessionId, config.base_url, chat, exit, handleConfirm, handleEvent, messages, history, mcpManager, client, sessionClient, projectMemory],
  );

  // ─── Keyboard ─────────────────────────────────────────────────

  useInput((ch, key) => {
    // Ctrl+C: first press cancels stream, second exits
    if (key.ctrl && ch === "c") {
      if (busy && !cancelledRef.current) {
        cancelledRef.current = true;
        setStatusMsg("Cancelled.");
        return;
      }
      exit();
      return;
    }

    // Confirmation prompt
    if (confirm) {
      if (ch === "y" || ch === "Y") { const { resolve } = confirm; setConfirm(null); resolve(true); }
      else if (ch === "n" || ch === "N" || key.escape) { const { resolve } = confirm; setConfirm(null); resolve(false); }
      return;
    }

    // Command history (up/down)
    if (!busy && key.upArrow && history.length > 0) {
      const idx = historyIdx < 0 ? history.length - 1 : Math.max(0, historyIdx - 1);
      setHistoryIdx(idx);
      setInput(history[idx] ?? "");
    }
    if (!busy && key.downArrow) {
      if (historyIdx >= 0 && historyIdx < history.length - 1) {
        setHistoryIdx(historyIdx + 1);
        setInput(history[historyIdx + 1] ?? "");
      } else {
        setHistoryIdx(-1);
        setInput("");
      }
    }
  });

  // ─── Render ───────────────────────────────────────────────────

  return (
    <Box flexDirection="column" padding={1}>
      {/* Header */}
      <Box marginBottom={1}>
        <Text bold color="cyan">AI Gateway CLI v{CLI_VERSION}</Text>
        <Text color="gray">
          {" "}| {model}
          {kbIds.length > 0 ? ` | KB: ${kbIds.join(", ")}` : ""}
          {sessionId ? ` | ${sessionId.slice(0, 8)}...` : ""}
          {mcpManager.servers.length > 0 ? ` | MCP: ${mcpManager.servers.join(",")}` : ""}
        </Text>
      </Box>

      {/* Message History */}
      {messages.map((msg, i) => (
        <MessageView key={i} message={msg} />
      ))}

      {/* Live Streaming */}
      {busy && (
        <Box flexDirection="column" marginTop={1}>
          {/* Thinking */}
          {thinkingText && (
            <Box marginLeft={2} marginBottom={1}>
              <Text color="gray" dimColor italic>Thinking: {truncate(thinkingText, 200)}</Text>
            </Box>
          )}

          {/* RAG context */}
          {ragDocs.length > 0 && (
            <Box flexDirection="column" marginLeft={2}>
              <Text color="blue">KB Retrieved ({ragDocs.length} docs):</Text>
              {ragDocs.slice(0, 3).map((doc, i) => (
                <Text key={i} color="gray">  {truncate(doc, 80)}</Text>
              ))}
            </Box>
          )}

          {/* Web search results */}
          {webResults.length > 0 && (
            <Box flexDirection="column" marginLeft={2}>
              <Text color="blue">Web Results ({webResults.length}):</Text>
              {webResults.slice(0, 3).map((r, i) => (
                <Text key={i} color="gray">  {truncate(r, 80)}</Text>
              ))}
            </Box>
          )}

          {/* Sub-agents */}
          {streamAgents.map((agent, i) => (
            <SubAgentView key={i} agent={agent} />
          ))}

          {/* Tool calls */}
          {streamTools.map((tool, i) => (
            <ToolCallView key={i} tool={tool} />
          ))}

          {/* Artifacts */}
          {artifacts.map((art, i) => (
            <Box key={i} marginLeft={2}>
              <Text color="green">+ {art.type}: {art.title}{art.format ? ` (.${art.format})` : ""}</Text>
              {art.url && <Text color="gray"> {truncate(art.url, 50)}</Text>}
            </Box>
          ))}

          {/* Status */}
          {statusMsg && !streamText && (
            <Box marginLeft={2}><Text color="gray">{statusMsg}</Text></Box>
          )}

          {/* Streaming text */}
          {streamText && (
            <Box>
              <Text color="cyan" bold>AI: </Text>
              <Text wrap="wrap">{streamText}</Text>
            </Box>
          )}

          {/* Token info */}
          {tokenInfo && (
            <Box marginLeft={2}><Text color="gray" dimColor>[{tokenInfo}]</Text></Box>
          )}
        </Box>
      )}

      {/* Confirmation Prompt */}
      {confirm && (
        <Box flexDirection="column" marginTop={1} borderStyle="round" borderColor="yellow" paddingX={1}>
          <Text color="yellow" bold>Permission Required</Text>
          <Text color="white">{confirm.summary}</Text>
          <Box marginTop={1}>
            <Text color="green" bold>[y]</Text><Text> Approve  </Text>
            <Text color="red" bold>[n]</Text><Text> Deny</Text>
          </Box>
        </Box>
      )}

      {/* Input Line */}
      <Box marginTop={1}>
        <Text color="green" bold>{"> "}</Text>
        {busy ? (
          <Text color="gray">{confirm ? "Waiting for permission..." : cancelledRef.current ? "Cancelling..." : "Processing..."}</Text>
        ) : (
          <TextInput value={input} onChange={setInput} onSubmit={handleSubmit} placeholder="Type a message or /help..." />
        )}
      </Box>
    </Box>
  );
}

// ─── Async command handler (extracted to reduce handleSubmit size) ────

async function handleAsyncCommand(
  result: any,
  client: GatewayClient,
  sessionClient: GatewaySessionClient,
  mcpManager: MCPManager,
  sysMsg: (s: string) => void,
  setSessionId: (s: string | undefined) => void,
  setMessages: React.Dispatch<React.SetStateAction<Message[]>>,
  setStatusMsg: (s: string) => void,
) {
  switch (result.action) {
    case "history": {
      setStatusMsg("Fetching sessions...");
      const data = await sessionClient.list();
      const sessions = Array.isArray(data) ? data : data.sessions ?? [];
      if (sessions.length === 0) { sysMsg("No conversation history found."); return; }
      const list = sessions.map((s: any) => {
        const title = s.metadata?.title || s.title || "Untitled";
        const date = (s.updated_at || s.created_at || "").slice(0, 16);
        return `  ${s.session_id || s.id}  ${title}  (${date})`;
      }).join("\n");
      sysMsg(`Recent Conversations:\n${list}\n\nResume: /resume <session_id>`);
      break;
    }
    case "resume": {
      const sid = result.resumeSessionId;
      setStatusMsg("Loading session...");
      const data = await sessionClient.history(sid);
      if (data?.session_id !== sid || !Array.isArray(data.messages)
        || data.messages.some((m: any) => !m || typeof m.role !== "string" || typeof m.content !== "string")) {
        throw new Error("Invalid session history response");
      }
      const msgs = data.messages;
      const loaded: Message[] = msgs.map((m: any) => ({
        role: m.role === "user" ? "user" as const : "assistant" as const,
        content: m.content || "",
      }));
      setSessionId(sid);
      setMessages([
        { role: "system", content: `Resumed session: ${sid.slice(0, 8)}... (${loaded.length} messages)` },
        ...loaded.slice(-20),
      ]);
      break;
    }
    case "mcp_connect": {
      setStatusMsg(`Connecting MCP: ${result.mcpName}...`);
      const tools = await mcpManager.connect(result.mcpName, result.mcpCommand, result.mcpArgs ?? []);
      sysMsg(`MCP connected: ${result.mcpName} (${tools.length} tools)\n` +
        tools.map((t: any) => `  - ${t.name}: ${t.description.slice(0, 60)}`).join("\n"));
      break;
    }
    case "mcp_disconnect":
      await mcpManager.disconnect(result.mcpName);
      sysMsg(`MCP disconnected: ${result.mcpName}`);
      break;
    case "mcp_list": {
      const servers = mcpManager.servers;
      if (servers.length === 0) { sysMsg("No MCP servers connected.\nUsage: /mcp connect <name> <command> [args]"); return; }
      const tools = mcpManager.listTools();
      sysMsg(`Connected: ${servers.join(", ")}\n\nTools (${tools.length}):\n` + tools.map((t: any) => `  ${t.name} (${t.server})`).join("\n"));
      break;
    }
    case "skill_list": sysMsg(`Installed Skills:\n${await listSkills(client)}`); break;
    case "skill_create": {
      const res = await createAndUploadSkill(client, result.skillName, result.skillArgs?.join(" ") || result.skillName,
        `Custom skill: ${result.skillName}`, "When invoked, follow these instructions:\n\n1. [Add instructions]\n2. [Describe output]");
      sysMsg(`Skill created: ${res.name}\n${res.message}`); break;
    }
    case "skill_install": {
      const res = await uploadSkill(client, result.skillName);
      sysMsg(`Skill installed: ${res.name}\n${res.message}`); break;
    }
    case "skill_enable": sysMsg(await toggleSkill(client, result.skillName, true)); break;
    case "skill_disable": sysMsg(await toggleSkill(client, result.skillName, false)); break;
    case "skill_delete": sysMsg(await deleteSkill(client, result.skillName)); break;
    case "skill_test": sysMsg(`Skill test result:\n${await testSkill(client, result.skillName, result.skillArgs?.join(" ") ?? "")}`); break;
  }

  // /kb list
  if (result.stateUpdates?._fetchKBList) {
    setStatusMsg("Fetching knowledge bases...");
    const data = await client.request("GET", "/api/v1/knowledge/datasets");
    const datasets = Array.isArray(data) ? data : data.datasets ?? [];
    const list = datasets.map((d: any) => `  ${d.dataset_id}: ${d.name} (${d.total_documents ?? 0} docs)`).join("\n");
    sysMsg(datasets.length ? `Available Knowledge Bases:\n${list}\n\nBind: /kb <id1>, <id2>` : "No knowledge bases found.");
  }
}

// ─── Sub-components ──────────────────────────────────────────────────

function MessageView({ message }: { message: Message }) {
  const { role, content, tools, agents, meta } = message;

  return (
    <Box flexDirection="column" marginBottom={1}>
      <Box>
        {role === "user" && <Text color="blue" bold>You: </Text>}
        {role === "assistant" && <Text color="cyan" bold>AI: </Text>}
        {role === "system" && <Text color="yellow" bold>System: </Text>}
        <Text wrap="wrap">{content}</Text>
      </Box>

      {agents?.map((agent, i) => <SubAgentView key={i} agent={agent} compact />)}
      {tools?.map((tool, i) => <ToolCallView key={i} tool={tool} compact />)}

      {/* Metadata footer */}
      {meta?.tokens && (
        <Box marginLeft={4}>
          <Text color="gray" dimColor>[{meta.tokens.input}→{meta.tokens.output} tokens{meta.durationMs ? ` | ${(meta.durationMs / 1000).toFixed(1)}s` : ""}]</Text>
        </Box>
      )}

      {/* Artifacts */}
      {meta?.artifacts?.map((art, i) => (
        <Box key={i} marginLeft={4}>
          <Text color="green">+ {art.type}: {art.title}</Text>
        </Box>
      ))}
    </Box>
  );
}

function ToolCallView({ tool, compact }: { tool: ToolDisplay; compact?: boolean }) {
  const icon = tool.status === "running" ? "..." : tool.status === "done" ? "v" : tool.status === "denied" ? "x" : "!";
  const color = tool.status === "running" ? "yellow" : tool.status === "done" ? "green" : "red";
  return (
    <Box marginLeft={compact ? 4 : 2}>
      <Text color={color}>{icon} {tool.name}</Text>
      {tool.durationMs !== undefined && <Text color="gray"> ({tool.durationMs}ms)</Text>}
      {tool.result && !compact && <Text color="gray"> — {truncate(tool.result, 60)}</Text>}
    </Box>
  );
}

const AGENT_ICONS: Record<string, string> = { explore: "Search", task: "Task", plan: "Plan" };

function SubAgentView({ agent, compact }: { agent: AgentDisplay; compact?: boolean }) {
  const icon = AGENT_ICONS[agent.type] ?? agent.type;
  const si = agent.status === "running" ? "..." : agent.status === "done" ? "v" : "x";
  const sc = agent.status === "running" ? "yellow" : agent.status === "done" ? "green" : "red";
  return (
    <Box flexDirection="column" marginLeft={compact ? 4 : 2} marginBottom={compact ? 0 : 1}>
      <Box>
        <Text color={sc}>{si} </Text><Text color="magenta" bold>{icon}</Text>
        {agent.description && <Text color="gray"> {agent.description}</Text>}
        {agent.durationMs != null && <Text color="gray"> ({(agent.durationMs / 1000).toFixed(1)}s)</Text>}
      </Box>
      {!compact && agent.tools.map((t, i) => (
        <Box key={i} marginLeft={4}>
          <Text color={t.status === "running" ? "yellow" : t.status === "done" ? "green" : "red"}>
            {t.status === "running" ? "..." : t.status === "done" ? "v" : "x"} {t.name}
          </Text>
          {t.summary && <Text color="gray"> — {truncate(t.summary, 50)}</Text>}
        </Box>
      ))}
      {compact && agent.resultSummary && <Box marginLeft={4}><Text color="gray">{truncate(agent.resultSummary, 80)}</Text></Box>}
      {!compact && agent.status === "running" && agent.streamingText && (
        <Box marginLeft={4}><Text color="gray">{truncate(agent.streamingText, 200)}</Text></Box>
      )}
    </Box>
  );
}

function truncate(str: string, max: number): string {
  if (str.length <= max) return str;
  return str.slice(0, max - 3) + "...";
}
