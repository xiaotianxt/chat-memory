# ChatGPT Local Search Cache Design

## Goal

Build a local-first search layer for ChatGPT web conversations that is more
reliable than ChatGPT's built-in `Search chats` UI.

The system must make two things explicit:

1. Search quality: full-text search should run over cached conversation JSON,
   not over ChatGPT's remote conversation-search index.
2. Freshness: search results must report whether they were produced from a
   verified-fresh conversation snapshot, a stale snapshot, or a partial index.
3. Scope: "latest" is a tiered SLA, not an absolute global guarantee.

This is not a replacement for ChatGPT. It is a local cache and indexer that uses
the user's existing logged-in browser session.

## Non-Goals

- Do not depend on unsupported ChatGPT APIs for long-lived unattended scraping
  without backoff and failure visibility.
- Do not print, export, or sync raw conversation text to remote services.
- Do not try to bypass account permissions, rate limits, or browser session
  boundaries.
- Do not make ChatGPT's own search UI appear fixed; expose a separate local
  search surface.

## Observed Problem

ChatGPT's built-in `Search chats` is a remote/indexed history search. It can
return snippets from some conversations, but it is not guaranteed to scan the
currently visible conversation JSON. It can miss terms that are visibly present
in the current chat, and browser state can briefly show a selected sidebar item
whose main content is not the same conversation.

The durable fix is to own a local canonical snapshot and index, then make
freshness and coverage visible.

## Architecture

```
ChatGPT page
  |
  |  browser UserScript observes fetch/XHR/navigation and safe page events
  v
Capture Bridge
  |
  |  browser refresh adapter executes approved ChatGPT-origin refresh jobs
  v
Refresh Executor
  |
  |  sends metadata and JSON snapshots to local cache service
  v
Local Cache Service
  |
  |-- Object Store: raw normalized conversation JSON
  |-- SQLite Metadata: conversation freshness, cursors, errors
  |-- Search Index: FTS/inverted index over messages and titles
  v
Search UI / CLI / bro tool
```

The UserScript is a capture mechanism, not the database. The durable index
should live outside the browser page so it can survive page reloads, browser
profile changes, and ChatGPT frontend churn.

Important boundary: the local service owns scheduling, budgets, leases, and
backoff, but it does not own ChatGPT credentials. ChatGPT-origin refresh
requests must run inside an already logged-in browser context.

## Components

### 1. UserScript Capture Layer

Installed with bro `userscripts_register` on `https://chatgpt.com/*`.

Responsibilities:

- Hook `fetch` and `XMLHttpRequest` responses for ChatGPT conversation routes.
- Observe SPA navigation to `/c/{conversation_id}` and conversation-list loads.
- Capture only metadata and JSON payloads needed for the local cache.
- Post captured payloads to a loopback local service, for example
  `http://127.0.0.1:<port>/ingest`.
- Never log auth headers, cookies, access tokens, or raw request headers.

It should capture:

- Conversation detail responses.
- Conversation list/sidebar responses when available.
- Streamed conversation updates when a message is added.
- Delete/archive/rename signals if observable.

It should not:

- Store large blobs inside the page itself.
- Perform bulk crawling from the page.
- Retry aggressively from the browser context.
- Depend on class names, React internals, or visible DOM text as the canonical
  source.

Injection guidance:

- Use page `MAIN` world only for fetch/XHR hooks that must see page-world
  network calls.
- Keep parsing and persistence in the UserScript's isolated/user-script side
  when possible.
- Keep the page-world hook as a narrow bridge: copy safe response payloads and
  route metadata, then hand off to the local service.
- If ChatGPT frontend or CSP changes block reliable page-world hooks, fall back
  to bro/CDP network capture for diagnostics and update the route adapter.

### 2. Browser Refresh Adapter

The browser refresh adapter is the only component allowed to execute ChatGPT
origin requests.

Valid implementations:

- UserScript running in the ChatGPT page.
- bro/CDP automation attached to a logged-in ChatGPT tab.
- Browser extension background script, if a future implementation needs a more
  reliable execution context.

Responsibilities:

- Ask the local service for a refresh lease before making any ChatGPT request.
- Execute only approved route-family requests within the granted budget.
- Return raw response payloads and route metadata to `/ingest`.
- Report status codes and retry headers without storing auth headers.
- Stop immediately when the local service revokes budget or reports cooldown.

It must not:

- Store cookies, auth headers, access tokens, or session tokens.
- Let every open ChatGPT tab refresh independently.
- Run its own unbounded queue.
- Fetch conversations that the local service did not approve.

Multi-tab rule:

- All tabs coordinate through the local service.
- A `(conversation_pk, route_family)` refresh requires a lease.
- Leases are short-lived and single-flight, so duplicate tabs cannot stampede
  the same endpoint.

### 3. Local Cache Service

The local service owns policy:

- Deduplication.
- Schema migration.
- Rate limiting.
- Refresh scheduling.
- Search index rebuilds.
- CLI/search API.
- Refresh leases for browser adapters.

This can be integrated into the existing `chat-memory` Rust CLI as a daemon
mode, or implemented as a small separate local service that shares the same
SQLite database.

Use loopback only. Bind to `127.0.0.1`, require a random local bearer token, and
store that token in a local file with owner-only permissions. The UserScript
should receive only that scoped ingest token, not ChatGPT credentials.

The local service never stores ChatGPT cookies, auth headers, access tokens, or
session tokens. If it needs a fresh remote payload, it schedules a job and waits
for a browser refresh adapter to execute that job in the logged-in browser
context.

### 4. Object Store

Store canonical snapshots separately from the search index.

Recommended first version:

- SQLite table for compressed JSON blobs, or
- OPFS/flat files managed by the local service with SQLite metadata.

Prefer SQLite blobs first unless snapshots become large enough that VACUUM and
write amplification become a real problem.

Canonical snapshot invariant:

- One row per `(conversation_pk, version_hash)`.
- A conversation's current pointer references the latest accepted snapshot.
- The search index is derived from the current pointer, not from arbitrary raw
  blobs.

Browser-only fallback:

- IndexedDB can be a no-daemon mode for prototypes or machines where the local
  service is unavailable.
- In that mode, store raw JSON in an IndexedDB object store and maintain a small
  n-gram index in IndexedDB.
- The UI must mark this mode as less durable because browser site storage can be
  cleared and migrations are harder to audit.
- Do not make SQLite WASM + OPFS the default until local service deployment is
  proven unacceptable.

### 5. Search Index

Use SQLite FTS5 first.

Reasons:

- It matches the existing `chat-memory` shape.
- It is inspectable and easy to rebuild.
- It handles Chinese text adequately with trigram-style indexing if configured
  intentionally.
- It avoids shipping a large browser-side WASM dependency before the storage
  and freshness model are proven.

Index granularity:

- `conversation` row: title, create/update times, workspace, archived/deleted
  flags, last known server update marker.
- `message` row: message id, role, parent id, create time, update time, text,
  content type, status, source snapshot hash.
- `search_document` row: chunk-level searchable text derived from messages,
  title, and optional metadata.
- `message_fts` row: message/chunk text plus title/context fields.

For Chinese and mixed-language search, the first practical option is a trigram
tokenizer or parallel n-gram field. Exact substring fallback should remain
available for short queries such as `电影`.

Do not make WASM search the first implementation. WASM SQLite may be useful for
an all-browser prototype, but a local Rust service plus SQLite FTS is easier to
secure, inspect, back up, and integrate with existing local tools.

## Data Model

### `accounts`

- `account_id`: local stable id, not necessarily an OpenAI user id.
- `label`: optional display label.
- `created_at`, `last_seen_at`.

### `workspaces`

- `workspace_id`: remote workspace id if known, else local placeholder.
- `account_id`.
- `label`.
- `last_seen_at`.

### `conversations`

- `conversation_pk`: local surrogate primary key.
- `account_id`.
- `workspace_id`.
- `remote_conversation_id`.
- `title`.
- `created_at_remote`.
- `updated_at_remote`: best server-provided update time if available.
- `last_message_at_remote`: best message timestamp observed.
- `last_seen_in_list_at`: when sidebar/list last mentioned it.
- `last_fetched_at`: when detail JSON was last fetched.
- `last_indexed_at`.
- `current_snapshot_hash`.
- `freshness_state`: `fresh`, `stale`, `unknown`, `deleted`, `error`.
- `priority_bucket`: `hot`, `warm`, `cold`.
- `etag` / `last_modified` / `remote_version`: nullable validators if exposed.
- `last_error`, `retry_after_at`, `consecutive_failures`.
- `visibility_state`: `active`, `archived`, `deleted`, `inaccessible`,
  `unknown`.

Unique identity:

- `unique(account_id, workspace_id, remote_conversation_id)`.
- Never join, merge, or deduplicate conversations by `remote_conversation_id`
  alone.

### `conversation_snapshots`

- `snapshot_hash`.
- `conversation_pk`.
- `fetched_at`.
- `schema_version`.
- `source`: `capture`, `manual_refresh`, `background_refresh`.
- `json_blob`.
- `json_size_bytes`.
- `message_count`.
- `max_message_time`.

Constraint:

- `snapshot_hash` must reference the same `conversation_pk` as
  `conversations.current_snapshot_hash`.

### `messages`

- `conversation_pk`.
- `message_id`.
- `parent_message_id`.
- `role`.
- `content_type`.
- `created_at_remote`.
- `updated_at_remote`.
- `text`.
- `text_hash`.
- `snapshot_hash`.
- `is_current`.

Constraint:

- `unique(conversation_pk, message_id)`.
- Do not assume `message_id` is globally unique across accounts/workspaces.

### `search_documents`

- `doc_id`.
- `conversation_pk`.
- `message_id`.
- `chunk_ord`.
- `title`.
- `text`.
- `text_ngram`.
- `indexed_at`.
- `snapshot_hash`.

Chunking rules:

- Keep short messages as one document.
- Split long assistant answers into stable chunks by structural boundaries
  first, then by size.
- Preserve offsets so snippets can link back to the source message.

### `message_fts`

FTS virtual table over:

- `conversation_pk`
- `message_id`
- `title`
- `role`
- `text`
- `text_ngram`

### `refresh_queue`

- `conversation_pk`.
- `reason`: `opened`, `search_candidate`, `hot_recent`, `list_delta`,
  `manual`, `stale_before_search`.
- `priority`.
- `not_before`.
- `attempt_count`.

### `tombstones`

- `conversation_pk`.
- `account_id`.
- `workspace_id`.
- `remote_conversation_id`.
- `last_known_title`.
- `deleted_or_inaccessible_at`.
- `reason`: `deleted`, `archived`, `permission_denied`, `not_found`.

Tombstones prevent stale indexed results from pretending a deleted or
inaccessible conversation is current.

## Freshness Model

Freshness is the core product contract.

The system must never promise "all ChatGPT history is latest" unless it has just
refreshed all known conversations within budget and without error. That will
usually be impossible. Instead, freshness is scoped.

### Freshness SLA

| Scope | Target behavior | Failure mode |
| --- | --- | --- |
| Current open conversation | Strong refresh before final search ranking when route is known | Result marked `current-refresh-failed` with last cached timestamp |
| Recent hot set | Refresh list metadata, then detail-refresh changed/stale candidates within budget | Results marked `hot-partial` if budget or cooldown stops refresh |
| Warm history | Use cached index; opportunistically refresh matching titles/snippets | Results marked `stale-possible` |
| Cold/archive history | Cached and eventually consistent only | Results marked `archive-cached` |

Default hot set:

- last 7 to 14 days,
- latest 200 conversations,
- currently open conversation,
- conversations recently searched or clicked.

Definitions:

- `fresh`: detail JSON fetched after the latest known list/sidebar update marker,
  or fetched during the current visible page session after the page displayed the
  conversation.
- `stale`: cached JSON exists but a newer list/sidebar marker or local TTL says
  it may be outdated.
- `unknown`: no detail snapshot exists or no reliable update marker is known.
- `partial`: search was run against an index that excludes some known
  conversations.
- `verified`: the result's source snapshot was refreshed during this search or
  is newer than the latest known remote update marker.

Search must show coverage:

- Number of conversations indexed.
- Number known but stale.
- Number known but never fetched.
- Whether hot conversations were refreshed before ranking.
- Search refresh budget used and skipped counts.
- Any global cooldown or endpoint-specific backoff state.

## Sync Strategy

### Capture-First

When the user naturally opens ChatGPT:

1. UserScript captures conversation list metadata.
2. It marks seen conversations and update markers.
3. UserScript captures detail JSON for any opened conversation.
4. Local service indexes the detail snapshot immediately.

This path is low-risk because it piggybacks on normal user behavior.

### Search-Time Refresh

When the user searches:

1. Tokenize/normalize the query.
2. Refresh the current open conversation if visible and route adapters are
   healthy.
3. Refresh recent conversation-list metadata.
4. Search the current local index immediately for fast provisional results.
5. Select refresh candidates:
   - conversations from the last 7 to 14 days,
   - conversations whose titles/list snippets match the query,
   - conversations with stale/unknown freshness,
   - currently open conversation, if any.
6. Refresh candidates under a strict budget.
7. Browser refresh adapters execute leased refresh jobs in the logged-in
   browser context.
8. Re-run search and mark results as `verified`, `possibly stale`, or
   `partial`.

Default budget:

- Recent hot set: last 7 days or latest 200 conversations.
- Per-search detail refresh: 10 to 30 conversations.
- Global concurrency: 1 to 2 requests.
- Stop immediately on 401/403/429 or repeated 5xx.

### Background Refresh

Optional, conservative:

- Refresh hot conversations while ChatGPT is open and idle.
- Use exponential backoff.
- Never run unbounded catch-up in the page.
- Prefer list endpoints to discover update markers before fetching details.

### Cross-Client Updates

The browser cannot receive reliable push events for conversations changed on
another device unless ChatGPT exposes them in the currently loaded web app.

Therefore, cross-client sync is poll-based:

- Refresh conversation list when ChatGPT page is opened or focused.
- Refresh hot list before search.
- Treat any conversation whose list update marker exceeds `last_fetched_at` as
  stale.
- If no reliable update marker exists, use TTL policy:
  - hot: stale after 15 minutes,
  - warm: stale after 24 hours,
  - cold: stale after 7 days.

This is not "always real-time"; it is "search-time latest within an explicit
budget." If the budget is exhausted, the UI must say so.

The strongest honest claim is:

- current conversation: latest if refresh succeeds,
- hot set: latest within search budget,
- full history: locally indexed, eventually consistent.

## Rate Limit and Load Discipline

Rules:

- One owner controls refresh budget and leases: local service, not every browser
  tab.
- One owner executes ChatGPT-origin network calls: a browser refresh adapter in
  the logged-in browser context.
- Single-flight per `(conversation_pk, route_family)`.
- Token bucket for ChatGPT-origin requests.
- Honor `Retry-After` when visible.
- Persist failure state and `retry_after_at`.
- Use jittered exponential backoff.
- Do not retry 401/403 until user reloads or reauthenticates.
- Treat 429 as a global cooldown.

Search-time refresh must degrade gracefully:

- Return existing local results with a stale warning.
- Show "N conversations skipped due to cooldown."
- Never hide rate-limit errors as "no results."
- Never convert an inaccessible/deleted conversation into a live result without
  a successful detail refresh.

## Query Semantics

Support three search modes:

1. Exact substring: best for Chinese short terms like `电影`.
2. Token/FTS: best for English and longer mixed queries.
3. Hybrid ranking: exact title match, exact message substring, FTS BM25, recency.

Correctness rule:

- CJK queries below a configured threshold, for example fewer than 4 CJK
  characters, must run exact substring verification against canonical
  `search_documents.text` or message text.
- FTS/BM25 may recall candidates and influence ordering, but it must not be the
  only proof of a short CJK hit.
- If trigram/ngram FTS is unavailable or corrupted, exact substring search must
  still return correct hits for short queries, albeit slower.

For browser-only fallback, use a lightweight local inverted index:

- Chinese: character bigram/trigram plus exact substring verification.
- English: lower-case tokenization plus phrase verification.
- Ranking: title hit, exact substring hit, recency, term density.

For local-service mode, SQLite FTS5 is the primary index and exact substring is
the correctness fallback for short queries.

Ranking should favor:

- exact title hit,
- exact message substring hit,
- recent conversations,
- currently open conversation,
- lower staleness,
- denser hit snippets.

The result object must include:

- conversation title,
- conversation id,
- message id,
- snippet,
- timestamp,
- freshness state,
- snapshot fetched time,
- direct ChatGPT URL.

## Privacy and Security

Conversation text is private user data.

Requirements:

- Local-only storage by default.
- Database file permissions owner-only where possible.
- No secret values in logs.
- No raw cookies/auth headers stored.
- Loopback service requires a local token.
- UserScript can only ingest to the local service; it cannot read arbitrary local
  files.
- Redact request headers and URL query parameters in diagnostics unless
  explicitly needed.
- Provide a command to delete cache by account/workspace/conversation.
- Provide a pause switch that stops capture without uninstalling the script.
- Partition all data by account/workspace. Never merge workspaces solely by
  conversation id.
- Logs should include route family and error class, not raw message text by
  default.

## Recovery and Maintenance

The cache must be rebuildable from raw snapshots.

Failure handling:

- If an index write fails, keep raw snapshot and mark index dirty.
- If a schema migration fails, do not destroy old data.
- If JSON shape changes, store raw payload and mark parse failure with route and
  app version.
- Provide `doctor` checks:
  - database readable,
  - index row counts match current messages,
  - orphan snapshots,
  - stale hot conversations,
  - last successful capture time.

Operational commands:

- `serve`: start local ingest/search service.
- `install-userscript`: register bro UserScript.
- `refresh --hot`: refresh recent conversations under budget.
- `search <query>`: local search with freshness summary.
- `doctor`: health check.
- `vacuum`: compact local database.
- `purge`: delete local cache scope.

## Implementation Phases

### Phase 0: Probe and Contract

- Use bro network tools to identify stable route families:
  - conversation detail,
  - conversation list,
  - stream/update responses,
  - delete/archive/rename if visible.
- Record route patterns and payload shapes in fixtures.
- Define normalized JSON-to-message extraction.

Exit criteria:

- Can capture and normalize one opened conversation.
- Can prove `电影` is searchable via exact substring from captured JSON.
- Can identify whether fetch/XHR hooks require `MAIN` world for the current
  ChatGPT frontend.
- Can execute a leased refresh through a browser adapter without storing
  ChatGPT credentials in the local service.

### Phase 1: Local Snapshot Cache

- Add SQLite metadata tables.
- Store raw detail snapshots.
- Track current snapshot pointer.
- Add ingest endpoint.
- Add minimal UserScript fetch/XHR capture.

Exit criteria:

- Opening a ChatGPT conversation updates local snapshot.
- Reopening same conversation deduplicates by hash.
- No auth headers or cookies are stored.
- All rows below `conversations` reference `conversation_pk`, not remote
  conversation id alone.

### Phase 2: Search Index

- Add messages table and FTS/exact-substring path.
- Index title and message text.
- Add CLI search with freshness summary.

Exit criteria:

- Search `电影` returns the movie conversation when its snapshot contains that
  term.
- Search result shows snapshot time and freshness.
- Search `电影` still works when trigram/FTS is disabled, using exact substring
  verification.

### Phase 3: Search-Time Refresh

- Add refresh queue and rate limiter.
- Refresh hot candidates before final ranking.
- Mark skipped/stale coverage.

Exit criteria:

- Search can refresh recent stale conversations without unbounded crawling.
- 429/401/403 causes visible cooldown/degraded result, not silent failure.
- Current open conversation is refreshed before final ranking when route
  adapters are healthy.
- Result coverage shows verified, stale, skipped, and unknown counts.

### Phase 4: Cross-Client Awareness

- Poll/list-refresh on ChatGPT page open/focus.
- Compare list update markers to local detail snapshots.
- TTL fallback for routes without remote validators.

Exit criteria:

- A conversation updated elsewhere becomes stale locally after list refresh.
- Search can refresh it before claiming verified results.
- Full-history results are labeled as cached/eventually consistent unless they
  were refreshed within the active search budget.

## Open Questions

- Which ChatGPT web endpoints expose reliable conversation update markers today?
- Are conversation ids stable across workspace/team scopes and URL variants?
- Can bro UserScripts access page-world fetch responses early enough for all
  route types, or do we need an extension/background bridge?
- Is there already a good local service in `chat-memory`, or should ChatGPT
  capture be a separate crate/binary?
- Which tokenizer gives the best Chinese short-query behavior with SQLite FTS5
  in this environment?
- Is IndexedDB fallback worth supporting in v1, or should v1 require the local
  service for durability?
- Should the first browser refresh adapter be UserScript-only, or should bro/CDP
  own refresh execution while UserScript only captures natural page traffic?

## Design Decisions

1. Use UserScript for capture, not for durable indexing.
2. Use local service plus SQLite as the system of record.
3. Store raw snapshots separately from derived search rows.
4. Treat freshness as first-class result metadata.
5. Prefer SQLite FTS5 plus exact substring fallback before WASM search engines.
6. Keep refresh budgets conservative and explicit.
7. Define "latest" as a tiered freshness SLA, not as global strong
   consistency.
8. Treat IndexedDB as a fallback/prototype mode, not as the durable default.
9. Use tombstones for deleted/inaccessible conversations.
10. Use local `conversation_pk` for all internal joins and indexes.
11. Run ChatGPT-origin refreshes only through a browser refresh adapter.
12. Require exact substring verification for short CJK queries.

## Acceptance Checklist

- Search result never implies full coverage unless hot/stale refresh completed
  or the scope is explicitly limited.
- `电影` works as an exact substring query.
- Recently updated conversations are refreshed before final ranking when budget
  allows.
- Rate limits produce visible cooldown state.
- Raw ChatGPT credentials are never stored.
- Cache can be rebuilt from raw snapshots.
- UserScript can be uninstalled without losing local data.
- Local service can be stopped without breaking ChatGPT itself.
- Current conversation, hot history, warm history, and archive history have
  distinct freshness labels.
- Cross-client updates are discovered by list refresh/polling, not falsely
  described as realtime push.
- Deleted, archived, or inaccessible conversations are hidden or labeled by
  tombstone state.
- Local service never stores ChatGPT credentials; browser refresh adapters
  execute leased refreshes.
- No table below `conversations` uses remote conversation id as its sole
  identity.
- Short CJK search remains correct when FTS/trigram indexing is unavailable.
