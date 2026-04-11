# Agent-L Architecture

## Vision

Enable small local models (7B–14B, running on consumer GPUs) to reliably perform agentic tasks by breaking work into small, focused stages with strict guardrails at every boundary. The thesis: a well-orchestrated chain of 7B models that never drift beats a single 70B model that does.

---

## Three-Layer Pipeline

The core design is a three-layer pipeline. No agent is allowed to call another agent directly; all communication flows through structured data, validated at every boundary.

```
┌──────────────────────────────────────────────────────┐
│  USER INPUT                                          │
└──────────────────┬───────────────────────────────────┘
                   │
                   ▼
┌──────────────────────────────────────────────────────┐
│  PERSONA LAYER  (Frontend Agent)                     │
│  • Maintains personality / tone system prompt        │
│  • Compresses old turns to prevent context drift     │
│  • Injects relevant episodic memory via retrieval    │
│  • Final response formatting and streaming to UI     │
│  Model: conversational (gemma3, llama3)              │
└──────────────────┬───────────────────────────────────┘
                   │ compressed context + enriched prompt
                   ▼
┌──────────────────────────────────────────────────────┐
│  AGENT L  (Orchestrator)                             │
│  • Classifies request → structured JSON task plan    │
│  • Output: { intent_type: Enum,                      │
│              steps: [{ agent, task, depends_on? }] } │
│  • Uses last 3–5 turns only (small context)          │
│  • Re-prompts with error on invalid output (max 3x)  │
│  Model: fast/small (mistral, qwen)                   │
└──────────────────┬───────────────────────────────────┘
                   │ ordered task plan
                   ▼
┌──────────────────────────────────────────────────────┐
│  SPECIALISTS  (one per task domain)                  │
│  Each specialist:                                    │
│  • Gets task-specific context only (no full history) │
│  • Has a fixed, declared tool list — no exceptions   │
│  • Uses ReAct loop: Thought → Action → Observation   │
│  • Max N steps; hard stop + fallback at limit        │
│  • Outputs structured result, validated before use   │
│                                                      │
│  chat     — conversational / creative, no tools      │
│  code      — code generation, explanation, review    │
│  search    — web or local file retrieval             │
│  shell     — sandboxed command execution             │
│  calendar  — date/time and scheduling tasks          │
│  memory    — explicit memory read/write operations   │
└──────────────────┬───────────────────────────────────┘
                   │ validated structured result(s)
                   ▼
         back to Persona Layer for synthesis
```

### Multi-step (compound) task handling

When a single request requires more than one specialist (e.g., "search for X and send me a summary"), Agent L returns an **ordered task plan** instead of a single routing decision:

```json
{
  "intent_type": "Task",
  "steps": [
    { "agent": "Search", "task": "find recent AI news" },
    { "agent": "Shell",  "task": "send summary email", "depends_on": 0 }
  ]
}
```

Specialists run in sequence. The output of each step is injected as context into the next step that declares a dependency on it. The Persona layer receives all step results and synthesizes the final response.

**Hard limit:** max 5 steps per plan. If Agent L returns more, the circuit breaker fires and the Persona asks the user to break the request into smaller parts.

### ReAct loop (specialist execution)

Each specialist that uses tools runs a **ReAct loop** (`src/tools/executor.rs`). The loop alternates between model turns and tool execution:

```
Round 1 — model turn:
  Thought: I need to find the current president.
  ToolCall: web_search {"query": "current US president 2025"}

Round 1 — executor:
  → Calls Tavily → gets "Donald Trump is the 47th president..."
  → Injects Observation into messages, including a quoted snippet:
    "The search result says: 'Donald Trump is the 47th and current president
     since January 20, 2025.' — your answer MUST use this."

Round 2 — model turn:
  FinalAnswer: Donald Trump is the 47th president. Source: https://...
```

**Premature FinalAnswer guard.** Small local models (e.g. gemma3) sometimes emit a `ToolCall` and a `FinalAnswer` in the same response — the tool call is a formality and the answer comes from training data, before the model has seen the search result. The executor detects this: if a `FinalAnswer` appears in the same turn as a `ToolCall`, the `FinalAnswer` is discarded, the tool runs, and the observation is injected. The model is forced to answer in a fresh round where the search result is the only new information in context.

**Observation injection.** After each tool call the executor appends a user message containing:
1. The full tool output (prefixed `[exit:N]`)
2. The first `Snippet:` line quoted back explicitly — making it structurally hard for the model to ignore
3. A strong reminder: "Your FinalAnswer MUST copy the exact names and facts from the Observation. Do NOT use your training knowledge."

**Circuit breaker.** If the model reaches `MAX_STEPS` (10) without a `FinalAnswer`, the loop returns a structured error instead of looping forever.

### Why this prevents drift

- **Constrained outputs at every boundary** — agents communicate via JSON schemas, not prose. Invalid tokens are masked via GBNF grammar sampling (Ollama supports this natively). A model cannot hallucinate a field that isn't in the schema.
- **Small context per agent** — each agent sees only what it needs. Agent L does not see the full conversation. Specialists do not see other specialists' outputs. This prevents compounding errors.
- **Retry with error feedback** — if an agent produces invalid output, the error is fed back into its next prompt (max 3 retries, then fallback). Small models respond well to explicit error correction.
- **Hard step limits** — specialists using the ReAct loop have a maximum step count (configurable, default 10). Beyond this a circuit breaker fires, returning a structured error to the persona layer.
- **Conversation compression** — the persona layer periodically summarizes old turns (every N tokens) so the context window never fills with stale content that causes behavioral drift.

### Guardrails summary (from 2024–2025 research)

| Problem | Mitigation |
|---|---|
| Hallucinated tool calls | GBNF grammar sampling; validate args before execution |
| Inter-agent misalignment | Typed boundaries; no free-text between agents |
| Context drift over time | Conversation compression; episodic memory; goal reminders |
| Compounding errors | Validate output at every boundary; don't pass prose between agents |
| Prompt injection | Strict input delimiters; semantic validation of tool args |
| Runaway loops | Hard step limit + circuit breaker per specialist |
| Compound task sprawl | Ordered task plan with max 5-step limit; each step validated independently |
| Model hallucination on facts | `intent_type: Factual` always routes to Search — model's internal knowledge is never trusted for real-world state |
| Premature FinalAnswer before search result | ReAct executor discards any `FinalAnswer` emitted in the same turn as a `ToolCall`; the model must answer again after seeing the observation |

---

## Use Case Scenarios

These illustrate how the pipeline handles real requests end-to-end.

### 1. Factual question — always grounded via Search
> "What's the capital of France?"

The model's internal knowledge can be wrong or stale. Agent L classifies this as `intent_type: Factual` — any question whose answer depends on real-world state routes to the Search specialist, not Chat.

```
Persona → Agent L → { intent_type: "Factual", steps: [{ agent: "Search", task: "capital of France" }] }
         → Search Specialist (web lookup) → Persona formats verified answer
```

**`intent_type` routing rules:**

| intent_type      | Routes to          | Why |
|---|---|---|
| `Factual`        | Search             | Real-world state; model knowledge may be wrong/stale |
| `Conversational` | Chat               | Greetings, opinions, subjective — no lookup needed |
| `Creative`       | Chat               | Writing, brainstorming — no ground truth |
| `Task`           | Shell/Calendar/etc | Action in the world |

### 2. Code explanation (single specialist)
> "Explain what startup.rs does"

```
Persona → Agent L → { steps: [{ agent: "Code", task: "explain startup.rs" }] }
         → Code Specialist reads file, returns structured explanation → Persona formats
```

### 3. Current events search
> "What's the latest on Rust async runtimes?"

```
Persona → Agent L → { intent_type: "Factual", steps: [{ agent: "Search", task: "Rust async runtimes 2026" }] }
         → Search Specialist (Tavily when TAVILY_API_KEY set, else DuckDuckGo) → returns citations → Persona writes prose summary
```

### 4. Compound task: search then email
> "Find the latest AI news and send me a summary email"

```json
{ "intent_type": "Task", "steps": [
    { "agent": "Search", "task": "latest AI news" },
    { "agent": "Shell",  "task": "send summary email via sendmail", "depends_on": 0 }
]}
```
Search result is injected into the Shell step's context. Persona confirms completion.

### 5. Memory-informed scheduling
> "Schedule a meeting with John like we did last week"

```json
{ "intent_type": "Task", "steps": [
    { "agent": "Memory",   "task": "recall last meeting with John" },
    { "agent": "Calendar", "task": "schedule meeting with same params", "depends_on": 0 }
]}
```

### 6. Run tests and explain failures
> "Run the tests and tell me what failed"

```json
{ "intent_type": "Task", "steps": [
    { "agent": "Shell", "task": "cargo test" },
    { "agent": "Code",  "task": "explain test failures from output", "depends_on": 0 }
]}
```

---

## Memory Architecture

Three layers, modeled on cognitive memory research:

```
Working Memory       — current context window (what the model sees right now)
Episodic Memory      — SQLite log: turns, tool calls, corrections, timestamps
Semantic Memory      — consolidated facts: "user prefers X", "project Y uses Z"
```

On each turn, the persona layer:
1. Queries semantic memory for relevant facts (BM25 + embedding search)
2. Pages those facts into the working memory prompt
3. Appends the current turn to episodic memory
4. Runs a consolidation pass: if 3+ episodes agree on a preference, promote to semantic

This gives the assistant persistent knowledge of the user across sessions without blowing the context window.

---

## Repository Structure

Current state (M1–M7 complete) and planned additions (M8+) are marked below.

```
src/
  main.rs             — ✅ event loop, keyboard, terminal I/O
  lib.rs              — ✅ re-exports all modules for integration tests
  app.rs              — ✅ AppEvent (Token, StreamDone, RouteDecision, ScopeDecision), StartupState
  config.rs           — ✅ OLLAMA_HOST/PORT/MODEL, SearchProvider enum (SEARCH_PROVIDER env var); planned: per-agent model config, TOML (M10)
  ollama.rs           — ✅ fetch_ollama_stream + post_json
  startup.rs          — ✅ /api/tags + /api/ps health checks
  ui.rs               — ✅ markdown, OSC 8 hyperlinks, route banners; planned: token budget display

  agents/
    mod.rs            — ✅ Agent trait, AgentErrorKind, call_with_retry
    orchestrator.rs   — ✅ Agent L: intent_type classification, TaskPlan (max 5 steps)
    persona.rs        — ✅ system prompt, goal reminders; planned: memory injection (M9)
    compression.rs    — ✅ conversation summarization
    schema.rs         — ✅ require_field / require_str helpers
    specialists/
      mod.rs          — ✅ run_plan(): step execution, depends_on chaining, fallback injection
      chat.rs         — ✅ Conversational/Creative: no tools, streams tokens to UI
      code.rs         — ✅ one-off code via claude CLI; project-scope limitation message
      search.rs       — ✅ Factual: DuckDuckGo + local ripgrep, citation formatting
      shell.rs        — planned M8: sandboxed shell commands, confirmation gate
      calendar.rs     — planned (future): date/time and scheduling
      memory.rs       — planned M9: explicit read/write/forget, wraps memory/

  tools/
    mod.rs            — ✅ Tool trait, ToolRegistry
    executor.rs       — ✅ ReAct loop: Thought → ToolCall → Observation → FinalAnswer; circuit breaker (10 steps); premature-FinalAnswer guard
    search_tools.rs   — ✅ web_search dispatcher (Tavily → Brave → DDG fallback) + local_search (ripgrep)
    tavily_search.rs  — ✅ Tavily Search API backend (requires TAVILY_API_KEY)
    claude_code.rs    — ✅ claude CLI subprocess runner
    code_tools.rs     — planned M8+: write to file, run snippet in sandbox
    shell_tools.rs    — planned M8: command execution, sandbox enforcement

  memory/             — planned M9
    mod.rs
    episodic.rs       — SQLite log of turns, tool calls, corrections
    semantic.rs       — consolidated facts ("user prefers X")
    retrieval.rs      — BM25 keyword + optional embedding search

tests/
  startup_integration.rs        — ✅
  ollama_integration.rs         — ✅
  orchestrator_integration.rs   — ✅
  pipeline_integration.rs       — ✅
  search_integration.rs         — ✅
  memory_integration.rs         — planned M9
  live/
    live_pipeline.rs            — ✅ live tests against real Ollama (#[ignore] by default)

config.toml                     — planned M10: TOML alternative to env vars; model per agent role

.env.example                    — ✅ template for OLLAMA_* and search API keys
```

### Search backend configuration (M7.6)

The `web_search` tool dispatches to one of three backends based on environment variables:

| `SEARCH_PROVIDER` | Required env var | Notes |
|---|---|---|
| `tavily` (recommended) | `TAVILY_API_KEY` | Live web results; 1,000 free credits/month |
| `brave` | `BRAVE_API_KEY` | Alternative live search |
| *(default)* | — | DuckDuckGo Instant Answer API; no key; may return stale Wikipedia data for current-events queries |

Set `SEARCH_PROVIDER=tavily` and `TAVILY_API_KEY=tvly-...` in `.env` to enable Tavily. When `TAVILY_API_KEY` is absent the tool returns an error rather than silently falling back, so misconfiguration is obvious. Copy `.env.example` to `.env` to get started.

---

## Model Recommendations (local, Ollama-compatible)

| Role | Recommended | Why |
|---|---|---|
| Persona / Chat | `gemma3:12b`, `llama3.1:8b` | Good instruction following, conversational |
| Agent L (Orchestrator) | `qwen2.5:7b`, `mistral:7b` | Fast, strong structured output |
| Code Specialist | `qwen2.5-coder:7b`, `deepseek-coder-v2` | Fine-tuned for code |
| General Specialist | `qwen3:14b` | Best tool calling at this size (0.971 F1) |

---

## References

Architecture patterns drawn from:
- ReAct (Yao et al., 2022) — interleaved reasoning and acting
- Reflexion (Shinn et al., 2023) — self-critique via linguistic reflection
- MAST failure taxonomy (2025) — 14 identified failure modes in multi-agent systems
- Agent Drift study (arxiv 2601.04170) — behavioral degradation quantified; compression mitigates it
- Constrained decoding for agents (GBNF, Ollama structured outputs, SGLang NeurIPS 2024)
- LangGraph hierarchical agent teams pattern — ported conceptually to Rust state machine
