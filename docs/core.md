# Core and CLI

`vetro serve` binds to `127.0.0.1`. It prints the URL with a fragment for local authentication. `--port` overrides `config.json`, and `--open` opens the URL in the default browser. The core embeds `apps/web/dist` if it exists at build time. `VETRO_WEB_DIR` can point at a different UI directory for development.

The binary forwards the `claude`, `codex`, and `acp` features to `vetro-core`. All three default to off. Build the binary with `cargo build -p vetro-cli --features claude,codex,acp` to register the built-in agents and configured ACP commands. `registry::built_ins` is the adapter wiring point. A caller can use `Registry::new` with its own adapters and pass the registry to `Core::new`. The desktop shell can bind a `Server`, get its `local_url`, and await `serve` with a shutdown signal.

- `home` owns the config, local overlays, cached options, paired devices, and logs. The raw log filename is the SHA-256 of the thread id. Each file holds the latest bounded snapshot of the adapter's wire log.
- `sessions` keeps per-thread timelines and sequence numbers. It queues prompts for sessions without steering, resumes on demand, and releases idle sessions while preserving pending requests.
- `service` merges paged history and live sessions through `storage::ThreadStore`. `Core::with_store` accepts a replacement store. `server` handles HTTP, cookie authentication, WebSocket RPC, and subscriptions. `files` implements read-only browsing and ignore-aware file search.
- `vetro agents` prints detection results. `vetro threads` lists history. `vetro probe` sends one prompt, handles interaction requests, and can print the wire log with `--raw`. `--yes` selects an offered allow-once option.

The current store is in memory. Search matches titles, and cross-agent links are empty. Pairing works on loopback. Remote ingress and Tailscale management return disabled status. Changes to adapter launch configuration take effect on restart. ACP accepts configured arguments and environment overrides. The current Claude and Codex constructors accept binary overrides.
