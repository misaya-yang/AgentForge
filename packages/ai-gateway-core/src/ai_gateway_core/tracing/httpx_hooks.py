"""Outbound httpx instrumentation — single function, ``instrument_httpx_client``.

The ``opentelemetry-instrumentation-httpx`` package already has a
correct, battle-tested ``HTTPXClientInstrumentor`` that:

- Opens a CLIENT-kind span around each request.
- Injects the active OTel context into outbound headers (so
  ``traceparent`` reaches the next service automatically).
- Records ``http.status_code`` / ``http.url`` attributes.
- Sets span status from the response code.
- Fires ``record_exception`` on transport failure.

We just wrap it in a thin helper so caller code reads as one line:

    instrument_httpx_client(client)

…instead of two imports + a method call. The helper also catches
``Exception`` so a missing OTel install can't break the proxy bootstrap
(some test envs intentionally exclude OTel deps).
"""

from __future__ import annotations

import logging
from typing import Any

logger = logging.getLogger(__name__)


def instrument_httpx_client(client: Any) -> None:
    """Attach OTel CLIENT-span instrumentation to an ``httpx.AsyncClient``.

    Idempotent at the instrumentor level: calling ``instrument_client``
    twice on the same client is a no-op (the instrumentor checks
    internal state). Calling it on a client that's already been used
    for requests is supported but those earlier requests obviously
    didn't get spans.

    Parameters
    ----------
    client
        An ``httpx.AsyncClient`` (or ``httpx.Client`` — same upstream
        API). Typed ``Any`` so this module doesn't import httpx
        unconditionally — keeps the import-time cost zero for callers
        that never use the proxy layer.

    Failure mode
    ------------
    If ``opentelemetry-instrumentation-httpx`` isn't installed, the
    function logs a debug message and returns. The proxy keeps working;
    outbound calls just won't show up as their own client spans.
    Inbound spans (from ``OTelInboundMiddleware``) and any spans the
    application code creates manually still flow through fine.
    """
    try:
        from opentelemetry.instrumentation.httpx import HTTPXClientInstrumentor

        HTTPXClientInstrumentor.instrument_client(client)
    except ImportError:
        logger.debug(
            "opentelemetry-instrumentation-httpx not installed; "
            "outbound httpx calls won't produce CLIENT spans."
        )
    except Exception as exc:  # noqa: BLE001
        logger.warning(
            "Failed to instrument httpx client with OTel: %s; "
            "outbound calls won't produce CLIENT spans.",
            exc,
        )


__all__ = ["instrument_httpx_client"]
