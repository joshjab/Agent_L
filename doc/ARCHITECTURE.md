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
         → Search Specialist (DuckDuckGo) → returns citations → Persona writes prose summary
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

## Final Repository Structure

What the repo looks like when all milestones are complete:

```
src/
  main.rs             — unchanged: event loop, keyboard, terminal I/O
  lib.rs              — re-exports all modules (agents/, tools/, memory/) for integration tests
  app.rs              — extended: AppEvent gains ToolCall, ToolResult, RouteDecision
  config.rs           — extended: per-agent model config, TOML file support
  ollama.rs           — unchanged: low-level HTTP streaming to Ollama (used by all agents)
  startup.rs          — unchanged: /api/tags + /api/ps health checks
  ui.rs               — extended: agent trace panel, token budget display

  agents/
    mod.rs            — Agent trait (prompt, parse, retry); shared retry + schema logic
    persona.rs        — Persona layer: system prompt, context compression, memory injection
    orchestrator.rs   — Agent L: classifies intent_type, builds ordered task plan (max 5 steps)
    compression.rs    — conversation summarization (triggered when token budget fills)
    specialists/
      mod.rs          — Specialist trait; step execution loop; depends_on chaining
      chat.rs         — Conversational/Creative: no tools, streams tokens directly to UI
      code.rs         — Code generation, explanation, review; uses code_tools
      search.rs       — Factual queries: web + local search; always used for facts, never model memory
      shell.rs        — Sandboxed shell commands with confirmation gate and allow/deny lists
      calendar.rs     — Date/time parsing and scheduling
      memory.rs       — Explicit read/write/forget; thin wrapper around memory/

  tools/
    mod.rs            — Tool trait: name, description, JSON schema, execute()
    executor.rs       — ReAct loop: Thought → ToolCall → Observation; hard step limit + circuit breaker
    search_tools.rs   — DuckDuckGo web search + ripgrep local file search
    code_tools.rs     — Syntax highlight, run snippet in sandbox, write to file
    shell_tools.rs    — Command execution, output capture, sandbox enforcement

  memory/
    mod.rs            — Unified read/write API (used by Persona + Memory specialist)
    episodic.rs       — SQLite log of all turns, tool calls, corrections, timestamps
    semantic.rs       — Key-value facts ("user prefers X"), consolidated from episodic
    retrieval.rs      — BM25 keyword search + optional embedding-based semantic search

tests/
  startup_integration.rs        — existing (unchanged)
  ollama_integration.rs         — existing (unchanged)
  orchestrator_integration.rs   — wiremock: single-step + multi-step classification
  pipeline_integration.rs       — end-to-end: Persona → Agent L → Specialist → Persona
  search_integration.rs         — wiremock: DuckDuckGo responses, citation formatting
  memory_integration.rs         — SQLite episodic log, semantic consolidation

config.toml (M10)               — TOML alternative to env vars; model per agent role
```

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
