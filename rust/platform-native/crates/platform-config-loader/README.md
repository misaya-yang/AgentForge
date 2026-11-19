# Native gateway configuration

`gateway --config-env` loads the existing deployment settings without a renderer,
intermediate credential file, shell pipeline or child gateway process.
`gateway --check-config-env` resolves and validates the same configuration without
connecting services. Both require `GATEWAY_NATIVE_CONFIG` to name an explicit
native deployment JSON or YAML file (the fields in GatewayDocument.native).

Settings precedence and shape:

1. Bundled source Settings defaults.
2. `.env` from the working directory, matching the container's `/app` deployment.
3. Process environment overrides `.env` without rewriting process environment.
4. Within each GATEWAY_ setting, nested `__` keys override top-level JSON objects.

`GATEWAY_DOTENV_FILE` selects another dotenv path; empty disables dotenv. This
explicit native boundary uses dotenvy and accepts quoted dotenv values and
interpolation. No service-registry YAML is misinterpreted as settings YAML.
The provider/bootstrap environment captured in GatewayDocument includes dotenv
values, matching the source main entrypoint's early load_dotenv(override=False).

Known source settings are case-insensitive and type-coerced against the bundled
schema, including JSON collections, booleans and numbers. Unknown settings are
ignored like the source's extra=ignore. DEFAULT_MODEL and MODEL_SETUP_MODE keep
their unprefixed aliases. CORS_ALLOWED_ORIGINS and RATE_LIMIT_* factory defaults
are preserved. OCR language and production JWT secret validators remain active.

The native deployment file is required because private HTTP policy, sandbox
origin/token, PDF implementation consent and publication policy cannot safely be
invented from unrelated environment variables. Existing deployment variables
AI_PLATFORM_AGENT_RUNTIME_KERNEL_REVISION and
AI_PLATFORM_CAPABILITY_WORKER_WRITES_ENABLED override their matching native
fields when set. All other native fields stay explicit. The existing
--config-stdin mode remains supported.

Source defaults/schema are embedded JSON fixtures captured at migration time,
not generated at build/startup. There is no Python dependency. Future settings
changes must update these snapshots and parity tests as one reviewed change.
Resolved configurations, secrets, DSNs and file contents are never printed;
errors contain only setting names or stable format/file errors.
