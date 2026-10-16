# Artifact-backed image history migration

Implemented `migration_content::image_history::{append_image_turns, inflate_history_with_bytes, build_gemini_contents_from_history}` from retained `packages/ai-gateway-core/src/ai_gateway_core/image/helpers.py`.

- Persist only artifact pointers, MIME, optional text and opaque thought signature. Never persist provider base64/file URI from a new result.
- Inflate a copied history with concurrent injected downloads; errors, missing and empty responses degrade only that visual anchor. Existing inline data is preserved. Caller must capture verified artifact owner scope; helpers grant no artifact access and do not log keys or signatures.
- Replay inlineData before legacy fileData, attach opaque thoughtSignature to the image part only, preserve text-only turns, append current user prompt.

`runtime_migration_parity_image_history` contains all13 original `tests/services/assistant/test_image_signature_flow.py` cases with corresponding names (inlineData test name normalized to inline_data). Includes1.5MB storage exclusion, unchanged input, missing artifact, five parallel downloads with original200ms bound plus a barrier ruling out sequential implementations, and two-turn signature roundtrip. Source file is eligible for retirement after root verifies all13 tests; no execution performed here.

Source activity was checked independently by Runtime and Gateway lanes: current retained application code and provider implementation contain no callers of these shared exported helpers. The source flow is an exported helper contract with tests, not an active session/provider integration. No new session persistence or provider wiring was invented during migration. Existing SQL image_turns thought_signature column alone is not claimed to prove integration.
