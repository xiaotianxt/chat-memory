# ChatGPT Browser Adapter Slice Design

## Purpose

Provide data for `chat-memory chatgpt-search` by capturing ChatGPT conversation
JSON from an already logged-in browser context.

This slice connects the first local-cache implementation to the browser:

```
ChatGPT web page
  -> UserScript captures conversation JSON from fetch/XHR responses
  -> local ingest HTTP service receives payloads on 127.0.0.1
  -> existing chatgpt-ingest normalization stores snapshots/messages/docs
  -> chatgpt-search queries the local SQLite cache
```

The local service still does not know or store ChatGPT credentials. Browser
code provides data because the browser owns the logged-in session.

## Scope

Implement the minimum durable data-provider path:

- A local loopback ingest server command in `chat-memory`.
- A UserScript generation/install command for bro `userscripts_register`.
- Capture of naturally occurring ChatGPT conversation detail JSON.
- Health/diagnostic commands that do not print conversation text or secrets.

Do not implement background crawling, full-history sync, or search-time refresh
leases in this slice. Those require a separate scheduler and stricter rate-limit
policy.

## New CLI Surface

### `chatgpt-serve`

Start a local ingest service:

```bash
chat-memory --cache ~/.cache/chat-memory/index.sqlite3 chatgpt-serve \
  --addr 127.0.0.1:37531 \
  --token-file ~/.cache/chat-memory/chatgpt-ingest-token
```

Behavior:

- Bind only to `127.0.0.1` by default.
- Create token file if missing with owner-only permissions where practical.
- Print only:
  - bind address,
  - token file path,
  - database path,
  - installed endpoints.
- Never print token value.
- Accept only requests with `Authorization: Bearer <token>`.
- Support browser CORS preflight for the ingest endpoint.

Minimum endpoints:

- `GET /health` -> JSON `{ "ok": true }`.
- `OPTIONS /ingest/chatgpt/conversation` -> CORS preflight.
- `POST /ingest/chatgpt/conversation` -> ingest one conversation payload.

Request body:

```json
{
  "account_id": "default",
  "workspace_id": "default",
  "source": "userscript:capture",
  "url": "https://chatgpt.com/c/...",
  "route": "/backend-api/conversation/{id}",
  "payload": { "...": "raw ChatGPT conversation JSON" }
}
```

Response:

```json
{
  "ok": true,
  "conversation_pk": 1,
  "deduped": false,
  "message_count": 5,
  "doc_count": 5
}
```

Error responses:

- `401` for missing/invalid token.
- `403` for browser requests with non-ChatGPT `Origin`.
- `400` for invalid JSON or missing `payload`.
- `500` for local DB failures.

CORS and Origin policy:

- Allow only `Origin: https://chatgpt.com` for browser-originated ingest
  requests.
- `OPTIONS /ingest/chatgpt/conversation` must return:
  - `Access-Control-Allow-Origin: https://chatgpt.com`
  - `Access-Control-Allow-Headers: Authorization, Content-Type`
  - `Access-Control-Allow-Methods: POST, OPTIONS`
- `POST /ingest/chatgpt/conversation` must also include
  `Access-Control-Allow-Origin: https://chatgpt.com` when accepting a browser
  request from ChatGPT.
- Do not accept bearer tokens in query strings.
- Non-browser local tests may omit `Origin`, but if `Origin` is present and is
  not `https://chatgpt.com`, reject the request.

### `chatgpt-userscript`

Print or install a browser UserScript:

```bash
chat-memory chatgpt-userscript print \
  --server http://127.0.0.1:37531 \
  --token-file ~/.cache/chat-memory/chatgpt-ingest-token

chat-memory chatgpt-userscript install \
  --server http://127.0.0.1:37531 \
  --token-file ~/.cache/chat-memory/chatgpt-ingest-token
```

`print` writes JavaScript to stdout. Default `print` must never embed or print
the token.

`print --embed-token` is a sensitive local-install mode:

- It may embed the token only in the USER_SCRIPT-world sender script.
- It must clearly label stdout as sensitive in stderr.
- It is intended only for piping into a local installer, not for logs or docs.
- It must reject non-loopback `--server` URLs because the embedded token and
  captured conversation JSON are only intended for the local ingest service.

Loopback server URL policy for token-bearing modes:

- `chatgpt-userscript install` and `chatgpt-userscript print --embed-token`
  must accept only `http://127.0.0.1:<port>`, `http://localhost:<port>`, or
  `http://[::1]:<port>`.
- `https`, non-loopback hosts, userinfo, fragments, and paths other than `/`
  must be rejected before reading the token file.
- Plain `print` without `--embed-token` may print a script for any server URL
  only if no token is embedded, but the generated sender still should not be
  described as safe for non-loopback production use.

`install` must call bro `userscripts_register` directly when bro is available.
It registers scripts matching `https://chatgpt.com/*` as two separate browser
user scripts:

- MAIN-world hook script, `runAt: document_start`, no token.
- USER_SCRIPT-world sender script, holds the token and posts to loopback.

Stable script IDs:

- `chat-memory-chatgpt-main`
- `chat-memory-chatgpt-sender`

The bro registration payload shape is:

```json
{
  "scripts": [
    {
      "id": "chat-memory-chatgpt-main",
      "matches": ["https://chatgpt.com/*"],
      "js": [{ "code": "<main world hook>" }],
      "runAt": "document_start",
      "allFrames": false,
      "world": "MAIN"
    },
    {
      "id": "chat-memory-chatgpt-sender",
      "matches": ["https://chatgpt.com/*"],
      "js": [{ "code": "<user script sender with token>" }],
      "runAt": "document_start",
      "allFrames": false,
      "world": "USER_SCRIPT"
    }
  ]
}
```

`install` should be idempotent:

- Unregister the two stable IDs before registering, or overwrite through bro if
  bro supports that safely.
- Do not unregister unrelated user scripts.
- Do not print the token or full sender script to stdout/stderr.
- Print only installed IDs, bro target, server URL, and token-file path.

If bro is unavailable, fail with a concise diagnostic and a non-zero exit. Do
not fall back to printing a token-bearing command line. The user can still use
`print --embed-token` explicitly for manual local installation.

Implementation must not pass the token-bearing registration payload through a
shell command, argv, environment variable, or stdout. The existing
`bro-call.mjs` helper accepts JSON in argv, so it is not acceptable for
`install`.

`install` must call bro MCP directly over `http://127.0.0.1:3500/mcp`:

- Read the bro bearer token from `~/.bro/settings.json`.
- Perform MCP `initialize`.
- Call `tools/call` for `userscripts_unregister` with only the two stable IDs.
- Call `tools/call` for `userscripts_register` with the two script objects.
- Call `tools/call` for `userscripts_list` with the two IDs and verify the
  installed state.

The ChatGPT ingest token must exist before `install`:

- `install` must fail if `--token-file` does not exist, is unreadable, or
  contains an empty token.
- `install` must not create a new token file, because that can silently diverge
  from the running `chatgpt-serve` token.
- If file permissions are world-readable or group-readable on Unix, warn with
  the token-file path only; do not print the token. Treating this as hard error
  is acceptable if implemented consistently.

## UserScript Behavior

The script should be tiny and conservative.

It is two pieces:

1. MAIN-world hook:
   - hooks page `fetch` and `XMLHttpRequest`;
   - performs route and payload guard;
   - emits a `window.postMessage` or `CustomEvent` with the payload;
   - never sees or stores the local ingest token.
2. USER_SCRIPT-world sender:
   - receives guarded payload events from the page;
   - holds the local ingest token;
   - POSTs to `127.0.0.1`;
   - handles local-service failures with in-memory backoff.

Responsibilities:

- Hook `window.fetch`.
- Hook `XMLHttpRequest`.
- Detect response URLs that look like ChatGPT conversation detail endpoints.
- Clone/read JSON responses without breaking ChatGPT's own response handling.
- POST the raw JSON payload to local `/ingest/chatgpt/conversation`.
- Include route/url/source/account/workspace metadata.
- Avoid logging conversation text.

Event trust boundary:

- The USER_SCRIPT sender must treat every page event as untrusted input.
- It must repeat the route guard on `detail.url` or `detail.route`.
- It must repeat the payload guard before POSTing.
- It must accept only the exact event schema emitted by the MAIN hook.
- It must ignore extra data and must not forward page-controlled headers,
  cookies, or authorization values.

Route matcher:

- Must parse with `new URL(response.url)`.
- `url.origin` must equal `https://chatgpt.com`.
- `url.pathname` must match exactly:
  `^/backend-api/conversation/[^/?#]+/?$`.
- Query string may exist.
- Any extra path segment must be rejected.
- This must reject `/textdocs`, `/stream_status`, `/prepare`, analytics, files,
  product search, and all third-party URLs.

Payload guard:

- Only send payloads that look like conversation JSON:
  - object,
  - `mapping` is an object,
  - `id` or `conversation_id` is a string.

Failure behavior:

- Local ingest failures are non-fatal.
- Rate-limit logs in browser console should be compact and metadata-only.
- Back off repeated local failures in memory for the page lifetime.
- If the local service is not running, do not cache locally. Log one
  metadata-only warning, then back off.
- Do not print conversation body text to console.
- Do not fall back to `localStorage`, file downloads, or IndexedDB in this
  slice.

## Security Boundaries

- UserScript may contain the local ingest token only when installed into the
  user's local browser profile, and only in USER_SCRIPT world.
- The MAIN-world hook must never contain the local ingest token.
- Token must never be printed by default.
- Local service must reject unauthenticated writes.
- Local service must bind to loopback.
- No cookies, auth headers, or ChatGPT request headers are forwarded.
- Diagnostics must redact body text by default.

## Integration With Existing Rust Code

Reuse existing functions:

- `open_chatgpt_db`
- `ingest_chatgpt`
- `chatgpt_db_path`

Refactor them only enough to support server handlers cleanly. Avoid changing
the existing `chatgpt-ingest`, `chatgpt-search`, and `chatgpt-doctor` behavior.

V1 handler contract:

- Parse request JSON.
- Read `payload`, `account_id`, `workspace_id`, and `source`.
- Re-serialize `payload` to bytes and call existing `ingest_chatgpt`.
- `url` and `route` are metadata-only diagnostics and are not stored in the
  schema in this slice.
- Do not add new database tables or columns for `url`/`route` in this slice.

Recommended dependencies:

- Prefer `tiny_http` or a similarly small blocking HTTP server for this slice.
- Use `serde_json` already present.
- Avoid async runtimes unless the implementation actually needs concurrency.

## Acceptance Tests

Unit tests:

- Route matcher accepts conversation detail endpoint and rejects analytics,
  files, textdocs, and stream status.
- Ingest request parser rejects missing payload.
- Token check rejects missing/wrong token.
- CORS preflight allows only `https://chatgpt.com`.
- Server handler can ingest fixture JSON and then `search_chatgpt("电影")`
  finds it.
- Default `chatgpt-userscript print` output does not contain the token.
- MAIN-world generated script does not contain the token even in embedded
  install mode.
- Bro install payload contains exactly two scripts with the stable IDs, correct
  worlds, `document_start`, `https://chatgpt.com/*`, and token only in the
  USER_SCRIPT sender code.
- `install` rejects non-loopback server URLs before reading the token file.
- `print --embed-token` rejects non-loopback server URLs before reading the
  token file.
- USER_SCRIPT sender repeats route and payload guards and does not POST malformed
  page events.
- Bro MCP request builder never places the token-bearing registration payload in
  shell strings, argv, environment variables, stdout, or stderr.
- Token-file missing/empty/unreadable behavior is deterministic and covered.

Manual E2E:

1. Start:

   ```bash
   chat-memory --cache target/e2e/index.sqlite3 chatgpt-serve \
     --addr 127.0.0.1:37531 \
     --token-file target/e2e/token
   ```

2. POST fixture:

   ```bash
   curl -H "Authorization: Bearer $(cat target/e2e/token)" \
     -H "Content-Type: application/json" \
     --data @target/chatgpt-ingest-request.json \
     http://127.0.0.1:37531/ingest/chatgpt/conversation
   ```

3. Verify:

   ```bash
   chat-memory --cache target/e2e/index.sqlite3 chatgpt-search 电影
   chat-memory --cache target/e2e/index.sqlite3 chatgpt-doctor
   ```

4. Real browser capture verification through bro:

   ```bash
   chat-memory --cache target/e2e/index.sqlite3 chatgpt-userscript install \
     --server http://127.0.0.1:37531 \
     --token-file target/e2e/token
   ```

   Then use bro to open or claim a `https://chatgpt.com/` tab in the logged-in
   browser, navigate to an existing conversation, and wait for the page to load
   the conversation detail endpoint naturally. Do not call ChatGPT private
   backend APIs directly from the agent for this acceptance check; the point is
   to verify the installed capture path.

   Success criteria:

   - bro `userscripts_list` shows both stable script IDs.
   - Browser console inspection is limited to message metadata. Do not dump full
     network responses, page data, or conversation bodies while checking this.
     No console message should contain the local ingest token.
   - `chatgpt-doctor` shows snapshot/message/doc counts increasing after the
     page loads a conversation.
   - `chatgpt-search <user-provided known term from that conversation>` finds
     the captured conversation. The agent must not enumerate conversation text
     to invent the term.

   Failure diagnosis:

- If bro is not running or no browser is connected, report that as an
  environment blocker rather than weakening the installer.
- If ChatGPT changed its route shape, update the route matcher and tests
  before broadening capture.
- If the local service is not running, the UserScript must only emit compact
  metadata warnings and back off; it must not cache the payload in browser
  storage.
- If registration partially fails, return non-zero and report only script IDs,
  bro endpoint, and non-sensitive error text. Do not print generated script code.
- If bro `userscripts_list` does not report both expected IDs after install,
  return non-zero even if `userscripts_register` reported success.

## Non-Goals For GLM Implementation

- No full-history crawler.
- No search-time refresh lease scheduler yet.
- No ChatGPT-origin active refresh requests in this slice; capture only natural
  responses already produced by the page. A future active refresh feature must
  implement lease/budget first.
- No storage of ChatGPT cookies or headers.
- No browser UI overlay.
- No IndexedDB fallback.
- No SQLite FTS migration.
- No active ChatGPT history crawling through the installed script in this
  slice. Real-page verification uses natural page loads only.

## Definition Of Done

- Existing tests still pass.
- `cargo fmt --all -- --check` passes.
- `cargo clippy --all-targets -- -D warnings` passes.
- New tests cover route matching, token auth, ingest handler, and no-token
  default for UserScript print.
- `chatgpt-userscript install` registers exactly the two bro user scripts with
  the correct worlds and no token in MAIN-world code.
- Manual fixture POST into `chatgpt-serve` makes `chatgpt-search 电影` work.
- Manual bro real-page E2E captures at least one real ChatGPT conversation into
  the local cache and makes a known term searchable.
