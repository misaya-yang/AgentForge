/**
 * MCP Client — connects to a single MCP server via stdio (subprocess).
 * Ported from Python SDK: sdk/python/ai_assistant/mcp/client.py
 *
 * Implements JSON-RPC 2.0 over stdio for the Model Context Protocol.
 */

import { spawn, type ChildProcess } from "node:child_process";
import { createInterface, type Interface } from "node:readline";
import { CLI_VERSION } from "../version.js";

export interface MCPTool {
  name: string;
  description: string;
  inputSchema: Record<string, any>;
  serverName: string;
}

export interface MCPServerConfig {
  name: string;
  command: string;
  args: string[];
  env?: Record<string, string>;
}

export class MCPClient {
  private config: MCPServerConfig;
  private process: ChildProcess | null = null;
  private rl: Interface | null = null;
  private requestId = 0;
  private initialized = false;
  private pending = new Map<number, { resolve: (v: any) => void; reject: (e: Error) => void }>();
  tools: MCPTool[] = [];

  static PROTOCOL_VERSION = "2025-11-25";
  static CLIENT_INFO = { name: "ai-gateway-cli", version: CLI_VERSION };

  constructor(config: MCPServerConfig) {
    this.config = config;
  }

  async connect(): Promise<MCPTool[]> {
    const env = buildMCPEnvironment(this.config.env);

    this.process = spawn(this.config.command, this.config.args, {
      stdio: ["pipe", "pipe", "pipe"],
      env,
    });

    // Read stdout line by line for JSON-RPC responses
    this.rl = createInterface({ input: this.process.stdout! });
    this.rl.on("line", (line) => this.handleLine(line));

    // Handle process exit
    this.process.on("error", (err) => {
      this.rejectAll(new Error(`MCP server ${this.config.name} error: ${err.message}`));
    });
    this.process.on("exit", (code) => {
      this.rejectAll(new Error(`MCP server ${this.config.name} exited with code ${code}`));
    });

    // Initialize handshake
    await this.jsonrpc("initialize", {
      protocolVersion: MCPClient.PROTOCOL_VERSION,
      capabilities: { tools: {} },
      clientInfo: MCPClient.CLIENT_INFO,
    });

    // Notify initialized
    this.notify("notifications/initialized");
    this.initialized = true;

    // Discover tools
    const result = await this.jsonrpc("tools/list", {});
    this.tools = (result.tools ?? []).map((t: any) => ({
      name: t.name,
      description: t.description ?? "",
      inputSchema: t.inputSchema ?? {},
      serverName: this.config.name,
    }));

    return this.tools;
  }

  async callTool(toolName: string, args: Record<string, any>): Promise<any> {
    if (!this.initialized) throw new Error("MCP client not connected");
    return this.jsonrpc("tools/call", { name: toolName, arguments: args });
  }

  async close(): Promise<void> {
    this.rl?.close();
    this.rl = null;

    if (this.process && this.process.exitCode === null) {
      this.process.kill("SIGTERM");
      // Wait up to 5s for clean exit
      await new Promise<void>((resolve) => {
        const timer = setTimeout(() => {
          this.process?.kill("SIGKILL");
          resolve();
        }, 5000);
        this.process!.on("exit", () => {
          clearTimeout(timer);
          resolve();
        });
      });
    }

    this.rejectAll(new Error("MCP client closed"));
    this.initialized = false;
  }

  // -- JSON-RPC helpers --

  private handleLine(line: string) {
    const trimmed = line.trim();
    if (!trimmed) return;

    let msg: any;
    try {
      msg = JSON.parse(trimmed);
    } catch {
      return; // Non-JSON output from server (stderr leak, etc.)
    }

    const id = msg.id;
    if (id != null && this.pending.has(id)) {
      const { resolve, reject } = this.pending.get(id)!;
      this.pending.delete(id);

      if (msg.error) {
        reject(new Error(msg.error.message ?? "MCP error"));
      } else {
        resolve(msg.result ?? {});
      }
    }
    // Server-initiated notifications — ignore
  }

  private jsonrpc(method: string, params: Record<string, any>): Promise<any> {
    return new Promise((resolve, reject) => {
      if (!this.process?.stdin?.writable) {
        reject(new Error(`MCP ${this.config.name}: process not available`));
        return;
      }

      const id = ++this.requestId;
      this.pending.set(id, { resolve, reject });

      const request = JSON.stringify({
        jsonrpc: "2.0",
        id,
        method,
        params,
      }) + "\n";

      this.process.stdin.write(request, (err) => {
        if (err) {
          this.pending.delete(id);
          reject(err);
        }
      });

      // 30s timeout
      const timer = setTimeout(() => {
        if (this.pending.has(id)) {
          this.pending.delete(id);
          reject(new Error(`MCP ${this.config.name}: ${method} timed out (30s)`));
        }
      }, 30000);

      // Clear timeout on resolution (handled in handleLine via pending.delete)
      const origResolve = resolve;
      const origReject = reject;
      this.pending.set(id, {
        resolve: (v) => { clearTimeout(timer); origResolve(v); },
        reject: (e) => { clearTimeout(timer); origReject(e); },
      });
    });
  }

  private notify(method: string, params?: Record<string, any>) {
    const msg = JSON.stringify({
      jsonrpc: "2.0",
      method,
      params: params ?? {},
    }) + "\n";
    this.process?.stdin?.write(msg);
  }

  private rejectAll(err: Error) {
    for (const { reject } of this.pending.values()) {
      reject(err);
    }
    this.pending.clear();
  }
}

const MCP_AMBIENT_ENV_ALLOWLIST = [
  "PATH",
  "HOME",
  "USER",
  "LOGNAME",
  "SHELL",
  "TMPDIR",
  "TMP",
  "TEMP",
  "LANG",
  "LC_ALL",
  "LC_CTYPE",
  "SYSTEMROOT",
  "WINDIR",
] as const;

/**
 * Build the environment for an operator-requested local MCP child.
 *
 * Ambient provider, Gateway, and internal service credentials are deliberately
 * excluded. A value is added outside the code-owned allowlist only when the
 * operator explicitly configured it for this MCP server.
 */
export function buildMCPEnvironment(
  explicit: Record<string, string> = {},
  ambient: NodeJS.ProcessEnv = process.env,
): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = {};
  for (const name of MCP_AMBIENT_ENV_ALLOWLIST) {
    const value = ambient[name];
    if (value) env[name] = value;
  }
  for (const [name, value] of Object.entries(explicit)) {
    if (value !== "") env[name] = value;
  }
  return env;
}
