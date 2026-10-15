# Open-Source Env Readiness TODO

This repository must be verifiable without maintainer-private `.env` files, while
real deployments must still fail on missing, weak, or placeholder secrets.

## Done

- [x] Add a portable example-config gate: `make validate-example-config`.
- [x] Keep `make validate-config` strict for real deployment env files.
- [x] Keep `make validate` strict for runtime dependency checks.
- [x] Wire the public example-config gate into CI.
- [x] Add tests proving `.env.example` passes only the public gate and still
      fails the real release gate.

## Remaining Before A Real Release

- [ ] Create or update a real env file outside the repository.
- [ ] Set non-placeholder secrets for `POSTGRES_PASSWORD`, `REDIS_PASSWORD`,
      `JWT_SECRET`, `GATEWAY_ASSISTANT_SHARED_SECRET`, and
      `GATEWAY_ENCRYPTION_KEY`.
- [ ] Set a real `AUTH_ALLOWED_EMAIL_DOMAIN` for any shared or production
      deployment.
- [ ] Set explicit production CORS origin arrays for
      `KNOWLEDGE_CORS_ALLOW_ORIGINS_JSON` and
      `ASSISTANT_CORS_ALLOW_ORIGINS_JSON`.
- [ ] Set at least one real chat provider key and one real KB embedding key.
- [ ] Confirm the first-run experience: with `MODEL_SETUP_MODE=ui` the
      console shows the setup banner and checklist until a provider is
      configured, then hides them once `GET /api/v1/setup/state` reports
      `configured: true`.
- [ ] Run `make validate-config ENV_FILE=/path/to/.env`.
- [ ] Start the target stack, then run `make validate ENV_FILE=/path/to/.env`.

## Rule

`make validate-example-config` is a contributor and CI gate. It is not a release
or deployment approval.
