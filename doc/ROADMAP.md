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

- [ ] Create `src/agents/persona.rs` — system prompt (personality/tone), handles all outbound prompts to Agent L and inbound results from specialists
- [ ] Create `src/agents/compression.rs` — when the conversation exceeds a token threshold, summarize the oldest N turns into a single "context summary" block and replace them; the summary is prepended to future prompts
- [ ] Inject a short goal reminder every N turns (e.g., "You are Agent-L, a local personal assistant…") to prevent personality drift over long sessions
- [ ] Support `PERSONA_SYSTEM_PROMPT` env var to override the built-in personality — useful for customizing the assistant without changing code

> **New files:** `src/agents/persona.rs`, `src/agents/compression.rs`

### Verification

```bash
cargo test agents::persona
cargo test agents::compression
```
Compression test: create a conversation that exceeds the token threshold, run it through the compressor, and assert that the output has fewer turns and begins with a summary block. The original content should still be recoverable as a summary — spot-check by asserting key words appear in the compressed output.

```bash
PERSONA_SYSTEM_PROMPT="You are Jarvis, a dry British assistant." cargo run
```
Start the app, send any message, and verify the assistant's tone matches the custom prompt. Then restart without the env var and confirm it falls back to the default personality.

```bash
cargo run
```
Send 20+ short messages in a row. After the compression threshold is crossed, the assistant should still answer questions accurately about early messages (the summary is working). If it loses context entirely, compression is discarding too much — lower the aggressiveness or check the summary prompt.

---

## M4 — First Specialist: Chat

The Chat specialist is the simplest — it handles Conversational and Creative intents with no tools. Getting this working end-to-end proves the full pipeline: Persona → Agent L → Specialist → back to Persona.

- [ ] Create `src/agents/specialists/mod.rs` — define the `Specialist` trait and the step runner loop: iterate over the task plan's steps, execute each specialist, inject outputs for `depends_on` steps
- [ ] Create `src/agents/specialists/chat.rs` — receives a task string, streams tokens directly to the UI (same as the current direct Ollama call, but now invoked by the step runner)
- [ ] Wire everything together in `App`: user message → Persona → Agent L → step runner → Chat specialist → Persona → UI
- [ ] Write `tests/pipeline_integration.rs` — end-to-end test using wiremock for both the Ollama calls (Agent L classification + Chat response); verify the full routing cycle

> **New files:** `src/agents/specialists/mod.rs` (Specialist trait, step runner), `src/agents/specialists/chat.rs`, `tests/pipeline_integration.rs`

### Verification

```bash
cargo test --test pipeline_integration
```
The end-to-end test uses wiremock to simulate both Ollama calls (Agent L classification + Chat response). Verify the test asserts the full sequence: message sent → Agent L called with last N turns → Chat specialist called with the task string → response streamed to UI.

```bash
cargo run
```
This is the first milestone where the full pipeline is live. With Ollama running:
- Type `"tell me a joke"` → response should arrive and stream normally, same as before, but now routed through the pipeline
- Type `"hi there"` → same — Conversational, routes to Chat
- Verify the status line shows `Agent L → Chat` during the response, and clears when done
- Quit and restart — confirm startup still works correctly (the startup flow should be unaffected)

The behavior should be identical to pre-M4 from the user's perspective. If responses are slower, Agent L is adding latency — check that it's using a small/fast model.

---

## M5 — Tool Call Infrastructure

Specialists that need to interact with the world (search the web, run shell commands, read files) do so through tools. The ReAct loop governs how a specialist decides which tool to call, observes the result, and decides what to do next.

- [ ] Define the `Tool` trait in `src/tools/mod.rs`: `name()`, `description()`, `schema()` (JSON schema for args), `execute(args) -> Result<String>`
- [ ] Create `src/tools/executor.rs` — implements the ReAct loop: the specialist outputs `Thought`, `ToolCall`, or `FinalAnswer`; the executor parses it, validates args against the tool's schema, calls `execute()`, appends the `Observation` to the prompt, and repeats; hard stop at 10 steps with a structured error returned to Persona
- [ ] Validate tool args against the JSON schema before calling `execute()` — never pass unvalidated user-influenced data to a tool
- [ ] Add `AppEvent::ToolCall(name, args)` and `AppEvent::ToolResult(name, result)` so the UI can show what tools were used

> **New files:** `src/tools/mod.rs` (Tool trait), `src/tools/executor.rs` (ReAct loop + circuit breaker)
> **Changed:** `src/app.rs` (AppEvent::ToolCall, AppEvent::ToolResult), `src/lib.rs` (pub mod tools)

### Verification

```bash
cargo test tools
```
Unit tests for the executor — no Ollama needed. Verify:
- A mock tool that always succeeds completes the loop and returns `FinalAnswer`
- A mock tool that always fails still terminates at the step limit (not an infinite loop)
- A tool call with args that fail schema validation returns an error observation rather than calling `execute()`
- The circuit breaker fires at exactly 10 steps and returns a structured error (not a panic)

```bash
cargo test -- --nocapture 2>&1 | grep -i "tool"
```
Spot-check that `ToolCall` and `ToolResult` log lines appear during executor tests, confirming the events are being emitted. The actual UI display comes in M6+, but the events should already be flowing through `AppEvent`.

---

## M6 — Specialist: Code

The Code specialist handles code generation, explanation, and review. It uses the ReAct loop from M5 to call code-specific tools.

- [ ] Create `src/agents/specialists/code.rs` — receives a task (e.g., "explain startup.rs", "write a function that…"), uses tools to read files and run snippets, returns a structured result
- [ ] Create `src/tools/code_tools.rs` with tools: `read_file` (read a path relative to working dir), `run_snippet` (execute a small code block in a sandbox, capture stdout/stderr), `write_file` (write content to a path)
- [ ] Format code output as fenced code blocks in the ratatui UI with a language label
- [ ] Tests: use a mock tool executor to verify the ReAct loop terminates correctly on success and on step limit

> **New files:** `src/agents/specialists/code.rs`, `src/tools/code_tools.rs`

### Verification

```bash
cargo test agents::specialists::code
```
Mock executor test: verify the specialist calls `read_file` when given a task like "explain this file", and that the loop terminates after `FinalAnswer` is reached.

```bash
cargo run
```
With Ollama running:
- Type `"explain what startup.rs does"` → status shows `Agent L → Code`; you should see a brief `read_file(src/startup.rs)` indicator in the UI, then a structured explanation of the startup logic
- Type `"what does the App struct contain?"` → should similarly read `app.rs` and describe the fields
- Type `"write a hello world function in Rust"` → should respond with a fenced Rust code block (` ```rust `) with a language label, not plain text
- Verify code blocks are visually distinct from prose in the TUI (syntax label, different color or border)

---

## M7 — Specialist: Search

The Search specialist handles all `Factual` intent types. It is the only specialist that may access the web. It never generates an answer from model knowledge alone — it always calls a search tool and returns citations.

- [ ] Create `src/agents/specialists/search.rs` — calls search tools, returns structured results (title, url, snippet) rather than prose; Persona formats the final answer
- [ ] Create `src/tools/search_tools.rs` with two tools:
  - `web_search(query)` — DuckDuckGo instant answer API (no API key required)
  - `local_search(query, path)` — ripgrep wrapper for searching within local files
- [ ] The specialist must always use at least one tool call — it cannot short-circuit to a direct answer
- [ ] Tests: `tests/search_integration.rs` using wiremock to simulate DuckDuckGo responses; verify citation format

> **New files:** `src/agents/specialists/search.rs`, `src/tools/search_tools.rs`, `tests/search_integration.rs`

### Verification

```bash
cargo test --test search_integration
```
Wiremock tests pass: verify a mocked DuckDuckGo response is parsed into structured citations (title, url, snippet), and that the specialist returns those — not a free-form prose answer.

```bash
cargo run
```
With Ollama running:
- Type `"what is the capital of France?"` → status shows `Agent L → Search (Factual)`; the response should include a citation (source URL or title), not just "Paris"
- Type `"what's the latest news in AI?"` → same routing; response cites sources
- Type `"how are you today?"` → this should **not** route to Search (it's Conversational) — confirm it goes to Chat instead. This is the key regression check: factual routing should not over-trigger.
- Type `"search my project files for the word 'retry'"` → should use `local_search` tool, return matching file paths and line snippets

---

## M8 — Specialist: Shell

The Shell specialist runs sandboxed commands on the user's machine. Because this is dangerous, it has an explicit confirmation gate — the UI must show the command and get user approval before execution.

- [ ] Create `src/agents/specialists/shell.rs` — receives a task, determines the command to run, sends it to the confirmation gate before executing
- [ ] Create `src/tools/shell_tools.rs` — `run_command(cmd, args)`: executes with no network access and no writes outside the working directory by default; captures stdout/stderr and streams back to Persona
- [ ] Confirmation gate: add an `AppEvent::AwaitingConfirmation(command)` that pauses execution and shows the command in the UI with Y/N prompt; only proceed on Y
- [ ] Configurable allow/deny lists for commands in env vars or `config.toml`

> **New files:** `src/agents/specialists/shell.rs`, `src/tools/shell_tools.rs`

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
- [ ] Graceful degradation: if a specialist fails all 3 retries, fall back to the Chat specialist and include an error note in the Persona's synthesis prompt
- [ ] Create `src/agents/specialists/calendar.rs` — date/time parsing and scheduling (deferred from earlier milestones)

> **New files:** `src/agents/specialists/calendar.rs`, `config.toml` (optional)
> **Changed:** `src/ui.rs` (trace panel, token budget display), `src/config.rs` (per-agent model config, TOML support), `doc/STRUCTURE.md` (update to reflect final layout)

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

## Minor Fixes / Features

These can be done in any order, independently of the milestones above:

- [ ] Fix generated logo transparency
- [ ] Fix auto-scrolling (currently broken during streaming)
- [ ] Paste support in input box (bracketed paste mode)
- [ ] Scrollable input for multi-line prompts
- [ ] Config file support (TOML) as alternative to env vars
