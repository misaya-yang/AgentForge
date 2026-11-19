import type { RequestQuery, RequestBody } from "@/api/contracts";
/**
 * Skills API client.
 */
import { api } from "@/lib/api";

export interface Skill {
  name: string;
  title: string;
  description: string;
  entrypoint: string;
  summary: string;
  version: string;
  tags: string[];
  permissions: string[];
  enabled: boolean;
  source: "builtin" | "user" | "marketplace";
  instructions?: string;
  trigger?: { patterns: string[]; auto: boolean } | null;
  config?: Record<string, unknown>;
}

export async function listSkills(params?: RequestQuery<"GET /api/v1/skills">) {
  const { data } = await api.get<{ skills: Skill[]; total: number }>("/api/v1/skills", { params: ((params ?? {}) satisfies RequestQuery<"GET /api/v1/skills">) });
  return data;
}


export async function uploadSkill(file: File) {
  const upload: RequestBody<"POST /api/v1/skills/upload"> = { file };
  const { data } = await api.post<{ name: string; status: string }>("/api/v1/skills/upload", upload);
  return data;
}

export async function enableSkill(name: string) {
  return api.post(`/api/v1/skills/${name}/enable`);
}

export async function disableSkill(name: string) {
  return api.post(`/api/v1/skills/${name}/disable`);
}

export async function deleteSkill(name: string) {
  return api.delete(`/api/v1/skills/${name}`);
}
