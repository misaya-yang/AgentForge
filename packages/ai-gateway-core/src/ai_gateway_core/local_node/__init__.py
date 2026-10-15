"""Gateway-owned Local Node wire contracts.

The module is deliberately transport agnostic.  Assistant and Runtime code
must exchange these plain contracts rather than importing a Local Node
implementation or a device credential store.
"""

from .contracts import (
    LOCAL_NODE_PROTOCOL_VERSION,
    LocalNodeAction,
    LocalNodeCapability,
    LocalNodeDeviceScope,
    LocalNodeReceipt,
    LocalNodeReceiptStatus,
    arguments_digest,
    validate_scope,
)

__all__ = [
    "LOCAL_NODE_PROTOCOL_VERSION",
    "LocalNodeAction",
    "LocalNodeCapability",
    "LocalNodeDeviceScope",
    "LocalNodeReceipt",
    "LocalNodeReceiptStatus",
    "arguments_digest",
    "validate_scope",
]
