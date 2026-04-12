# Repository Structure

## Overview

Agent L is an async terminal UI (TUI) for local LLMs via Ollama. Requests flow through a three-layer agent pipeline: **Persona → Agent L (orchestrator) → Specialist**. Each layer communicates via validated JSON.

---

## Directory Layout

```
Agent_L/
├── Cargo.toml                  # Rust project manifest and dependencies
├── Cargo.lock                  # Locked dependency versions
├── .env                        # Local config (gitignored)
├── .gitignore
├── README.md
├── doc/
│   ├── ROADMAP.md
│   ├── ARCHITECTURE.md
│   ├── STRUCTURE.md            # This file
│   ├── test-cases.md           # Live test catalogue
│   └── logo.png
├── src/
│   ├── main.rs
│   ├── lib.rs
│   ├── app.rs
│   ├── config.rs
│   ├── ollama.rs
│   ├── startup.rs
│   ├── ui.rs
│   ├── agents/
│   │   ├── mod.rs
│   │   ├── orchestrator.rs
│   │   ├── persona.rs
│   │   ├── compression.rs
│   │   ├── schema.rs
│   │   └── specialists/
│   │       ├── mod.rs
│   │       ├── chat.rs
│   │       ├── code.rs
│   │       └── search.rs
│   └── tools/
│       ├── mod.rs
│       ├── executor.rs
│       ├── search_tools.rs
│       └── claude_code.rs
└── tests/
    ├── ollama_integration.rs
    ├── startup_integration.rs
    ├── orchestrator_integration.rs
    ├── pipeline_integration.rs
    ├── search_integration.rs
    ├── live.rs
    └── live/
        ├── README.md
        ├── live_pipeline.rs
        ├── live_factual_review.rs
        └── live_synthesis_review.rs
```

---

## Source Files

### `src/main.rs` — Entry Point

Initializes the ratatui terminal, creates the `App` instance, and runs the main event loop at ~60 FPS.

- Terminal setup and teardown (crossterm alternate screen + raw mode)
- Keyboard event polling via `event::poll` with a 16ms timeout
- Dispatches key events: `Ctrl+Q` quit, character input, `Backspace`, arrow keys, `Enter`
- Calls `App::update()` each frame to drain the event channel

---

### `src/lib.rs` — Library Root

Re-exports all modules as `pub mod` so `tests/` integration tests can use `agent_l::*`. Both `main.rs` and `lib.rs` compile the same source files independently (dual-target pattern).

---

### `src/app.rs` — Application State

`App` holds all mutable state. `AppEvent` is the event enum that all background tasks send into the main loop via an `mpsc::UnboundedChannel`.

**Key types:**

| Type | Purpose |
|------|---------|
| `App` | Full application state (history, input, scroll, startup state, current plan) |
| `AppEvent` | `Token(String)`, `StreamDone`, `StartupUpdate(StartupState)`, `RouteDecision(TaskPlan)`, `ScopeDecision(TaskScope)` |
| `StartupState` | `Connecting`, `CheckingModel`, `Loading`, `Ready`, `Failed(String)` |

`App::ask_ollama()` pushes the user message and spawns the full pipeline (Persona → Agent L → Specialist) as background tasks. `App::update()` drains `AppEvent`s from the channel each frame.

---

### `src/config.rs` — Configuration

Reads `OLLAMA_HOST`, `OLLAMA_PORT`, `OLLAMA_MODEL` from env or `.env` (via `dotenvy`). `.env` loading is suppressed in tests with `#[cfg(not(test))]`. `Config::new(host, port, model)` is the direct constructor used by integration tests.

---

### `src/ollama.rs` — Ollama HTTP Client

Two public functions:

| Function | Purpose |
|----------|---------|
| `fetch_ollama_stream(url, model, messages, tx)` | Streams NDJSON from `/api/chat`; sends `AppEvent::Token` per chunk and `AppEvent::StreamDone` when done |
| `post_json(url, body)` | One-shot POST returning the full response body as `serde_json::Value`; used by orchestrator and specialist agents |

---

### `src/startup.rs` — Startup Health Checks

`run_startup_checks(config, tx, timings)` runs three phases:
1. Connect to `/api/tags` with retry (default 10 retries, 3s delay)
2. Check the configured model exists in the tags list
3. Poll `/api/ps` until the model is loaded or timeout (default 60s)

`StartupTimings` controls all delays — tests use short values so they run fast.

---

### `src/ui.rs` — TUI Rendering

Renders the full TUI via ratatui's `Widget` trait. Layout: chat area (scrollable) + prompt input (3 lines) + status bar (1 line).

`parse_simple_markdown(text)` handles:
- `**bold**` — yellow bold spans
- Bare `https://` URLs — wrapped in OSC 8 terminal hyperlink sequences for clickable links in supported terminals (iTerm2, Kitty, recent GNOME Terminal)
- Route decision banners (e.g. `[Factual → Search]`) rendered in dim style

---

### `src/agents/mod.rs` — Agent Trait and Retry

Defines the `Agent` trait (all orchestrator and specialist agents implement it) and `call_with_retry()` — calls an agent up to N times, feeding the previous error back into the prompt on each retry.

`AgentErrorKind` enumerates structured failure modes: `InvalidJson`, `SchemaViolation`, `TokenOverflow`, `Timeout`, `AuthFailure`.

---

### `src/agents/orchestrator.rs` — Agent L

`OrchestratorAgent` classifies the user's request and returns a `TaskPlan`:

```json
{
  "intent_type": "Factual",
  "steps": [{ "agent": "Search", "task": "...", "depends_on": null }]
}
```

`IntentType`: `Conversational`, `Factual`, `Creative`, `Task`.
`AgentKind`: `Chat`, `Search`, `Code`, `Shell`, `Calendar`, `Memory`.

The plan is validated against a JSON schema before use. Max 5 steps; self-referential `depends_on` rejected.

---

### `src/agents/persona.rs` — Persona Layer

`PersonaAgent` wraps the conversation history with a system prompt that defines Agent L's personality and behavior. Injects a goal-reminder message into the history every N turns to prevent drift. Builds the message list passed to Agent L and to specialists.

---

### `src/agents/compression.rs` — Conversation Compression

`CompressionAgent` summarizes old turns when the estimated token count exceeds a threshold. The summary is injected as a `<summary>` system message; recent turns are preserved in full. Prevents context drift in long sessions.

---

### `src/agents/schema.rs` — Schema Helpers

`require_field` and `require_str` — typed accessors for JSON objects that return structured errors on missing or wrong-typed fields. Used by orchestrator and specialist parsers.

---

### `src/agents/specialists/mod.rs` — Plan Executor

`run_plan(plan, history, model, url, cwd, tx)` executes a `TaskPlan` step by step. Resolves `depends_on` chaining (injects prior step output as context). Dispatches each step to the right specialist. On 3 consecutive specialist failures, injects a failure-reason system message so the Persona can explain it to the user.

---

### `src/agents/specialists/chat.rs` — Chat Specialist

Handles `Conversational` and `Creative` intents. No tools — streams tokens directly to the UI via the `AppEvent::Token` channel. Uses the persona system prompt.

---

### `src/agents/specialists/code.rs` — Code Specialist

Handles `Task` intents classified as code work. Uses a keyword heuristic to detect project-scope tasks (e.g. "edit src/main.rs") and shows a limitation message for those (project-scope editing not yet implemented — M8). One-off tasks delegate to the `claude` CLI subprocess via `ClaudeCodeTool`.

---

### `src/agents/specialists/search.rs` — Search Specialist

Handles `Factual` intents. Uses the ReAct executor with two tools:
- `web_search` — DuckDuckGo Instant Answer API; includes current date in system prompt so model can flag stale results
- `local_search` — ripgrep over the project directory

Observation formatting: `Title | URL | Snippet` on separate lines (prevents model from copying raw JSON into the answer). Post-processing collapses duplicate consecutive sentences.

---

### `src/tools/mod.rs` — Tool Trait and Registry

`Tool` trait: `name()`, `description()`, `schema()` (JSON Schema object), `execute(args)`.
`ToolRegistry` is a `HashMap<String, Box<dyn Tool>>` — specialists register their allowed tools at construction time.

---

### `src/tools/executor.rs` — ReAct Loop

`ToolExecutor::run_loop(prompt, registry, tx)` runs the Thought → ToolCall → Observation cycle. Each iteration:
1. Sends the accumulated messages to Ollama
2. Parses lines tagged `Thought:`, `Action:`, or `FinalAnswer:`
3. Executes the tool and appends the `Observation:` to the message list
4. Hard stops at 10 steps (circuit breaker returns structured error)

---

### `src/tools/search_tools.rs` — Search Tools

| Tool | Description |
|------|-------------|
| `WebSearchTool` | POST to DuckDuckGo Instant Answer API; parses abstract + related topics; filters non-`https://` URLs |
| `LocalSearchTool` | Runs `grep -rn` over a project directory; caps output at 50 lines |

---

### `src/tools/claude_code.rs` — Claude CLI Tool

Runs `claude` as a subprocess for one-off code tasks. `ClaudeCodeTool::run(task, cwd)` captures stdout. `run_streaming(task, cwd, tx)` streams output tokens to the UI channel as they arrive.

---

## Test Files

| File | What it covers |
|------|----------------|
| `tests/ollama_integration.rs` | `fetch_ollama_stream` against a wiremock server |
| `tests/startup_integration.rs` | All startup check sequences (happy path, retry, timeout, model not found) |
| `tests/orchestrator_integration.rs` | Intent classification, plan validation, retry on bad JSON |
| `tests/pipeline_integration.rs` | Full Persona → Agent L → Specialist end-to-end with wiremock |
| `tests/search_integration.rs` | DuckDuckGo response parsing, citation format, local search |
| `tests/live.rs` | Live test entry point — all categories compiled into one `--test live` binary |
| `tests/live/live_pipeline.rs` | Routing + specialist live tests (`#[ignore]` by default) |
| `tests/live/live_factual_review.rs` | Factual accuracy live tests (`#[ignore]` by default) |
| `tests/live/live_synthesis_review.rs` | Synthesis voice live tests (`#[ignore]` by default) |

---

## Dependencies

| Crate | Purpose |
|-------|---------|
| `ratatui` | Terminal UI framework |
| `crossterm` | Cross-platform terminal input/output |
| `tokio` | Async runtime |
| `reqwest` | HTTP client (JSON + streaming) |
| `serde` / `serde_json` | JSON serialization |
| `futures-util` | Async stream utilities |
| `dotenvy` | `.env` file loading |
| `urlencoding` | URL-encodes DuckDuckGo query strings |
| `tempfile` | Temporary files for Code specialist tests |
| `wiremock` (dev) | Real local HTTP server for integration tests |
