# Native connector search extensions

The existing authenticated `POST /api/v1/connectors/google_drive/search` endpoint
now executes the Google Drive v3 files API. The Google Drive provider already
exists in migration 046; the normal connector administrator setup, OAuth consent,
encrypted user credential storage and refresh flow remain required. Search
resolves the requesting tenant/user's connected credential before making a
request. No application credentials are embedded or created by this extension.

Request JSON uses the existing `query` and `limit` fields. Google Drive queries
require 1 to 4096 characters and a limit from 1 to 100. The text is escaped as a
Drive `fullText contains` literal, with trashed files excluded. Results search
the authenticated user's corpus, including accessible shared-drive items, and
are ordered by modified time. They contain `id`, `title`, `url`, `type`,
`mime_type`, `date`, `size`, and a description excerpt of at most 500 characters.
Descriptions are metadata, not downloaded document contents. Access is bounded
by the existing OAuth grant and Google Drive's file permissions.

The adapter follows opaque page tokens, deduplicates file IDs, and stops at the
requested result count or the final page. Empty pages do not imply completion.
Repeated/invalid cursors, an incomplete-search flag and a ten-page work ceiling
fail explicitly rather than silently returning an incomplete search. Non-200
provider responses preserve status with a fixed safe error message. Provider
bodies, credentials and diagnostics are never included in errors. All provider
I/O stays inside the request future; no detached work or external mutations occur.

References read during implementation:
- https://developers.google.com/workspace/drive/api/guides/search-files
- https://developers.google.com/workspace/drive/api/reference/rest/v3/files/list

Compilation, unified regression and authenticated provider acceptance are pending.
