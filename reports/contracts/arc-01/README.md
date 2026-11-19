# ARC-01/01B Assistant API contract evidence

The three dated JSON files are historical v1 route-surface witnesses:

- `assistant-api-routes-before.json` — captured before the ARC-01 split;
- `assistant-api-routes-after.json` — captured immediately after the split;
- `assistant-api-routes-now.json` — captured during the original integration check.

Those v1 files contain only path, method, operation ID and response-code names. They did not compare
descriptions, tags, parameters, request bodies, response schemas or security. They therefore do not,
by themselves, prove zero public OpenAPI drift and must not be regenerated to absorb a mismatch.

`arc01-export-routes.py` is the v2 exporter and checker. It exports complete FastAPI operation objects
for every public path and a scoped set for the 23 handlers moved by ARC-01. The published baseline is
`sdk/openapi.json`. A deliberate difference is accepted only through the exporter's exact
`INTENTIONAL_PUBLIC_OPERATION_DELTAS` manifest, which names the operation, field, published value,
current value, reason and source commit.

The current manifest contains one intentional field delta: task cancellation says Runtime rather than
the former Assistant process because `fe2e1b88` completed the single Rust Runtime cutover. Every other
field of all 23 operation objects must equal the published SDK contract.

Run the non-writing comparison and its regression test with:

```bash
PYTHONDONTWRITEBYTECODE=1 uv run --all-packages --extra test \
  python reports/contracts/arc-01/arc01-export-routes.py --check-sdk
PYTHONDONTWRITEBYTECODE=1 uv run --all-packages --extra test \
  pytest -q --no-cov tests/api/test_assistant_openapi_contract.py
```

Writing a fresh v2 diagnostic export is optional supporting evidence, not contract approval:

```bash
uv run --all-packages --extra test \
  python reports/contracts/arc-01/arc01-export-routes.py tmp/arc01-openapi-v2.json
```
