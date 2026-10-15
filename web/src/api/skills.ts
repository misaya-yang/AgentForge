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

export async function listSkills(params?: { enabled_only?: boolean }) {
  const { data } = await api.get<{ skills: Skill[]; total: number }>("/api/v1/skills", { params });
  return data;
}


export async function uploadSkill(file: File) {
  const formData = new FormData();
  formData.append("file", file);
  const { data } = await api.post<{ name: string; status: string }>("/api/v1/skills/upload", formData);
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
