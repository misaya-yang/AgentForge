/**
 * Memory system — persistent notes across CLI sessions.
 * Stored at ~/.ai-gateway/memories.json
 */

import { existsSync, readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

const MEMORY_FILE = join(homedir(), ".ai-gateway", "memories.json");

export interface MemoryEntry {
  text: string;
  timestamp: string;
}

export function loadMemories(): MemoryEntry[] {
  try {
    if (existsSync(MEMORY_FILE)) {
      return JSON.parse(readFileSync(MEMORY_FILE, "utf-8"));
    }
  } catch {
    // Corrupted file
  }
  return [];
}

export function saveMemory(text: string): MemoryEntry {
  const memories = loadMemories();
  const entry: MemoryEntry = {
    text,
    timestamp: new Date().toISOString(),
  };
  memories.push(entry);

  const dir = join(homedir(), ".ai-gateway");
  if (!existsSync(dir)) mkdirSync(dir, { recursive: true });
  writeFileSync(MEMORY_FILE, JSON.stringify(memories, null, 2) + "\n", "utf-8");
  return entry;
}

export function listMemories(): string {
  const memories = loadMemories();
  if (memories.length === 0) return "No memories saved yet.\nUsage: /memory <note>";

  return memories
    .map((m, i) => `  ${i + 1}. ${m.text}  (${m.timestamp.slice(0, 10)})`)
    .join("\n");
}

export function getMemoriesForPrompt(): string {
  const memories = loadMemories();
  if (memories.length === 0) return "";

  return (
    "\n\n[User Memories]\n" +
    memories.map((m) => `- ${m.text}`).join("\n")
  );
}
