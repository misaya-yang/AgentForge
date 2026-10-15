"""Use-case modules behind the ``src/api/v1/agent_runtime.py`` facade (ARC-01B).

Layout (import graph is a DAG rooted at ``core``):

- ``core``: request/auth/repository plumbing shared by every use case.
- ``rate_limit``: published-channel quota and rate limiting.
- ``resolution``: model/capability/knowledge resolution for snapshots.
- ``snapshot``: immutable runtime Snapshot assembly.
- ``attachments``: published attachment storage and resolution.
- ``streaming``: session binding, idempotency and streaming startup.
- ``preview`` / ``published``: the Studio preview and published handlers.

Route registration stays in the facade so the route table, operation ids and
registration order are visible in one place; handler modules define plain
functions and never import each other's handlers as a library.
"""
