"""Reviewed application windows for the immutable schema authority epoch."""

DATABASE_BASELINE_ID = "2026_08_post_kb_v1"
# Legacy Python Gateway: later native Gateway admission policy is not implemented.
MIN_REQUIRED_EPOCH_SEQUENCE = 2
MAX_SUPPORTED_EPOCH_SEQUENCE = 10

# Python Knowledge API/worker: epochs 11..17 leave Knowledge storage/ACLs intact.
# Keep this explicit; a future epoch requires review, never automatic acceptance.
KNOWLEDGE_MIN_REQUIRED_EPOCH_SEQUENCE = 5
KNOWLEDGE_MAX_SUPPORTED_EPOCH_SEQUENCE = 17
