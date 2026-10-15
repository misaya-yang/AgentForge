/**
 * CLI configuration types.
 * Config stored at ~/.ai-gateway/config.json
 */

export interface CLIConfig {
  api_key: string;
  base_url: string;
  tenant_id: string;
  user_id?: string;
  default_model: string;
  kb_dataset_ids?: string[];
  timeout: number;
  stream_idle_timeout: number;
  max_retries: number;
}

export interface MCPServerConfig {
  name: string;
  command: string;
  args: string[];
  env?: Record<string, string>;
}

export const DEFAULT_CONFIG: CLIConfig = {
  api_key: "",
  base_url: "http://localhost:8080",
  tenant_id: "",
  default_model: "",
  timeout: 30,
  stream_idle_timeout: 300,
  max_retries: 3,
};
