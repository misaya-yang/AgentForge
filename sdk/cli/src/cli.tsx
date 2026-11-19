/**
 * Independent Agent CLI product entrypoint.
 *
 * This launcher owns provider selection and an isolated CLI home, then starts
 * the lock-pinned composed Rust CLI. The Agent loop remains entirely inside
 * the embedded Rust App Server; this Node process never implements tools,
 * approvals, session state, or a second model loop.
 */

import { runIndependentCli } from "./native/launcher.js";

const argv = process.argv.slice(2);
if (argv[0] === "gateway" && argv[1] === "auth") {
  const { runGatewayAuth } = await import("./remote/device_auth.js");
  const controller=new AbortController();const stop=()=>controller.abort();process.once("SIGINT",stop);process.once("SIGTERM",stop);
  try {process.exitCode=await runGatewayAuth(argv.slice(2),{signal:controller.signal});} finally {process.off("SIGINT",stop);process.off("SIGTERM",stop);}
} else if (argv[0] === "gateway" && argv[1] === "knowledge") {
  const { runKnowledgeCli } = await import("./remote/knowledge_cli.js");
  process.exitCode = await runKnowledgeCli(argv.slice(2));
} else if (argv[0] === "gateway") {
  const { runLegacyGatewayCli } = await import("./legacy_gateway.js");
  process.exitCode = await runLegacyGatewayCli(argv.slice(1));
} else {
  process.exitCode = await runIndependentCli(argv);
}
