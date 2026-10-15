"""ARC-04 core-boundary tooling: import/data-access inventory + allowlist gate.

- ``inventory_core_consumption`` — regenerate the machine-readable inventory
  of which services consume which ``ai_gateway_core`` modules.
- ``check_core_boundary`` — the mechanical allowlist gate (contracts content
  whitelist, forbidden-dependency scan, core no-growth, Knowledge→core
  no-growth, no circular dependency, shim no-growth).
- ``selftest`` — negative self-tests proving the gate actually fails on
  synthetic violations.
"""
