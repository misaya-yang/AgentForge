# Native release tools

Source-equivalent Rust replacements for the former release Python tools:

- `platform-release evidence`: release matrix, durable receipt, schema and plan-retirement validation.
- `platform-release compatibility`: offline identity validation; `--write --source-rev SHA --inputs FILE` generates a candidate only from clean, explicit Git identity and complete passing evidence.
- `platform-release version-agreement`: live HTTP/container identity inspection through the original three environment variables.
- `platform-release integration --gate NAME`: exact named nested gate execution, required environment checks, skip detection and candidate-bound receipts. `--dry-run` remains a dry run.
- `gateway-preflight`: static native authentication/Helm checks and bounded Helm lint.
- `verify-local-stack`: existing API health/OpenAPI identity check.

No Python interpreter is invoked. Native identity replaces Python source hashes and toolchain fields; old frozen release receipts are not automatically upgraded. The source script's unsafe cleanup after a failed database creation is narrowed: cleanup only targets a scratch database successfully created by this invocation. No release candidate, image agreement, live database execution or deployment is asserted by compiling these tools.

The owner paused database/environment acceptance on 2026-10-15. These executable operations are provided for later explicitly authorized verification; this migration has not run them against user databases.
