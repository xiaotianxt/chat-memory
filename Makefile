.PHONY: all fmt clippy test check build install-local install release clean

all: check build

fmt:
	cargo fmt --all -- --check

clippy:
	cargo clippy --all-targets -- -D warnings

test:
	cargo test

check: fmt clippy test

build:
	cargo build --release

install-local: build
	mkdir -p ~/.local/bin
	cp target/release/chat-memory ~/.local/bin/
	@echo "installed: ~/.local/bin/chat-memory"

install: build
	sudo cp target/release/chat-memory /usr/local/bin/
	@echo "installed: /usr/local/bin/chat-memory"

release:
	scripts/release.sh

clean:
	cargo clean
	rm -f ~/.local/bin/chat-memory 2>/dev/null || true

