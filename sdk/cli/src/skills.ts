/**
 * Skills management — create, install, list, enable/disable skills.
 * Skills are uploaded as SKILL.md files with YAML frontmatter.
 */

import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { join } from "node:path";
import type { GatewayClient } from "./remote/client.js";

/**
 * Generate a SKILL.md template for a new skill.
 */
export function generateSkillTemplate(
  name: string,
  title: string,
  description: string,
  instructions: string,
): string {
  return `---
name: ${name}
title: "${title}"
description: "${description}"
version: 1.0.0
tags: []
permissions: []
trigger:
  patterns: []
  auto: false
---

# ${title}

## Instructions

${instructions}
`;
}

/**
 * Upload a SKILL.md file to the gateway.
 */
export async function uploadSkill(
  client: GatewayClient,
  filePath: string,
): Promise<{ name: string; message: string }> {
  if (!existsSync(filePath)) {
    throw new Error(`File not found: ${filePath}`);
  }

  const content = readFileSync(filePath, "utf-8");

  // Parse frontmatter to extract name
  const fmMatch = content.match(/^---\n([\s\S]*?)\n---/);
  if (!fmMatch) {
    throw new Error("Invalid SKILL.md: missing YAML frontmatter (---...---)");
  }

  const nameMatch = fmMatch[1]!.match(/^name:\s*(.+)$/m);
  if (!nameMatch) {
    throw new Error("Invalid SKILL.md: missing 'name' in frontmatter");
  }
  const skillName = nameMatch[1]!.trim();

  // Upload via multipart-like JSON (gateway accepts content as text)
  const result = await client.request("POST", "/api/v1/skills/upload", {
    content,
    filename: filePath.split("/").pop() ?? "skill.md",
  });

  return {
    name: skillName,
    message: result?.message ?? `Skill '${skillName}' uploaded successfully`,
  };
}

/**
 * Create a skill interactively and upload it.
 */
export async function createAndUploadSkill(
  client: GatewayClient,
  name: string,
  title: string,
  description: string,
  instructions: string,
): Promise<{ name: string; message: string }> {
  const content = generateSkillTemplate(name, title, description, instructions);

  const result = await client.request("POST", "/api/v1/skills/upload", {
    content,
    filename: `${name}.md`,
  });

  return {
    name,
    message: result?.message ?? `Skill '${name}' created successfully`,
  };
}

/**
 * List all skills from gateway.
 */
export async function listSkills(
  client: GatewayClient,
): Promise<string> {
  const data = await client.request("GET", "/api/v1/skills");
  const skills = Array.isArray(data) ? data : data.skills ?? [];

  if (skills.length === 0) return "No skills installed.";

  return skills
    .map((s: any) => {
      const status = s.enabled ? "ON" : "OFF";
      const source = s.source ?? "user";
      return `  [${status}] ${s.name}: ${(s.description || s.title || "").slice(0, 60)}  (${source})`;
    })
    .join("\n");
}

/**
 * Enable/disable a skill.
 */
export async function toggleSkill(
  client: GatewayClient,
  name: string,
  enable: boolean,
): Promise<string> {
  const endpoint = enable
    ? `/api/v1/skills/${name}/enable`
    : `/api/v1/skills/${name}/disable`;
  await client.request("POST", endpoint);
  return `Skill '${name}' ${enable ? "enabled" : "disabled"}`;
}

/**
 * Delete a skill.
 */
export async function deleteSkill(
  client: GatewayClient,
  name: string,
): Promise<string> {
  await client.request("DELETE", `/api/v1/skills/${name}`);
  return `Skill '${name}' deleted`;
}

/**
 * Test a skill with input.
 */
export async function testSkill(
  client: GatewayClient,
  name: string,
  input: string,
): Promise<string> {
  const result = await client.request("POST", `/api/v1/skills/${name}/test`, {
    input,
  });
  return typeof result === "string"
    ? result
    : JSON.stringify(result, null, 2);
}
