# Agent-L Roadmap

This is the implementation checklist. For architecture decisions, design rationale, use case scenarios, and the final file structure, see [ARCHITECTURE.md](ARCHITECTURE.md).

Each milestone builds directly on the previous one. Complete them in order. All tests must pass before moving to the next milestone — no Ollama required for any of the test suites.

---

## M1 — Structured Output & Agent Skeleton *(foundation)*

Everything else depends on this. Before writing any agent logic, we need a common pattern for sending a prompt to Ollama, enforcing a JSON schema on the response, and retrying on failure.

- [x] Create `src/agents/mod.rs` — define the `Agent` trait with three methods: `prompt()` (builds the request), `parse()` (validates the JSON response against a schema), and `retry()` (re-prompts with the parse error on failure, max 3 attempts)
- [x] Use Ollama's `format` field to enforce JSON schema on all non-streaming calls (GBNF grammar sampling) — this prevents the model from producing output that doesn't match the expected shape
- [x] Add serde-based validation at agent output boundaries — if the JSON parses but a required field is missing or the wrong type, treat it as a parse failure and retry
- [x] Write unit tests for the schema validation and retry logic — these should run without Ollama by passing mock responses

> **New files:** `src/agents/mod.rs` (Agent trait, retry logic), `src/agents/schema.rs` (serde types, validation helpers)

### Verification ✅

```bash
cargo test
```
All existing tests still pass. The new agent unit tests also pass — no Ollama needed.

```bash
cargo test agents
```
Verified (48 tests pass, 8 new):
- ✅ A valid JSON response passes `parse()` on the first attempt with no retries
- ✅ A response with a missing required field triggers `parse()` failure, the error message is injected into the next prompt, and the attempt counter increments
- ✅ After 3 failed attempts, the function returns a structured `Err` rather than panicking or looping forever

---

## M2 — Agent L (Orchestrator)

Agent L is the brain of the pipeline. It reads the last few turns of conversation and decides what to do — which specialist(s) to call, in what order, and with what task description. It never sees the full conversation history.

- [x] Define `IntentType` enum in `src/agents/orchestrator.rs`: `Factual`, `Conversational`, `Creative`, `Task`
  - `Factual` — any question about real-world state (facts, current events, prices) → always routes to Search, never trusts model's own knowledge
  - `Conversational` — greetings, opinions, back-and-forth chat → Chat specialist
  - `Creative` — writing, brainstorming, summarizing → Chat specialist
  - `Task` — actions like scheduling, running code, sending email → relevant specialist
- [x] Define `AgentKind` enum: `Chat`, `Code`, `Search`, `Shell`, `Calendar`, `Memory`, `Unknown`
- [x] Define the task plan schema that Agent L outputs:
  ```json
  { "intent_type": "Task", "steps": [{ "agent": "Search", "task": "...", "depends_on": 0 }] }
  ```
  `depends_on` is an optional index into `steps[]` — the output of that step is injected as context. Max 5 steps enforced at parse time; return an error and ask the user to break it up if exceeded.
- [x] Implement Agent L using the `Agent` trait from M1 — it receives the last 3–5 turns and outputs a validated task plan
- [x] Wire the task plan into `App` state: add a `RouteDecision` variant to `AppEvent` so the UI knows what was decided
- [x] Show the routing decision in the UI status line (e.g., "Agent L → Search + Shell")
- [x] Write integration tests with wiremock covering: single-step Factual, single-step Conversational, multi-step Task with depends_on, Unknown intent fallback

> **New files:** `src/agents/orchestrator.rs`, `tests/orchestrator_integration.rs`
> **Changed:** `src/app.rs` (AppEvent gets RouteDecision), `src/lib.rs` (pub mod agents)

### Verification ✅

```bash
cargo test --test orchestrator_integration
```
All wiremock scenarios pass (6 tests): single-step Factual, single-step Conversational, multi-step Task with depends_on, Unknown fallback, over-5-steps error case, and retry prompt embedding.

```bash
cargo test
```
200 tests pass across all targets. Agent L is now wired into `ask_ollama()` — every message first runs through the orchestrator (non-streaming POST) which emits `RouteDecision`, then the chat response streams. The status line at the bottom of the UI shows e.g. `Agent L → Chat` or `Agent L → Search (Factual)` after each classification.

```bash
cargo run
```
With Ollama running, type a few different messages and watch the status line at the bottom of the UI:
- `"hello"` → status line shows `Agent L → Chat`
- `"what's the weather in Berlin right now?"` → status line shows `Agent L → Search (Factual)`
- `"write me a poem about Rust"` → status line shows `Agent L → Chat (Creative)`
- `"run cargo build and tell me if it passes"` → status line shows `Agent L → Shell + Code`

If the status line doesn't update or shows `Unknown`, Agent L returned an invalid task plan — check that the Ollama `format` field is being set correctly in the request.

---

## M3 — Persona Layer

The Persona layer is the face of the assistant — it handles the conversation with the user, compresses old context so the model doesn't drift, and synthesizes specialist results into natural responses. It wraps every call to Agent L and every specialist result before the user sees it.

- [x] Create `src/agents/persona.rs` — system prompt (personality/tone), handles all outbound prompts to Agent L and inbound results from specialists
- [x] Create `src/agents/compression.rs` — when the conversation exceeds a token threshold, summarize the oldest N turns into a single "context summary" block and replace them; the summary is prepended to future prompts
- [x] Inject a short goal reminder every N turns (e.g., "You are Agent-L, a local personal assistant…") to prevent personality drift over long sessions
- [x] Support `PERSONA_SYSTEM_PROMPT` env var to override the built-in personality — useful for customizing the assistant without changing code

> **New files:** `src/agents/persona.rs`, `src/agents/compression.rs`

### Verification ✅

```bash
cargo test agents::persona
cargo test agents::compression
```
116 tests pass (29 new). Persona tests: default prompt, env-var override, system message format, goal-reminder injection at GOAL_REMINDER_INTERVAL (10) and multiples, and build_messages layout. Compression tests: estimate_tokens (chars/4), below-threshold passthrough, above-threshold summarisation, SUMMARY_TAG prefix, keyword preservation, remaining-turns preservation, post-error propagation.

```bash
PERSONA_SYSTEM_PROMPT="You are Jarvis, a dry British assistant." cargo run
```
Verified via env-var unit test. `Persona::new()` reads `PERSONA_SYSTEM_PROMPT` and uses it as the system prompt; falls back to DEFAULT_PERSONA_PROMPT when absent.

```bash
cargo run
```
Persona system message and optional compression are now wired into `ask_ollama()` in app.rs. Chat calls use `persona.build_messages()` (prepends system prompt, injects goal reminder every 10 turns). Compression runs via `compressor.maybe_compress()` before building the persona-wrapped messages; Agent L continues to receive raw (undecorated) context for accurate intent classification.

---

## M4 — First Specialist: Chat

The Chat specialist is the simplest — it handles Conversational and Creative intents with no tools. Getting this working end-to-end proves the full pipeline: Persona → Agent L → Specialist → back to Persona.

- [x] Create `src/agents/specialists/mod.rs` — define the `Specialist` trait and the step runner loop: iterate over the task plan's steps, execute each specialist, inject outputs for `depends_on` steps
- [x] Create `src/agents/specialists/chat.rs` — receives a task string, streams tokens directly to the UI (same as the current direct Ollama call, but now invoked by the step runner)
- [x] Wire everything together in `App`: user message → Persona → Agent L → step runner → Chat specialist → Persona → UI
- [x] Write `tests/pipeline_integration.rs` — end-to-end test using wiremock for both the Ollama calls (Agent L classification + Chat response); verify the full routing cycle

> **New files:** `src/agents/specialists/mod.rs` (Specialist trait, step runner), `src/agents/specialists/chat.rs`, `tests/pipeline_integration.rs`

### Verification ✅

```bash
cargo test --test pipeline_integration
```
5 pipeline integration tests pass: `chat_plan_streams_response`, `chat_plan_sends_stream_done`, `chat_plan_uses_provided_messages`, `multistep_plan_with_depends_on_runs_all_steps`, `unknown_specialist_falls_back_to_chat`. All use wiremock to simulate Ollama responses; no running Ollama required.

```bash
cargo test
```
163 tests pass across all targets (126 lib + 37 integration). The full pipeline is wired: `ask_ollama()` now calls Persona (system prompt + goal reminders), Compressor (history compression), Agent L (intent classification), and `run_plan()` (step runner → ChatSpecialist → streams tokens via tx). Agent L failure falls back to a Chat step. `StreamDone` is owned by `run_plan`, sent after all steps complete. Also fixed: `fetch_ollama_stream` now splits chunks by newline before JSON-parsing, handling multi-object NDJSON chunks correctly.

---

## M5 — Tool Call Infrastructure

Specialists that need to interact with the world (search the web, run shell commands, read files) do so through tools. The ReAct loop governs how a specialist decides which tool to call, observes the result, and decides what to do next.

- [x] Define the `Tool` trait in `src/tools/mod.rs`: `name()`, `description()`, `schema()` (JSON schema for args), `execute(args) -> Result<String>`
- [x] Create `src/tools/executor.rs` — implements the ReAct loop: the specialist outputs `Thought`, `ToolCall`, or `FinalAnswer`; the executor parses it, validates args against the tool's schema, calls `execute()`, appends the `Observation` to the prompt, and repeats; hard stop at 10 steps with a structured error returned to Persona
- [x] Validate tool args against the JSON schema before calling `execute()` — never pass unvalidated user-influenced data to a tool
- [x] Add `AppEvent::ToolCall(name, args)` and `AppEvent::ToolResult(name, result)` so the UI can show what tools were used

> **New files:** `src/tools/mod.rs` (Tool trait), `src/tools/executor.rs` (ReAct loop + circuit breaker)
> **Changed:** `src/app.rs` (AppEvent::ToolCall, AppEvent::ToolResult), `src/lib.rs` (pub mod tools)

### Verification ✅

```bash
cargo test tools
```
178 tests pass (15 new tool tests). Verified: `AlwaysOkTool` completes the loop and returns `FinalAnswer` in 2 steps; `AlwaysFailTool` returns an error observation and the model recovers via `FinalAnswer`; missing required schema fields produce a `Validation Error` observation without calling `execute()`; circuit breaker fires at exactly `max_steps` with `ExecutorError { steps_taken, message: "...step limit..." }`. `AppEvent::ToolCall` and `AppEvent::ToolResult` variants added to `app.rs`.

---

## M6 — Specialist: Code (Claude Code Integration)

Instead of using Ollama for code tasks, the Code specialist delegates to the `claude` CLI (Claude Code). This means Agent-L can handle real coding work — generating scripts, explaining files, building features — by letting Claude Code run agentically with its own tools (read, write, bash, etc.).

The specialist figures out which of two modes to use based on the task:

- **One-off script** — the user wants a small script or snippet (e.g., "write a Python script that renames files"). The specialist creates a temporary sandbox directory, runs `claude` non-interactively inside it, captures the output, and returns the result to Persona. The sandbox is cleaned up afterwards.
- **Whole project** — the user wants something larger or ongoing (e.g., "add a logging module to this project"). The specialist spawns `claude` as a background subprocess, streams progress events back to the TUI so the user can see what's happening, and reports when it's done.

**What to build:**

- [x] Create `src/tools/claude_code.rs` — a tool that invokes the `claude` CLI as a subprocess. It takes a prompt string and a working directory, runs `claude --print "<prompt>"` (non-interactive mode), captures stdout/stderr, and returns the output as a `String`. Include a timeout so a hung process doesn't block the app forever.
- [x] Add a scope detector to `src/agents/specialists/code.rs` — sends a short classification prompt to Ollama (non-streaming, JSON schema enforced like Agent L does) asking whether the task is a self-contained one-off script or a change to an existing project. Returns `TaskScope::OneOff` or `TaskScope::Project`. This reuses the `Agent` trait from M1 so the retry/validation logic is already handled.
- [x] Implement the **one-off path** in `code.rs`: create a `tempdir`, call the `claude_code` tool with the task prompt and the temp dir as the working directory, return the output to Persona, then delete the temp dir.
- [x] Implement the **project path** in `code.rs`: spawn `claude` as a background `tokio::process::Child` in the project's working directory, read its stdout line-by-line, and send each line as an `AppEvent::Token` so it streams into the TUI chat. When the process exits, send `AppEvent::StreamDone`.
- [x] Add a `TaskScope` field to the `RouteDecision` event so the UI status line can show `Agent L → Code (one-off)` or `Agent L → Code (project)`.
- [x] Format code blocks in the output: scan the returned text for fenced code blocks (` ``` `) and render them with a language label in the ratatui UI (a different background color or a border is enough — no need for full syntax highlighting yet).
- [x] Tests: write unit tests for the scope detector (check that known keywords route correctly). Write an integration test that uses a mock subprocess (or a real `echo` command) to verify the one-off path captures output and the project path streams it.

> **New files:** `src/tools/claude_code.rs`, `src/agents/specialists/code.rs`
> **Changed:** `src/app.rs` (TaskScope in RouteDecision), `src/ui.rs` (fenced code block rendering)

### Verification ⚠️ Needs Re-validation

`src/agents/specialists/code.rs` and `src/tools/claude_code.rs` were modified after the initial verification pass. Re-run the suite below and update results before marking this milestone complete again.

```bash
cargo test agents::specialists::code
cargo test tools::claude_code
```
Previously 228 tests passed (43 new). Verified at that point:
- ✅ `ClaudeCodeInvoker::run()` captures stdout, respects working dir, returns Err on non-zero exit or timeout, returns Err when binary not found
- ✅ `ClaudeCodeInvoker::run_streaming()` sends each output line as `AppEvent::Token`, returns Err on non-zero exit or timeout
- ✅ `ScopeDetector` classifies tasks: mock Ollama returns `{"scope":"one_off"}` → `OneOff`; `{"scope":"project"}` → `Project`; invalid JSON triggers retry; all 3 attempts exhausted returns `AgentError`
- ✅ Keyword pre-classifier routes unambiguous tasks without hitting Ollama: file extension or path references → `Project`; "write a script/bash/python" phrases → `OneOff`; ambiguous phrasing falls through to Ollama
- ✅ `CodeSpecialist::run()` sends `AppEvent::ScopeDecision` before executing; one-off path uses a temp dir sandbox and returns the full output string; project path sends a limitation message (no subprocess) — direct file editing is deferred to M8
- ✅ `AppEvent::ScopeDecision` stores scope in `App.code_scope`; `RouteDecision` resets it for each new message
- ✅ Status line shows `Agent L → Code (one-off)` or `Agent L → Code (project)` when scope is known
- ✅ Fenced code blocks render with a yellow language label header, green code lines, and a gray closing line; bold markers inside code blocks are not processed

**Current limitation:** Code specialist only fully executes the one-off path. Project scope tasks display a message explaining that direct file editing is not yet supported and suggesting the user rephrase as a one-off script (e.g., "write a standalone script that does X"). Full project-scope execution (background subprocess + permission relay) is deferred to M8.

```bash
cargo run
```
With Ollama and the `claude` CLI both available:
- Type `"write a bash script that lists all .rs files"` → status shows `Agent L → Code (one-off)`; a bash fenced code block appears in the chat
- Type `"add a --verbose flag to the config module"` → status shows `Agent L → Code (project)`; a limitation message appears explaining direct file editing is not yet supported

---

## M7 — Specialist: Search

The Search specialist handles all `Factual` intent types. It is the only specialist that may access the web. It never generates an answer from model knowledge alone — it always calls a search tool and returns citations.

- [x] Create `src/agents/specialists/search.rs` — calls search tools, returns structured results (title, url, snippet) rather than prose; Persona formats the final answer
- [x] Create `src/tools/search_tools.rs` with two tools:
  - `web_search(query)` — DuckDuckGo instant answer API (no API key required)
  - `local_search(query, path)` — grep wrapper for searching within local files
- [x] The specialist must always use at least one tool call — it cannot short-circuit to a direct answer
- [x] Add a `concurrency_safe() -> bool` method to the `Specialist` trait in `src/agents/specialists/mod.rs`. Return `true` for Chat, Search, and Memory (they only read; they don't write files or run commands). Return `false` for Code and Shell. This flag lets the step runner know it is safe to run two specialists at the same time when the task plan calls for both — for example, searching the web and looking up a memory fact simultaneously instead of one after the other.
- [x] In `src/tools/executor.rs`, make the `Observation` appended to the ReAct loop include three things: (1) the exit code, (2) stdout/stderr trimmed to 2 000 characters, and (3) a structural warning note if the output contains the words `"error:"`, `"failed:"`, `"panic:"`, or `"WARN:"` even though the command exited with code 0. This last check prevents the model from claiming success on a command that technically passed but printed errors — for example, a `cargo build` that exits 0 but fills the output with warnings.
- [x] Tests: `tests/search_integration.rs` using wiremock to simulate DuckDuckGo responses; verify citation format

> **New files:** `src/agents/specialists/search.rs`, `src/tools/search_tools.rs`, `tests/search_integration.rs`
> **Changed:** `src/agents/specialists/mod.rs` (concurrency_safe on Specialist trait), `src/tools/executor.rs` (semantic exit-code analysis in Observation)

### Verification ✅

```bash
cargo test --test search_integration
```
5 wiremock integration tests pass: citation format verified, DDG always called, local_search finds file content, conversational query does not route to Search.

```bash
cargo check && cargo clippy -- -D warnings && cargo test && cargo fmt --check
```
268 tests pass across all targets (215 lib + 53 integration). Zero warnings, zero clippy errors, clean fmt. Also fixed 12 pre-existing clippy violations in compression.rs, orchestrator.rs, persona.rs, app.rs, ollama.rs, startup.rs, main.rs, ui.rs.

**Note:** `local_search` uses `grep -rn` (always available) rather than ripgrep. `web_search` uses `reqwest::blocking` with `block_in_place` inside the sync `Tool::execute()`. Tests use `#[tokio::test(flavor = "multi_thread")]` to support `block_in_place`.

```bash
cargo run
```
With Ollama running:
- Type `"what is the capital of France?"` → status shows `Agent L → Search (Factual)`; the response should include a citation (source URL or title), not just "Paris"
- Type `"what's the latest news in AI?"` → same routing; response cites sources
- Type `"how are you today?"` → this should **not** route to Search (it's Conversational) — confirm it goes to Chat instead. This is the key regression check: factual routing should not over-trigger.
- Type `"search my project files for the word 'retry'"` → should use `local_search` tool, return matching file paths and line snippets

---

## M7.5 — Search Polish + Live Tests

Fixes for bugs and UX issues found during M7 manual validation. Also adds a live-Ollama integration test layer that was missing since M1 — each milestone has unit tests against mocks, but there is no automated check that the full pipeline works with a real model.

### Bug fixes

- [x] **Duplicate response in Search answers** — The model outputs something like `"According to [url], Chiefs won LVII. The Kansas City Chiefs won Super Bowl LVII"` (the DDG snippet is copied verbatim into the FinalAnswer, producing the same sentence twice). Root cause: the Observation includes the raw JSON snippet text and the model pastes it directly into the answer. Fix: format the Observation more cleanly — show `Title | URL | Snippet` on separate lines instead of raw JSON so the model synthesises rather than copies. Also add a post-processing step in `SearchSpecialist::run()` to collapse consecutive identical sentences.

- [x] **"Search project files" routes to Code specialist, not Search** — Query `"search my project files for 'retry'"` triggers the Code specialist (keyword `"project"` matches `classify_scope_from_keywords`) and shows the M8 limitation message. Fix: improve `OrchestratorAgent` system prompt to clearly distinguish "search/grep/find in files" (→ Search with `local_search` tool) from "edit/add/fix code in the project" (→ Code). Also remove `"project"` as a standalone project-scope signal in `classify_scope_from_keywords` — it is too broad.

- [x] **Stale/inaccurate DDG results** (e.g. `tokio.ts` URL, 2023 release dates returned for a 2024 query) — The DuckDuckGo Instant Answer API returns Wikipedia-style abstracts, which can be outdated. Fix: (1) include the current date in the Search specialist's system prompt so the model can flag when a result looks stale; (2) strip `RelatedTopics` entries whose `FirstURL` does not start with `https://` (the `tokio.ts` issue is a malformed URL from DDG); (3) add a URL validation step in `parse_ddg` — skip results with no valid `https://` URL.

### UX improvements

- [x] **Clickable hyperlinks in the TUI** — Markdown links `[text](url)` are rendered as literal text. Use OSC 8 terminal hyperlink escape sequences (`\x1b]8;;url\x1b\\text\x1b]8;;\x1b\\`) inside `parse_simple_markdown()` in `ui.rs` so that terminals supporting OSC 8 (iTerm2, Kitty, recent GNOME Terminal) render clickable links. Fall back gracefully: if a span contains an OSC 8 sequence and the terminal doesn't support it, it will still display the text. Also detect bare `https://...` URLs in prose and linkify them the same way. Note: OSC 8 is applied to bare `https://` URLs in prose; markdown-style `[text](url)` links are not yet parsed (deferred).

- [ ] **Update docs** — `README.md`, `doc/ARCHITECTURE.md`, and `doc/STRUCTURE.md` were written before M1 was implemented. Update them to reflect the actual M1–M7 codebase: current module list, event flow diagram, specialist routing table, and tool inventory.

### Live integration tests (applies retroactively from M1)

Tests throughout M1–M7 use wiremock to simulate Ollama. That approach is fast and deterministic, but it never catches prompt-engineering regressions — a broken system prompt still passes all mocks. Add a thin layer of live tests that run against a real Ollama instance.

- [x] Create `tests/live/` directory with a `README` explaining how to run (requires `OLLAMA_HOST` env var set and the configured model pulled locally).

- [x] Create `tests/live/live_pipeline.rs` — each test is `#[ignore]` by default so `cargo test` stays fast; run with `cargo test --test live_pipeline -- --ignored`. Each test:
  1. Sends a real prompt through the full pipeline (Persona → Agent L → Specialist → response)
  2. Asserts structural properties on the output, not exact strings (e.g. "response is non-empty", "response contains a URL for a Factual query", "response does not contain 'I cannot'" for a conversational query)

- [x] Initial test cases to cover (one per intent type; add more as regressions are found):
  - **Conversational** — `"say the word 'hello' and nothing else"` → response contains `"hello"` (case-insensitive). Verifies Chat specialist returns at all.
  - **Factual** — `"what country is Paris the capital of? reply in one word"` → response contains `"France"`. Verifies Search routes and returns a sensible answer.
  - **Agent L routing** — directly call `OrchestratorAgent` with `"what is 2+2?"` and assert `intent_type == Conversational` or `Creative` (not `Factual`). Verifies the orchestrator prompt works with the live model.
  - **Regression: no duplicate sentences** — `"in one sentence, what is the capital of France?"` → response does not contain the same sentence twice. Catches the M7 duplication bug.
  - **Regression: file search routing** — `"search my project files for 'retry'"` → orchestrator routes to `agent=Search`. Catches the M7.5 routing bug.

- [x] Create `doc/test-cases.md` — a human-readable catalogue of the live test cases: the input, what the test asserts, and why. This is the document to update whenever a new prompt regression is discovered.

> **New files:** `tests/live/live_pipeline.rs`, `tests/live/README.md`, `doc/test-cases.md`
> **Changed:** `src/agents/specialists/search.rs` (Observation formatting, post-processing), `src/agents/orchestrator.rs` (system prompt clarification), `src/tools/search_tools.rs` (URL validation in `parse_ddg`, human-readable format), `src/ui.rs` (OSC 8 hyperlinks in `parse_simple_markdown`), `Cargo.toml` (`[[test]]` for live_pipeline)

### Verification ✅

```bash
# Fast suite — no Ollama needed
cargo check && cargo clippy -- -D warnings && cargo test && cargo fmt --check
```

✅ `cargo check` clean, `cargo clippy -- -D warnings` zero warnings, `cargo fmt --check` clean.
✅ 226 unit tests + 11 unit tests (ui/search modules) + 6 pipeline + 5 search_integration + 10 startup = all pass.
✅ 5 live tests registered as `#[ignore]` (compile and run with `-- --ignored`).

```bash
# Live suite — requires Ollama running with the configured model
cargo test --test live_pipeline -- --ignored --nocapture
```
Run this after completing manual validation with a live model.

```bash
cargo run
```
Re-run the M7 manual checks with the fixes applied:
- `"who won the most recent Super Bowl?"` → single non-duplicated sentence with a citation URL
- `"search my project files for 'retry'"` → routes to `Agent L → Search (Factual)`, uses `local_search`, returns file paths
- `"what's the latest version of tokio?"` → answer cites a valid `https://` URL (not `tokio.ts`)
- `"how are you today?"` → still routes to Chat, no regression
- Click a URL in the response → terminal opens the link (if OSC 8 supported)

---

## M8 — Specialist: Shell + Code Permission Relay

The Shell specialist runs sandboxed commands on the user's machine. Because this is dangerous, it has an explicit confirmation gate — the UI must show the command and get user approval before execution.

This milestone also enables full project-scope Code tasks. The `claude` CLI asks for permission before it reads or writes files. Right now there is no stdin pipe from the TUI to the subprocess, so those prompts would stall silently. The permission relay wires that up: Agent-L intercepts the `claude` process's permission requests and surfaces them in the TUI as a confirmation gate (the same Y/N flow as the Shell confirmation gate). Until this is done, the Code specialist only supports one-off sandbox scripts — no direct file editing.

- [ ] Create `src/agents/specialists/shell.rs` — receives a task, determines the command to run, sends it to the confirmation gate before executing
- [ ] Create `src/tools/shell_tools.rs` — `run_command(cmd, args)`: executes with no network access and no writes outside the working directory by default; captures stdout/stderr and streams back to Persona
- [ ] Confirmation gate: add an `AppEvent::AwaitingConfirmation(command)` that pauses execution and shows the command in the UI with Y/N prompt; only proceed on Y
- [ ] Configurable allow/deny lists for commands in env vars or `config.toml`
- [ ] **Code permission relay**: when `CodeSpecialist` runs a project-scope task, open a stdin pipe to the `claude` subprocess; monitor stdout for permission request lines; when one is detected, emit `AppEvent::AwaitingConfirmation(permission_request)` and pipe the user's Y/N response back to `claude`'s stdin before resuming the output stream. This reuses the same confirmation gate as the Shell specialist. Enable `run_streaming` in `ClaudeCodeInvoker` (currently `#[allow(dead_code)]`) as the basis for this.
- [ ] Add `check_permissions(args: &Value) -> Result<(), PermissionError>` and `is_concurrency_safe() -> bool` methods to the `Tool` trait in `src/tools/mod.rs`. `check_permissions()` runs before `execute()` every time — it looks at what class of action the tool performs (read-only, write, destructive) and whether the current permission mode allows it. `is_concurrency_safe()` defaults to `false`; only mark read-only tools as `true`. This keeps dangerous tools from running silently in parallel.
- [ ] Create a `ToolRegistry` struct in `src/tools/mod.rs`. It holds the full list of available tools and filters them by allow/deny rules before a specialist can see or call them. Rules come from config (env var or `config.toml`). Example: no Shell tools are visible when running in read-only mode. This is the single place where "which tools can run right now" is decided — specialists never bypass it.
- [ ] Add a `PermissionMode` enum to `src/config.rs` with four variants: `Default` (ask the user before any destructive action), `AcceptEdits` (auto-approve file writes, still ask for shell), `BypassPermissions` (fully autonomous — skip all confirmation prompts; useful for headless scripts), `PlanOnly` (no tool ever executes; Agent L produces a plan but nothing runs). Wire this into `ToolRegistry` so the mode controls which tools are offered to specialists.
- [ ] Add an `AgentErrorKind` enum to `src/agents/mod.rs` so different failure types are handled differently instead of all being retried the same way:
  - `ParseFailure(String)` — the model returned invalid JSON → retry and include the parse error in the next prompt so the model can correct itself
  - `TokenOverflow` — the context is too long for the model → trigger compression first, then retry the same request (retrying without compressing would fail again)
  - `RateLimit` — HTTP 429 → exponential backoff before retrying
  - `ModelUnavailable` — HTTP 503 → surface to the user, do not retry silently
  - `AuthFailure` — HTTP 401 → surface immediately and stop; retrying will never fix an auth problem
  Map Ollama HTTP error codes to these variants in the retry logic.
- [ ] When `run_plan()` in `src/agents/specialists/mod.rs` falls back to Chat after a specialist exhausts all 3 retries, inject the failure reason as a system context message so the Persona can explain it to the user — for example: `"The Code specialist failed after 3 attempts (TokenOverflow). Answering from available context."` Never silently fall back without telling the user why.

> **New files:** `src/agents/specialists/shell.rs`, `src/tools/shell_tools.rs`
> **Changed:** `src/tools/mod.rs` (check_permissions + is_concurrency_safe on Tool trait, ToolRegistry), `src/config.rs` (PermissionMode enum), `src/agents/mod.rs` (AgentErrorKind enum), `src/agents/specialists/mod.rs` (failure reason injected on fallback)

### Verification

```bash
cargo run
```
With Ollama running:
- Type `"run cargo build"` → a confirmation prompt should appear in the UI showing the exact command (`cargo build`) with a `[Y/N]` prompt before anything executes
  - Press `Y` → the command runs; stdout/stderr streams into the chat
  - Restart and try again, press `N` → command is cancelled; the Persona should acknowledge the cancellation
- Type `"run cargo test"` → same confirmation flow; after approving, output should stream in and the Persona should summarize pass/fail at the end
- Type `"delete all .rs files"` → this should be **blocked** by the deny list before a confirmation is even shown; the Persona should explain it refused to run a destructive command
- Type `"list files in src/"` → `ls src/` should be allowed (read-only); confirm output appears

The confirmation gate is the most important safety feature here — double-check that **no command ever executes without showing the prompt first**, even if the model tries to skip it.

---

## M9 — Memory System

The memory system gives the assistant persistence across sessions. The Persona layer reads from it on every turn; the Memory specialist exposes it to the user for explicit operations.

- [ ] Create `src/memory/episodic.rs` — SQLite-backed log; records every turn (role, content, timestamp) and every tool call (name, args, result); append-only
- [ ] Create `src/memory/semantic.rs` — key-value store for consolidated facts (e.g., `"user.name" = "Josh"`); backed by SQLite; supports get, set, delete, list
- [ ] Create `src/memory/retrieval.rs` — BM25 keyword search over episodic memory; optional embedding-based semantic search (disabled by default, requires an embedding model)
- [ ] Create `src/memory/mod.rs` — unified API used by both Persona (for injection) and the Memory specialist (for explicit operations)
- [ ] Create `src/agents/specialists/memory.rs` — handles explicit memory operations the user can request: "remember that…", "forget that…", "what do you know about me?"
- [ ] Consolidation job (runs async after each session): if 3+ episodic entries agree on a user preference, promote it to semantic memory automatically
- [ ] Threshold-triggered background extraction: when `App` detects the conversation is approaching the token budget (use `estimate_tokens()` from `compression.rs`), spawn a background `tokio::task` to summarize recent turns into the episodic store without interrupting the current conversation. Do not wait for the session to end — long sessions should be checkpointed continuously. Add a `compression_failures: u8` field to `App`; if the background task fails 3 times in a row, stop attempting auto-compression and show a warning in the UI so the user knows their context is no longer being saved.
- [ ] Tests: `tests/memory_integration.rs` — write turns, retrieve by keyword, verify consolidation promotes correctly

> **New files:** `src/memory/mod.rs`, `src/memory/episodic.rs`, `src/memory/semantic.rs`, `src/memory/retrieval.rs`, `src/agents/specialists/memory.rs`, `tests/memory_integration.rs`
> **Changed:** `src/lib.rs` (pub mod memory)

### Verification

```bash
cargo test --test memory_integration
```
Verify: episodic log records turns and tool calls correctly; BM25 retrieval returns the right turns for a keyword query; the consolidation job promotes a fact to semantic memory after 3+ matching episodes; deleting a semantic fact removes it from future retrievals.

```bash
cargo run
```
Session 1 — persistence test:
- Type `"remember that my name is Josh and I prefer dark mode"` → Memory specialist confirms it was saved
- Type `"what do you know about me?"` → should list back the stored facts
- Quit (`Ctrl+Q`)

Session 2 — cross-session recall:
- `cargo run` again (new session)
- Type `"what's my name?"` → should answer "Josh" from semantic memory without you having said it in this session
- Type `"what are my preferences?"` → should recall "dark mode"

Consolidation test:
- In three separate sessions, mention the same preference (e.g., `"I prefer concise answers"`)
- After the third session, check that it has been promoted to semantic memory and appears in `"what do you know about me?"`

---

## M10 — Polish & Observability

Make the pipeline visible and configurable. A developer should be able to watch exactly what Agent L decided, which tools fired, and how many tokens each call consumed.

- [ ] Agent trace panel in TUI: toggled with a key (e.g., `t`), shows the last routing decision, each specialist invoked, tool calls made, and step count
- [ ] Token budget display: show estimated tokens used per agent call in the trace panel
- [ ] Per-agent model config: allow different Ollama models per role (e.g., `qwen2.5:7b` for Agent L, `gemma3:12b` for Persona) via env vars or `config.toml`
- [ ] Graceful degradation: if a specialist fails all 3 retries, fall back to the Chat specialist and include an error note in the Persona's synthesis prompt (the `AgentErrorKind` enum added in M8 provides the failure reason)
- [ ] Create `src/agents/specialists/calendar.rs` — date/time parsing and scheduling (deferred from earlier milestones)
- [ ] Upgrade `src/agents/compression.rs` to produce a structured 9-section summary instead of a freeform prose blob. The prompt should ask the model to fill in exactly these sections: (1) Primary Request & Current State, (2) Key Technical Concepts, (3) Files & Code Touched, (4) Errors & Fixes Made, (5) Problem-Solving Approach, (6) User Preferences & Decisions, (7) Pending Tasks / Open Questions, (8) Current Work, (9) Next Steps. Structured sections let the model find context faster — it can jump to "Files & Code Touched" rather than reading a wall of prose.
- [ ] After compression fires, re-inject the last 5 file paths that any tool accessed during the session (tracked in a `Vec<PathBuf>` in `App` state) and their current contents, capped at 50 000 tokens total. Prepend them as a "recently accessed files" block. Without this, the Code specialist may hallucinate file contents after a long session because the compaction wiped out the file context.
- [ ] Add a `BoundedMsgIdSet` to `src/ollama.rs`: a fixed-capacity `VecDeque<u64>` that stores hashes of recently seen NDJSON chunks. Before processing any chunk, hash it and check the set — if the hash is already there, skip the chunk. If it is new, process it and push the hash (evicting the oldest entry if the deque is full). This prevents a token from appearing twice in the chat if the stream reconnects and replays chunks. "Bounded" means memory is constant regardless of session length.
- [ ] Add a shared `validate_plan()` function in `src/agents/orchestrator.rs` that checks semantic constraints the JSON schema cannot express: `steps.len() <= 5`, each `depends_on` index is less than the step's own index (so a step cannot depend on something that hasn't run yet), and no two steps form a circular dependency. Call this from the `Agent` trait's `parse()` rather than duplicating the checks inline.
- [ ] Wire up parallel execution of concurrency-safe steps in `src/agents/specialists/mod.rs`: when two or more consecutive steps all return `concurrency_safe() = true`, run them with `tokio::join!` (for exactly 2) or `FuturesUnordered` (for 3+) instead of sequentially. For example, a "Search + Memory recall" plan should fire both at the same time. Enforce that no two running steps write to the same file at once by maintaining a `HashSet<PathBuf>` of files currently being written; a second writer for the same file should wait until the first finishes.

> **New files:** `src/agents/specialists/calendar.rs`, `config.toml` (optional)
> **Changed:** `src/ui.rs` (trace panel, token budget display), `src/config.rs` (per-agent model config, TOML support), `src/agents/compression.rs` (structured 9-section format, file restoration), `src/ollama.rs` (BoundedMsgIdSet), `src/agents/orchestrator.rs` (shared validate_plan()), `src/agents/specialists/mod.rs` (parallel execution), `src/app.rs` (recently_accessed_files tracking), `doc/STRUCTURE.md` (update to reflect final layout)

### Verification

```bash
cargo run
```
Trace panel:
- Press `t` → a side or bottom panel opens showing the last routing decision (e.g., `Agent L → Search (Factual) [2 steps]`), each tool call with its args, and a token count per agent call
- Send a multi-step message (e.g., `"search for Rust news and summarize it"`) → the trace panel should show both steps, their order, and the `depends_on` link between them
- Press `t` again → panel closes; normal chat view resumes

Per-agent model config:
```bash
OLLAMA_MODEL_AGENT_L=qwen2.5:7b OLLAMA_MODEL_PERSONA=gemma3:12b cargo run
```
- Start the app and send a message; verify in the trace panel that the token counts reflect two different models being called (Agent L and Persona)

Graceful degradation:
```bash
OLLAMA_MODEL_CODE=nonexistent:model cargo run
```
- Ask `"explain startup.rs"` → Code specialist should fail all 3 retries, then fall back to Chat; the Persona's response should include a note like `"(Code specialist unavailable, answering from context)"` rather than crashing or hanging

---

## M11 — Skills & Custom Commands

Skills are user-defined slash commands that plug into the normal pipeline. Instead of typing a long prompt every time, the user writes it once as a `.md` file and invokes it with a short name like `/standup`. Agent-L discovers these at startup, registers them as commands, and routes them through the same Persona → Agent L → Specialist pipeline as any other message.

- [ ] At startup, scan `~/.config/agent_l/skills/` for `.md` files. Each file is one skill. Parse a YAML frontmatter block at the top of each file containing `name` (the slash command, e.g. `standup`), `description` (shown in the help list), and optionally `specialist` (to hard-route this skill to a specific specialist, bypassing Agent L's intent classification). The rest of the file is the prompt body.
- [ ] Register discovered skills in a `SkillRegistry` (a simple `HashMap<String, Skill>` in `src/skills/mod.rs`). If a file fails to parse, log a warning and skip it — don't crash the whole startup.
- [ ] In the TUI input handler, detect when the user types `/` followed by a known skill name and presses Enter. Replace the user-visible input with the skill's prompt body and route it through the pipeline as if the user had typed the full prompt. If the skill specifies a `specialist`, skip Agent L's classification and send the task directly to that specialist.
- [ ] Add a `/skills` built-in command that lists all discovered skills with their names and descriptions.
- [ ] Example skill to ship as documentation at `~/.config/agent_l/skills/standup.md`:
  ```yaml
  ---
  name: standup
  description: Summarize my git commits from the last 24 hours as a standup update
  specialist: Code
  ---
  Run `git log --since="24 hours ago" --oneline --author="$(git config user.name)"` in the current project directory and format the output as a standup update: what I worked on, what I finished, and what is still in progress.
  ```
- [ ] Tests: unit tests for YAML frontmatter parsing (valid file, missing required field, malformed YAML); integration test that registers a skill and verifies the input handler routes it correctly.

> **New files:** `src/skills/mod.rs`, `tests/skills_integration.rs`
> **Changed:** `src/app.rs` (skill dispatch in input handler), `src/ui.rs` (/skills listing), `src/lib.rs` (pub mod skills)

### Verification

```bash
cargo test --test skills_integration
```
Verify: a skill file with valid frontmatter is discovered and registered; a skill with a missing `name` field is skipped with a warning; typing `/standup` in the input box sends the full prompt body through the pipeline.

```bash
cargo run
```
- Create `~/.config/agent_l/skills/standup.md` with the example above
- Type `/skills` → the UI lists `standup` with its description
- Type `/standup` → the full prompt fires; the Code specialist runs `git log` and returns a standup summary
- Type `/unknowncommand` → the app should treat it as a regular message (not crash or hang)

---

## M12 — Specialist: Scheduling

The Scheduling specialist gives Agent-L a sense of time. It lets users say things like "run /standup every morning at 9am" or "remind me to review the PR in two hours," and it handles the daily midnight trigger that AutoImprove (M13) depends on. Scheduled tasks survive restarts — they are stored in the same SQLite database as episodic memory (M9), so they are not lost if the app exits.

- [ ] Create `src/agents/specialists/scheduling.rs` — interprets scheduling requests from the user (e.g., "every day at midnight", "in 2 hours", "every Monday at 9am"). Parse the time expression into an absolute next-fire timestamp using the `chrono` crate. Store the schedule entry in the SQLite memory store (M9): fields are `id`, `name`, `cron_expression` (optional), `next_fire_at` (Unix timestamp), `action` (the prompt or slash command to run), and `status` (`active` / `paused` / `completed`).
- [ ] On startup, load all `active` schedule entries from SQLite and register a `tokio::time::sleep_until` task for each. When a task fires, dispatch its stored action through the normal Persona → Agent L → Specialist pipeline as if the user had typed it. After firing, compute the next-fire timestamp (for recurring tasks) and update the row in SQLite, or mark it `completed` (for one-shot tasks).
- [ ] Add `AppEvent::ScheduledTrigger(name, action)` so the UI can show a notification like `"[Scheduled] Running: standup"` when a task fires automatically.
- [ ] Add a `/schedule list` command to the TUI that shows all active scheduled tasks — name, next fire time, and the action that will run. Add `/schedule pause <name>` and `/schedule cancel <name>`.
- [ ] If the app is not running when a scheduled task was supposed to fire (the user had it closed), detect the missed window on next startup: any entry whose `next_fire_at` is in the past and whose status is `active` should fire immediately on launch with a note in the UI explaining it was delayed.
- [ ] Tests: unit tests for time expression parsing (valid cron, natural language like "every day at midnight", invalid input); integration test that registers a schedule entry, advances a mock clock past `next_fire_at`, and verifies `AppEvent::ScheduledTrigger` is emitted.

> **New files:** `src/agents/specialists/scheduling.rs`, `tests/scheduling_integration.rs`
> **Changed:** `src/memory/mod.rs` (schedule table in SQLite), `src/app.rs` (AppEvent::ScheduledTrigger, startup missed-task detection), `src/ui.rs` (/schedule list display)

### Verification

```bash
cargo test --test scheduling_integration
```
Verify: a task registered with `next_fire_at = now + 100ms` fires and emits `ScheduledTrigger`; a missed task (next_fire_at in the past) fires on startup; cancelling a task prevents it from firing.

```bash
cargo run
```
- Type `"remind me to review the PR in 5 seconds"` → Scheduling specialist creates a one-shot task; 5 seconds later the UI shows `[Scheduled] Running: review the PR`
- Type `/schedule list` → the entry appears with its fire time
- Type `/schedule cancel remind me to review the PR` → it disappears from the list and does not fire
- Stop and restart the app with an overdue scheduled task in the database → it fires immediately on launch with a `[Delayed]` note

---

## M13 — Specialist: AutoImprove

AutoImprove turns Agent-L into a self-improving system. Once a day at midnight (via the Scheduling specialist), it reads an `ideas.md` file you maintain in the project root, picks the next unstarted idea, implements it using the Code specialist, validates the result against the project's development rules, opens a pull request, and watches for your review comments to iterate. You stay in control — you review the PR and decide whether to merge. Agent-L handles the grunt work.

### The `ideas.md` format

Each idea is a task item with a status marker:

```markdown
## Ideas

- [ ] Add fuzzy search to the chat history
- [ ] Show token count in the status line
- [~] Improve compression summary format  ← in progress (AutoImprove is working on this)
- [x] Fix auto-scrolling during streaming  ← done / merged
```

`[ ]` = not started, `[~]` = AutoImprove has picked this and is working on it, `[x]` = done. AutoImprove only ever picks `[ ]` items. It will not start a second item until the first PR is merged or explicitly cancelled.

### What to build

- [ ] Create `src/agents/specialists/auto_improve.rs`. On trigger, it reads `ideas.md` from the project root, finds the first `[ ]` item, and marks it `[~]` before doing anything else — this prevents a second run from picking the same idea if the first is still in flight.
- [ ] Create a new git branch named `auto/<idea-slug>` where the slug is the idea text lowercased and spaces replaced with hyphens, trimmed to 50 characters (e.g., `auto/add-fuzzy-search-to-chat-history`). Use the Shell specialist's `run_command` tool to run `git checkout -b <branch>`. If the branch already exists (a previous attempt), check it out instead of creating a new one.
- [ ] Invoke the Code specialist with the idea text as the task, plus a strict implementation prompt that injects the project's development rules: TDD (write the failing test first), zero-warnings policy, and the full verification checklist (`cargo check && cargo clippy -- -D warnings && cargo test && cargo fmt --check`). The Code specialist runs `claude` non-interactively with the full prompt and the project root as the working directory.
- [ ] After the Code specialist returns, run the validation suite via the Shell specialist: `cargo check`, `cargo clippy -- -D warnings`, `cargo test`, `cargo fmt --check`. If any step fails, feed the error output back to the Code specialist and retry (up to 3 attempts, same branch). If all 3 attempts fail, mark the idea back to `[ ]` in `ideas.md`, delete the branch, and emit an `AppEvent::Token` message explaining what was tried and what failed.
- [ ] If validation passes: commit the changes using the Shell specialist (`git add -p` is not safe to automate — use `git add src/ tests/` explicitly, then `git commit -m "<conventional commit message>"`), push the branch (`git push -u origin <branch>`), and create a pull request using `gh pr create` with a description that includes the idea text, the validation results (test count, clippy output), and a checklist of the development rules that were followed.
- [ ] Emit `AppEvent::Token` with the PR URL so it appears in the TUI chat, e.g.: `"AutoImprove opened PR #42: Add fuzzy search to chat history → https://github.com/..."`.
- [ ] PR comment polling: schedule a recurring check every 4 hours (via the Scheduling specialist) for any open AutoImprove PR. Use `gh pr view <number> --json reviews,comments` to check for new review comments since the last poll. If there are unaddressed comments, run the Code specialist again on the same branch with the comments injected as context, re-run validation, commit, and push. Emit a TUI notification that the PR was updated.
- [ ] Add a guard: if the user has manually pushed commits to the branch since AutoImprove last touched it, skip the automated update pass and emit a warning instead. Check with `git log origin/<branch> --not auto-improve-last-sha` — if any commits are found that AutoImprove did not make, do not overwrite them.
- [ ] Tests: unit tests for `ideas.md` parsing (pick first `[ ]`, skip `[~]` and `[x]`, handle empty file); integration test using mock shell commands (echo stubs for git and gh) that verifies the full flow: pick idea → mark in-progress → branch → validate → commit → PR. Use `wiremock` for any GitHub API calls.

> **New files:** `src/agents/specialists/auto_improve.rs`, `tests/auto_improve_integration.rs`
> **Changed:** `src/agents/specialists/scheduling.rs` (register midnight trigger on startup)

### Verification

```bash
cargo test --test auto_improve_integration
```
Verify: the `[ ]` → `[~]` state transition happens before any git operations; a validation failure after 3 attempts reverts `[~]` → `[ ]`; the PR creation step receives the correct branch name and description; the guard prevents overwriting manual commits.

```bash
cargo run
```
End-to-end test (requires Ollama, `claude` CLI, and `gh` authenticated):

1. Add a small, concrete idea to `ideas.md`:
   ```
   - [ ] Add a `/version` command that prints the app version from Cargo.toml
   ```
2. Type `"run auto improve now"` → AutoImprove should trigger immediately (without waiting for midnight), pick the idea, mark it `[~]`, create a branch, implement it, validate, and open a PR. Watch the TUI for each step.
3. Open the PR on GitHub and leave a review comment asking for a change (e.g., "also print the git commit hash").
4. Wait for the 4-hour poll (or manually trigger `"check auto improve PR comments"`) → AutoImprove should pick up the comment, push a fix, and notify the TUI.
5. Merge the PR → on next midnight run, AutoImprove should detect `ideas.md` still has `[~]` and update it to `[x]`, then move on to the next `[ ]` idea.

---

## Minor Fixes / Features

These can be done in any order, independently of the milestones above:

- [ ] Fix generated logo transparency
- [ ] Fix auto-scrolling (currently broken during streaming)
- [ ] Paste support in input box (bracketed paste mode)
- [ ] Scrollable input for multi-line prompts
- [ ] Config file support (TOML) as alternative to env vars
