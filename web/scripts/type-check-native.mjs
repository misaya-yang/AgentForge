import { spawnSync } from "node:child_process";
import process from "node:process";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";
const require = createRequire(import.meta.url);
const cli = fileURLToPath(new URL("./bin/tsgo", new URL(require.resolve("@typescript/native-preview/package.json"), "file:")));
for (const project of ["tsconfig.app.json", "tsconfig.node.json", "tsconfig.contracts.json"]) {
  const result = spawnSync(process.execPath, [cli, "--noEmit", "-p", project], { cwd: fileURLToPath(new URL("..", import.meta.url)), stdio: "inherit" });
  if (result.error) throw result.error;
  if (result.status !== 0) process.exit(result.status ?? 1);
}
