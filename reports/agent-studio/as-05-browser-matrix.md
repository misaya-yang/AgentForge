# AS-05 Browser, Accessibility, and Runtime Matrix

- Phase: AS-05 — Agent Studio Frontend and Preview
- Feature: AS-F006
- Date: 2026-07-19
- Deterministic browser result: 25 passed, 0 failed, 0 skipped
- Existing-route regression result: 31 passed, 0 failed, 0 skipped
- Live repository-owned stack result: normal flow 1 passed; flag-off rollback 1 passed; final normal flow 1 passed
- Browsers: Playwright Chromium, desktop/tablet/mobile viewports

## Route and State Matrix

| Route/surface | Viewport | State or interaction | Verified result | Durable evidence |
| --- | --- | --- | --- | --- |
| `/agents` | 1440x900 | populated directory | Three Agents render with role/status/update facts, search and filters; keyboard focus reaches Create; no horizontal overflow | `reports/agent-studio/as-05/matrix/directory-populated-desktop-1440x900.png` |
| `/agents` | 390x844 | populated directory | Mobile cards replace the table without losing actions or filters; no horizontal overflow | `reports/agent-studio/as-05/matrix/directory-populated-mobile-390x844.png` |
| `/agents` | 1440x900, 390x844 | loading | Skeleton/status is announced and no invented Agent data appears before the response | `directory-loading-desktop-1440x900.png`, `directory-loading-mobile-390x844.png` |
| `/agents` | 1440x900, 390x844 | true empty | Blank, support and Knowledge controlled starters are reachable; no fake rows are shown | `directory-empty-desktop-1440x900.png`, `directory-empty-mobile-390x844.png` |
| `/agents` | 1440x900, 390x844 | filtered empty | Clear-filter recovery returns the real empty or populated directory state | `directory-filtered-empty-desktop-1440x900.png`, `directory-filtered-empty-mobile-390x844.png` |
| `/agents` | 1440x900, 390x844 | API error | Stable retryable error is shown and no stale success is implied | `directory-api-error-desktop-1440x900.png`, `directory-api-error-mobile-390x844.png` |
| `/agents` | 1440x900, 390x844 | permission denied | Create/action controls are absent and a bounded access message is shown; the UI is not used as the authorization source | `directory-permission-desktop-1440x900.png`, `directory-permission-mobile-390x844.png` |
| `/agents` | desktop | copy/archive | Copy calls the API and opens a new editable identity; owner-only archive requires confirmation and renders Archived after success | deterministic E2E test 7 |
| `/agents` | 390x844 | mobile role actions | Owner Copy/Archive work; Editor Copy remains enabled with Archive denied; Viewer Copy/Archive are visibly denied; link and action button are separate accessible controls; the open overlay stays inside the viewport without document overflow | `directory-role-actions-mobile-390x844.png` and deterministic E2E test 8 |
| `/agents` | desktop and live built container | feature flag off | Agent navigation and Agent routes disappear while `/assistant` and the existing protected shell/composer remain | deterministic E2E test 9 and live rollback 1/1 |
| `/agents/new` | 1440x900 | blank identity | Required fields, public HTTPS icon validation and disabled Continue state are visible and associated | `create-blank-identity-desktop-1440x900.png` |
| `/agents/new` | 1440x900 | behavior | Instructions, welcome content and default Qwen model remain controlled form state | `create-behavior-desktop-1440x900.png` |
| `/agents/new` | 1440x900 | start/review | UI states that only a Draft is created and nothing is published | `create-start-desktop-1440x900.png` |
| `/agents/new` | 1440x900 | controlled template | Template copies prompt/defaults only; capabilities, Knowledge, credentials, sessions and memory authority remain empty | `create-controlled-template-desktop-1440x900.png` |
| `/agents/new` | 1440x900 | API failure | Name/description and other non-secret local fields survive a retryable 503 | `create-api-failure-desktop-1440x900.png` |
| `/agents/:agentId` | 1440x900 | clean/dirty/saving/saved | One ETag PUT atomically saves metadata/spec/revision; explicit state and revision labels match server mutations; no Studio metadata PATCH occurs | `studio-clean`, `studio-dirty`, `studio-saving`, `studio-saved` screenshots plus API/real-PostgreSQL tests |
| `/agents/:agentId` | 1440x900 | 422 field error | First invalid field receives focus, server message is associated, and metadata/Draft remain at the prior saved batch | `studio-field-error-desktop-1440x900.png` |
| `/agents/:agentId` | 1440x900 | 409 conflict | Current server revision and local changed fields are named; Copy, Reload and Reload-and-reapply remain available; local metadata is not partially committed | `studio-conflict-desktop-1440x900.png` |
| `/agents/:agentId` | 1440x900 | network failure | Failed save never renders Saved; local edits remain, server metadata stays unchanged and Retry save reaches a complete successful mutation | `studio-network-error-desktop-1440x900.png` |
| `/agents/:agentId` | 1440x900 | in-flight edit | A second edit made during the first save batch remains dirty and is persisted only by the second save | deterministic in-flight save test |
| `/agents/:agentId` | 1440x900 | degraded Viewer | Existing selections remain visible, the unavailable catalog is named, editing is disabled, Preview remains allowed | `studio-degraded-viewer-desktop-1440x900.png` |
| `/agents/:agentId` | desktop | Editor | Editor atomically saves the description as Draft r9, starts an isolated Draft r9 Preview and receives its streamed response; directory Copy remains enabled while Archive is denied | deterministic Editor test and harness revision/session counters |
| `/agents/:agentId` | 1024x768 | tablet | Section navigation, configuration and Preview remain simultaneously reachable with no overflow | `studio-tablet-1024x768.png` |
| `/agents/:agentId` | 390x844 | mobile Configure | Real Tab/Enter opens the section Drawer; Tab/Shift+Tab stay contained, Escape returns focus, and keyboard selection closes/returns focus; capability tabs remain reachable without overflow | `studio-mobile-configure-390x844.png`, `studio-mobile-section-drawer-390x844.png` |
| `/agents/:agentId` | 390x844 | mobile Preview | Configure is hidden, Preview is visible, and Test in Preview moves focus to the message field | `studio-mobile-preview-390x844.png` |
| `/agents/:agentId` | all | reduced motion | `prefers-reduced-motion: reduce` removes the Agent Studio animation duration | deterministic reduced-motion test |

All screenshot basenames in the table resolve beneath
`reports/agent-studio/as-05/matrix/` unless an explicit path is shown.
The capture helper rejects any `-WIDTHxHEIGHT` filename whose labelled size
does not match the actual Playwright viewport. Full-page raster height may
exceed the viewport height; the labelled viewport itself is exact.

## Preview Matrix

| Case | Expected truth | Observed result | Evidence |
| --- | --- | --- | --- |
| Saved Draft | Preview names `Draft r8`, starts an isolated session and uses the saved spec | New session receipt names Draft r8; effective model/capability/Knowledge counts are derived from the saved response | `preview-draft-events-desktop-1440x900.png` |
| Unsaved edit | Unsaved form fields do not change the active runtime | Warning says Preview still uses saved Draft r8; current session/history remains pinned | deterministic Draft/Version Preview E2E |
| Immutable Version | Historical Version uses its own resolved spec | Version 7 shows model `qwen3.7-max`, zero configured capabilities and zero Knowledge sources | `preview-version-desktop-1440x900.png` |
| Target switch | Version/Draft selection cannot hot-swap an active session | Confirmation is required and a separate Version session is started; returning to Draft restores its prior isolated history | deterministic Draft/Version Preview E2E |
| Draft revision change | A changed saved revision cannot reuse the previous Draft session | Preview clears/rebuilds its Draft target on revision change | deterministic E2E save/Preview fixtures |
| Tool and RAG events | User sees effective events without protected internals | Server-side closed projection emits only public tool/status/duration and Dataset/count fields. A malicious raw-SSE test proves nested credential-shaped data, results, arguments, metadata, output files and arbitrary errors are absent before React sees the response. | deterministic Preview E2E plus Assistant resolver/raw-SSE tests 11/11 |
| Clear and Trace | Session lifecycle is explicit | Clear removes the session; Trace link remains unavailable until a run supplies a trace ID | deterministic and live browser checks |
| Stable failures | Configuration, resource, permission, provider and runtime failures are distinguishable | Five deterministic scenarios render stable user-safe labels and omit internal payloads | named deterministic Preview-failure cases |
| Real local provider boundary | Missing provider configuration cannot be presented as a model success | Repository-owned live stack shows `Preview failed — Model unavailable: Agent model is unavailable` | `reports/agent-studio/as-05/studio-desktop.png`, `studio-tablet.png`, `studio-mobile.png` |

The machine-readable counterpart is
`reports/agent-studio/as-05-preview-golden.json`.

## Accessibility, Focus, Console, and Network

- Real `@axe-core/playwright` scans ran on the Agent directory, creation flow,
  Studio/Preview and mobile Studio. Every scan returned zero serious or
  critical violations.
- Create template activation uses real Tab/Space and step transition uses
  Tab/Enter. Field-error focus, mobile Preview
  focus, Drawer Tab/Shift+Tab containment, Escape close, trigger focus return,
  keyboard section selection, dialog, tab, form-label and status semantics are
  asserted in the passing suite.
- Color is supplemented by text, icons and ARIA state for save, role, risk,
  health, permission, warning and error states.
- Desktop, tablet and mobile assertions compare document scroll width to client
  width; all required viewports passed.
- Happy-path tests listen for `pageerror`, console error and unexpected API
  responses. No unexpected browser error or failed happy-path Agent API request
  was observed. Deliberate 403/409/422/500/503 fixtures are asserted in their
  dedicated negative scenarios and are not counted as clean-network receipts.
- User-visible Agent copy is stored in synchronized English and Chinese locale
  bundles. The Chinese route test and repository i18n key checker both pass.
- Template/recovery assertions reject browser-held API key, access-token,
  refresh-token, secret and OAuth fields; provider and local runtime secrets are
  never placed in fixture templates, screenshots or this report.

## Live Stack Receipt

The live test ran against `http://127.0.0.1:8081` and the repository-owned
Gateway at `http://127.0.0.1:8080`. It created a real Draft, atomically saved a
new name, description, spec and revision 2, reloaded and observed the complete
saved batch, attempted an isolated Preview, captured all three viewports and
deleted the Agent in `finally`. A PostgreSQL follow-up query returned `0`
non-deleted Agents whose names begin with `AS05 Live`.

The same built frontend was then recreated with
`VITE_AGENT_STUDIO_ENABLED=false`; its runtime config returned false and a
remote browser proved Agent routes/navigation absent while the existing
Assistant composer remained visible. The container was restored with true and
the full live Agent flow passed again. All eight Compose services remained
healthy at about 763 MiB total.

This is local development evidence. No external-provider success, production
deployment, load test, Hosted page, Embed or Runtime API claim is made.
