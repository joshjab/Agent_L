# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## KB Context

On session start: read `_KB/projects/active/agent-l/overview.md`, `doc/ROADMAP.md`, `doc/ARCHITECTURE.md`.

After completing a milestone or ending a session: update `_KB/projects/active/agent-l/overview.md` with current milestone, decisions made, and what's next. Tick off completed items in `doc/ROADMAP.md` and update `doc/ARCHITECTURE.md` if any structural decisions changed.

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

# Run all live tests (requires Ollama running; read REVIEW REQUIRED blocks)
cargo test --test live -- --ignored --nocapture

# Run a specific live category
cargo test --test live live_pipeline:: -- --ignored --nocapture
cargo test --test live live_factual_review:: -- --ignored --nocapture
cargo test --test live live_synthesis_review:: -- --ignored --nocapture
```

## Pre-commit hook

`.githooks/pre-commit` runs the fast suite on every commit and the live suite
when Ollama is reachable. Activate once per clone:

```bash
git config core.hooksPath .githooks
```

When Ollama is reachable, the hook runs all live tests with `--nocapture` and
pauses to ask you to confirm the printed answers look correct. Read the
"REVIEW REQUIRED" blocks before pressing y.

The hook skips live tests (with a warning) if Ollama is not running — but the
full live suite must pass before merging any milestone to `main`.

## Factual accuracy

Factual questions ("Who is the president?", "What is the latest X?") must always
route to the Search specialist — never answered from model knowledge.

- `src/agents/persona.rs` — `DEFAULT_PERSONA_PROMPT` tells the Chat specialist
  to refuse current-events questions and ask the user to search instead.
- `src/agents/orchestrator.rs` — `SYSTEM_PROMPT` includes explicit examples of
  Factual intent so the orchestrator routes these questions to Search.
- `tests/live/live_factual_review.rs` — manual-review tests that print the full
  response for human sign-off; assert only the mechanism (web_search called,
  non-empty response), not the specific answer.
- `tests/live/live_synthesis_review.rs` — manual-review tests that verify
  synthesis voice (no raw `FinalAnswer:` or `[SEARCH RESULT]` tags in output).

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

### Roadmap tracking

After completing any work tied to `doc/ROADMAP.md`:
- Check off each finished task with `[x]`
- Mark the milestone's `### Verification` section with ✅ and note the actual results (test counts, observed behavior)

Do this immediately after the work is done, not at the end of a session.

### Milestone commit

After a milestone is fully complete (all tasks `[x]`, verification filled in, `cargo check && cargo clippy -- -D warnings && cargo test && cargo fmt --check` all pass), run:

```bash
git add src/ tests/ doc/ Cargo.toml Cargo.lock
git commit -m "Complete milestone N: <title>"
```

Use the milestone number and title from ROADMAP.md. No push — commit only. Stage specific files, never `git add -A`.

### Writing style for ROADMAP entries

Write ROADMAP tasks and verification steps as if explaining to a junior developer — use plain language, spell out what each step does and why, avoid jargon, and keep bullet points short and concrete. If a step involves a new concept (e.g., ReAct loop, wiremock FIFO), add a one-sentence explanation in parentheses.


### Testing rules

For every function you write, always add at minimum:
- one **happy-path** unit test (valid input, expected output)
- one **sad-path** unit test (invalid/edge-case input, expected error or fallback)

Place unit tests in a `#[cfg(test)] mod tests { ... }` block at the bottom of the same file. Integration tests go in `tests/`.

**Write tests before implementation (TDD).** For each ROADMAP task, write the failing tests first, verify they fail, then write the implementation to make them pass. Do not batch tests up and write them all at the end of a milestone.

### Testing approach

No running Ollama required. HTTP tests use `wiremock` (a real local HTTP server), not trait mocks. Wiremock uses **FIFO** matching — the first-registered mock has highest priority; `up_to_n_times(n)` exhausts a mock so later requests fall through to subsequent registrations.

`App::new_for_test()` creates an app with `startup_state: Ready` without spawning the startup task — use this for all `app.rs` unit tests.

Env-var tests in `config.rs` use a `static ENV_MUTEX` to prevent parallel test races. All `std::env::set_var`/`remove_var` calls require `unsafe {}` (Rust 2024 edition).
