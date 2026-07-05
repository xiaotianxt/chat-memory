# ChatGPT Refresh And Sync Design

## Purpose

Move `chat-memory` from passive capture only to a bounded sync system that can
answer:

- What happens to a brand-new ChatGPT conversation?
- What happens when an already cached conversation changes?
- What happens to conversations that have not been opened since the local
  service started?
- What can be searched when no ChatGPT tab is open?

The system must still be local-first. The local service owns cache, schedule,
leases, rate limits, and search. The browser owns ChatGPT credentials and is the
only place allowed to make ChatGPT-origin requests.

## Product Contract

`chat-memory` has three freshness levels, not one global promise:

- **Captured-current**: a conversation detail JSON was captured from a natural
  ChatGPT page load or from an approved browser refresh lease.
- **Known-not-fetched**: the conversation was seen in a ChatGPT list/sidebar
  response, but no detail JSON has been fetched yet.
- **Unknown**: the service has not observed the conversation ID at all.

Search must be honest:

- Results come only from indexed detail snapshots.
- Doctor/status must report known conversations that are not fetched.
- If no ChatGPT tab is connected, the service cannot improve freshness. It can
  queue work but cannot execute ChatGPT requests.

## Current Limitations

The current passive userscript captures only natural conversation detail
responses:

```text
open ChatGPT conversation
  -> page fetches /backend-api/conversation/<id>
  -> MAIN hook captures response
  -> USER_SCRIPT sender posts JSON to local service
  -> local SQLite index updates
```

This misses:

- New chats whose UI never triggers a full conversation-detail response after
  the conversation ID is created.
- Existing chats updated through streaming or mutation endpoints without a
  follow-up detail fetch.
- Historical conversations that were not opened after service startup.
- Conversations created or updated in another browser/device/client.

## V2 Architecture

```text
ChatGPT page
  -> natural capture: detail JSON, list JSON, navigation IDs, mutation hints
  -> refresh executor: polls local service for one lease at a time
  -> same-origin ChatGPT fetches run only inside browser
  -> local service receives list/detail/status reports

Local service
  -> known conversation registry
  -> refresh queue
  -> endpoint budgets and cooldowns
  -> SQLite snapshots and search index
  -> honest doctor/search coverage
```

## Data Sources

### 1. Conversation Detail

Route family:

- `GET https://chatgpt.com/backend-api/conversation/{id}`

Use cases:

- Natural open of an existing conversation.
- Approved refresh lease for one known conversation ID.
- Debounced refresh after a new conversation ID appears.
- Debounced refresh after a mutation/stream completion hint.

The detail payload is the canonical indexed source.

### 2. Conversation List Metadata

Likely route family:

- `GET https://chatgpt.com/backend-api/conversations?...`

The exact query shape is ChatGPT-version dependent. The adapter must discover
and capture natural list responses first. Active list refresh can only use a
route shape that has been observed during the current page session or a
conservative default that has tests and failure reporting.

List payloads are metadata only:

- conversation ID,
- title,
- create/update times if present,
- workspace/account if known,
- archived/deleted markers if present.

List payloads do not populate the search index directly. They update
`conversations` rows and queue detail refreshes.

Privacy boundary:

- The MAIN hook may inspect a natural list response, but the USER_SCRIPT sender
  must reduce it to a metadata-only `items[]` array before posting locally.
- The sender must never forward the raw list response body.
- Metadata extraction allowlist is limited to scalar fields needed for identity
  and scheduling: `id`, `conversation_id`, `title`, `create_time`,
  `update_time`, `updated_at`, `create_time`, `is_archived`,
  `is_deleted`, and workspace/account hints if scalar.
- The sender must drop `mapping`, `messages`, `content`, `parts`, `text`,
  `attachments`, and any object/array fields not explicitly handled.
- The service must defensively reject or ignore body-bearing fields even if a
  malicious page event supplies them.

### 3. SPA Navigation

Route family:

- `https://chatgpt.com/c/{conversation_id}`

The userscript must observe navigation changes. When a `/c/{id}` route appears:

- mark the conversation as seen/opened,
- enqueue a high-priority detail refresh after a short debounce unless a detail
  snapshot was just captured,
- do not scrape visible DOM text as canonical content.

This covers new conversations where the ID appears in the URL but no detail
endpoint is naturally captured.

### 4. Mutation And Stream Hints

The userscript should detect successful same-origin responses that imply the
current conversation changed, without storing raw stream tokens:

- message send / continue / regenerate / edit endpoints,
- streamed completion end markers if observable,
- title rename/archive/delete if observable.

V2 does not index streaming tokens directly. It records a dirty hint and
enqueues a debounced detail refresh. The refresh fetches canonical JSON later.

## Core Scenarios

### A. Brand-New Conversation

Problem: a new chat may be created through a flow that does not naturally return
the full `/backend-api/conversation/{id}` detail JSON.

Design:

1. MAIN hook observes URL changes and safe same-origin responses.
2. When `/c/{id}` first appears, sender posts a metadata event:

   ```json
   {
     "kind": "navigation",
     "conversation_id": "abc",
     "url": "https://chatgpt.com/c/abc",
     "reason": "opened"
   }
   ```

3. Local service upserts a `conversations` row in `freshness_state='unknown'`
   and enqueues `refresh_queue(reason='opened', priority=100)`.
4. Browser adapter polls `/refresh/chatgpt/lease`.
5. After a debounce, service grants a single detail lease for that ID.
6. Browser adapter fetches detail JSON inside `chatgpt.com` and posts it to the
   existing ingest endpoint.

If detail fetch returns 404/403/401:

- mark `last_error`,
- increment failures,
- apply cooldown,
- keep the row as known-but-not-fetched or inaccessible based on status.

### B. Current Conversation Updated

Problem: streaming updates or edits may not cause a full detail response.

Design:

1. Natural detail captures still update immediately.
2. If a mutation/stream completion hint is observed, sender posts:

   ```json
   {
     "kind": "dirty",
     "conversation_id": "abc",
     "reason": "mutation_observed"
   }
   ```

3. Local service records the dirty marker and enqueues a detail refresh with
   short debounce, for example 2-5 seconds after the last dirty event.
4. Duplicate dirty events coalesce into one queue row.
5. Search before refresh completes may still use the last snapshot, but should
   report stale/dirty counts through doctor/status.

V2 must not index partial streaming text directly. That creates duplicate,
ordering, and privacy problems. Canonical detail JSON remains the source.

### C. Conversations Not Opened Since Service Startup

Problem: historical chats will not be cached if the user never opens them after
the service starts.

Design:

1. When any ChatGPT tab is open, the userscript captures natural conversation
   list/sidebar responses.
2. List ingest upserts known conversation rows and update markers.
3. The local service schedules a hot-set refresh:
   - latest 20 by default for V2,
   - max one detail fetch at a time,
   - minimum spacing between detail fetches, for example 750-1500 ms,
   - stop on 429 or global cooldown.
4. The browser adapter executes leases only while a ChatGPT tab exists.
5. Known conversations outside the hot set remain known-not-fetched until the
   user opens them, searches for them by title, or manually expands sync scope.

V2 should not attempt full account history crawling by default.

Hard anti-crawl caps for V2:

- At most 20 automatic detail leases per ChatGPT tab session.
- At most 60 detail leases per rolling hour.
- At most 3 failed attempts per conversation per 24 hours.
- Search-triggered enqueue may add at most 5 conversations per search.
- List active refresh may request only the first page (`offset=0`, `limit<=50`)
  or a route shape naturally observed in the current page session with
  equivalent first-page semantics.
- No automatic pagination in V2. Any later pagination feature requires a new
  design review and explicit user command.

### D. Conversations Created Or Updated Elsewhere

Problem: another device or client may create/update chats while this browser is
not seeing the detail response.

Design:

1. Periodic list refresh leases run only when a ChatGPT tab is open and the
   service budget permits.
2. List update markers enqueue detail refreshes for changed hot-set
   conversations.
3. If the conversation is not in the hot set, it is marked stale/known and
   refreshed only when:
   - user opens it,
   - user manually requests broader sync,
   - search refresh budget selects it as a candidate.

### E. No ChatGPT Tab Open

Problem: the local service cannot access ChatGPT credentials.

Design:

- Service keeps running.
- Search runs over cached data.
- Queue rows remain pending.
- Doctor/status reports `browser_adapter_connected=false` or
  `last_adapter_seen_at`.
- No ChatGPT-origin refresh happens until a ChatGPT page with userscript is
  loaded again.

Do not store ChatGPT cookies, access tokens, or browser profile secrets locally
to solve this.

## Local Service API

All endpoints bind to loopback and require the existing local ingest bearer
token unless explicitly marked health-only.

CORS/auth rules for all browser-facing endpoints:

- Browser requests must have `Origin: https://chatgpt.com`.
- Non-browser local tests may omit `Origin`; if `Origin` is present and not
  exactly `https://chatgpt.com`, reject with 403.
- Bearer token is accepted only in the `Authorization` header. Tokens in query
  strings are ignored.
- Support `OPTIONS` preflight for:
  - `/ingest/chatgpt/conversation`
  - `/ingest/chatgpt/list`
  - `/events/chatgpt`
  - `/refresh/chatgpt/lease`
  - `/refresh/chatgpt/report`
- Preflight must allow only the needed method(s) and headers:
  `Authorization, Content-Type`.
- Accepted browser responses should include
  `Access-Control-Allow-Origin: https://chatgpt.com` and `Vary: Origin`.

### Existing

- `GET /health`
- `POST /ingest/chatgpt/conversation`

### New Metadata Ingest

`POST /ingest/chatgpt/list`

Request:

```json
{
  "source": "userscript:list-capture",
  "account_id": "default",
  "workspace_id": "default",
  "url": "https://chatgpt.com/backend-api/conversations?...",
  "items": [
    {
      "id": "conversation-id",
      "title": "optional",
      "create_time": 1700000000.0,
      "update_time": 1700000100.0,
      "mapping": null
    }
  ]
}
```

Response:

```json
{
  "ok": true,
  "seen": 50,
  "upserted": 10,
  "queued": 8
}
```

Rules:

- Accept only metadata fields. If request items contain `mapping`, `messages`,
  `content`, `parts`, `text`, `attachments`, or any nested body-like object,
  ignore those fields and do not echo them in responses or errors.
- Request body size limit: 256 KiB for list ingest in V2.
- Do not create snapshots or search documents from list items.
- Upsert `conversations`.
- Set `last_seen_in_list_at`.
- If list update marker is newer than `last_fetched_at`, enqueue detail refresh.
- Response must contain counts only, never titles or raw item JSON.

### New Event Ingest

`POST /events/chatgpt`

Request:

```json
{
  "kind": "navigation|dirty|delete|archive|adapter_hello",
  "conversation_id": "optional",
  "reason": "opened|mutation_observed|stream_complete|title_changed",
  "url": "https://chatgpt.com/c/...",
  "account_id": "default",
  "workspace_id": "default"
}
```

Rules:

- `navigation` to `/c/{id}` upserts known row and queues `opened`.
- `dirty` queues debounced detail refresh.
- `delete/archive` marks visibility and removes or suppresses stale search
  documents if reliable.
- `adapter_hello` updates `last_adapter_seen_at` in a service metadata table.

### Refresh Lease

`GET /refresh/chatgpt/lease?capabilities=detail,list`

Response when no work:

```json
{
  "ok": true,
  "lease": null,
  "poll_after_ms": 5000
}
```

Response with detail work:

```json
{
  "ok": true,
  "lease": {
    "lease_id": "opaque-random",
    "type": "detail",
    "conversation_id": "abc",
    "url": "https://chatgpt.com/backend-api/conversation/abc",
    "deadline_ms": 30000
  }
}
```

Response with list work:

```json
{
  "ok": true,
  "lease": {
    "lease_id": "opaque-random",
    "type": "list",
    "url": "https://chatgpt.com/backend-api/conversations?limit=50&offset=0",
    "deadline_ms": 30000
  }
}
```

Rules:

- At most one active lease per browser adapter.
- V2 can start with one global active lease.
- Lease IDs must be unpredictable enough to prevent accidental cross-complete.
- Lease grants respect `not_before`, endpoint cooldown, retry-after, and max
  attempts.
- Leases expire if not completed.

### Refresh Report

`POST /refresh/chatgpt/report`

Request:

```json
{
  "lease_id": "opaque-random",
  "ok": false,
  "status": 429,
  "retry_after_ms": 60000,
  "error": "rate_limited"
}
```

For successful detail responses, the adapter should post the full payload to
`/ingest/chatgpt/conversation` and then report lease success. For successful
list responses, it should post to `/ingest/chatgpt/list` and then report lease
success.

Rules:

- Only `2xx` ChatGPT detail responses whose JSON body passes the existing
  `payload_guard` may be posted to `/ingest/chatgpt/conversation`.
- Only `2xx` list responses reduced to metadata allowlist items may be posted
  to `/ingest/chatgpt/list`.
- For `401`, `403`, `404`, `429`, `5xx`, network errors, HTML responses,
  invalid JSON, or JSON that fails guards, the browser adapter must not forward
  the ChatGPT response body to the local service. It may report only
  `lease_id`, status code, coarse error class, and retry-after.
- 401/403: stop active refresh and surface adapter-auth error.
- 404: mark conversation not found/inaccessible.
- 429: set endpoint/global cooldown from `Retry-After` if present, otherwise
  exponential backoff.
- Network/CORS errors: short retry, then cooldown.

## Browser Adapter Behavior

The installed userscript becomes three cooperating pieces:

1. MAIN detail/list/navigation capture:
   - observes fetch/XHR for approved route families,
   - emits safe events,
   - never sees local ingest token.
2. USER_SCRIPT sender:
   - holds local ingest token,
   - validates every event as untrusted,
   - posts detail/list/events to the local service.
3. USER_SCRIPT refresh executor:
   - polls `/refresh/chatgpt/lease`,
   - performs approved ChatGPT-origin requests,
   - posts results and reports.

### ChatGPT Credential Boundary

The local service must never receive ChatGPT cookies, access tokens, auth
headers, or session JSON.

The browser adapter may use ChatGPT credentials only transiently inside the
ChatGPT page:

- First try same-origin `fetch(url, { credentials: "include" })`.
- If ChatGPT requires an access token, a future adapter may obtain it from the
  page/session endpoint in memory only and use it only for the approved request.
- V2 implementation should prefer ambient credential fetch first and report
  401/403 rather than broadening credential handling silently.

No ChatGPT credential value may be posted to the local service or logged.

Non-2xx body boundary:

- The browser adapter must treat all non-2xx ChatGPT response bodies as
  sensitive and discard them unread or after status-only classification.
- Local service report endpoints must reject optional fields such as
  `body`, `response`, `html`, `json`, `headers`, `authorization`, `cookie`, or
  `accessToken` if present.

### Route Allowlist

Active refresh may fetch only:

- `/backend-api/conversation/{id}`
- `/backend-api/conversations?...`

Reject:

- any third-party origin,
- any extra path segment under conversation detail,
- files, textdocs, analytics, product surfaces,
- arbitrary URLs supplied by page events.

The local service constructs lease URLs. The browser adapter must still verify
the URL before fetching.

### Budgets

Default V2 budgets:

- detail refresh: one in flight globally,
- detail spacing: at least 1000 ms between attempts,
- hot-set size: latest 20 known conversations,
- list refresh: no more than once every 5 minutes while a ChatGPT tab is open,
- backoff on 429: honor `Retry-After`, otherwise 1m, 5m, 15m, 60m.
- session detail cap: 20 leases per adapter tab session,
- hourly detail cap: 60 leases,
- per-conversation failed-attempt cap: 3 per 24 hours,
- search enqueue cap: 5 stale candidates per search.

Search should not trigger a full crawl. Search may enqueue stale candidates and
tell the user that refresh is pending.

## Database Changes

Existing tables already have many fields needed by V2. Add only narrow support:

Migration rules:

- `ensure_chatgpt_schema` must remain additive and non-destructive.
- Existing deployed databases may have tables created by older versions. The
  implementation must inspect `PRAGMA table_info(<table>)` and add missing
  nullable/defaulted columns with `ALTER TABLE`.
- Do not drop or rebuild user tables during normal startup.
- Existing `refresh_queue(conversation_pk, reason)` remains the per-conversation
  detail queue. Do not put list work into it because `conversation_pk` is
  non-null.

### `service_state`

Key/value metadata:

- `last_adapter_seen_at`
- `last_list_refresh_at`
- `global_cooldown_until`

### `refresh_leases`

- `lease_id`
- `conversation_pk` nullable for list leases
- `lease_type`: `detail` or `list`
- `url`
- `granted_at`
- `deadline_at`
- `completed_at`
- `status`: `active`, `succeeded`, `failed`, `expired`
- `last_error`

### `conversations`

Use existing columns where possible:

- `last_seen_in_list_at`
- `last_fetched_at`
- `freshness_state`
- `retry_after_at`
- `consecutive_failures`
- `visibility_state`

If missing from implementation, add with migrations rather than rebuilding the
schema destructively.

### List Lease State

List refresh is synthetic work derived from `service_state`, not a row in
`refresh_queue`.

Required keys:

- `last_list_refresh_at`
- `list_cooldown_until`
- `observed_list_route`

The lease scheduler may grant a list lease only when:

- a browser adapter has sent `adapter_hello` recently,
- `last_list_refresh_at` is older than the configured interval,
- no global/list cooldown is active,
- the request fits V2 first-page/no-pagination caps.

## CLI Surface

V2 should keep CLI small:

```bash
chat-memory chatgpt-doctor
chat-memory chatgpt-sync status
chat-memory chatgpt-sync enqueue --hot 50
chat-memory chatgpt-sync enqueue --conversation <id>
```

Implementation can start with:

- `chatgpt-sync status`: counts known, fetched, queued, active leases, adapter
  last seen, cooldown.
- `chatgpt-sync enqueue --hot N`: enqueue detail refreshes for the latest N known
  conversations.

Do not add a command that directly fetches ChatGPT from the local service.

## Implementation Slices

### Slice 1a: Server Metadata/Event Core

- Add `/ingest/chatgpt/list`.
- Add `/events/chatgpt`.
- Upsert known conversations without snapshots.
- Queue opened/dirty/list-delta detail refresh rows.
- Add doctor/status counts for known-not-fetched and queued rows.
- Add endpoint CORS/token/body-limit tests.
- Do not change userscript yet.

### Slice 1b: Userscript Natural Metadata Capture

- Extend userscript to capture natural list responses and SPA navigation.
- Sender must reduce list responses to metadata allowlist items before posting.

This slice improves visibility and handles new chat IDs as soon as navigation
reveals them, but active background fetch may still be absent.

### Slice 2a: Server Lease Core

- Add `/refresh/chatgpt/lease` and `/refresh/chatgpt/report`.
- Add `refresh_leases` and simple global budget.
- Unit-test lease selection, active lease exclusion, 429 cooldown, body-field
  rejection, and old-DB migration.

### Slice 2b: Userscript Detail Lease Executor

- Extend userscript sender with a refresh executor loop.
- Execute one approved detail fetch at a time inside ChatGPT page.
- Ingest detail payload through existing conversation ingest.
- Do not implement list active refresh yet.

This slice handles new chats and stale open/current chats even when natural
detail capture is missing.

### Slice 3: Hot-Set List Refresh

- Capture natural list route shape.
- Add list lease with conservative route.
- Enqueue latest N known conversations.
- Add cooldown/backoff for 429.
- Enforce first-page/no-pagination and session/hourly caps.

This starts covering conversations not opened since service startup, provided a
ChatGPT tab is open.

### Slice 4: Search-Time Freshness

- Search reports coverage and stale counts.
- Optional `--refresh` enqueues stale candidates and waits up to a small budget.
- Results include snapshot age/freshness labels.

## Acceptance Tests

Unit tests:

- List route matcher accepts only `/backend-api/conversations` on
  `https://chatgpt.com`.
- List ingest upserts metadata and does not create snapshots/search documents.
- List ingest with `mapping`, `messages`, `content`, `parts`, `text`, or nested
  body fields does not store or echo them.
- Navigation event to `/c/{id}` upserts a known row and enqueues one opened
  refresh row.
- Dirty events coalesce into one refresh queue row with updated `not_before`.
- Lease grant returns one due detail job and creates one active lease.
- A second lease call while one active lease exists returns no work.
- Lease report 429 sets cooldown/retry-after.
- Cooldown prevents further lease grants until `not_before`.
- Userscript sender validates lease URLs before fetching.
- No ChatGPT credential fields are accepted in local ingest/report bodies.
- All new endpoints enforce bearer auth, ChatGPT Origin, and CORS preflight.
- Old pre-v2 SQLite schemas are migrated additively.
- Non-2xx ChatGPT response body is never forwarded in simulated userscript
  helper code.
- New-chat URL first appears, initial detail fetch fails 404, later dirty event
  requeues successfully after cooldown.

Manual E2E:

1. Start Homebrew service.
2. Install userscript against service token.
3. Open ChatGPT homepage.
4. Verify list metadata increases known conversation count without creating
   search docs.
5. Create a new chat and wait for navigation to `/c/{id}`.
6. Verify queue receives opened/dirty job.
7. Verify lease executor fetches detail JSON and search can find a user-provided
   known term.
8. Stop ChatGPT tab and verify pending queue remains visible but no refresh
   executes.

## Non-Goals

- No full unbounded history crawler.
- No local storage of ChatGPT cookies, access tokens, auth headers, or session
  JSON.
- No indexing of partial streaming text in V2.
- No DOM text scraping as canonical conversation content.
- No refresh execution when no logged-in ChatGPT tab is open.
- No bypassing ChatGPT rate limits or permission boundaries.

## Definition Of Done

- `cargo fmt --all -- --check`, `cargo test`,
  `cargo clippy --all-targets -- -D warnings`, and `cargo build` pass.
- Existing passive capture behavior still works.
- New chat navigation creates a known row and queues refresh.
- Open ChatGPT tab can execute a lease and populate a detail snapshot.
- Doctor/status makes known-not-fetched, queued, active lease, cooldown, and
  adapter-connected states visible.
- Logs and diagnostics do not contain conversation bodies, ChatGPT credentials,
  local ingest tokens, or generated token-bearing script code.
