# chat-memory

`chat-memory` is a local-first search tool for agent chat history and captured
ChatGPT web conversations.

The ChatGPT path is intentionally local: a browser userscript captures
conversation JSON from the logged-in ChatGPT page and posts it to a loopback
service. The service stores searchable snapshots in SQLite. It does not store
ChatGPT cookies, access tokens, or request headers.

## Install

From Homebrew, after the first release is published:

```bash
brew install xiaotianxt/tap/chat-memory
```

From source:

```bash
scripts/install.sh
```

## ChatGPT Capture Service

Start the loopback ingest service:

```bash
chat-memory --cache ~/.cache/chat-memory/index.sqlite3 chatgpt-serve \
  --addr 127.0.0.1:37531 \
  --token-file ~/.cache/chat-memory/chatgpt-ingest-token
```

Install the browser capture userscripts through bro:

```bash
chat-memory chatgpt-userscript install \
  --server http://127.0.0.1:37531 \
  --token-file ~/.cache/chat-memory/chatgpt-ingest-token
```

Search captured conversations:

```bash
chat-memory --cache ~/.cache/chat-memory/index.sqlite3 chatgpt-search "电影"
```

## Homebrew Service

The tap formula installs a launchd service:

```bash
brew services start xiaotianxt/tap/chat-memory
brew services stop xiaotianxt/tap/chat-memory
```

The service uses:

```text
cache:      $(brew --prefix)/var/chat-memory/index.sqlite3
token-file: $(brew --prefix)/var/chat-memory/chatgpt-ingest-token
addr:       127.0.0.1:37531
logs:       $(brew --prefix)/var/log/chat-memory.log
```

After starting the service, install the browser capture scripts with:

```bash
chat-memory --cache "$(brew --prefix)/var/chat-memory/index.sqlite3" \
  chatgpt-userscript install \
  --server http://127.0.0.1:37531 \
  --token-file "$(brew --prefix)/var/chat-memory/chatgpt-ingest-token"
```

## Release

The release shape follows `~/dev/cx`:

```bash
scripts/release.sh --version 0.1.0
```

The script pushes a `v*` tag, waits for GitHub Actions to publish
`chat-memory-vX.Y.Z-darwin-arm64.tar.gz`, updates
`xiaotianxt/tap/Formula/chat-memory.rb`, and verifies the Homebrew install.

