---
name: space-writer
description: Draft or update a page in a knowledge space when the user asks to write, append, or organize space content.
compatibility: Requires a connected knowledge space and approval for write tools.
allowed-tools:
  - space_search
  - space_read_page
  - space_create_page
  - space_append_blocks
---

Use this skill when the user wants a page created or extended inside a knowledge space.

1. Search the space with `space_search` before creating a duplicate page.
2. Read the current page with `space_read_page` when appending.
3. Create a page with `space_create_page` only after approval.
4. Append with `space_append_blocks` only after approval.
5. Keep user-provided facts unchanged and do not invent citations.
