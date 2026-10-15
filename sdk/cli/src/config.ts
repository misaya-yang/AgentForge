/**
 * Config manager — reads/writes CLI and MCP server configuration.
 */

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import {
  DEFAULT_CONFIG,
  type CLIConfig,
  type MCPServerConfig,
} from "./types/config.js";

const CONFIG_DIR = join(homedir(), ".ai-gateway");
const CONFIG_FILE = join(CONFIG_DIR, "config.json");
const MCP_SERVERS_FILE = join(CONFIG_DIR, "mcp_servers.json");
const SESSIONS_DIR = join(CONFIG_DIR, "sessions");

function ensureDir(dir: string) {
  if (!existsSync(dir)) mkdirSync(dir, { recursive: true, mode: 0o700 });
}

function readJSON<T>(path: string, fallback: T): T {
  try {
    if (existsSync(path)) {
      return JSON.parse(readFileSync(path, "utf-8")) as T;
    }
  } catch {
    // Corrupted file — return fallback
  }
  return fallback;
}

function writeJSON(path: string, data: any) {
  ensureDir(CONFIG_DIR);
  writeFileSync(path, JSON.stringify(data, null, 2) + "\n", { encoding: "utf-8", mode: 0o600 });
}

export function loadConfig(): CLIConfig {
  return { ...DEFAULT_CONFIG, ...readJSON<Partial<CLIConfig>>(CONFIG_FILE, {}) };
}

export function saveConfig(config: CLIConfig) {
  writeJSON(CONFIG_FILE, config);
}

export function updateConfig(updates: Partial<CLIConfig>) {
  const current = loadConfig();
  const merged = { ...current, ...updates };
  saveConfig(merged);
  return merged;
}

export function loadMCPServers(): MCPServerConfig[] {
  return readJSON<MCPServerConfig[]>(MCP_SERVERS_FILE, []);
}

export function saveMCPServers(servers: MCPServerConfig[]) {
  writeJSON(MCP_SERVERS_FILE, servers);
}

/**
 * Initialize config directory on first run.
 * Returns the loaded config (may be empty if first run).
 */
export function initConfigDir(): CLIConfig {
  ensureDir(CONFIG_DIR);
  ensureDir(SESSIONS_DIR);

  if (!existsSync(CONFIG_FILE)) {
    writeJSON(CONFIG_FILE, DEFAULT_CONFIG);
  }
  if (!existsSync(MCP_SERVERS_FILE)) {
    writeJSON(MCP_SERVERS_FILE, []);
  }

  return loadConfig();
}

export { CONFIG_DIR, CONFIG_FILE, SESSIONS_DIR };
