# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
# Build
cargo build --release

# Run (requires Ollama running locally)
cargo run

# Run all tests (no Ollama needed)
cargo test

# Run a single test by name
cargo test test_name

# Run a specific test file's tests
cargo test --test startup_integration
cargo test --test ollama_integration

# Run with output visible
cargo test -- --nocapture
```

## Architecture

Agent-L is a `tokio`-based async TUI that streams tokens from Ollama's `/api/chat` endpoint. The app loop runs at ~60fps driven by a 16ms `event::poll` timeout.

### Dual-target crate structure

- `src/main.rs` — binary entry point; declares modules with `mod` (private to bin target)
- `src/lib.rs` — re-exports all modules as `pub mod`; required so `tests/` integration tests can use `agent_l::*`

Both targets compile the same source files independently. This is why `main.rs` uses `mod` and `lib.rs` uses `pub mod`.

### Event flow

```
Ollama HTTP (background tokio::spawn)
    └─ fetch_ollama_stream() → tx.send(AppEvent::Token / StreamDone)

startup::run_startup_checks() (background tokio::spawn)
    └─ tx.send(AppEvent::StartupUpdate(StartupState::*))

main loop (App::update)
    └─ rx.try_recv() → mutates App state

ratatui terminal.draw()
    └─ ui.rs Widget impl renders App state
```

### Key modules

- **`app.rs`** — `App` struct (all mutable state), `AppEvent` enum, `StartupState` enum. `ask_ollama()` pushes messages and spawns the HTTP task. `update()` drains the channel each frame.
- **`ollama.rs`** — `fetch_ollama_stream(url, model, messages, tx)` — streams NDJSON chunks from Ollama, sends `Token` events per chunk, sends `StreamDone` when done.
- **`startup.rs`** — `run_startup_checks(config, tx, timings)` — three-phase: connect to `/api/tags` (with retry), check model exists, poll `/api/ps` until loaded or timeout. `StartupTimings` controls all delays (production defaults: 10 retries, 3s delay, 1s poll, 60s timeout).
- **`config.rs`** — reads `OLLAMA_HOST`, `OLLAMA_PORT`, `OLLAMA_MODEL` env vars (`.env` loaded via `dotenvy`, suppressed in tests with `#[cfg(not(test))]`). `Config::new(host, port, model)` is the direct constructor used by integration tests.
- **`ui.rs`** — `ratatui` rendering. Shows startup splash screen until `StartupState::Ready`, then the chat view. `parse_simple_markdown()` handles `**bold**` highlighting.

### Testing approach

No running Ollama required. HTTP tests use `wiremock` (a real local HTTP server), not trait mocks. Wiremock uses **FIFO** matching — the first-registered mock has highest priority; `up_to_n_times(n)` exhausts a mock so later requests fall through to subsequent registrations.

`App::new_for_test()` creates an app with `startup_state: Ready` without spawning the startup task — use this for all `app.rs` unit tests.

Env-var tests in `config.rs` use a `static ENV_MUTEX` to prevent parallel test races. All `std::env::set_var`/`remove_var` calls require `unsafe {}` (Rust 2024 edition).
